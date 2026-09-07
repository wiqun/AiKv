//! 存储层类型与 `KvStorage` trait: 命令层唯一依赖的存储契约 (多 DB、typed value、TTL).
//!
//! # 关键类型
//!
//! - `KvStorage`: 命令层存储接口, `MemoryEngine` 与 `KvStorageAdapter` 双实现.
//! - `StoredValue { value, expires_at }`: `expires_at` 为 Unix 毫秒, `None` = 永不过期.
//! - `ValueType`: String / Hash / List / Set / ZSet, 另有 `CollectionHeader`
//!   (subkey 格式的大集合元数据, 见 `storage/subkey.rs`).
//! - `WriteOp` (命令层批量) 与 `AdapterWriteOp` (扁平 KV) 独立, 转换在
//!   `KvStorageAdapter::write_batch`.
//! - 常量: `DB_COUNT` = 16; `TTL_NO_EXPIRY` = -1 (存储层 sentinel, 命令层映射 Redis -1).
//!
//! # Invariant
//!
//! - `as_*` / `as_*_mut` 类型不符 → `Error::Command(WRONGTYPE)`.
//! - `raw_subkey_*` 仅持久化引擎实现; `MemoryEngine` 返回
//!   `Error::Storage("raw subkey access not supported")`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyspaceStats {
    pub keys: usize,
    pub expires: usize,
    /// 带 TTL 的 key 平均剩余生存时间 (毫秒); 无 TTL key 时为 0.
    pub avg_ttl: u64,
}

/// 从剩余 TTL 样本计算 keyspace avg_ttl (毫秒).
pub fn compute_avg_ttl_ms(remaining_ms: impl Iterator<Item = u64>) -> u64 {
    let mut sum = 0u64;
    let mut count = 0u64;
    for ttl in remaining_ms {
        sum = sum.saturating_add(ttl);
        count += 1;
    }
    if count == 0 {
        0
    } else {
        sum.checked_div(count).unwrap_or(0)
    }
}

/// 扫描结果 (内部游标, 供 Phase 10 SCAN 复用)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    pub cursor: u64,
    pub keys: Vec<Vec<u8>>,
}

/// 批量写入操作 (命令层; 与底层 LSM WriteOp 独立)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteOp {
    Put(Vec<u8>),
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValueType {
    String(Vec<u8>),
    Hash(HashMap<Vec<u8>, Vec<u8>>),
    List(VecDeque<Vec<u8>>),
    Set(HashSet<Vec<u8>>),
    ZSet(BTreeMap<Vec<u8>, f64>),
    /// Subkey 格式的集合元数据.
    /// key 自身存储 `StoredValue { value: CollectionHeader, expires_at }`,
    /// 每个 field/member 以独立 subkey 存储在引擎中.
    CollectionHeader {
        kind: CollectionKind,
        count: u32,
    },
}

/// Subkey 格式支持的集合类型.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollectionKind {
    Hash = 1,
    Set = 2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredValue {
    pub value: ValueType,
    /// Unix 毫秒时间戳; None = 永不过期
    pub expires_at: Option<u64>,
}

impl StoredValue {
    pub fn string(value: Vec<u8>) -> Self {
        Self {
            value: ValueType::String(value),
            expires_at: None,
        }
    }

