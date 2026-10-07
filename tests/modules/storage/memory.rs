//! MemoryEngine 内存存储引擎测试
//! @component aikv-storage

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aikv::storage::{KvStorage, MemoryEngine, ValueType};

fn engine() -> Arc<MemoryEngine> {
    MemoryEngine::new(16)
}

/// 验证 MemoryEngine Get/Set 基本读写
#[tokio::test]
async fn test_memory_engine_get_set() {
    let e = engine();
    assert!(e.get(0, b"k").await.unwrap().is_none());
    e.set(0, b"k", b"v").await.unwrap();
    assert_eq!(e.get(0, b"k").await.unwrap(), Some(b"v".to_vec()));
}

#[tokio::test]
async fn test_memory_engine_delete() {
    let e = engine();
    assert!(!e.delete(0, b"missing").await.unwrap());
    e.set(0, b"k", b"v").await.unwrap();
    assert!(e.delete(0, b"k").await.unwrap());
    assert!(e.get(0, b"k").await.unwrap().is_none());
}

#[tokio::test]
async fn test_memory_engine_expire_ttl() {
    let e = engine();
    e.set(0, b"k", b"v").await.unwrap();
    assert!(e.expire(0, b"k", 60_000).await.unwrap());
    let ttl = e.ttl(0, b"k").await.unwrap();
    assert!(ttl.unwrap() > 0);
    assert!(e.persist(0, b"k").await.unwrap());
    assert_eq!(e.ttl(0, b"k").await.unwrap(), Some(-1));
}

#[tokio::test]
async fn test_memory_engine_expire_negative() {
    let e = engine();
    e.set(0, b"k", b"v").await.unwrap();
    assert!(e.expire(0, b"k", 0).await.unwrap());
    assert!(e.get(0, b"k").await.unwrap().is_none());
}

#[tokio::test]
async fn test_memory_engine_lazy_expiry() {
    let e = engine();
    let past = SystemTime::now()
        .checked_sub(Duration::from_millis(1))
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    e.set_with_ttl(0, b"k", b"v", past).await.unwrap();
    assert!(e.get(0, b"k").await.unwrap().is_none());
    assert_eq!(e.len(0).await.unwrap(), 0);
}

#[tokio::test]
async fn test_memory_engine_scan() {
    let e = engine();
    for i in 0..5 {
        e.set(0, format!("k{i}").as_bytes(), b"v").await.unwrap();
    }
    let r1 = e.scan(0, 0, b"", 2).await.unwrap();
    assert_eq!(r1.keys.len(), 2);
    assert_ne!(r1.cursor, 0);
    let r2 = e.scan(0, r1.cursor, b"", 10).await.unwrap();
    assert_eq!(r2.cursor, 0);
    assert!(r1.keys.len() + r2.keys.len() >= 5);
}

#[tokio::test]
async fn test_memory_engine_scan_empty() {
    let e = engine();
    let r = e.scan(0, 0, b"", 10).await.unwrap();
    assert!(r.keys.is_empty());
    assert_eq!(r.cursor, 0);
}

#[tokio::test]
async fn test_memory_engine_scan_pattern() {
    let e = engine();
    e.set(0, b"foo", b"1").await.unwrap();
    e.set(0, b"bar", b"2").await.unwrap();
    let r = e.scan(0, 0, b"f*", 10).await.unwrap();
    assert_eq!(r.keys, vec![b"foo".to_vec()]);
}

#[tokio::test]
async fn test_memory_engine_scan_expired() {
    let e = engine();
    let past = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    e.set_with_ttl(0, b"gone", b"v", past).await.unwrap();
    e.set(0, b"stay", b"v").await.unwrap();
    let r = e.scan(0, 0, b"", 10).await.unwrap();
    assert_eq!(r.keys, vec![b"stay".to_vec()]);
}

#[tokio::test]
async fn test_memory_engine_mget_wrongtype_nil() {
    use std::collections::HashMap;

    let e = engine();
    e.set(0, b"s", b"hello").await.unwrap();
    e.set_typed(
        0,
        b"h",
        aikv::storage::StoredValue {
            value: ValueType::Hash(HashMap::new()),
            expires_at: None,
        },
    )
    .await
    .unwrap();

    let vals = e
        .mget(0, &[b"s".to_vec(), b"h".to_vec(), b"missing".to_vec()])
        .await
        .unwrap();
    assert_eq!(vals[0], Some(b"hello".to_vec()));
    assert_eq!(vals[1], None);
    assert_eq!(vals[2], None);
}

#[tokio::test]
async fn test_memory_engine_get_wrongtype() {
    use std::collections::HashMap;

    let e = engine();
    e.set_typed(
        0,
        b"h",
        aikv::storage::StoredValue {
            value: ValueType::Hash(HashMap::new()),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let err = e.get(0, b"h").await.unwrap_err();
    assert!(err.to_string().contains("WRONGTYPE"));
}

#[tokio::test]
async fn test_memory_engine_rename_copy_random() {
    let e = engine();
    e.set(0, b"src", b"v").await.unwrap();
    e.rename_key(0, b"src", b"dst").await.unwrap();
    assert!(e.get(0, b"src").await.unwrap().is_none());
    assert_eq!(e.get(0, b"dst").await.unwrap(), Some(b"v".to_vec()));
    e.set(0, b"taken", b"x").await.unwrap();
    assert!(!e.rename_key_nx(0, b"dst", b"taken").await.unwrap());
    assert!(e.get(0, b"dst").await.unwrap().is_some());
    assert!(e.copy_key(0, 0, b"dst", b"copy", false).await.unwrap());
    assert_eq!(e.get(0, b"copy").await.unwrap(), Some(b"v".to_vec()));
    let rk = e.random_key(0).await.unwrap();
    assert!(rk.is_some());
}

#[tokio::test]
async fn test_memory_engine_concurrent() {
    use tokio::task;

    let e = engine();
    let mut handles = vec![];
    for i in 0..32 {
        let e = Arc::clone(&e);
        handles.push(task::spawn(async move {
            let key = format!("k{i}");
            e.set(0, key.as_bytes(), b"v").await.unwrap();
            assert_eq!(e.get(0, key.as_bytes()).await.unwrap(), Some(b"v".to_vec()));
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn test_memory_engine_keyspace_stats() {
    let e = engine();
    e.set(0, b"k1", b"v1").await.unwrap();
    e.set(0, b"k2", b"v2").await.unwrap();
    e.expire(0, b"k1", 60_000).await.unwrap();

    let stats = e.keyspace_stats(0).await.unwrap();
    assert_eq!(stats.keys, 2);
    assert_eq!(stats.expires, 1);
}

#[tokio::test]
async fn keys_purge_decrements_expires() {
    let e = engine();
    e.set_with_ttl(0, b"k", b"v", 1).await.unwrap();
    assert_eq!(e.expires(0).await.unwrap(), 1);
    assert!(e.keys(0, b"*").await.unwrap().is_empty());
    assert_eq!(e.expires(0).await.unwrap(), 0);
}
