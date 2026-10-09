//! 一条 EVAL 的写入必须一次 apply_writes，而不是每个 key 一次 put_many/set.
//! @component aikv-storage

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Notify;

use aikv::command::CommandRouter;
use aikv::error::{Error, Result};
use aikv::protocol::RespValue;
use aikv::storage::adapter::{AdapterWriteOp, KvStorageAdapter, StorageAdapter};

struct Gate {
    key: Vec<u8>,
    entered: Notify,
    release: Notify,
    armed: AtomicBool,
}

struct CountingAdapter {
    data: Mutex<std::collections::HashMap<Vec<u8>, Vec<u8>>>,
    set_calls: AtomicUsize,
    put_many_calls: AtomicUsize,
    apply_writes_calls: AtomicUsize,
    get_calls: AtomicUsize,
    max_apply_ops: AtomicUsize,
    fail_apply: AtomicBool,
    gate: Mutex<Option<Arc<Gate>>>,
}

impl CountingAdapter {
    fn new() -> Self {
        Self {
            data: Mutex::new(std::collections::HashMap::new()),
            set_calls: AtomicUsize::new(0),
            put_many_calls: AtomicUsize::new(0),
            apply_writes_calls: AtomicUsize::new(0),
            get_calls: AtomicUsize::new(0),
            max_apply_ops: AtomicUsize::new(0),
            fail_apply: AtomicBool::new(false),
            gate: Mutex::new(None),
        }
    }

    fn arm_gate(&self, key: &[u8]) -> Arc<Gate> {
        let gate = Arc::new(Gate {
            key: key.to_vec(),
            entered: Notify::new(),
            release: Notify::new(),
            armed: AtomicBool::new(true),
        });
        *self.gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }
}

#[async_trait]
impl StorageAdapter for CountingAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        self.get_calls.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            if key == gate.key && gate.armed.swap(false, Ordering::SeqCst) {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
        }
        Ok(self.data.lock().unwrap().get(&key).cloned())
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        self.data.lock().unwrap().insert(key, value);
        Ok(())
    }

    async fn put_many(&self, ops: Vec<(Vec<u8>, Vec<u8>)>) -> Result<()> {
        self.put_many_calls.fetch_add(1, Ordering::SeqCst);
        for (key, value) in ops {
            self.data.lock().unwrap().insert(key, value);
        }
        Ok(())
    }

    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<()> {
        self.apply_writes_calls.fetch_add(1, Ordering::SeqCst);
        self.max_apply_ops.fetch_max(ops.len(), Ordering::SeqCst);
        if self.fail_apply.swap(false, Ordering::SeqCst) {
            return Err(Error::Storage("forced failure".into()));
        }
        let mut data = self.data.lock().unwrap();
        for op in ops {
            match op {
                AdapterWriteOp::Put { key, value } => {
                    data.insert(key, value);
                }
                AdapterWriteOp::Delete { key } => {
                    data.remove(&key);
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().remove(&key).is_some())
    }

    async fn exists(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().contains_key(&key))
    }

    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        self.apply_writes(batch).await
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let to_remove: Vec<Vec<u8>> = data
            .keys()
            .filter(|k| k.as_slice() >= start.as_slice() && k.as_slice() < end.as_slice())
            .cloned()
            .collect();
        for k in to_remove {
            data.remove(&k);
        }
        Ok(())
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.data.lock().unwrap().len())
    }

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.data.lock().unwrap().is_empty())
    }

    async fn clear(&self) -> Result<()> {
        self.data.lock().unwrap().clear();
        Ok(())
    }
}

fn router(mock: Arc<CountingAdapter>) -> CommandRouter {
    let kv = KvStorageAdapter::new(mock);
    CommandRouter::new(kv)
}

fn b(s: &str) -> Bytes {
    Bytes::from(s.to_string())
}

fn bulk(resp: RespValue) -> Vec<u8> {
    match resp {
        RespValue::BulkString(Some(v)) => v.to_vec(),
        other => panic!("expected bulk, got {other:?}"),
    }
}

