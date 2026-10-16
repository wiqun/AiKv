//! AiDb 存储引擎适配器: 把同步 `aidb::DB` 包装为 async `StorageAdapter`.
//! 普通请求的 DB 操作经 `spawn_blocking`, 避免 LSM 写盘或扫描堵住 tokio worker.
//! 脚本线程已由 `DirectStorageIo` 标记时, 调用在该线程上直接执行.
//!
//! # 持久化模式
//!
//! - 物理 key 编码: `encode_key(db, user_key)` = `{db_index}:{user_key}` (ASCII);
//!   `decode_key` 反解, `prefix_end` 求范围扫描上界 (`for_each_prefix` 使用).
//! - 值: 命令层 blob = `postcard(StoredValue)` (与 `storage/dump.rs` 同类型, DUMP 多
//!   1 字节 version 前缀).
//! - `write_batch` 组装 aidb `WriteBatch` 一次提交 (原子); `clear` 亦走批量 delete.
//! - 持久化: `flush` → `DB::flush`; `create_checkpoint` → `Checkpoint::create`;
//!   `close` → `DB::close`.
//!
//! # Invariant
//!
//! - key 编码 `{db_index}:{user_key}` 是全部范围操作 (scan / clear / delete_range) 的
//!   前缀隔离基础, 不得变更.
//! - 单库多 DB 语义: 16 个逻辑 DB 共享一个 `aidb::DB`, 靠 key 前缀隔离
//!   (非 oldmain 的 16 个独立 DB 目录).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aidb::config::Options;
use aidb::{Checkpoint, WriteBatch, WriteOp as DbWriteOp, DB};
use async_trait::async_trait;
use tokio::task;

use crate::error::{Error, Result};
use crate::storage::adapter::{AdapterWriteOp, StorageAdapter};
use crate::storage::types::StorageEngineKind;

/// AiDb 扁平 KV 引擎
pub struct AiDbEngine {
    pub db: Arc<DB>,
}

impl AiDbEngine {
    /// 构造真实 key: `{db_index}:{user_key}`
    pub fn encode_key(db: usize, user_key: &[u8]) -> Vec<u8> {
        let mut key = Vec::with_capacity(8 + user_key.len());
        key.extend_from_slice(db.to_string().as_bytes());
        key.push(b':');
        key.extend_from_slice(user_key);
        key
    }

    pub fn decode_key(encoded: &[u8]) -> Option<(usize, Vec<u8>)> {
        let pos = encoded.iter().position(|&b| b == b':')?;
        let db_str = std::str::from_utf8(&encoded[..pos]).ok()?;
        let db = db_str.parse().ok()?;
        Some((db, encoded[pos + 1..].to_vec()))
    }

    pub(crate) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
        let mut end = prefix.to_vec();
        while let Some(last) = end.last_mut() {
            if *last == 0xff {
                end.pop();
            } else {
                *last += 1;
                return Some(end);
            }
        }
        None
    }

    /// 生产 / CLI 默认: `Options::default()` preset.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        Self::open_with_options(path, crate::storage::aidb_options::server_db_options(false))
    }

    /// 单测 preset: 小 memtable、关 background_compaction.
    pub fn open_for_testing(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        Self::open_with_options(path, crate::storage::aidb_options::testing_db_options())
    }

    pub fn open_with_options(path: impl AsRef<Path>, mut opts: Options) -> Result<Arc<Self>> {
        opts.create_if_missing = true;
        opts.validate().map_err(|e| Error::Storage(e.to_string()))?;
        let db = DB::open(path, opts).map_err(|e| Error::Storage(e.to_string()))?;
        Ok(Arc::new(Self { db }))
    }

    pub fn approximate_data_memory_bytes(&self) -> u64 {
        self.db.approximate_memory_bytes()
    }

    async fn blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(Arc<DB>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = Arc::clone(&self.db);
        if crate::storage::direct::storage_io_is_direct() {
            return f(db);
        }
        task::spawn_blocking(move || f(db))
            .await
            .map_err(|e| Error::Storage(e.to_string()))?
    }
}

#[async_trait]
impl StorageAdapter for AiDbEngine {
    fn get_sync(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        self.db.get(&key).map_err(|e| Error::Storage(e.to_string()))
    }

    fn write_batch_sync(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        let mut wb = aidb::WriteBatch::new();
        for op in batch {
            match op {
                AdapterWriteOp::Put { key, value } => wb.put(key, value),
                AdapterWriteOp::Delete { key } => wb.delete(key),
            }
        }
        self.db
            .write(&wb)
            .map_err(|e| Error::Storage(e.to_string()))
    }

    fn last_in_range_sync(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let mut iter = self
            .db
            .scan(Some(start.as_slice()), Some(end.as_slice()))
            .map_err(|e| Error::Storage(e.to_string()))?;
        iter.seek_to_last();
        match (iter.key(), iter.value()) {
            (Some(key), Some(value)) => Ok(Some((key.to_vec(), value.to_vec()))),
            _ => Ok(None),
        }
    }

