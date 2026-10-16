//! `StorageAdapter` trait 与 `KvStorage` 适配层: 把扁平 KV 抽象 (`StorageAdapter`,
//! 底层为 `AiDbEngine` 或 `ClusterDataAdapter`) 包装为命令层多 DB 语义
//! (`KvStorageAdapter`, 实现 `KvStorage`).
//!
//! # Key 布局 (扁平 KV → 多 DB)
//!
//! ```text
//! 物理 key: {db_index}:{user_key}          # ASCII, 例 b"0:mykey"
//!   ├─ db_prefix(db) = encode_key(db, "")  # 每 DB 的扫描前缀
//!   ├─ prefix_end    = 前缀进位            # 范围扫描上界
//!   └─ 值: postcard(StoredValue)            # 类型 + expires_at
//! subkey:  {db_index}:{user_key}\x01H|S|Z|Y... # 大 Hash/Set/ZSet 的 field/member, raw bytes
//! ```
//!
//! 多 DB 完全靠 key 前缀隔离 (`clear`/`keys`/`scan` 依赖 `db_prefix` + `prefix_end`).
//!
//! # Invariant
//!
//! - 两套 WriteOp: `storage::WriteOp` (命令/Lua batch) ≠ `AdapterWriteOp` (扁平 KV);
//!   转换在 `KvStorageAdapter::write_batch` (Put → postcard `StoredValue`).
//! - AiDb key 编码: 物理 key = `{db_index}:{user_key}` (ASCII); `clear`/`keys`/`scan`
//!   依赖 `db_prefix` + `prefix_end`.
//! - String `get`/`set` 仅 String; 非 String → `WRONGTYPE`; Hash/List/Set/ZSet
//!   必须 `get_typed`/`set_typed`.
//! - 惰性 TTL: 读路径 (`load_typed`) 遇过期返回"不存在"并尽力物理删除
//!   (`try_lazy_expire_delete`, 先经 `allow_lazy_expire_delete` 判断是否值得发起).
//! - MGET wrong-type: 非 String 或 missing key 对该位返回 `nil`, 整命令不失败
//!   (对齐 Redis 7, 与 `GET` 的 WRONGTYPE 不同).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::storage::memory::glob_match;
use crate::storage::observation::{PointReadCounter, StorageObservation};
use crate::storage::types::{
    now_ms, BatchWriteHandle, ExpiresCounters, ImmediateWriteHandle, KeyspaceStats, KvStorage,
    ScanResult, StorageEngineKind, StoredValue, ValueType, WriteOp, DB_COUNT, TTL_NO_EXPIRY,
    WRONGTYPE,
};
use crate::storage::watch_registry::WatchRegistry;
use crate::storage::watch_version::{
    decode_version, encode_version, is_watch_meta_user_key, meta_user_key,
};
use crate::storage::AiDbEngine;

/// 底层扁平 KV 写操作 (与 `storage::WriteOp` 不同)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterWriteOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

