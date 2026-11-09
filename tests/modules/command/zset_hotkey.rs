//! 单热 Key 算子 (ZADD) 提前放锁与写缓冲折叠确定性回归测试.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tempfile::TempDir;
use tokio::sync::Notify;

use aikv::command::CommandRouter;
use aikv::error::{Error, Result};
use aikv::protocol::RespValue;
use aikv::storage::aidb::AiDbEngine;
use aikv::storage::types::{
    BatchWriteHandle, KeyspaceStats, KvStorage, ScanResult, StorageEngineKind, StoredValue, WriteOp,
};
use aikv::storage::{zset_subkey, KvStorageAdapter};

/// 带有可控闸门的三入口测试存储 (同时拦截 set, write_batch, enqueue_batch)
struct GatedKvStorage {
    inner: Arc<dyn KvStorage>,
    gate: Arc<Notify>,
    entered: Arc<Notify>,
    should_block: AtomicBool,
    fail_enqueue: AtomicBool,
}

impl GatedKvStorage {
    fn new(inner: Arc<dyn KvStorage>) -> Self {
        Self {
            inner,
            gate: Arc::new(Notify::new()),
            entered: Arc::new(Notify::new()),
            should_block: AtomicBool::new(false),
            fail_enqueue: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl KvStorage for GatedKvStorage {
    async fn get(&self, db: usize, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.get(db, key).await
    }

    async fn get_bytes(&self, db: usize, key: &[u8]) -> Result<Option<Bytes>> {
        self.inner.get_bytes(db, key).await
    }

    async fn set(&self, db: usize, key: &[u8], value: &[u8]) -> Result<()> {
        if self.should_block.load(Ordering::SeqCst) {
            self.entered.notify_waiters();
            self.gate.notified().await;
        }
        self.inner.set(db, key, value).await
    }

    async fn set_with_ttl(
        &self,
        db: usize,
        key: &[u8],
        value: &[u8],
        expire_at_ms: u64,
    ) -> Result<()> {
        if self.should_block.load(Ordering::SeqCst) {
            self.entered.notify_waiters();
            self.gate.notified().await;
        }
        self.inner.set_with_ttl(db, key, value, expire_at_ms).await
    }

    async fn delete(&self, db: usize, key: &[u8]) -> Result<bool> {
        self.inner.delete(db, key).await
    }

    async fn exists(&self, db: usize, key: &[u8]) -> Result<bool> {
        self.inner.exists(db, key).await
    }

    async fn mget(&self, db: usize, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        self.inner.mget(db, keys).await
    }

    async fn mset(&self, db: usize, pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        self.inner.mset(db, pairs).await
    }

    async fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
        if self.should_block.load(Ordering::SeqCst) {
            self.entered.notify_waiters();
            self.gate.notified().await;
        }
        self.inner.write_batch(db, ops).await
    }

    async fn enqueue_batch(
        &self,
        db: usize,
        ops: Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<Box<dyn BatchWriteHandle>> {
        if self.fail_enqueue.load(Ordering::SeqCst) {
            return Err(Error::Command("ERR enqueue failed".into()));
        }
        if self.should_block.load(Ordering::SeqCst) {
            self.entered.notify_waiters();
            self.gate.notified().await;
        }
        self.inner.enqueue_batch(db, ops).await
    }

    async fn keys(&self, db: usize, pattern: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.inner.keys(db, pattern).await
    }

    async fn scan(
        &self,
        db: usize,
        cursor: u64,
        pattern: &[u8],
        count: usize,
    ) -> Result<ScanResult> {
        self.inner.scan(db, cursor, pattern, count).await
    }

    async fn len(&self, db: usize) -> Result<usize> {
        self.inner.len(db).await
    }

    async fn sample_avg_ttl(&self, db: usize) -> Result<u64> {
        self.inner.sample_avg_ttl(db).await
    }

    async fn keyspace_stats(&self, db: usize) -> Result<KeyspaceStats> {
        self.inner.keyspace_stats(db).await
    }

    async fn clear(&self, db: usize) -> Result<()> {
        self.inner.clear(db).await
    }

    async fn clear_all(&self) -> Result<()> {
        self.inner.clear_all().await
    }

    async fn expire(&self, db: usize, key: &[u8], ttl_ms: u64) -> Result<bool> {
        self.inner.expire(db, key, ttl_ms).await
    }

    async fn expire_at(&self, db: usize, key: &[u8], timestamp_ms: u64) -> Result<bool> {
        self.inner.expire_at(db, key, timestamp_ms).await
    }

    async fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        self.inner.ttl(db, key).await
    }

    async fn persist(&self, db: usize, key: &[u8]) -> Result<bool> {
        self.inner.persist(db, key).await
    }

    async fn db_count(&self) -> Result<usize> {
        self.inner.db_count().await
    }

    async fn swap_db(&self, a: usize, b: usize) -> Result<()> {
        self.inner.swap_db(a, b).await
    }

    async fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        self.inner.get_typed(db, key).await
    }

    async fn set_typed(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        if self.should_block.load(Ordering::SeqCst) {
            self.entered.notify_waiters();
            self.gate.notified().await;
        }
        self.inner.set_typed(db, key, value).await
    }

    async fn rename_key(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<()> {
        self.inner.rename_key(db, old_key, new_key).await
    }

    async fn rename_key_nx(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<bool> {
        self.inner.rename_key_nx(db, old_key, new_key).await
    }

    async fn copy_key(
        &self,
        src_db: usize,
        dst_db: usize,
        src_key: &[u8],
        dst_key: &[u8],
        replace: bool,
    ) -> Result<bool> {
        self.inner
            .copy_key(src_db, dst_db, src_key, dst_key, replace)
            .await
    }

    async fn random_key(&self, db: usize) -> Result<Option<Vec<u8>>> {
        self.inner.random_key(db).await
    }

    async fn raw_subkey_get(&self, db: usize, encoded_key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        self.inner.raw_subkey_get(db, encoded_key).await
    }

    async fn raw_subkey_set(&self, db: usize, encoded_key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.inner.raw_subkey_set(db, encoded_key, value).await
    }

    async fn raw_subkey_delete(&self, db: usize, encoded_key: Vec<u8>) -> Result<bool> {
        self.inner.raw_subkey_delete(db, encoded_key).await
    }

    async fn raw_prefix_last(&self, prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.inner.raw_prefix_last(prefix).await
    }

    async fn raw_range_last(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.inner.raw_range_last(start, end).await
    }

    async fn raw_range_scan(
        &self,
        start: Vec<u8>,
        end: Vec<u8>,
        limit: usize,
        reverse: bool,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner
            .raw_range_scan(start, end, limit, reverse)
            .await
    }

    fn watch_registry(&self) -> Arc<aikv::storage::WatchRegistry> {
        self.inner.watch_registry()
    }

    fn script_io(&self) -> &dyn aikv::storage::script_io::ScriptIo {
        self.inner.script_io()
    }

    fn engine_kind(&self) -> StorageEngineKind {
        self.inner.engine_kind()
    }
}

fn setup_gated_env() -> (TempDir, Arc<GatedKvStorage>, Arc<CommandRouter>) {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let gated = Arc::new(GatedKvStorage::new(storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));
    (dir, gated, router)
}

#[tokio::test]
async fn test_zadd_hotkey_gate_releases_lock_and_preserves_count() {
    let (_dir, gated, router) = setup_gated_env();

    // 预设 Header (count = 0)
    let mut db = 0;
    gated
        .inner
        .set_typed(0, b"z_hot", zset_subkey::header(0, None))
        .await
        .unwrap();

    // 开启写闸门拦截底层写入
    gated.should_block.store(true, Ordering::SeqCst);

    // 发起第一条写入: ZADD z_hot 10 m1
    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "ZADD",
            &[Bytes::from("z_hot"), Bytes::from("10"), Bytes::from("m1")],
            &mut db,
        )
        .await
    });

