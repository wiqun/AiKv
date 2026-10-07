//! memory vs AiDb 双引擎行为一致性测试
//!
//! 同一命令序列分别在 MemoryEngine 和 KvStorageAdapter(AiDbEngine) 上执行,
//! 断言结果一致.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use aikv::storage::{
    AiDbEngine, KvStorage, KvStorageAdapter, MemoryEngine, StoredValue, ValueType,
};
use tempfile::TempDir;

fn mem() -> Arc<dyn KvStorage> {
    MemoryEngine::new(16)
}

fn aidb(dir: &TempDir) -> Arc<dyn KvStorage> {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    KvStorageAdapter::new(engine)
}

// ── String ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_string_set_get() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage.set(0, b"k", b"hello").await.unwrap();
        let v = storage.get(0, b"k").await.unwrap();
        assert_eq!(v, Some(b"hello".to_vec()), "{label}: GET after SET");

        let missing = storage.get(0, b"nosuchkey").await.unwrap();
        assert_eq!(missing, None, "{label}: GET missing key");
    }
}

#[tokio::test]
async fn test_compat_string_del() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage.set(0, b"k", b"v").await.unwrap();
        assert!(
            storage.delete(0, b"k").await.unwrap(),
            "{label}: DEL existing"
        );
        assert_eq!(
            storage.get(0, b"k").await.unwrap(),
            None,
            "{label}: GET after DEL"
        );
        assert!(
            !storage.delete(0, b"k").await.unwrap(),
            "{label}: DEL missing"
        );
    }
}

#[tokio::test]
async fn test_compat_exists() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        assert!(
            !storage.exists(0, b"k").await.unwrap(),
            "{label}: EXISTS missing"
        );
        storage.set(0, b"k", b"v").await.unwrap();
        assert!(
            storage.exists(0, b"k").await.unwrap(),
            "{label}: EXISTS present"
        );
        assert_eq!(
            storage.get(0, b"k").await.unwrap().as_deref(),
            Some(b"v".as_slice()),
            "{label}: EXISTS matches GET"
        );
    }
}

#[tokio::test]
async fn test_compat_set_nx_does_not_overwrite() {
    use aikv::command::CommandRouter;
    use aikv::protocol::RespValue;
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        let router = CommandRouter::new(Arc::clone(&storage));
        let mut db = 0;
        let arg = |s: &str| Bytes::copy_from_slice(s.as_bytes());
        router
            .execute("SET", &[arg("k"), arg("v")], &mut db)
            .await
            .unwrap();
        let nx = router
            .execute("SET", &[arg("k"), arg("v2"), arg("NX")], &mut db)
            .await
            .unwrap();
        assert_eq!(nx, RespValue::BulkString(None), "{label}: SET NX");
        assert_eq!(
            storage.get(0, b"k").await.unwrap().as_deref(),
            Some(b"v".as_slice()),
            "{label}: SET NX leaves the value"
        );
    }
}

#[tokio::test]
async fn test_compat_mget_wrongtype() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage.set(0, b"s", b"hello").await.unwrap();
        storage
            .set_typed(
                0,
                b"h",
                StoredValue {
                    value: ValueType::Hash(HashMap::new()),
                    expires_at: None,
                },
            )
            .await
            .unwrap();

        let vals = storage
            .mget(0, &[b"s".to_vec(), b"h".to_vec(), b"missing".to_vec()])
            .await
            .unwrap();
        assert_eq!(vals[0], Some(b"hello".to_vec()), "{label}: MGET string");
        assert_eq!(vals[1], None, "{label}: MGET hash → nil");
        assert_eq!(vals[2], None, "{label}: MGET missing");
    }
}

#[tokio::test]
async fn test_compat_mget_mset() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage
            .mset(
                0,
                &[
                    (b"a".to_vec(), b"1".to_vec()),
                    (b"b".to_vec(), b"2".to_vec()),
                ],
            )
            .await
            .unwrap();
        let vals = storage
            .mget(0, &[b"a".to_vec(), b"b".to_vec(), b"c".to_vec()])
            .await
            .unwrap();
        assert_eq!(vals[0], Some(b"1".to_vec()), "{label}: MGET a");
        assert_eq!(vals[1], Some(b"2".to_vec()), "{label}: MGET b");
        assert_eq!(vals[2], None, "{label}: MGET missing c");
    }
}

