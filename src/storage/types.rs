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
use std::sync::Arc;

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
#[derive(Debug, Clone, PartialEq)]
pub enum WriteOp {
    Put(Arc<Vec<u8>>),
    PutStored(Arc<StoredValue>),
    Delete,
    /// 配对 key 已是 engine key, 值为原始字节, 不再套 `StoredValue`.
    PutEngine {
        value: Arc<Vec<u8>>,
    },
    /// 配对 key 已是 engine key.
    DeleteEngine,
}

impl WriteOp {
    pub fn put(bytes: Vec<u8>) -> Self {
        WriteOp::Put(Arc::new(bytes))
    }

    pub fn put_stored(stored: StoredValue) -> Self {
        WriteOp::PutStored(Arc::new(stored))
    }

    pub fn put_engine(value: Vec<u8>) -> Self {
        WriteOp::PutEngine {
            value: Arc::new(value),
        }
    }
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
    ZSet = 3,
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
                CollectionKind::ZSet => "zset",
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

    /// 键计数器全库重扫耗时 (微秒). 重扫已去掉, 固定返回 0.
    fn rebuild_counters_duration_us(&self) -> u64 {
        0
    }

    /// 绑定点查计数. 默认空实现, 内存引擎和适配器会记 hit / miss.
    fn set_point_read_counter(&self, _counter: Arc<crate::storage::observation::PointReadCounter>) {
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
    async fn enqueue_batch(
        &self,
        db: usize,
        ops: Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<Box<dyn BatchWriteHandle>> {
        self.write_batch(db, ops).await?;
        Ok(Box::new(ImmediateWriteHandle))
    }

    async fn keys(&self, db: usize, pattern: &[u8]) -> Result<Vec<Vec<u8>>>;
    async fn scan(
        &self,
        db: usize,
        cursor: u64,
        pattern: &[u8],
        count: usize,
    ) -> Result<ScanResult>;
    async fn len(&self, db: usize) -> Result<usize>;
    /// 本进程在该逻辑 DB 上登记过的 TTL 写入次数, 不是磁盘上的 TTL 存量.
    async fn expires(&self, db: usize) -> Result<u64> {
        let _ = db;
        Ok(0)
    }
    /// 抽样该逻辑 DB 的平均剩余 TTL (毫秒). 默认 INFO 不调用.
    async fn sample_avg_ttl(&self, db: usize) -> Result<u64>;
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

    /// 这次调用已经看到该逻辑 DB 上的一条 TTL 时饱和减. 盲写不要调用.
    async fn release_seen_ttl(&self, db: usize) -> Result<()> {
        let _ = db;
        Ok(())
    }

    /// 把已经读到的值写回同一个 key. 值上带 TTL 时, 写入的加 1 再饱和减.
    async fn rewrite_stored(&self, db: usize, key: &[u8], stored: StoredValue) -> Result<()> {
        let saw_ttl = stored.expires_at.is_some();
        self.set_typed(db, key, stored).await?;
        if saw_ttl {
            self.release_seen_ttl(db).await?;
        }
        Ok(())
    }

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

    /// 脚本提交是否只写本地引擎. 集群实现要等 Raft, 必须留在脚本线程外面.
    fn local_script_commit(&self) -> bool {
        true
    }

    /// 脚本线程上的同步存储. 实现必须不调用 `spawn_blocking`.
    fn script_io(&self) -> &dyn crate::storage::script_io::ScriptIo;

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

    /// 前缀下字典序最大的原始 KV. 大 ZSET 的 `ZREVRANGE 0 0` 用它读最高分.
    async fn raw_prefix_last(&self, _prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }

    /// 半开区间 `[start, end)` 内字典序最大的原始 KV. 大 ZSET 的 `ZREVRANGEBYSCORE ... LIMIT 0 1` 用它定位单条.
    async fn raw_range_last(
        &self,
        _start: Vec<u8>,
        _end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
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

/// 批量写入在途等待 handle: 支持将写操作排入底层凑批队列后立即释放外层排他锁,
/// 后续在锁外 await 批量落盘与复制结果.
#[async_trait::async_trait]
pub trait BatchWriteHandle: Send + Sync {
    async fn wait(self: Box<Self>) -> Result<()>;
}

/// 默认立即完成的批量写入 handle (用于无异步凑批队列的存储实现).
pub struct ImmediateWriteHandle;

#[async_trait::async_trait]
impl BatchWriteHandle for ImmediateWriteHandle {
    async fn wait(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

/// 进程内各逻辑 DB 的 TTL 写入计数. 重启从 0 开始, 不扫库补存量.
#[derive(Debug, Default)]
pub struct ExpiresCounters {
    counts: [AtomicU64; DB_COUNT],
}

impl ExpiresCounters {
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

    /// 本次成功写入 `expires_at = Some` 时加 1, 包括没有读过旧值的覆盖写.
    #[inline]
    pub fn on_ttl_write(&self, db: usize) {
        if db < DB_COUNT {
            self.counts[db].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 本次调用已经看到旧值带 TTL, 或惰性过期删掉了带 TTL 的值时饱和减.
    #[inline]
    pub fn on_ttl_clear(&self, db: usize) {
        self.on_ttl_clear_n(db, 1);
    }

    #[inline]
    pub fn on_ttl_clear_n(&self, db: usize, n: u64) {
        if n == 0 || db >= DB_COUNT {
            return;
        }
        let _ = self.counts[db].fetch_update(Ordering::Relaxed, Ordering::Relaxed, |curr| {
            Some(curr.saturating_sub(n))
        });
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

    #[inline]
    pub fn swap(&self, a: usize, b: usize) {
        if a >= DB_COUNT || b >= DB_COUNT || a == b {
            return;
        }
        let left = self.counts[a].load(Ordering::Relaxed);
        let right = self.counts[b].load(Ordering::Relaxed);
        self.counts[a].store(right, Ordering::Relaxed);
        self.counts[b].store(left, Ordering::Relaxed);
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
    fn expires_counters_write_clear_and_saturation() {
        let counters = ExpiresCounters::new();
        assert_eq!(counters.get(0), 0);
        counters.on_ttl_write(0);
        counters.on_ttl_write(0);
        assert_eq!(counters.get(0), 2);
        counters.on_ttl_clear(0);
        assert_eq!(counters.get(0), 1);
        counters.on_ttl_clear(0);
        counters.on_ttl_clear(0);
        assert_eq!(counters.get(0), 0);
        counters.on_ttl_write(DB_COUNT);
        assert_eq!(counters.get(DB_COUNT), 0);
    }
}