#[async_trait]
pub trait StorageAdapter: Send + Sync {
    /// 暴露底层引擎的无锁原子统计实例 (若支持)
    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        None
    }

    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>>;
    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()>;
    /// 一次提交多笔写. 集群实现须把 ops 全部送进同一 group 的 `GroupSetBatcher`
    /// 后再等 ack, 避免每条 SET 串行两次 Raft propose (Issue #83).
    /// 默认逐条 `set` / `delete`.
    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<()> {
        for op in ops {
            match op {
                AdapterWriteOp::Put { key, value } => self.set(key, value).await?,
                AdapterWriteOp::Delete { key } => {
                    self.delete(key).await?;
                }
            }
        }
        Ok(())
    }
    /// 一次提交多笔 PUT; 默认转发 `apply_writes`.
    async fn put_many(&self, ops: Vec<(Vec<u8>, Vec<u8>)>) -> Result<()> {
        self.apply_writes(
            ops.into_iter()
                .map(|(key, value)| AdapterWriteOp::Put { key, value })
                .collect(),
        )
        .await
    }
    async fn delete(&self, key: Vec<u8>) -> Result<bool>;
    async fn exists(&self, key: Vec<u8>) -> Result<bool>;
    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()>;
    fn get_sync(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        let _ = key;
        Err(Error::Storage("sync get not supported".into()))
    }
    fn write_batch_sync(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        let _ = batch;
        Err(Error::Storage("sync write_batch not supported".into()))
    }
    fn last_in_range_sync(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let _ = (start, end);
        Err(Error::Storage("sync last_in_range not supported".into()))
    }
    fn for_each_prefix_sync(
        &self,
        prefix: Vec<u8>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        let _ = (prefix, f);
        Err(Error::Storage("sync for_each_prefix not supported".into()))
    }
    async fn enqueue_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<Box<dyn BatchWriteHandle>> {
        self.write_batch(batch).await?;
        Ok(Box::new(ImmediateWriteHandle))
    }
    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()>;

    /// 半开区间 `[start, end)` 里字典序最大的一条. 默认扫前缀后过滤.
    async fn last_in_range(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let best = std::sync::Arc::new(std::sync::Mutex::new(None));
        let best_c = std::sync::Arc::clone(&best);
        let end_c = end.clone();
        self.for_each_prefix(
            start,
            Box::new(move |key, value| {
                if key.as_slice() < end_c.as_slice() {
                    let mut slot = best_c.lock().expect("last key");
                    if slot.as_ref().is_none_or(|(prev, _)| key > *prev) {
                        *slot = Some((key, value));
                    }
                }
                Ok(())
            }),
        )
        .await?;
        Ok(std::sync::Arc::try_unwrap(best)
            .unwrap_or_else(|_| unreachable!("last key still shared"))
            .into_inner()
            .expect("last key"))
    }
    async fn len(&self) -> Result<usize>;
    async fn estimate_entry_count(&self) -> Result<u64> {
        Ok(self.len().await? as u64)
    }
    async fn is_empty(&self) -> Result<bool>;
    async fn clear(&self) -> Result<()>;

    /// 对前缀范围内的每个 (key, value) 调用 f (同步回调).
    /// 回调是同步的, 无法直接执行 async 操作; 若需要 async, 应在回调中收集后再统一处理.
    ///
    /// 默认回退到 `scan_prefix` (兼容只覆盖 `scan_prefix` 的实现者如 MockAdapter).
    /// 要实现流式处理, 应覆盖此方法 (并删除 `scan_prefix` 覆盖, 由本 trait 的 `scan_prefix` 默认实现转发).
    async fn for_each_prefix(
        &self,
        prefix: Vec<u8>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        for (k, v) in self.scan_prefix(&prefix).await? {
            f(k, v)?;
        }
        Ok(())
    }

    /// 扫描前缀范围内的所有 KV 对.
    /// 默认实现基于 `for_each_prefix`. 要提供更高效的收集实现, 可直接覆盖此方法.
    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let out = Arc::new(std::sync::Mutex::new(Vec::new()));
        let out_c = out.clone();
        self.for_each_prefix(
            prefix.to_vec(),
            Box::new(move |k, v| {
                out_c.lock().unwrap().push((k, v));
                Ok(())
            }),
        )
        .await?;
        Ok(Arc::try_unwrap(out).unwrap().into_inner().unwrap())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn create_checkpoint(&self, _dest: &Path) -> Result<PathBuf> {
        Err(Error::Command(
            "ERR Persistence not supported on memory engine".into(),
        ))
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    fn engine_kind(&self) -> StorageEngineKind {
        StorageEngineKind::Memory
    }

    /// 脚本提交是否只写本地引擎. 集群实现要等 Raft, 必须留在脚本线程外面.
    fn local_script_commit(&self) -> bool {
        true
    }

    /// 近似进程内热数据内存 (字节); 仅持久化引擎实现.
    fn approximate_memory_bytes(&self) -> Option<u64> {
        None
    }

    /// 惰性过期清理路径是否应该对 `key` 发起物理删除写.
    ///
    /// 单机/本地引擎总是允许 (无 Raft, 删除即时生效). 集群模式下
    /// (`ClusterDataAdapter`) 只有本节点是该 key 所在 data group 的 Raft leader
    /// 时才允许; 只读副本上惰性过期发现的 key 直接跳过物理删除, 留给 leader 或
    /// 后续写连接清理, 避免只读连接因为一次 GET 就触发 (并且必然失败的) Raft 写.
    fn allow_lazy_expire_delete(&self, _key: &[u8]) -> bool {
        true
    }
}

/// 将 StorageAdapter 包装为命令层 KvStorage
pub struct KvStorageAdapter {
    storage: Arc<dyn StorageAdapter>,
    db_count: usize,
    observation: Option<Arc<StorageObservation>>,
    point_reads: OnceLock<Arc<PointReadCounter>>,
    expires: Arc<ExpiresCounters>,
    watchers: Arc<WatchRegistry>,
}

impl KvStorageAdapter {
    pub async fn open(
        storage: Arc<dyn StorageAdapter>,
        observation: Option<Arc<StorageObservation>>,
    ) -> Result<Arc<Self>> {
        Ok(Self::with_observation(storage, observation))
    }

    pub fn new(storage: Arc<dyn StorageAdapter>) -> Arc<Self> {
        Self::with_observation(storage, None)
    }