#[tokio::test]
async fn script_commit_is_one_apply_writes() {
    let mock = Arc::new(CountingAdapter::new());
    let r = router(mock.clone());
    let mut db = 0;
    r.execute("SET", &[b("gone"), b("old")], &mut db)
        .await
        .unwrap();

    let apply_before = mock.apply_writes_calls.load(Ordering::SeqCst);
    let put_before = mock.put_many_calls.load(Ordering::SeqCst);
    let set_before = mock.set_calls.load(Ordering::SeqCst);

    let script = r#"
        redis.call('SET', KEYS[1], 'va')
        redis.call('HSET', KEYS[2], 'f', 'vb')
        redis.call('ZADD', KEYS[3], '1', 'm')
        redis.call('DEL', KEYS[4])
        return 1
    "#;
    r.execute(
        "EVAL",
        &[b(script), b("4"), b("a"), b("h"), b("z"), b("gone")],
        &mut db,
    )
    .await
    .unwrap();

    assert_eq!(
        mock.apply_writes_calls.load(Ordering::SeqCst) - apply_before,
        1
    );
    assert_eq!(mock.put_many_calls.load(Ordering::SeqCst) - put_before, 0);
    assert_eq!(mock.set_calls.load(Ordering::SeqCst) - set_before, 0);
    assert_eq!(
        bulk(r.execute("GET", &[b("a")], &mut db).await.unwrap()),
        b"va"
    );
    assert_eq!(
        bulk(r.execute("HGET", &[b("h"), b("f")], &mut db).await.unwrap()),
        b"vb"
    );
    assert_eq!(
        bulk(
            r.execute("ZSCORE", &[b("z"), b("m")], &mut db)
                .await
                .unwrap()
        ),
        b"1"
    );
    assert!(matches!(
        r.execute("GET", &[b("gone")], &mut db).await.unwrap(),
        RespValue::BulkString(None)
    ));
}