    pub fn is_expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        now_ms() >= expires_at
    }

    pub fn type_name(&self) -> &'static str {
        match self.value {
            ValueType::String(_) => "string",
            ValueType::Hash(_) => "hash",
            ValueType::List(_) => "list",
            ValueType::Set(_) => "set",
            ValueType::ZSet(_) => "zset",
            ValueType::CollectionHeader { kind, .. } => match kind {
                CollectionKind::Hash => "hash",
                CollectionKind::Set => "set",
            },
        }
    }

    /// 近似堆占用 (key 本体由调用方单独计入).
    pub fn approximate_heap_bytes(&self) -> u64 {
        match &self.value {
            ValueType::String(v) => v.len() as u64,
            ValueType::Hash(h) => h.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() as u64,
            ValueType::List(l) => l.iter().map(|v| v.len()).sum::<usize>() as u64,
            ValueType::Set(s) => s.iter().map(|v| v.len()).sum::<usize>() as u64,
            ValueType::ZSet(z) => z.keys().map(|k| k.len()).sum::<usize>() as u64,
            // CollectionHeader 自身很小; subkey 中的 field 数据由引擎单独统计
            ValueType::CollectionHeader { .. } => 13,
        }
    }

    pub fn new_list(list: VecDeque<Vec<u8>>) -> Self {
        Self {
            value: ValueType::List(list),
            expires_at: None,
        }
    }

    pub fn new_set(set: HashSet<Vec<u8>>) -> Self {
        Self {
            value: ValueType::Set(set),
            expires_at: None,
        }
    }

    pub fn new_zset(zset: BTreeMap<Vec<u8>, f64>) -> Self {
        Self {
            value: ValueType::ZSet(zset),
            expires_at: None,
        }
    }

    pub fn as_hash(&self) -> Result<&HashMap<Vec<u8>, Vec<u8>>> {
        match &self.value {
            ValueType::Hash(hash) => Ok(hash),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_hash_mut(&mut self) -> Result<&mut HashMap<Vec<u8>, Vec<u8>>> {
        match &mut self.value {
            ValueType::Hash(hash) => Ok(hash),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_list(&self) -> Result<&VecDeque<Vec<u8>>> {
        match &self.value {
            ValueType::List(list) => Ok(list),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_list_mut(&mut self) -> Result<&mut VecDeque<Vec<u8>>> {
        match &mut self.value {
            ValueType::List(list) => Ok(list),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_set(&self) -> Result<&HashSet<Vec<u8>>> {
        match &self.value {
            ValueType::Set(set) => Ok(set),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_set_mut(&mut self) -> Result<&mut HashSet<Vec<u8>>> {
        match &mut self.value {
            ValueType::Set(set) => Ok(set),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_zset(&self) -> Result<&BTreeMap<Vec<u8>, f64>> {
        match &self.value {
            ValueType::ZSet(zset) => Ok(zset),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn as_zset_mut(&mut self) -> Result<&mut BTreeMap<Vec<u8>, f64>> {
        match &mut self.value {
            ValueType::ZSet(zset) => Ok(zset),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }
}

pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_millis() as u64
}

/// 永不过期 key 在 `ttl()` 中的 sentinel (仅存储层; 命令层映射为 Redis -1)
pub const TTL_NO_EXPIRY: i64 = -1;

pub const WRONGTYPE: &str = "WRONGTYPE Operation against a key holding the wrong kind of value";

/// AiKv 支持的数据库数量 (与 Redis 默认 16 个 DB 一致)
pub const DB_COUNT: usize = 16;

/// 判断是否为 WRONGTYPE 命令错误 (不依赖 Display 字符串匹配).
pub fn is_wrongtype(err: &Error) -> bool {
    matches!(err, Error::Command(msg) if msg.starts_with("WRONGTYPE"))
}

#[async_trait]
pub trait KvStorage: Send + Sync {
    /// 暴露底层 AiDb 引擎的无锁原子统计实例 (若支持)
    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        None
    }

    /// 最近一次键计数器全库重建端到端耗时 (微秒).
    ///
    /// 包含全库重扫与过期 key 探测的端到端墙钟时间 (含调度与 I/O await 等待).
    fn rebuild_counters_duration_us(&self) -> u64 {
        0
    }

    async fn get(&self, db: usize, key: &[u8]) -> Result<Option<Vec<u8>>>;
    async fn set(&self, db: usize, key: &[u8], value: &[u8]) -> Result<()>;
    async fn set_with_ttl(
        &self,
        db: usize,
        key: &[u8],
        value: &[u8],
        expire_at_ms: u64,
    ) -> Result<()>;
    async fn delete(&self, db: usize, key: &[u8]) -> Result<bool>;
    async fn exists(&self, db: usize, key: &[u8]) -> Result<bool>;

    async fn mget(&self, db: usize, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>>;
    async fn mset(&self, db: usize, pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<()>;
    async fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()>;

    async fn keys(&self, db: usize, pattern: &[u8]) -> Result<Vec<Vec<u8>>>;
    async fn scan(
        &self,
        db: usize,
        cursor: u64,
        pattern: &[u8],
        count: usize,
    ) -> Result<ScanResult>;
    async fn len(&self, db: usize) -> Result<usize>;
    async fn keyspace_stats(&self, db: usize) -> Result<KeyspaceStats>;
    async fn clear(&self, db: usize) -> Result<()>;
    async fn clear_all(&self) -> Result<()>;

    async fn expire(&self, db: usize, key: &[u8], ttl_ms: u64) -> Result<bool>;
    async fn expire_at(&self, db: usize, key: &[u8], timestamp_ms: u64) -> Result<bool>;
    /// 剩余 TTL 毫秒; None = 不存在/已过期; Some(-1) = 永不过期
    async fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>>;
    async fn persist(&self, db: usize, key: &[u8]) -> Result<bool>;

    async fn db_count(&self) -> Result<usize>;
    async fn swap_db(&self, a: usize, b: usize) -> Result<()>;

    async fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>>;
    async fn set_typed(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()>;

    async fn rename_key(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<()>;
    async fn rename_key_nx(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<bool>;
    async fn copy_key(
        &self,
        src_db: usize,
        dst_db: usize,
        src_key: &[u8],
        dst_key: &[u8],
        replace: bool,
    ) -> Result<bool>;
    async fn random_key(&self, db: usize) -> Result<Option<Vec<u8>>>;

    /// WATCH 乐观锁版本; 0 表示从未写入.
    async fn get_watch_version(&self, db: usize, key: &[u8]) -> Result<u64> {
        let _ = (db, key);
        Ok(0)
    }

    /// 本节点当前被 WATCH 的 key. 热路径无人 watch 时跳过 meta 写 (`#83`).
    fn watch_registry(&self) -> std::sync::Arc<crate::storage::WatchRegistry>;

    fn engine_kind(&self) -> StorageEngineKind {
        StorageEngineKind::Memory
    }

    async fn flush_engine(&self) -> Result<()> {
        Ok(())
    }

    async fn create_checkpoint(&self, _dest: &Path) -> Result<PathBuf> {
        Err(Error::Command(
            "ERR Persistence not supported on memory engine".into(),
        ))
    }

    async fn close_engine(&self) -> Result<()> {
        Ok(())
    }

    /// 各逻辑 DB 的键数量 (index = db id). 热路径实现含尚未惰性清理的过期 key.
    async fn db_key_counts(&self) -> Result<Vec<usize>> {
        let n = self.db_count().await?;
        let mut out = Vec::with_capacity(n);
        for db in 0..n {
            out.push(self.len(db).await?);
        }
        Ok(out)
    }

    /// 冷启动或集群就绪后重建内存键计数. 默认 no-op (MemoryEngine).
    async fn rebuild_key_counts(&self) -> Result<()> {
        Ok(())
    }

    /// 近似内存占用 (字节); 默认 0, 各引擎自行实现.
    async fn memory_usage_bytes(&self) -> Result<u64> {
        Ok(0)
    }

    // ---- 原始 subkey 访问 (仅持久化引擎) ----

    /// 绕过 StoredValue 反序列化, 直接读取 subkey 的原始字节.
    /// 默认返回 None (MemoryEngine 不支持).
    async fn raw_subkey_get(&self, _db: usize, _encoded_key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }

    /// 绕过 StoredValue 序列化, 直接写入 subkey 的原始字节.
    async fn raw_subkey_set(
        &self,
        _db: usize,
        _encoded_key: Vec<u8>,
        _value: Vec<u8>,
    ) -> Result<()> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }

    /// 绕过 StoredValue 编解码, 直接删除 subkey.
    async fn raw_subkey_delete(&self, _db: usize, _encoded_key: Vec<u8>) -> Result<bool> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }

    /// 绕过 StoredValue 编解码, 扫描 subkey 前缀范围内的所有原始 KV 对.
    /// 仅持久化引擎实现; MemoryEngine 返回 Err.
    async fn raw_subkey_for_each(
        &self,
        _db: usize,
        _prefix: Vec<u8>,
        _f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }
}

/// 内存逻辑 DB 键计数器 (固定容量 `DB_COUNT`, 线程安全原子数组).
#[derive(Debug, Default)]
pub struct DbKeyCounters {
    counts: [AtomicU64; DB_COUNT],
}

/// 过期键 `decr` 单飞门闩: 惰性删除/`DEL`/compaction 至多一方扣减, 持有至 `set_typed` 重生.
#[derive(Debug, Default)]
pub struct ExpireDecrGate {
    claimed: dashmap::DashSet<Vec<u8>>,
}

impl ExpireDecrGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 若本调用方赢得扣减权则返回 true.
    #[inline]
    pub fn try_claim(&self, encoded_or_lsm_key: &[u8]) -> bool {
        self.claimed.insert(encoded_or_lsm_key.to_vec())
    }

    #[inline]
    pub fn release(&self, encoded_or_lsm_key: &[u8]) {
        self.claimed.remove(encoded_or_lsm_key);
    }

    #[inline]
    pub fn clear(&self) {
        self.claimed.clear();
    }
}

impl DbKeyCounters {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn get(&self, db: usize) -> u64 {
        if db < DB_COUNT {
            self.counts[db].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    #[inline]
    pub fn incr(&self, db: usize) {
        if db < DB_COUNT {
            self.counts[db].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn decr(&self, db: usize) {
        if db < DB_COUNT {
            let _ = self.counts[db].fetch_update(Ordering::Relaxed, Ordering::Relaxed, |curr| {
                Some(curr.saturating_sub(1))
            });
        }
    }

    #[inline]
    pub fn set(&self, db: usize, count: u64) {
        if db < DB_COUNT {
            self.counts[db].store(count, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn reset_db(&self, db: usize) {
        if db < DB_COUNT {
            self.counts[db].store(0, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn reset_all(&self) {
        for count in &self.counts {
            count.store(0, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> Vec<usize> {
        self.counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed) as usize)
            .collect()
    }
}

/// 底层存储引擎类型 (CLI / INFO / 持久化命令分支).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageEngineKind {
    Memory,
    AiDb,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_wrongtype() {
        assert!(is_wrongtype(&Error::Command(WRONGTYPE.into())));
        assert!(!is_wrongtype(&Error::Command("ERR other".into())));
    }

    #[test]
    fn test_db_key_counters_initial_and_bounds() {
        let counters = DbKeyCounters::new();
        for db in 0..DB_COUNT {
            assert_eq!(counters.get(db), 0);
        }
        // 越界读返回 0
        assert_eq!(counters.get(DB_COUNT), 0);
        assert_eq!(counters.get(999), 0);

        // 越界操作 no-op
        counters.incr(DB_COUNT);
        counters.decr(DB_COUNT);
        counters.set(DB_COUNT, 100);
        counters.reset_db(DB_COUNT);
        assert_eq!(counters.get(DB_COUNT), 0);
    }

    #[test]
    fn test_db_key_counters_incr_decr_and_saturation() {
        let counters = DbKeyCounters::new();
        counters.incr(0);
        counters.incr(0);
        assert_eq!(counters.get(0), 2);

        counters.decr(0);
        assert_eq!(counters.get(0), 1);

        counters.decr(0);
        assert_eq!(counters.get(0), 0);

        // 下溢饱和保护: 0 减 1 保持 0, 不 wrap 成 u64::MAX
        counters.decr(0);
        assert_eq!(counters.get(0), 0);
    }

    #[test]
    fn test_db_key_counters_set_and_resets() {
        let counters = DbKeyCounters::new();
        counters.set(1, 42);
        counters.set(2, 100);
        assert_eq!(counters.get(1), 42);
        assert_eq!(counters.get(2), 100);

        counters.reset_db(1);
        assert_eq!(counters.get(1), 0);
        assert_eq!(counters.get(2), 100);

        counters.reset_all();
        assert_eq!(counters.get(2), 0);
    }

    #[test]
    fn test_db_key_counters_snapshot() {
        let counters = DbKeyCounters::new();
        counters.set(0, 10);
        counters.set(3, 30);
        let snap = counters.snapshot();
        assert_eq!(snap.len(), DB_COUNT);
        assert_eq!(snap[0], 10);
        assert_eq!(snap[1], 0);
        assert_eq!(snap[3], 30);
    }

    #[test]
    fn test_expire_decr_gate_claim_release() {
        let gate = ExpireDecrGate::new();
        assert!(gate.try_claim(b"k"));
        assert!(!gate.try_claim(b"k"));
        gate.release(b"k");
        assert!(gate.try_claim(b"k"));
        gate.clear();
        assert!(gate.try_claim(b"k"));
        assert!(!gate.try_claim(b"k"));
    }
}
