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
//! subkey:  {db_index}:{user_key}\x01H|S... # 大 Hash/Set 的 field/member, raw bytes
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::storage::memory::glob_match;
use crate::storage::observation::StorageObservation;
use crate::storage::types::{
    now_ms, DbKeyCounters, ExpireDecrGate, KeyspaceStats, KvStorage, ScanResult, StorageEngineKind,
    StoredValue, ValueType, WriteOp, DB_COUNT, TTL_NO_EXPIRY, WRONGTYPE,
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

/// `write_batch` 成功后的键变更摘要 (inserted=新 key 数, deleted=真实删除数).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteBatchStats {
    pub inserted: u64,
    pub deleted: u64,
}

#[async_trait]
pub trait StorageAdapter: Send + Sync {
    /// 暴露底层引擎的无锁原子统计实例 (若支持)
    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        None
    }

    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>>;
    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<bool>;
    /// 一次提交多笔写. 集群实现须把 ops 全部送进同一 group 的 `GroupSetBatcher`
    /// 后再等 ack, 避免每条 SET 串行两次 Raft propose (Issue #83).
    /// 默认逐条 `set` / `delete`.
    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            match op {
                AdapterWriteOp::Put { key, value } => out.push(self.set(key, value).await?),
                AdapterWriteOp::Delete { key } => out.push(self.delete(key).await?),
            }
        }
        Ok(out)
    }
    /// 一次提交多笔 PUT; 默认转发 `apply_writes`.
    async fn put_many(&self, ops: Vec<(Vec<u8>, Vec<u8>)>) -> Result<Vec<bool>> {
        self.apply_writes(
            ops.into_iter()
                .map(|(key, value)| AdapterWriteOp::Put { key, value })
                .collect(),
        )
        .await
    }
    async fn delete(&self, key: Vec<u8>) -> Result<bool>;
    async fn exists(&self, key: Vec<u8>) -> Result<bool>;
    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<WriteBatchStats>;
    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()>;
    async fn len(&self) -> Result<usize>;
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
    counters: Arc<DbKeyCounters>,
    expire_gate: Arc<ExpireDecrGate>,
    watchers: Arc<WatchRegistry>,
    rebuild_counters_duration_us: Arc<AtomicU64>,
}

impl KvStorageAdapter {
    pub async fn open(
        storage: Arc<dyn StorageAdapter>,
        observation: Option<Arc<StorageObservation>>,
    ) -> Result<Arc<Self>> {
        Self::open_with_counters(
            storage,
            observation,
            Arc::new(DbKeyCounters::new()),
            Arc::new(ExpireDecrGate::new()),
        )
        .await
    }

    /// 使用外部提供的计数器与过期扣减门闩 open (便于在 open 前注册 compaction listener).
    pub async fn open_with_counters(
        storage: Arc<dyn StorageAdapter>,
        observation: Option<Arc<StorageObservation>>,
        counters: Arc<DbKeyCounters>,
        expire_gate: Arc<ExpireDecrGate>,
    ) -> Result<Arc<Self>> {
        let adapter = Arc::new(Self {
            storage,
            db_count: DB_COUNT,
            observation,
            counters,
            expire_gate,
            watchers: WatchRegistry::new(),
            rebuild_counters_duration_us: Arc::new(AtomicU64::new(0)),
        });
        adapter.rebuild_counters().await?;
        Ok(adapter)
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
            counters: Arc::new(DbKeyCounters::new()),
            expire_gate: Arc::new(ExpireDecrGate::new()),
            watchers: WatchRegistry::new(),
            rebuild_counters_duration_us: Arc::new(AtomicU64::new(0)),
        })
    }

    /// 最近一次键计数器全库重建端到端耗时 (微秒).
    ///
    /// 包含全库重扫与过期 key 探测的端到端墙钟时间 (含调度与 I/O await 等待).
    pub fn rebuild_counters_duration_us(&self) -> u64 {
        self.rebuild_counters_duration_us.load(Ordering::Relaxed)
    }

    /// 重建全部逻辑 DB 的键计数器 (存活未过期 user key; 过期 key `try_claim` 门闩, 不清空).
    ///
    /// 度量含 await 等待的端到端耗时 (墙钟时间，微秒)，并更新 `rebuild_counters_duration_us`。
    pub async fn rebuild_counters(&self) -> Result<()> {
        let start = std::time::Instant::now();
        for db in 0..self.db_count {
            self.claim_expired_logical_keys(db).await?;
            self.counters
                .set(db, self.keys_for_db(db, b"").await?.len() as u64);
        }
        let elapsed_us = start.elapsed().as_micros() as u64;
        self.rebuild_counters_duration_us
            .store(elapsed_us, Ordering::Relaxed);
        Ok(())
    }

    async fn claim_expired_logical_keys(&self, db: usize) -> Result<()> {
        self.check_db(db)?;
        let prefix = Self::db_prefix(db);
        let gate = self.expire_gate.clone();
        self.storage
            .for_each_prefix(
                prefix,
                Box::new(move |encoded, raw| {
                    if Self::decode_user_key(&encoded).is_none() {
                        return Ok(());
                    }
                    let Ok(stored) = Self::deserialize(&raw) else {
                        return Ok(());
                    };
                    if stored.is_expired() {
                        let _ = gate.try_claim(&encoded);
                    }
                    Ok(())
                }),
            )
            .await
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
        if let Ok(flags) = self.storage.apply_writes(ops).await {
            if flags.first().copied().unwrap_or(false) && self.expire_gate.try_claim(&encoded) {
                self.counters.decr(db);
            }
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
        let gate = self.expire_gate.clone();
        self.storage
            .for_each_prefix(
                prefix.clone(),
                Box::new(move |encoded, raw| {
                    if Self::decode_user_key(&encoded).is_none() {
                        return Ok(());
                    }
                    if Self::deserialize(&raw).is_ok() {
                        let _ = gate.try_claim(&encoded);
                    }
                    Ok(())
                }),
            )
            .await?;
        let end = AiDbEngine::prefix_end(&prefix).unwrap_or_else(|| {
            let mut max = prefix.clone();
            max.push(0xff);
            max
        });
        self.storage.delete_range(prefix, end).await?;
        self.counters.reset_db(db);
        Ok(())
    }
}