    fn for_each_prefix_sync(
        &self,
        prefix: Vec<u8>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        let end = Self::prefix_end(&prefix);
        let iter = self
            .db
            .scan(Some(prefix.as_slice()), end.as_deref())
            .map_err(|e| Error::Storage(e.to_string()))?;
        for item in iter {
            let (key, value) = item.map_err(|e| Error::Storage(e.to_string()))?;
            f(key, value)?;
        }
        Ok(())
    }

    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        self.blocking(move |db| db.get(&key).map_err(|e| Error::Storage(e.to_string())))
            .await
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.blocking(move |db| {
            db.put(&key, &value)
                .map_err(|e| Error::Storage(e.to_string()))
        })
        .await
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
        self.blocking(move |db| {
            let existed = db
                .get(&key)
                .map_err(|e| Error::Storage(e.to_string()))?
                .is_some();
            db.delete(&key).map_err(|e| Error::Storage(e.to_string()))?;
            Ok(existed)
        })
        .await
    }

    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<()> {
        self.blocking(move |db| {
            let mut wb = WriteBatch::new();
            for op in ops {
                match op {
                    AdapterWriteOp::Put { key, value } => wb.put(key, value),
                    AdapterWriteOp::Delete { key } => wb.delete(key),
                }
            }
            db.write(&wb).map_err(|e| Error::Storage(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn exists(&self, key: Vec<u8>) -> Result<bool> {
        self.blocking(move |db| {
            db.key_exists(&key)
                .map_err(|e| Error::Storage(e.to_string()))
        })
        .await
    }

    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        self.blocking(move |db| {
            let mut wb = WriteBatch::new();
            for op in batch {
                match op {
                    AdapterWriteOp::Put { key, value } => {
                        wb.put(key, value);
                    }
                    AdapterWriteOp::Delete { key } => {
                        wb.delete(key);
                    }
                }
            }
            db.write(&wb).map_err(|e| Error::Storage(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn for_each_prefix(
        &self,
        prefix: Vec<u8>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        self.blocking(move |db| {
            let end = AiDbEngine::prefix_end(&prefix);
            let iter = db
                .scan(Some(prefix.as_slice()), end.as_deref())
                .map_err(|e| Error::Storage(e.to_string()))?;
            for item in iter {
                let (k, v) = item.map_err(|e| Error::Storage(e.to_string()))?;
                f(k, v)?;
            }
            Ok(())
        })
        .await
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        self.blocking(move |db| {
            db.delete_range(&start, &end)
                .map_err(|e| Error::Storage(e.to_string()))
        })
        .await
    }

    async fn last_in_range(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.blocking(move |db| {
            let mut iter = db
                .scan(Some(start.as_slice()), Some(end.as_slice()))
                .map_err(|e| Error::Storage(e.to_string()))?;
            iter.seek_to_last();
            match (iter.key(), iter.value()) {
                (Some(key), Some(value)) => Ok(Some((key.to_vec(), value.to_vec()))),
                _ => Ok(None),
            }
        })
        .await
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.estimate_entry_count().await? as usize)
    }

    async fn estimate_entry_count(&self) -> Result<u64> {
        self.blocking(move |db| Ok(db.estimate_entry_count())).await
    }

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.len().await? == 0)
    }

    async fn clear(&self) -> Result<()> {
        self.blocking(move |db| {
            let iter = db.iter().map_err(|e| Error::Storage(e.to_string()))?;
            let mut keys = Vec::new();
            for item in iter {
                let (k, _) = item.map_err(|e| Error::Storage(e.to_string()))?;
                keys.push(k);
            }
            let mut wb = WriteBatch::new();
            for key in keys {
                wb.delete(key);
            }
            if !wb.is_empty() {
                let _ = db.write(&wb).map_err(|e| Error::Storage(e.to_string()))?;
            }
            Ok(())
        })
        .await
    }

    async fn flush(&self) -> Result<()> {
        self.blocking(move |db| db.flush().map_err(|e| Error::Storage(e.to_string())))
            .await
    }

    async fn create_checkpoint(&self, dest: &Path) -> Result<PathBuf> {
        let dest = dest.to_path_buf();
        self.blocking(move |db| {
            Checkpoint::create(db.as_ref(), &dest).map_err(|e| Error::Storage(e.to_string()))
        })
        .await
    }

    async fn close(&self) -> Result<()> {
        self.blocking(move |db| db.close().map_err(|e| Error::Storage(e.to_string())))
            .await
    }

    fn engine_kind(&self) -> StorageEngineKind {
        StorageEngineKind::AiDb
    }

    fn approximate_memory_bytes(&self) -> Option<u64> {
        Some(self.db.approximate_memory_bytes())
    }

    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        Some(self.db.statistics())
    }
}

#[expect(dead_code)]
fn db_write_op_to_adapter(op: DbWriteOp) -> AdapterWriteOp {
    match op {
        DbWriteOp::Put { key, value } => AdapterWriteOp::Put { key, value },
        DbWriteOp::Delete { key } => AdapterWriteOp::Delete { key },
    }
}