    pub fn with_observation(
        storage: Arc<dyn StorageAdapter>,
        observation: Option<Arc<StorageObservation>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage,
            db_count: DB_COUNT,
            observation,
            point_reads: OnceLock::new(),
            expires: Arc::new(ExpiresCounters::new()),
            watchers: WatchRegistry::new(),
        })
    }

    fn check_db(&self, db: usize) -> Result<()> {
        if db >= self.db_count {
            return Err(Error::Command(format!(
                "ERR DB index is out of range (db={db}, max={})",
                self.db_count
            )));
        }
        Ok(())
    }

    fn encode(db: usize, key: &[u8]) -> Vec<u8> {
        AiDbEngine::encode_key(db, key)
    }

    fn decode_user_key(encoded: &[u8]) -> Option<Vec<u8>> {
        AiDbEngine::decode_key(encoded).map(|(_, k)| k)
    }

    fn db_prefix(db: usize) -> Vec<u8> {
        AiDbEngine::encode_key(db, b"")
    }

    async fn get_raw(&self, db: usize, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.check_db(db)?;
        self.storage.get(Self::encode(db, key)).await
    }

    fn deserialize(bytes: &[u8]) -> Result<StoredValue> {
        postcard::from_bytes(bytes).map_err(|e| Error::Storage(format!("postcard decode: {e}")))
    }

    fn serialize(value: &StoredValue) -> Result<Vec<u8>> {
        postcard::to_allocvec(value).map_err(|e| Error::Storage(format!("postcard encode: {e}")))
    }

    async fn read_watch_version(&self, db: usize, user_key: &[u8]) -> Result<u64> {
        if is_watch_meta_user_key(user_key) {
            return Ok(0);
        }
        let meta = meta_user_key(user_key);
        match self.get_raw(db, &meta).await? {
            Some(bytes) => Ok(decode_version(&bytes)),
            None => Ok(0),
        }
    }

    /// 下一次 watch meta 的物理 key 与 8 字节 version. meta key 自身不再 bump.
    async fn next_watch_meta_put(
        &self,
        db: usize,
        user_key: &[u8],
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        if is_watch_meta_user_key(user_key) {
            return Ok(None);
        }
        if !self.watchers.is_watched(db, user_key) {
            return Ok(None);
        }
        let next = self
            .read_watch_version(db, user_key)
            .await?
            .saturating_add(1);
        Ok(Some((
            Self::encode(db, &meta_user_key(user_key)),
            encode_version(next).to_vec(),
        )))
    }

    async fn load_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        let Some(raw) = self.get_raw(db, key).await? else {
            return Ok(None);
        };
        let stored = Self::deserialize(&raw)?;
        if stored.is_expired() {
            if let Some(obs) = &self.observation {
                obs.record_expired_key();
            }
            self.try_lazy_expire_delete(db, key).await;
            return Ok(None);
        }
        Ok(Some(stored))
    }

    /// 惰性过期清理: 逻辑上 key 已过期时始终对调用方返回"不存在", 物理删除只是
    /// 尽力而为的清理动作. 集群模式下, 只读副本连接也会走到这里 (Read + readonly
    /// 允许在副本本地 Execute), 而副本对 Raft group 发起 propose 必然以
    /// NotLeader 失败 —— 之前 `?` 会把这个失败一路传播成 GET 报错, 而不是期望的
    /// nil. 先用 `allow_lazy_expire_delete` 判断是否值得尝试 (副本上直接跳过,
    /// 避免无意义的 propose), 再吞掉任何残余错误作为兜底.
    async fn try_lazy_expire_delete(&self, db: usize, key: &[u8]) {
        let encoded = Self::encode(db, key);
        if !self.storage.allow_lazy_expire_delete(&encoded) {
            return;
        }
        let mut ops = vec![AdapterWriteOp::Delete {
            key: encoded.clone(),
        }];
        if let Ok(Some((mk, mv))) = self.next_watch_meta_put(db, key).await {
            ops.push(AdapterWriteOp::Put { key: mk, value: mv });
        }
        if self.storage.apply_writes(ops).await.is_ok() {
            self.expires.on_ttl_clear(db);
        }
    }

    async fn keys_for_db(&self, db: usize, pattern: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.check_db(db)?;
        let prefix = Self::db_prefix(db);
        let pattern = pattern.to_vec();
        let observation = self.observation.clone();
        let keys = Arc::new(std::sync::Mutex::new(Vec::new()));
        let keys_c = keys.clone();
        let expired = Arc::new(std::sync::Mutex::new(Vec::new()));
        let expired_c = expired.clone();

        self.storage
            .for_each_prefix(
                prefix,
                Box::new(move |encoded, raw| {
                    let Some(user_key) = Self::decode_user_key(&encoded) else {
                        return Ok(());
                    };
                    if is_watch_meta_user_key(&user_key) {
                        return Ok(());
                    }
                    // 跳过 subkey entry (非 StoredValue 编码)
                    let Ok(stored) = Self::deserialize(&raw) else {
                        return Ok(());
                    };
                    if stored.is_expired() {
                        if let Some(obs) = &observation {
                            obs.record_expired_key();
                        }
                        expired_c.lock().unwrap().push(user_key);
                        return Ok(());
                    }
                    if pattern.is_empty() || glob_match(&pattern, &user_key) {
                        keys_c.lock().unwrap().push(user_key);
                    }
                    Ok(())
                }),
            )
            .await?;

        // 延迟清理: for_each_prefix 返回后统一处理过期 key
        let expired = Arc::try_unwrap(expired).unwrap().into_inner().unwrap();
        for key in expired {
            self.try_lazy_expire_delete(db, &key).await;
        }

        Ok(Arc::try_unwrap(keys).unwrap().into_inner().unwrap())
    }

    async fn collect_db_entries(&self, db: usize) -> Result<Vec<(Vec<u8>, StoredValue)>> {
        self.check_db(db)?;
        let prefix = Self::db_prefix(db);
        let out = Arc::new(std::sync::Mutex::new(Vec::new()));
        let out_c = out.clone();

        self.storage
            .for_each_prefix(
                prefix,
                Box::new(move |encoded, raw| {
                    let Some(user_key) = Self::decode_user_key(&encoded) else {
                        return Ok(());
                    };
                    if is_watch_meta_user_key(&user_key) {
                        return Ok(());
                    }
                    // 跳过 subkey entry (非 StoredValue 编码)
                    let Ok(stored) = Self::deserialize(&raw) else {
                        return Ok(());
                    };
                    if !stored.is_expired() {
                        out_c.lock().unwrap().push((user_key, stored));
                    }
                    Ok(())
                }),
            )
            .await?;

        Ok(Arc::try_unwrap(out).unwrap().into_inner().unwrap())
    }

    async fn clear_db(&self, db: usize) -> Result<()> {
        self.check_db(db)?;
        let prefix = Self::db_prefix(db);
        let end = AiDbEngine::prefix_end(&prefix).unwrap_or_else(|| {
            let mut max = prefix.clone();
            max.push(0xff);
            max
        });
        self.storage.delete_range(prefix, end).await?;
        self.expires.reset_db(db);
        Ok(())
    }

    async fn prepare_adapter_batch(
        &self,
        db: usize,
        ops: Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<Vec<AdapterWriteOp>> {
        self.check_db(db)?;
        let mut batch = Vec::with_capacity(ops.len());
        let mut touched: Vec<Vec<u8>> = Vec::with_capacity(ops.len());
        for (key, op) in ops {
            match op {
                WriteOp::PutEngine { value } => {
                    batch.push(AdapterWriteOp::Put {
                        key,
                        value: (*value).clone(),
                    });
                }
                WriteOp::DeleteEngine => {
                    batch.push(AdapterWriteOp::Delete { key });
                }
                WriteOp::Put(value) => {
                    let encoded = Self::encode(db, &key);
                    touched.push(encoded.clone());
                    let stored = StoredValue::string((*value).clone());
                    let bytes = Self::serialize(&stored)?;
                    batch.push(AdapterWriteOp::Put {
                        key: encoded,
                        value: bytes,
                    });
                }
                WriteOp::PutStored(stored) => {
                    let encoded = Self::encode(db, &key);
                    touched.push(encoded.clone());
                    let bytes = Self::serialize(&stored)?;
                    batch.push(AdapterWriteOp::Put {
                        key: encoded,
                        value: bytes,
                    });
                }
                WriteOp::Delete => {
                    let encoded = Self::encode(db, &key);
                    touched.push(encoded.clone());
                    if self.is_zset_header_key(db, &key).await? {
                        self.push_zset_subkey_deletes(db, &key, &mut batch).await?;
                    }
                    batch.push(AdapterWriteOp::Delete { key: encoded });
                }
            }
        }
        for encoded in &touched {
            let Some(user_key) = Self::decode_user_key(encoded) else {
                continue;
            };
            if let Some((mk, mv)) = self.next_watch_meta_put(db, &user_key).await? {
                batch.push(AdapterWriteOp::Put { key: mk, value: mv });
            }
        }
        Ok(batch)
    }

    fn prepare_adapter_batch_sync(
        &self,
        db: usize,
        ops: Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<Vec<AdapterWriteOp>> {
        self.check_db(db)?;
        let mut batch = Vec::with_capacity(ops.len());
        let mut touched: Vec<Vec<u8>> = Vec::with_capacity(ops.len());
        for (key, op) in ops {
            match op {
                WriteOp::PutEngine { value } => {
                    batch.push(AdapterWriteOp::Put {
                        key,
                        value: (*value).clone(),
                    });
                }
                WriteOp::DeleteEngine => {
                    batch.push(AdapterWriteOp::Delete { key });
                }
                WriteOp::Put(value) => {
                    let encoded = Self::encode(db, &key);
                    touched.push(encoded.clone());
                    let stored = StoredValue::string((*value).clone());
                    batch.push(AdapterWriteOp::Put {
                        key: encoded,
                        value: Self::serialize(&stored)?,
                    });
                }
                WriteOp::PutStored(stored) => {
                    let encoded = Self::encode(db, &key);
                    touched.push(encoded.clone());
                    batch.push(AdapterWriteOp::Put {
                        key: encoded,
                        value: Self::serialize(&stored)?,
                    });
                }
                WriteOp::Delete => {
                    let encoded = Self::encode(db, &key);
                    touched.push(encoded.clone());
                    if self.is_zset_header_key_sync(db, &key)? {
                        self.push_zset_subkey_deletes_sync(db, &key, &mut batch)?;
                    }
                    batch.push(AdapterWriteOp::Delete { key: encoded });
                }
            }
        }
        for encoded in &touched {
            let Some(user_key) = Self::decode_user_key(encoded) else {
                continue;
            };
            if let Some((mk, mv)) = self.next_watch_meta_put_sync(db, &user_key)? {
                batch.push(AdapterWriteOp::Put { key: mk, value: mv });
            }
        }
        Ok(batch)
    }

    fn is_zset_header_key_sync(&self, db: usize, key: &[u8]) -> Result<bool> {
        let stored = crate::storage::script_io::ScriptIo::get_typed(self, db, key)?;
        Ok(stored
            .as_ref()
            .and_then(crate::storage::zset_subkey::is_zset_header)
            .is_some())
    }

    fn push_zset_subkey_deletes_sync(
        &self,
        db: usize,
        user_key: &[u8],
        batch: &mut Vec<AdapterWriteOp>,
    ) -> Result<()> {
        let encoded = Self::encode(db, user_key);
        self.push_prefix_deletes_sync(crate::storage::zset_subkey::member_prefix(&encoded), batch)?;
        self.push_prefix_deletes_sync(crate::storage::zset_subkey::score_prefix(&encoded), batch)
    }

    fn push_prefix_deletes_sync(
        &self,
        prefix: Vec<u8>,
        batch: &mut Vec<AdapterWriteOp>,
    ) -> Result<()> {
        let keys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let keys_c = std::sync::Arc::clone(&keys);
        self.storage.for_each_prefix_sync(
            prefix,
            Box::new(move |key, _| {
                keys_c.lock().expect("prefix deletes").push(key);
                Ok(())
            }),
        )?;
        let keys = std::sync::Arc::try_unwrap(keys)
            .unwrap_or_else(|_| unreachable!("prefix delete keys still shared"))
            .into_inner()
            .expect("prefix deletes");
        for key in keys {
            batch.push(AdapterWriteOp::Delete { key });
        }
        Ok(())
    }

    fn read_watch_version_sync(&self, db: usize, user_key: &[u8]) -> Result<u64> {
        if is_watch_meta_user_key(user_key) {
            return Ok(0);
        }
        let meta = meta_user_key(user_key);
        match self.storage.get_sync(Self::encode(db, &meta))? {
            Some(bytes) => Ok(decode_version(&bytes)),
            None => Ok(0),
        }
    }

    fn next_watch_meta_put_sync(
        &self,
        db: usize,
        user_key: &[u8],
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        if is_watch_meta_user_key(user_key) || !self.watchers.is_watched(db, user_key) {
            return Ok(None);
        }
        let next = self
            .read_watch_version_sync(db, user_key)?
            .saturating_add(1);
        Ok(Some((
            Self::encode(db, &meta_user_key(user_key)),
            encode_version(next).to_vec(),
        )))
    }

    fn load_typed_for_script(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        let Some(raw) = self.storage.get_sync(Self::encode(db, key))? else {
            return Ok(None);
        };
        let stored = Self::deserialize(&raw)?;
        if stored.is_expired() {
            if let Some(obs) = &self.observation {
                obs.record_expired_key();
            }
            let encoded = Self::encode(db, key);
            if self.storage.allow_lazy_expire_delete(&encoded)
                && self
                    .storage
                    .write_batch_sync(vec![AdapterWriteOp::Delete { key: encoded }])
                    .is_ok()
            {
                self.expires.on_ttl_clear(db);
            }
            return Ok(None);
        }
        Ok(Some(stored))
    }

    fn ttl_for_script(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        let Some(stored) = self.load_typed_for_script(db, key)? else {
            return Ok(None);
        };
        match stored.expires_at {
            None => Ok(Some(TTL_NO_EXPIRY)),
            Some(expires_at) => {
                let now = now_ms();
                if now >= expires_at {
                    Ok(None)
                } else {
                    Ok(Some((expires_at - now) as i64))
                }
            }
        }
    }

    fn prefix_last_for_script(&self, prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let mut end = prefix.clone();
        if let Some(last) = end.last_mut() {
            *last = last.saturating_add(1);
        }
        self.storage.last_in_range_sync(prefix, end)
    }

    async fn is_zset_header_key(&self, db: usize, key: &[u8]) -> Result<bool> {
        let stored = self.get_typed(db, key).await?;
        Ok(stored
            .as_ref()
            .and_then(crate::storage::zset_subkey::is_zset_header)
            .is_some())
    }

    async fn push_zset_subkey_deletes(
        &self,
        db: usize,
        user_key: &[u8],
        batch: &mut Vec<AdapterWriteOp>,
    ) -> Result<()> {
        let encoded = Self::encode(db, user_key);
        self.push_prefix_deletes(crate::storage::zset_subkey::member_prefix(&encoded), batch)
            .await?;
        self.push_prefix_deletes(crate::storage::zset_subkey::score_prefix(&encoded), batch)
            .await
    }

    async fn push_prefix_deletes(
        &self,
        prefix: Vec<u8>,
        batch: &mut Vec<AdapterWriteOp>,
    ) -> Result<()> {
        let keys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let keys_c = std::sync::Arc::clone(&keys);
        self.storage
            .for_each_prefix(
                prefix,
                Box::new(move |key, _| {
                    keys_c.lock().expect("prefix deletes").push(key);
                    Ok(())
                }),
            )
            .await?;
        let keys = std::sync::Arc::try_unwrap(keys)
            .unwrap_or_else(|_| unreachable!("prefix delete keys still shared"))
            .into_inner()
            .expect("prefix deletes");
        for key in keys {
            batch.push(AdapterWriteOp::Delete { key });
        }
        Ok(())
    }
}

#[async_trait]
impl KvStorage for KvStorageAdapter {
    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        self.storage.aidb_statistics()
    }

    async fn get(&self, db: usize, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let stored = self.get_typed(db, key).await?;
        match stored {
            None => Ok(None),
            Some(s) => match s.value {
                ValueType::String(v) => Ok(Some(v)),
                _ => Err(Error::Command(WRONGTYPE.into())),
            },
        }
    }

    async fn set(&self, db: usize, key: &[u8], value: &[u8]) -> Result<()> {
        self.set_typed(db, key, StoredValue::string(value.to_vec()))
            .await
    }

    async fn set_with_ttl(
        &self,
        db: usize,
        key: &[u8],
        value: &[u8],
        expire_at_ms: u64,
    ) -> Result<()> {
        self.set_typed(
            db,
            key,
            StoredValue {
                value: ValueType::String(value.to_vec()),
                expires_at: Some(expire_at_ms),
            },
        )
        .await
    }

    async fn delete(&self, db: usize, key: &[u8]) -> Result<bool> {
        let Some(raw) = self.get_raw(db, key).await? else {
            return Ok(false);
        };
        let stored = Self::deserialize(&raw).ok();
        let saw_ttl = stored.as_ref().and_then(|value| value.expires_at).is_some();
        let encoded = Self::encode(db, key);
        let mut ops = Vec::new();
        if stored
            .as_ref()
            .and_then(crate::storage::zset_subkey::is_zset_header)
            .is_some()
        {
            self.push_zset_subkey_deletes(db, key, &mut ops).await?;
        }
        ops.push(AdapterWriteOp::Delete { key: encoded });
        if let Some((mk, mv)) = self.next_watch_meta_put(db, key).await? {
            ops.push(AdapterWriteOp::Put { key: mk, value: mv });
        }
        self.storage.apply_writes(ops).await?;
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn exists(&self, db: usize, key: &[u8]) -> Result<bool> {
        Ok(self.get_typed(db, key).await?.is_some())
    }

    async fn mget(&self, db: usize, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let stored = self.load_typed(db, key).await?;
            out.push(match stored {
                None => None,
                Some(s) => match s.value {
                    ValueType::String(v) => Some(v),
                    _ => None,
                },
            });
        }
        Ok(out)
    }

    async fn mset(&self, db: usize, pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        let mut ops = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            ops.push((key.clone(), WriteOp::put(value.clone())));
        }
        self.write_batch(db, ops).await
    }

    async fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
        let batch = self.prepare_adapter_batch(db, ops).await?;
        self.storage.write_batch(batch).await?;
        Ok(())
    }

    async fn enqueue_batch(
        &self,
        db: usize,
        ops: Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<Box<dyn BatchWriteHandle>> {
        let batch = self.prepare_adapter_batch(db, ops).await?;
        self.storage.enqueue_batch(batch).await
    }

    async fn keys(&self, db: usize, pattern: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.keys_for_db(db, pattern).await
    }

    async fn scan(
        &self,
        db: usize,
        cursor: u64,
        pattern: &[u8],
        count: usize,
    ) -> Result<ScanResult> {
        let mut valid = self.keys_for_db(db, pattern).await?;
        valid.sort();
        let skip = cursor as usize;
        let end = (skip + count).min(valid.len());
        let keys = valid[skip..end].to_vec();
        let next_cursor = if end < valid.len() { end as u64 } else { 0 };
        Ok(ScanResult {
            cursor: next_cursor,
            keys,
        })
    }

    async fn len(&self, db: usize) -> Result<usize> {
        self.check_db(db)?;
        Ok(self.storage.estimate_entry_count().await? as usize)
    }

    async fn expires(&self, db: usize) -> Result<u64> {
        self.check_db(db)?;
        Ok(self.expires.get(db))
    }

    async fn release_seen_ttl(&self, db: usize) -> Result<()> {
        self.check_db(db)?;
        self.expires.on_ttl_clear(db);
        Ok(())
    }

    async fn sample_avg_ttl(&self, db: usize) -> Result<u64> {
        let entries = self.collect_db_entries(db).await?;
        let now = now_ms();
        let remaining = entries.iter().filter_map(|(_, value)| {
            value
                .expires_at
                .filter(|exp| *exp > now)
                .map(|exp| exp - now)
        });
        Ok(crate::storage::types::compute_avg_ttl_ms(remaining))
    }

    async fn keyspace_stats(&self, db: usize) -> Result<KeyspaceStats> {
        let entries = self.collect_db_entries(db).await?;
        let now = now_ms();
        let keys = entries.len();
        let mut expires = 0usize;
        let mut ttl_remaining = Vec::new();
        for (_, v) in &entries {
            if let Some(exp) = v.expires_at {
                expires += 1;
                if exp > now {
                    ttl_remaining.push(exp - now);
                }
            }
        }
        let avg_ttl = crate::storage::types::compute_avg_ttl_ms(ttl_remaining.into_iter());
        Ok(KeyspaceStats {
            keys,
            expires,
            avg_ttl,
        })
    }

    async fn clear(&self, db: usize) -> Result<()> {
        self.clear_db(db).await
    }

    async fn clear_all(&self) -> Result<()> {
        for db in 0..self.db_count {
            self.clear_db(db).await?;
        }
        self.expires.reset_all();
        Ok(())
    }

    async fn expire(&self, db: usize, key: &[u8], ttl_ms: u64) -> Result<bool> {
        if ttl_ms == 0 {
            return self.delete(db, key).await;
        }
        let Some(mut stored) = self.load_typed(db, key).await? else {
            return Ok(false);
        };
        let saw_ttl = stored.expires_at.is_some();
        stored.expires_at = Some(now_ms().saturating_add(ttl_ms));
        self.set_typed(db, key, stored).await?;
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn expire_at(&self, db: usize, key: &[u8], timestamp_ms: u64) -> Result<bool> {
        let now = now_ms();
        if timestamp_ms <= now {
            return self.delete(db, key).await;
        }
        let Some(mut stored) = self.load_typed(db, key).await? else {
            return Ok(false);
        };
        let saw_ttl = stored.expires_at.is_some();
        stored.expires_at = Some(timestamp_ms);
        self.set_typed(db, key, stored).await?;
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        let Some(stored) = self.load_typed(db, key).await? else {
            return Ok(None);
        };
        match stored.expires_at {
            None => Ok(Some(TTL_NO_EXPIRY)),
            Some(expires_at) => {
                let now = now_ms();
                if now >= expires_at {
                    let _ = self.delete(db, key).await?;
                    Ok(None)
                } else {
                    Ok(Some((expires_at - now) as i64))
                }
            }
        }
    }

    async fn persist(&self, db: usize, key: &[u8]) -> Result<bool> {
        let Some(mut stored) = self.load_typed(db, key).await? else {
            return Ok(false);
        };
        let saw_ttl = stored.expires_at.is_some();
        stored.expires_at = None;
        self.set_typed(db, key, stored).await?;
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn db_count(&self) -> Result<usize> {
        Ok(self.db_count)
    }

    async fn swap_db(&self, a: usize, b: usize) -> Result<()> {
        self.check_db(a)?;
        self.check_db(b)?;
        if a == b {
            return Ok(());
        }
        let entries_a = self.collect_db_entries(a).await?;
        let entries_b = self.collect_db_entries(b).await?;
        self.clear_db(a).await?;
        self.clear_db(b).await?;
        for (key, value) in entries_b {
            self.set_typed(a, &key, value).await?;
        }
        for (key, value) in entries_a {
            self.set_typed(b, &key, value).await?;
        }
        Ok(())
    }

    async fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        let result = self.load_typed(db, key).await;
        if let Ok(value) = &result {
            if let Some(counter) = self.point_reads.get() {
                counter.note(value.is_some());
            }
        }
        result
    }

    fn set_point_read_counter(&self, counter: Arc<PointReadCounter>) {
        let _ = self.point_reads.set(counter);
    }

    async fn set_typed(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        let bytes = Self::serialize(&value)?;
        let encoded = Self::encode(db, key);
        let mut ops = vec![(encoded.clone(), bytes)];
        if let Some(meta) = self.next_watch_meta_put(db, key).await? {
            ops.push(meta);
        }
        self.storage.put_many(ops).await?;
        if value.expires_at.is_some() {
            self.expires.on_ttl_write(db);
        }
        Ok(())
    }

    async fn get_watch_version(&self, db: usize, key: &[u8]) -> Result<u64> {
        self.read_watch_version(db, key).await
    }

    fn watch_registry(&self) -> Arc<WatchRegistry> {
        Arc::clone(&self.watchers)
    }

    async fn rename_key(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<()> {
        if old_key == new_key {
            return Ok(());
        }
        let Some(value) = self.load_typed(db, old_key).await? else {
            return Err(Error::Command("ERR no such key".into()));
        };
        self.delete(db, old_key).await?;
        self.set_typed(db, new_key, value).await
    }

    async fn rename_key_nx(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<bool> {
        if old_key == new_key {
            return Ok(true);
        }
        let Some(value) = self.load_typed(db, old_key).await? else {
            return Err(Error::Command("ERR no such key".into()));
        };
        if self.load_typed(db, new_key).await?.is_some() {
            return Ok(false);
        }
        self.delete(db, old_key).await?;
        self.set_typed(db, new_key, value).await?;
        Ok(true)
    }

    async fn copy_key(
        &self,
        src_db: usize,
        dst_db: usize,
        src_key: &[u8],
        dst_key: &[u8],
        replace: bool,
    ) -> Result<bool> {
        let Some(stored) = self.load_typed(src_db, src_key).await? else {
            return Ok(false);
        };
        if src_db == dst_db && src_key == dst_key {
            return Ok(replace);
        }
        let existing = self.load_typed(dst_db, dst_key).await?;
        if existing.is_some() && !replace {
            return Ok(false);
        }
        let saw_ttl = existing
            .as_ref()
            .and_then(|value| value.expires_at)
            .is_some();
        self.set_typed(dst_db, dst_key, stored).await?;
        if saw_ttl {
            self.expires.on_ttl_clear(dst_db);
        }
        Ok(true)
    }

    async fn random_key(&self, db: usize) -> Result<Option<Vec<u8>>> {
        let keys = self.keys_for_db(db, b"*").await?;
        if keys.is_empty() {
            return Ok(None);
        }
        let idx = unix_nanos() as usize % keys.len();
        Ok(Some(keys[idx].clone()))
    }

    fn engine_kind(&self) -> StorageEngineKind {
        self.storage.engine_kind()
    }

    fn local_script_commit(&self) -> bool {
        self.storage.local_script_commit()
    }

    async fn flush_engine(&self) -> Result<()> {
        self.storage.flush().await
    }

    async fn create_checkpoint(&self, dest: &Path) -> Result<PathBuf> {
        self.storage.create_checkpoint(dest).await
    }

    async fn close_engine(&self) -> Result<()> {
        self.storage.close().await
    }

    async fn memory_usage_bytes(&self) -> Result<u64> {
        Ok(self.storage.approximate_memory_bytes().unwrap_or(0))
    }

    // ---- raw subkey 访问 ----

    async fn raw_prefix_last(&self, prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.check_db(0)?;
        let Some(end) = AiDbEngine::prefix_end(&prefix) else {
            return Ok(None);
        };
        self.storage.last_in_range(prefix, end).await
    }

    async fn raw_range_last(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.check_db(0)?;
        self.storage.last_in_range(start, end).await
    }

    async fn raw_subkey_get(&self, db: usize, encoded_key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        self.check_db(db)?;
        self.storage.get(encoded_key).await
    }

    async fn raw_subkey_set(&self, db: usize, encoded_key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.check_db(db)?;
        self.storage.set(encoded_key, value).await?;
        Ok(())
    }

    async fn raw_subkey_delete(&self, db: usize, encoded_key: Vec<u8>) -> Result<bool> {
        self.check_db(db)?;
        self.storage.delete(encoded_key).await
    }

    async fn raw_subkey_for_each(
        &self,
        db: usize,
        prefix: Vec<u8>,
        f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        self.check_db(db)?;
        self.storage.for_each_prefix(prefix, f).await
    }

    fn script_io(&self) -> &dyn crate::storage::script_io::ScriptIo {
        self
    }
}

impl crate::storage::script_io::ScriptIo for KvStorageAdapter {
    fn engine_kind(&self) -> StorageEngineKind {
        self.storage.engine_kind()
    }

    fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Get,
            || self.load_typed_for_script(db, key),
        )
    }

    fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Get,
            || self.ttl_for_script(db, key),
        )
    }

    fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Write,
            || {
                let batch = self.prepare_adapter_batch_sync(db, ops)?;
                self.storage.write_batch_sync(batch)
            },
        )
    }

    fn raw_prefix_last(&self, prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Prefix,
            || self.prefix_last_for_script(prefix),
        )
    }

    fn raw_range_last(&self, start: Vec<u8>, end: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Range,
            || self.storage.last_in_range_sync(start, end),
        )
    }

    fn raw_subkey_get(&self, db: usize, encoded_key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Get,
            || {
                self.check_db(db)?;
                self.storage.get_sync(encoded_key)
            },
        )
    }

    fn raw_subkey_for_each(
        &self,
        db: usize,
        prefix: Vec<u8>,
        f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        crate::storage::script_io::time_script_io(
            crate::storage::script_io::ScriptIoOp::Scan,
            || {
                self.check_db(db)?;
                self.storage.for_each_prefix_sync(prefix, f)
            },
        )
    }
}

fn unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