    // 等待第一条抵达底层写闸门
    gated.entered.notified().await;

    // 关键红绿灯断言 1:
    // 第一条挂在写闸门上时，第二条尝试获取同一把 KeyLock:
    // 旧代码持有 KeyLock 卡在 storage.write_batch，第二条无法拿锁而在 150ms 超时失败;
    // 新代码已经提前释放 KeyLock，第二条能瞬时拿锁!
    let key = Bytes::from("z_hot");
    let key_lock = router.key_lock();
    let lock_res = tokio::time::timeout(Duration::from_millis(150), async {
        key_lock.lock(&key).await
    })
    .await;
    assert!(lock_res.is_ok(), "第二条 ZADD 写入未能在提前放锁后快速拿到锁");
    let guard = lock_res.unwrap();

    // 关键红绿灯断言 2:
    // 在途 Header 计数为 1 (m1 成功在途暂存)
    let in_flight = &router.in_flight_context().in_flight;
    let stored = in_flight.get(0, &key);
    assert!(stored.is_some(), "在途应当包含 z_hot header");
    let stored_val = stored.unwrap().expect("header 不应为 deleted");
    assert_eq!(
        zset_subkey::is_zset_header(&stored_val),
        Some(1),
        "在途 Header 计数值必须为 1"
    );