#[async_trait]
impl KvStorage for KvStorageAdapter {
    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        self.storage.aidb_statistics()
    }

    fn rebuild_counters_duration_us(&self) -> u64 {
        self.rebuild_counters_duration_us.load(Ordering::Relaxed)
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
        let encoded = Self::encode(db, key);
        if self.get_raw(db, key).await?.is_none() {
            return Ok(false);
        }
        let mut ops = vec![AdapterWriteOp::Delete {
            key: encoded.clone(),
        }];
        if let Some((mk, mv)) = self.next_watch_meta_put(db, key).await? {
            ops.push(AdapterWriteOp::Put { key: mk, value: mv });
        }
        let flags = self.storage.apply_writes(ops).await?;
        let deleted = flags.first().copied().unwrap_or(false);
        if deleted && self.expire_gate.try_claim(&encoded) {
            self.counters.decr(db);
        }
        Ok(deleted)
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
            ops.push((key.clone(), WriteOp::Put(value.clone())));
        }
        self.write_batch(db, ops).await
    }

    async fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
        self.check_db(db)?;
        let mut batch = Vec::with_capacity(ops.len());
        let mut deleted_keys: Vec<Vec<u8>> = Vec::new();
        let mut put_keys: Vec<Vec<u8>> = Vec::new();
        let mut user_is_put: Vec<bool> = Vec::with_capacity(ops.len());
        for (key, op) in ops {
            let encoded = Self::encode(db, &key);
            match op {
                WriteOp::Put(value) => {
                    let stored = StoredValue::string(value);
                    let bytes = Self::serialize(&stored)?;
                    put_keys.push(encoded.clone());
                    batch.push(AdapterWriteOp::Put {
                        key: encoded,
                        value: bytes,
                    });
                    user_is_put.push(true);
                }
                WriteOp::Delete => {
                    deleted_keys.push(encoded.clone());
                    batch.push(AdapterWriteOp::Delete { key: encoded });
                    user_is_put.push(false);
                }
            }
        }
        for encoded in put_keys.iter().chain(deleted_keys.iter()) {
            let Some(user_key) = Self::decode_user_key(encoded) else {
                continue;
            };
            if let Some((mk, mv)) = self.next_watch_meta_put(db, &user_key).await? {
                batch.push(AdapterWriteOp::Put { key: mk, value: mv });
            }
        }
        let flags = match self.storage.apply_writes(batch).await {
            Ok(flags) => flags,
            Err(e) => {
                if let Err(rebuild_err) = self.rebuild_counters().await {
                    tracing::warn!(error = %rebuild_err, "write_batch failed and rebuild_counters also failed");
                }
                crate::storage::counter_batch::release_live_puts(
                    self.storage.as_ref(),
                    &self.expire_gate,
                    &put_keys,
                )
                .await;
                return Err(e);
            }
        };
        let mut inserted = 0u64;
        let mut deleted = 0u64;
        for (i, is_put) in user_is_put.iter().enumerate() {
            if flags.get(i).copied().unwrap_or(false) {
                if *is_put {
                    inserted += 1;
                } else {
                    deleted += 1;
                }
            }
        }
        crate::storage::counter_batch::apply_successful_batch(
            &self.counters,
            &self.expire_gate,
            db,
            &put_keys,
            &deleted_keys,
            WriteBatchStats { inserted, deleted },
        );
        Ok(())
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
        Ok(self.counters.get(db) as usize)
    }

    async fn db_key_counts(&self) -> Result<Vec<usize>> {
        Ok(self.counters.snapshot())
    }

    async fn rebuild_key_counts(&self) -> Result<()> {
        self.rebuild_counters().await
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
        self.counters.reset_all();
        Ok(())
    }

    async fn expire(&self, db: usize, key: &[u8], ttl_ms: u64) -> Result<bool> {
        if ttl_ms == 0 {
            return self.delete(db, key).await;
        }
        let Some(mut stored) = self.load_typed(db, key).await? else {
            return Ok(false);
        };
        stored.expires_at = Some(now_ms().saturating_add(ttl_ms));
        self.set_typed(db, key, stored).await?;
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
        stored.expires_at = Some(timestamp_ms);
        self.set_typed(db, key, stored).await?;
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
        stored.expires_at = None;
        self.set_typed(db, key, stored).await?;
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
        self.load_typed(db, key).await
    }

    async fn set_typed(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        let bytes = Self::serialize(&value)?;
        let encoded = Self::encode(db, key);
        let mut ops = vec![(encoded.clone(), bytes)];
        if let Some(meta) = self.next_watch_meta_put(db, key).await? {
            ops.push(meta);
        }
        let flags = self.storage.put_many(ops).await?;
        if flags.first().copied().unwrap_or(false) {
            self.counters.incr(db);
        }
        self.expire_gate.release(&encoded);
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
        if self.load_typed(dst_db, dst_key).await?.is_some() && !replace {
            return Ok(false);
        }
        self.set_typed(dst_db, dst_key, stored).await?;
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
}

fn unix_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