// ── Hash ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_hash_set_get() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        let mut map = HashMap::new();
        map.insert(b"field1".to_vec(), b"val1".to_vec());
        storage
            .set_typed(
                0,
                b"myhash",
                StoredValue {
                    value: ValueType::Hash(map),
                    expires_at: None,
                },
            )
            .await
            .unwrap();

        let stored = storage.get_typed(0, b"myhash").await.unwrap().unwrap();
        let ValueType::Hash(got) = stored.value else {
            panic!("{label}: expected Hash")
        };
        assert_eq!(
            got.get(b"field1".as_ref()),
            Some(&b"val1".to_vec()),
            "{label}: HGET"
        );
        assert_eq!(
            got.get(b"nosuchfield".as_ref()),
            None,
            "{label}: HGET missing field"
        );
    }
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_list_push_range() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        let list = VecDeque::from(vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        storage
            .set_typed(
                0,
                b"mylist",
                StoredValue {
                    value: ValueType::List(list),
                    expires_at: None,
                },
            )
            .await
            .unwrap();

        let stored = storage.get_typed(0, b"mylist").await.unwrap().unwrap();
        let ValueType::List(got) = stored.value else {
            panic!("{label}: expected List")
        };
        let items: Vec<_> = got.into_iter().collect();
        assert_eq!(
            items,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            "{label}: LRANGE"
        );
    }
}

// ── Set ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_set_members() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        let mut set = HashSet::new();
        set.insert(b"m1".to_vec());
        set.insert(b"m2".to_vec());
        storage
            .set_typed(
                0,
                b"myset",
                StoredValue {
                    value: ValueType::Set(set),
                    expires_at: None,
                },
            )
            .await
            .unwrap();

        let stored = storage.get_typed(0, b"myset").await.unwrap().unwrap();
        let ValueType::Set(got) = stored.value else {
            panic!("{label}: expected Set")
        };
        assert!(got.contains(b"m1".as_ref()), "{label}: SISMEMBER m1");
        assert!(
            !got.contains(b"m3".as_ref()),
            "{label}: SISMEMBER missing m3"
        );
    }
}

// ── ZSet ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_zset_score() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        let mut zset = BTreeMap::new();
        zset.insert(b"m1".to_vec(), 1.5f64);
        zset.insert(b"m2".to_vec(), 2.0f64);
        storage
            .set_typed(
                0,
                b"myzset",
                StoredValue {
                    value: ValueType::ZSet(zset),
                    expires_at: None,
                },
            )
            .await
            .unwrap();

        let stored = storage.get_typed(0, b"myzset").await.unwrap().unwrap();
        let ValueType::ZSet(got) = stored.value else {
            panic!("{label}: expected ZSet")
        };
        assert_eq!(got.get(b"m1".as_ref()), Some(&1.5f64), "{label}: ZSCORE m1");
        assert_eq!(got.get(b"m2".as_ref()), Some(&2.0f64), "{label}: ZSCORE m2");
        assert_eq!(got.get(b"nosuch".as_ref()), None, "{label}: ZSCORE missing");
    }
}

// ── TTL ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_ttl_persist() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage.set(0, b"k", b"v").await.unwrap();
        // set TTL of 60 seconds
        let expire_at = aikv::storage::now_ms() + 60_000;
        storage
            .set_with_ttl(0, b"k", b"v", expire_at)
            .await
            .unwrap();

        let ttl = storage.ttl(0, b"k").await.unwrap();
        assert!(ttl.unwrap() > 0, "{label}: TTL should be positive");
        assert!(ttl.unwrap() <= 60_000, "{label}: TTL should be ≤ 60s");

        // persist removes TTL
        assert!(storage.persist(0, b"k").await.unwrap(), "{label}: PERSIST");
        assert_eq!(
            storage.ttl(0, b"k").await.unwrap(),
            Some(-1),
            "{label}: TTL after PERSIST"
        );
    }
}

// ── Rename ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_rename() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage.set(0, b"src", b"value").await.unwrap();
        storage.rename_key(0, b"src", b"dst").await.unwrap();
        assert_eq!(
            storage.get(0, b"dst").await.unwrap(),
            Some(b"value".to_vec()),
            "{label}: RENAME dst"
        );
        assert_eq!(
            storage.get(0, b"src").await.unwrap(),
            None,
            "{label}: RENAME src gone"
        );
    }
}

// ── Scan ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_scan_returns_all_keys() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        for i in 0u8..5 {
            let key = format!("key:{i}");
            storage.set(0, key.as_bytes(), b"v").await.unwrap();
        }
        let result = storage.scan(0, 0, b"*", 100).await.unwrap();
        assert_eq!(
            result.keys.len(),
            5,
            "{label}: SCAN should return all 5 keys"
        );
    }
}

// ── DB isolation ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_compat_db_isolation() {
    let dir = TempDir::new().unwrap();
    for (label, storage) in [("memory", mem()), ("aidb", aidb(&dir))] {
        storage.set(0, b"k", b"db0").await.unwrap();
        storage.set(1, b"k", b"db1").await.unwrap();
        assert_eq!(
            storage.get(0, b"k").await.unwrap(),
            Some(b"db0".to_vec()),
            "{label}: db 0"
        );
        assert_eq!(
            storage.get(1, b"k").await.unwrap(),
            Some(b"db1".to_vec()),
            "{label}: db 1"
        );
    }
}
