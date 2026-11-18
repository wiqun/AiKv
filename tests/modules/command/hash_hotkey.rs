//! 单热 Key 算子 (HSET / HDEL / HINCRBY) 提前放锁与在途折叠确定性回归测试.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use aikv::storage::subkey;
use aikv::storage::types::{
    BatchWriteHandle, CollectionKind, KeyspaceStats, KvStorage, ScanResult, StorageEngineKind,
    StoredValue, ValueType, WriteOp,
};
use aikv::storage::KvStorageAdapter;

struct GatedKvStorage {
    inner: Arc<dyn KvStorage>,
    gate: Arc<Notify>,
    entered: Arc<Notify>,
    should_block: AtomicBool,
    fail_enqueue: AtomicBool,
    enqueue_calls: AtomicUsize,
}

impl GatedKvStorage {
    fn new(inner: Arc<dyn KvStorage>) -> Self {
        Self {
            inner,
            gate: Arc::new(Notify::new()),
            entered: Arc::new(Notify::new()),
            should_block: AtomicBool::new(false),
            fail_enqueue: AtomicBool::new(false),
            enqueue_calls: AtomicUsize::new(0),
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
        self.inner.set(db, key, value).await
    }

    async fn set_with_ttl(
        &self,
        db: usize,
        key: &[u8],
        value: &[u8],
        expire_at_ms: u64,
    ) -> Result<()> {
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
        self.enqueue_calls.fetch_add(1, Ordering::SeqCst);
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
        self.inner.raw_range_scan(start, end, limit, reverse).await
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
async fn test_hset_inline_hotkey_gate_releases_lock_and_folds() {
    let (_dir, gated, router) = setup_gated_env();
    let mut db = 0;

    gated.should_block.store(true, Ordering::SeqCst);

    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "HSET",
            &[Bytes::from("h_hot"), Bytes::from("f1"), Bytes::from("v1")],
            &mut db,
        )
        .await
    });

    gated.entered.notified().await;

    // 第一条阻塞在 enqueue_batch 时，第二条请求必须能立即拿到 KeyLock
    let key = Bytes::from("h_hot");
    let key_lock = router.key_lock();
    let lock_res = tokio::time::timeout(Duration::from_millis(150), async {
        key_lock.lock(&key).await
    })
    .await;
    assert!(lock_res.is_ok(), "HSET 未在 enqueue 前释放 KeyLock");
    let guard = lock_res.unwrap();

    let stored = router
        .in_flight_context()
        .in_flight
        .get(0, &key)
        .expect("in_flight must hold h_hot")
        .expect("h_hot must not be deleted");
    match &stored.value {
        ValueType::Hash(map) => assert_eq!(map.get(b"f1".as_slice()), Some(&b"v1".to_vec())),
        other => panic!("expected inline hash, got {other:?}"),
    }
    drop(guard);

    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    assert_eq!(h1.await.unwrap().unwrap(), RespValue::Integer(1));