    // 关键红绿灯断言 3:
    // m1 通过 get_raw 能查到在途 10.0，而 m2 查不到 (为 None)
    let encoded = AiDbEngine::encode_key(0, &key);
    let m1_engine = zset_subkey::member_key(&encoded, b"m1");
    let m2_engine = zset_subkey::member_key(&encoded, b"m2");

    let raw_m1 = in_flight.get_raw(&m1_engine);
    assert!(raw_m1.is_some(), "m1 在途应当存在");
    let m1_bytes = raw_m1.unwrap().expect("m1 不应为 deleted");
    assert_eq!(zset_subkey::decode_score(&m1_bytes), Some(10.0));

    let raw_m2 = in_flight.get_raw(&m2_engine);
    assert!(raw_m2.is_none(), "m2 尚未写入，在途应为 None");

    // 释放第二条拿到的锁
    drop(guard);

    // 释放写闸门，让第一条写完成
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    let res1 = h1.await.unwrap().unwrap();
    assert_eq!(res1, RespValue::Integer(1));

    // 执行第二条写入: ZADD z_hot 20 m2
    let res2 = router
        .execute(
            "ZADD",
            &[Bytes::from("z_hot"), Bytes::from("20"), Bytes::from("m2")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(res2, RespValue::Integer(1));

    // 最终断言: 底库已提交的 ZCARD 必须严格等于 2 (验证 header 计数未丢失)
    let card = router
        .execute("ZCARD", &[Bytes::from("z_hot")], &mut db)
        .await
        .unwrap();
    assert_eq!(card, RespValue::Integer(2));
}

#[tokio::test]
async fn test_zadd_hotkey_same_member_overwrites_score() {
    let (_dir, gated, router) = setup_gated_env();

    let mut db = 0;
    gated
        .inner
        .set_typed(0, b"z_same", zset_subkey::header(0, None))
        .await
        .unwrap();

    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "ZADD",
            &[Bytes::from("z_same"), Bytes::from("10"), Bytes::from("m1")],
            &mut db,
        )
        .await
    });

    gated.entered.notified().await;

    // 放行第一条
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    h1.await.unwrap().unwrap();

    // 同一 member 更新分值: 10 -> 25
    let res2 = router
        .execute(
            "ZADD",
            &[Bytes::from("z_same"), Bytes::from("25"), Bytes::from("m1")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(res2, RespValue::Integer(0)); // 新增成员数为 0

    // ZCARD 保持为 1
    let card = router
        .execute("ZCARD", &[Bytes::from("z_same")], &mut db)
        .await
        .unwrap();
    assert_eq!(card, RespValue::Integer(1));

    // ZSCORE 读出更新后的 25
    let score = router
        .execute(
            "ZSCORE",
            &[Bytes::from("z_same"), Bytes::from("m1")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(score, RespValue::BulkString(Some(Bytes::from_static(b"25"))));
}

#[tokio::test]
async fn test_zadd_public_zcard_reads_committed_only() {
    let (_dir, gated, router) = setup_gated_env();

    let mut db = 0;
    gated
        .inner
        .set_typed(0, b"z_iso", zset_subkey::header(0, None))
        .await
        .unwrap();

    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "ZADD",
            &[Bytes::from("z_iso"), Bytes::from("10"), Bytes::from("m1")],
            &mut db,
        )
        .await
    });

    gated.entered.notified().await;

    // 验证公开读隔离: 在途写未落盘时，独立 ZCARD 读到底库依然是 0
    let cur_card = router
        .execute("ZCARD", &[Bytes::from("z_iso")], &mut db)
        .await
        .unwrap();
    assert_eq!(cur_card, RespValue::Integer(0));

    // 放行
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    h1.await.unwrap().unwrap();

    // 提交后 ZCARD 为 1
    let final_card = router
        .execute("ZCARD", &[Bytes::from("z_iso")], &mut db)
        .await
        .unwrap();
    assert_eq!(final_card, RespValue::Integer(1));
}

#[tokio::test]
async fn test_zadd_enqueue_failure_rolls_back() {
    let (_dir, gated, router) = setup_gated_env();

    let mut db = 0;
    gated
        .inner
        .set_typed(0, b"z_fail", zset_subkey::header(0, None))
        .await
        .unwrap();

    gated.fail_enqueue.store(true, Ordering::SeqCst);

    let res = router
        .execute(
            "ZADD",
            &[Bytes::from("z_fail"), Bytes::from("10"), Bytes::from("m1")],
            &mut db,
        )
        .await;
    assert!(res.is_err(), "入队失败应返回错误");

    // 验证在途项被回滚清除，底库 ZCARD 仍为 0
    assert!(router.in_flight_context().in_flight.is_empty());
    let card = router
        .execute("ZCARD", &[Bytes::from("z_fail")], &mut db)
        .await
        .unwrap();
    assert_eq!(card, RespValue::Integer(0));
}
