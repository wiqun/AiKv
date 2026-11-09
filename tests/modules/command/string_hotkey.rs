//! 单热 Key 算子 (SETRANGE / INCR) 提前放锁与写缓冲折叠确定性回归测试.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Notify;

use aikv::command::CommandRouter;
use aikv::error::{Error, Result};
use aikv::protocol::RespValue;
use aikv::storage::memory::MemoryEngine;
use aikv::storage::types::{
    BatchWriteHandle, KeyspaceStats, KvStorage, ScanResult, StorageEngineKind, StoredValue, WriteOp,
};

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

#[tokio::test]
async fn test_setrange_hotkey_gate_releases_lock_and_folds() {
    let raw_storage = MemoryEngine::new(1);
    let gated = Arc::new(GatedKvStorage::new(raw_storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));

    // 预设一个 10 字节字符串 "0000000000"
    let mut db = 0;
    router
        .execute(
            "SET",
            &[Bytes::from("doc"), Bytes::from("0000000000")],
            &mut db,
        )
        .await
        .unwrap();

    // 开启闸门拦截写操作
    gated.should_block.store(true, Ordering::SeqCst);

    // 发起第一条写入: 在 offset 0 写入 "AA"
    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "SETRANGE",
            &[Bytes::from("doc"), Bytes::from("0"), Bytes::from("AA")],
            &mut db,
        )
        .await
    });

    // 等待第一条抵达底层写闸门
    gated.entered.notified().await;

    // 关键红绿灯断言:
    // 第一条挂在闸门上时，第二条尝试获取同一把 KeyLock:
    // 旧代码持有 KeyLock 卡在 storage.set，第二条无法拿锁将在超时时间内超时失败;
    // 新代码已经提前释放 KeyLock，第二条能迅速拿锁并在锁内读到在途的 "AA00000000"
    let key = Bytes::from("doc");
    let key_lock = router.key_lock();
    let lock_res = tokio::time::timeout(Duration::from_millis(150), async {
        key_lock.lock(&key).await
    })
    .await;
    assert!(lock_res.is_ok(), "第二条写入未能在提前放锁后快速拿到锁");
    let guard = lock_res.unwrap();

    // 验证锁内能读到在途写入的切片
    let in_flight_val = router.in_flight_context().in_flight.get(0, &key);
    assert!(in_flight_val.is_some());
    let s = in_flight_val.unwrap().unwrap();
    assert_eq!(s.as_string().unwrap(), b"AA00000000");

    // 释放第二条拿到的锁
    drop(guard);

    // 释放写闸门，让第一条写完成
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    let res1 = h1.await.unwrap().unwrap();
    assert_eq!(res1, RespValue::Integer(10));

    // 发起第二条完整写入: offset 2 写入 "BB"
    let res2 = router
        .execute(
            "SETRANGE",
            &[Bytes::from("doc"), Bytes::from("2"), Bytes::from("BB")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(res2, RespValue::Integer(10));

    // 最终读取底库已提交的值: 必须包含两个切片 "AABB000000"
    let final_val = router
        .execute("GET", &[Bytes::from("doc")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        final_val,
        RespValue::BulkString(Some(Bytes::from_static(b"AABB000000")))
    );
}

#[tokio::test]
async fn test_setrange_public_get_reads_committed_only() {
    let raw_storage = MemoryEngine::new(1);
    let gated = Arc::new(GatedKvStorage::new(raw_storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));

    let mut db = 0;
    router
        .execute(
            "SET",
            &[Bytes::from("doc_iso"), Bytes::from("INIT")],
            &mut db,
        )
        .await
        .unwrap();

    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "SETRANGE",
            &[
                Bytes::from("doc_iso"),
                Bytes::from("0"),
                Bytes::from("NEW_"),
            ],
            &mut db,
        )
        .await
    });

    gated.entered.notified().await;

    // 验证公开 GET 读隔离: 在途写未落盘期间，独立 GET 读到的必须是底库已提交的 "INIT"，不能读未提交脏数据
    let get_val = router
        .execute("GET", &[Bytes::from("doc_iso")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        get_val,
        RespValue::BulkString(Some(Bytes::from_static(b"INIT")))
    );

    // 放行
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    h1.await.unwrap().unwrap();

    // 提交后读到 "NEW_"
    let final_val = router
        .execute("GET", &[Bytes::from("doc_iso")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        final_val,
        RespValue::BulkString(Some(Bytes::from_static(b"NEW_")))
    );
}

#[tokio::test]
async fn test_enqueue_failure_cascades_and_rolls_back() {
    let raw_storage = MemoryEngine::new(1);
    let gated = Arc::new(GatedKvStorage::new(raw_storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));

    let mut db = 0;
    router
        .execute(
            "SET",
            &[Bytes::from("fail_k"), Bytes::from("BASE")],
            &mut db,
        )
        .await
        .unwrap();

    // 模拟入队失败
    gated.fail_enqueue.store(true, Ordering::SeqCst);

    let res = router
        .execute(
            "SETRANGE",
            &[Bytes::from("fail_k"), Bytes::from("0"), Bytes::from("FAIL")],
            &mut db,
        )
        .await;
    assert!(res.is_err(), "入队失败应返回错误");

    // 验证在途项被回滚清除，底库依然是 "BASE"
    assert!(router.in_flight_context().in_flight.is_empty());
    let cur = router
        .execute("GET", &[Bytes::from("fail_k")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        cur,
        RespValue::BulkString(Some(Bytes::from_static(b"BASE")))
    );
}

#[tokio::test]
async fn test_concurrent_incr_50_accuracy() {
    let storage = MemoryEngine::new(1);
    let router = Arc::new(CommandRouter::new(storage));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let r = Arc::clone(&router);
        handles.push(tokio::spawn(async move {
            let mut db = 0;
            r.execute("INCR", &[Bytes::from("counter")], &mut db).await
        }));
    }

    for h in handles {
        h.await.unwrap().unwrap();
    }

    let mut db = 0;
    let final_res = router
        .execute("GET", &[Bytes::from("counter")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        final_res,
        RespValue::BulkString(Some(Bytes::from_static(b"50")))
    );
}

#[tokio::test]
async fn test_incr_followed_by_set_nx_in_flight() {
    let raw_storage = MemoryEngine::new(1);
    let gated = Arc::new(GatedKvStorage::new(raw_storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));

    // 阻塞在途提交
    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute("INCR", &[Bytes::from("cnt_nx")], &mut db).await
    });

    // 等待 INCR 算完新值 (1) 并登记在途，放锁后卡在存储闸门
    gated.entered.notified().await;

    // 并发执行 SET cnt_nx hello NX: 此时键已在在途缓冲中存在 (值为 1)，NX 应判定键已存在并返回 nil
    let r2 = Arc::clone(&router);
    let h2 = tokio::spawn(async move {
        let mut db2 = 0;
        r2.execute(
            "SET",
            &[
                Bytes::from("cnt_nx"),
                Bytes::from("hello"),
                Bytes::from("NX"),
            ],
            &mut db2,
        )
        .await
    });

    // 稍微等待调度以触发可能的误写
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 放行存储
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    let incr_res = h1.await.unwrap().unwrap();
    assert_eq!(incr_res, RespValue::Integer(1));

    let set_nx_res = h2.await.unwrap().unwrap();

    // NX 必须返回 nil (而不是误判不存在而写入 OK)
    assert_eq!(
        set_nx_res,
        RespValue::BulkString(None),
        "SET NX 应感知在途 INCR 产生的键存在，返回 nil"
    );

    // 最终值必须是 "1"
    let mut db = 0;
    let final_val = router
        .execute("GET", &[Bytes::from("cnt_nx")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        final_val,
        RespValue::BulkString(Some(Bytes::from_static(b"1")))
    );
}

#[tokio::test]
async fn test_incr_followed_by_set_xx_in_flight() {
    let raw_storage = MemoryEngine::new(1);
    let gated = Arc::new(GatedKvStorage::new(raw_storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));

    // 阻塞在途提交
    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute("INCR", &[Bytes::from("cnt_xx")], &mut db).await
    });

    // 等待 INCR 算完新值 (1) 并登记在途，放锁后卡在存储闸门
    gated.entered.notified().await;

    // 并发执行 SET cnt_xx world XX: 此时键在在途缓冲中存在，XX 判定通过并覆写为 "world"
    let r2 = Arc::clone(&router);
    let h2 = tokio::spawn(async move {
        let mut db2 = 0;
        r2.execute(
            "SET",
            &[
                Bytes::from("cnt_xx"),
                Bytes::from("world"),
                Bytes::from("XX"),
            ],
            &mut db2,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // 放行存储
    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    let incr_res = h1.await.unwrap().unwrap();
    assert_eq!(incr_res, RespValue::Integer(1));

    let set_xx_res = h2.await.unwrap().unwrap();
    assert_eq!(
        set_xx_res,
        RespValue::SimpleString("OK".into()),
        "SET XX 应感知在途 INCR 产生的键存在，成功返回 OK"
    );

    // 最终值必须是 "world"
    let mut db = 0;
    let final_val = router
        .execute("GET", &[Bytes::from("cnt_xx")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        final_val,
        RespValue::BulkString(Some(Bytes::from_static(b"world")))
    );
}

#[tokio::test]
async fn test_incr_followed_by_getdel_in_flight() {
    let raw_storage = MemoryEngine::new(1);
    let gated = Arc::new(GatedKvStorage::new(raw_storage));
    let router = Arc::new(CommandRouter::new(gated.clone()));

    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute("INCR", &[Bytes::from("cnt_gd")], &mut db).await
    });

    gated.entered.notified().await;

    // 并发执行 GETDEL cnt_gd: 必须读出在途的 "1" 并标记在途删除
    let r2 = Arc::clone(&router);
    let h2 = tokio::spawn(async move {
        let mut db2 = 0;
        r2.execute("GETDEL", &[Bytes::from("cnt_gd")], &mut db2)
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    let incr_res = h1.await.unwrap().unwrap();
    assert_eq!(incr_res, RespValue::Integer(1));

    let getdel_res = h2.await.unwrap().unwrap();
    assert_eq!(
        getdel_res,
        RespValue::BulkString(Some(Bytes::from_static(b"1"))),
        "GETDEL 应读到在途 INCR 产生的 1"
    );

    // 最终键已被删除，GET 返回 nil
    let mut db = 0;
    let final_val = router
        .execute("GET", &[Bytes::from("cnt_gd")], &mut db)
        .await
        .unwrap();
    assert_eq!(final_val, RespValue::BulkString(None));
}