#[tokio::test]
async fn script_without_writes_skips_apply_writes() {
    let mock = Arc::new(CountingAdapter::new());
    let r = router(mock.clone());
    let mut db = 0;
    r.execute("EVAL", &[b("return 1"), b("0")], &mut db)
        .await
        .unwrap();
    r.execute(
        "EVAL",
        &[b("return redis.call('DEL', KEYS[1])"), b("1"), b("missing")],
        &mut db,
    )
    .await
    .unwrap();
    assert_eq!(mock.apply_writes_calls.load(Ordering::SeqCst), 0);
    assert_eq!(mock.put_many_calls.load(Ordering::SeqCst), 0);
    assert_eq!(mock.set_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn script_reuses_loaded_hash_and_zset() {
    let mock = Arc::new(CountingAdapter::new());
    let r = router(mock.clone());
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f1"), b("a"), b("f2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z"), b("1"), b("m")], &mut db)
        .await
        .unwrap();

    let gets_before = mock.get_calls.load(Ordering::SeqCst);
    let script = r#"
        redis.call('HGET', KEYS[1], 'f1')
        redis.call('HGET', KEYS[1], 'f1')
        redis.call('HDEL', KEYS[1], 'f1')
        redis.call('ZCARD', KEYS[2])
        redis.call('ZCARD', KEYS[2])
        return redis.call('HGET', KEYS[1], 'f2')
    "#;
    let got = r
        .execute("EVAL", &[b(script), b("2"), b("h"), b("z")], &mut db)
        .await
        .unwrap();
    assert_eq!(bulk(got), b"b");
    assert_eq!(mock.get_calls.load(Ordering::SeqCst) - gets_before, 2);
    assert_eq!(
        bulk(
            r.execute("HGET", &[b("h"), b("f2")], &mut db)
                .await
                .unwrap()
        ),
        b"b"
    );
}

#[tokio::test]
async fn same_declared_keys_share_one_apply_and_read_previous_write() {
    let mock = Arc::new(CountingAdapter::new());
    let gate = mock.arm_gate(b"0:gate");
    let router = Arc::new(router(mock.clone()));
    let first = Arc::clone(&router);
    let first_task = tokio::spawn(async move {
        let mut db = 0;
        first
            .execute(
                "EVAL",
                &[
                    b("redis.call('SET', KEYS[1], 'first'); redis.call('GET', KEYS[2]); return 1"),
                    b("2"),
                    b("data"),
                    b("gate"),
                ],
                &mut db,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("first script did not reach the gate");

    let second = Arc::clone(&router);
    let second_task = tokio::spawn(async move {
        let mut db = 0;
        second
            .execute(
                "EVAL",
                &[
                    b("local v = redis.call('GET', KEYS[1]); redis.call('SET', KEYS[1], 'second'); return v"),
                    b("2"),
                    b("data"),
                    b("gate"),
                ],
                &mut db,
            )
            .await
    });
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    gate.release.notify_one();

    let first_result = tokio::time::timeout(Duration::from_secs(5), first_task)
        .await
        .expect("first script timed out")
        .unwrap()
        .unwrap();
    let second_result = tokio::time::timeout(Duration::from_secs(5), second_task)
        .await
        .expect("second script timed out")
        .unwrap()
        .unwrap();
    assert!(matches!(first_result, RespValue::Integer(1)));
    assert_eq!(bulk(second_result), b"first");
    assert_eq!(mock.apply_writes_calls.load(Ordering::SeqCst), 1);
    let mut db = 0;
    assert_eq!(
        bulk(router.execute("GET", &[b("data")], &mut db).await.unwrap()),
        b"second"
    );
}

#[tokio::test]
async fn different_declared_keys_do_not_see_uncommitted_writes() {
    let mock = Arc::new(CountingAdapter::new());
    let gate = mock.arm_gate(b"0:gate");
    let router = Arc::new(router(mock.clone()));
    let writer = Arc::clone(&router);
    let writer_task = tokio::spawn(async move {
        let mut db = 0;
        writer
            .execute(
                "EVAL",
                &[
                    b("redis.call('SET', KEYS[1], 'from-a'); redis.call('GET', KEYS[2]); return 1"),
                    b("2"),
                    b("keyA"),
                    b("gate"),
                ],
                &mut db,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("writer did not reach the gate");

    let mut db = 0;
    let seen = router
        .execute(
            "EVAL",
            &[
                b("return redis.call('GET', ARGV[1])"),
                b("1"),
                b("keyB"),
                b("keyA"),
            ],
            &mut db,
        )
        .await
        .unwrap();
    assert!(matches!(seen, RespValue::BulkString(None)));
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), writer_task)
        .await
        .expect("writer timed out")
        .unwrap()
        .unwrap();

    assert_eq!(
        bulk(router.execute("GET", &[b("keyA")], &mut db).await.unwrap()),
        b"from-a"
    );
    assert!(matches!(
        router.execute("GET", &[b("keyB")], &mut db).await.unwrap(),
        RespValue::BulkString(None)
    ));
}

#[tokio::test]
async fn apply_failure_fails_the_whole_burst_and_keeps_storage() {
    let mock = Arc::new(CountingAdapter::new());
    mock.fail_apply.store(true, Ordering::SeqCst);
    let gate = mock.arm_gate(b"0:gate");
    let router = Arc::new(router(mock.clone()));
    let first = Arc::clone(&router);
    let first_task = tokio::spawn(async move {
        let mut db = 0;
        first
            .execute(
                "EVAL",
                &[
                    b("redis.call('SET', KEYS[1], 'a'); redis.call('GET', KEYS[2]); return 1"),
                    b("2"),
                    b("data"),
                    b("gate"),
                ],
                &mut db,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("first script did not reach the gate");
    let second = Arc::clone(&router);
    let second_task = tokio::spawn(async move {
        let mut db = 0;
        second
            .execute(
                "EVAL",
                &[
                    b("redis.call('SET', KEYS[1], 'b'); return 1"),
                    b("2"),
                    b("data"),
                    b("gate"),
                ],
                &mut db,
            )
            .await
    });
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    gate.release.notify_one();

    let first_result = tokio::time::timeout(Duration::from_secs(5), first_task)
        .await
        .expect("first script timed out")
        .unwrap()
        .unwrap_err();
    let second_result = tokio::time::timeout(Duration::from_secs(5), second_task)
        .await
        .expect("second script timed out")
        .unwrap()
        .unwrap_err();
    assert!(first_result.to_string().contains("forced failure"));
    assert!(second_result.to_string().contains("forced failure"));
    let mut db = 0;
    assert!(matches!(
        router.execute("GET", &[b("data")], &mut db).await.unwrap(),
        RespValue::BulkString(None)
    ));
}

#[tokio::test]
async fn queued_single_key_sets_stay_within_batch_cap() {
    let mock = Arc::new(CountingAdapter::new());
    let gate = mock.arm_gate(b"0:gate");
    let router = Arc::new(router(mock.clone()));
    let mut tasks = Vec::new();
    let started = Arc::new(AtomicUsize::new(0));

    for i in 0..600 {
        let router = Arc::clone(&router);
        let started = Arc::clone(&started);
        tasks.push(tokio::spawn(async move {
            started.fetch_add(1, Ordering::SeqCst);
            let mut db = 0;
            let key = format!("k{i}");
            let value = format!("v{i}");
            router
                .execute(
                    "EVAL",
                    &[
                        b("redis.call('SET', ARGV[1], ARGV[2]); return redis.call('GET', KEYS[1])"),
                        b("1"),
                        b("gate"),
                        Bytes::from(key),
                        Bytes::from(value),
                    ],
                    &mut db,
                )
                .await
        }));
        if i == 0 {
            tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
                .await
                .expect("first of 600 did not reach the gate");
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while started.load(Ordering::SeqCst) < 600 && std::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 600);
    let settle = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < settle {
        tokio::task::yield_now().await;
    }
    gate.release.notify_one();

    for task in tasks {
        tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .expect("queued script timed out")
            .unwrap()
            .unwrap();
    }
    assert!(mock.max_apply_ops.load(Ordering::SeqCst) <= 512);
    assert!(mock.apply_writes_calls.load(Ordering::SeqCst) < 600);
    let mut db = 0;
    for i in 0..600 {
        let key = format!("k{i}");
        let value = format!("v{i}");
        assert_eq!(
            bulk(
                router
                    .execute("GET", &[Bytes::from(key)], &mut db)
                    .await
                    .unwrap()
            ),
            value.as_bytes()
        );
    }
}

#[tokio::test]
async fn buffered_same_key_scripts_share_one_apply() {
    let mock = Arc::new(CountingAdapter::new());
    let router = router(mock.clone());
    let results = router
        .execute_script_burst(
            0,
            vec![
                (
                    "EVAL".into(),
                    vec![
                        b("redis.call('SET', KEYS[1], 'first'); return 'first'"),
                        b("1"),
                        b("data"),
                    ],
                ),
                (
                    "EVAL".into(),
                    vec![
                        b("local v = redis.call('GET', KEYS[1]); redis.call('SET', KEYS[1], 'second'); return v"),
                        b("1"),
                        b("data"),
                    ],
                ),
            ],
        )
        .await;
    assert_eq!(bulk(results[0].as_ref().unwrap().clone()), b"first");
    assert_eq!(bulk(results[1].as_ref().unwrap().clone()), b"first");
    assert_eq!(mock.apply_writes_calls.load(Ordering::SeqCst), 1);
    let mut db = 0;
    assert_eq!(
        bulk(router.execute("GET", &[b("data")], &mut db).await.unwrap()),
        b"second"
    );
}

#[tokio::test]
async fn buffered_interleaved_keys_commit_in_arrival_order() {
    let mock = Arc::new(CountingAdapter::new());
    let router = router(mock.clone());
    let results = router
        .execute_script_burst(
            0,
            vec![
                (
                    "EVAL".into(),
                    vec![
                        b("redis.call('SET', KEYS[1], 'from-a'); return 1"),
                        b("1"),
                        b("dataA"),
                    ],
                ),
                (
                    "EVAL".into(),
                    vec![b("return redis.call('GET', 'dataA')"), b("1"), b("dataB")],
                ),
                (
                    "EVAL".into(),
                    vec![
                        b("redis.call('SET', KEYS[1], 'second'); return 1"),
                        b("1"),
                        b("dataA"),
                    ],
                ),
            ],
        )
        .await;
    assert!(matches!(
        results[0].as_ref().unwrap(),
        RespValue::Integer(1)
    ));
    assert_eq!(bulk(results[1].as_ref().unwrap().clone()), b"from-a");
    assert!(matches!(
        results[2].as_ref().unwrap(),
        RespValue::Integer(1)
    ));
    assert_eq!(mock.apply_writes_calls.load(Ordering::SeqCst), 2);
    let mut db = 0;
    assert_eq!(
        bulk(router.execute("GET", &[b("dataA")], &mut db).await.unwrap()),
        b"second"
    );
}