    let res2 = router
        .execute(
            "HSET",
            &[Bytes::from("h_hot"), Bytes::from("f2"), Bytes::from("v2")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(res2, RespValue::Integer(1));

    let len = router
        .execute("HLEN", &[Bytes::from("h_hot")], &mut db)
        .await
        .unwrap();
    assert_eq!(len, RespValue::Integer(2));
}

#[tokio::test]
async fn test_hset_large_subkey_migration_and_update_single_batch() {
    let (_dir, gated, router) = setup_gated_env();
    let mut db = 0;

    // 1. 一次写入 65 个字段，触发 inline -> subkey 迁移，应只调一次 enqueue_batch
    let mut args = vec![Bytes::from("h_big")];
    for i in 0..65 {
        args.push(Bytes::from(format!("f{i:02}")));
        args.push(Bytes::from(format!("v{i:02}")));
    }
    let before_calls = gated.enqueue_calls.load(Ordering::SeqCst);
    let res = router.execute("HSET", &args, &mut db).await.unwrap();
    assert_eq!(res, RespValue::Integer(65));
    assert_eq!(
        gated.enqueue_calls.load(Ordering::SeqCst) - before_calls,
        1,
        "超过 64 字段迁移为 subkey 必须合进单次 enqueue_batch"
    );

    // 2. 阻塞闸门下，更新已有字段 + 新增第 66 个字段，KeyLock 必须立刻释放且在途可见
    gated.should_block.store(true, Ordering::SeqCst);
    let r1 = Arc::clone(&router);
    let h1 = tokio::spawn(async move {
        let mut db = 0;
        r1.execute(
            "HSET",
            &[
                Bytes::from("h_big"),
                Bytes::from("f00"),
                Bytes::from("v00_new"),
                Bytes::from("f65"),
                Bytes::from("v65"),
            ],
            &mut db,
        )
        .await
    });

    gated.entered.notified().await;

    let key = Bytes::from("h_big");
    let key_lock = router.key_lock();
    let guard = tokio::time::timeout(Duration::from_millis(150), key_lock.lock(&key))
        .await
        .expect("subkey HSET 未在 enqueue 前释放 KeyLock");
    let stored = router
        .in_flight_context()
        .in_flight
        .get(0, &key)
        .unwrap()
        .unwrap();
    let version = match stored.value {
        ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count: 66,
            version,
            ..
        } => version,
        other => panic!("expected CollectionHeader(Hash, 66), got {other:?}"),
    };
    let encoded = AiDbEngine::encode_key(0, &key);
    let f65_key = subkey::encode_hash_field_key(&encoded, version, b"f65");
    assert_eq!(
        router.in_flight_context().in_flight.get_raw(&f65_key),
        Some(Some(b"v65".to_vec()))
    );
    drop(guard);

    gated.should_block.store(false, Ordering::SeqCst);
    gated.gate.notify_waiters();
    assert_eq!(h1.await.unwrap().unwrap(), RespValue::Integer(1));

    assert_eq!(
        router
            .execute("HLEN", &[Bytes::from("h_big")], &mut db)
            .await
            .unwrap(),
        RespValue::Integer(66)
    );
    assert_eq!(
        router
            .execute("HGET", &[Bytes::from("h_big"), Bytes::from("f00")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(Bytes::from_static(b"v00_new")))
    );
}

#[tokio::test]
async fn test_hdel_subkey_to_empty_and_hincrby_fold() {
    let (_dir, _gated, router) = setup_gated_env();
    let mut db = 0;

    // 创建 65 字段大 Hash
    let mut hset_args = vec![Bytes::from("h_del")];
    let mut hdel_args = vec![Bytes::from("h_del")];
    for i in 0..65 {
        let f = Bytes::from(format!("f{i:02}"));
        hset_args.push(f.clone());
        hset_args.push(Bytes::from("1"));
        hdel_args.push(f);
    }
    router.execute("HSET", &hset_args, &mut db).await.unwrap();

    // HINCRBY 递增既有字段与新字段
    assert_eq!(
        router
            .execute(
                "HINCRBY",
                &[Bytes::from("h_del"), Bytes::from("f00"), Bytes::from("4")],
                &mut db
            )
            .await
            .unwrap(),
        RespValue::Integer(5)
    );
    assert_eq!(
        router
            .execute("HLEN", &[Bytes::from("h_del")], &mut db)
            .await
            .unwrap(),
        RespValue::Integer(65)
    );

    // 全部删空后 key 消失
    assert_eq!(
        router.execute("HDEL", &hdel_args, &mut db).await.unwrap(),
        RespValue::Integer(65)
    );
    assert_eq!(
        router
            .execute("EXISTS", &[Bytes::from("h_del")], &mut db)
            .await
            .unwrap(),
        RespValue::Integer(0)
    );
}

#[tokio::test]
async fn test_hset_enqueue_failure_rolls_back_in_flight() {
    let (_dir, gated, router) = setup_gated_env();
    let mut db = 0;

    gated.fail_enqueue.store(true, Ordering::SeqCst);
    let err = router
        .execute(
            "HSET",
            &[Bytes::from("h_fail"), Bytes::from("f"), Bytes::from("v")],
            &mut db,
        )
        .await;
    assert!(err.is_err());
    assert!(router.in_flight_context().in_flight.is_empty());
    assert_eq!(
        router
            .execute("EXISTS", &[Bytes::from("h_fail")], &mut db)
            .await
            .unwrap(),
        RespValue::Integer(0)
    );
}

#[tokio::test]
async fn test_hset_inline_noop_same_value_skips_enqueue() {
    let (_dir, gated, router) = setup_gated_env();
    let mut db = 0;

    assert_eq!(
        router
            .execute(
                "HSET",
                &[Bytes::from("h_noop"), Bytes::from("f1"), Bytes::from("v1")],
                &mut db,
            )
            .await
            .unwrap(),
        RespValue::Integer(1)
    );

    let before = gated.enqueue_calls.load(Ordering::SeqCst);
    assert_eq!(
        router
            .execute(
                "HSET",
                &[Bytes::from("h_noop"), Bytes::from("f1"), Bytes::from("v1")],
                &mut db,
            )
            .await
            .unwrap(),
        RespValue::Integer(0)
    );
    assert_eq!(
        gated.enqueue_calls.load(Ordering::SeqCst),
        before,
        "inline HSET 同值覆盖写应跳过 enqueue_batch"
    );

    assert_eq!(
        router
            .execute(
                "HSET",
                &[Bytes::from("h_noop"), Bytes::from("f1"), Bytes::from("v2")],
                &mut db,
            )
            .await
            .unwrap(),
        RespValue::Integer(0)
    );
    assert_eq!(gated.enqueue_calls.load(Ordering::SeqCst), before + 1);
    assert_eq!(
        router
            .execute("HGET", &[Bytes::from("h_noop"), Bytes::from("f1")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(Bytes::from_static(b"v2")))
    );
}

#[tokio::test]
async fn test_hset_subkey_noop_same_value_skips_enqueue() {
    let (_dir, gated, router) = setup_gated_env();
    let mut db = 0;

    let mut args = vec![Bytes::from("h_big_noop")];
    for i in 0..65 {
        args.push(Bytes::from(format!("f{i:02}")));
        args.push(Bytes::from(format!("v{i:02}")));
    }
    assert_eq!(
        router.execute("HSET", &args, &mut db).await.unwrap(),
        RespValue::Integer(65)
    );

    let before = gated.enqueue_calls.load(Ordering::SeqCst);
    assert_eq!(
        router
            .execute(
                "HSET",
                &[
                    Bytes::from("h_big_noop"),
                    Bytes::from("f00"),
                    Bytes::from("v00"),
                    Bytes::from("f01"),
                    Bytes::from("v01"),
                ],
                &mut db,
            )
            .await
            .unwrap(),
        RespValue::Integer(0)
    );
    assert_eq!(
        gated.enqueue_calls.load(Ordering::SeqCst),
        before,
        "subkey HSET 同值覆盖写应跳过 enqueue_batch"
    );

    assert_eq!(
        router
            .execute(
                "HSET",
                &[
                    Bytes::from("h_big_noop"),
                    Bytes::from("f00"),
                    Bytes::from("v00"),
                    Bytes::from("f01"),
                    Bytes::from("v01_new"),
                ],
                &mut db,
            )
            .await
            .unwrap(),
        RespValue::Integer(0)
    );
    assert_eq!(gated.enqueue_calls.load(Ordering::SeqCst), before + 1);
    assert_eq!(
        router
            .execute(
                "HGET",
                &[Bytes::from("h_big_noop"), Bytes::from("f01")],
                &mut db,
            )
            .await
            .unwrap(),
        RespValue::BulkString(Some(Bytes::from_static(b"v01_new")))
    );
}
