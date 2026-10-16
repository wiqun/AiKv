use std::sync::Arc;

use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter, StoredValue, ValueType};
use tempfile::TempDir;

fn adapter(dir: &TempDir) -> Arc<dyn KvStorage> {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    KvStorageAdapter::new(engine)
}

#[tokio::test]
async fn test_aidb_engine_roundtrip() {
    let dir = TempDir::new().unwrap();
    let storage = adapter(&dir);

    storage.set(0, b"foo", b"bar").await.unwrap();
    assert_eq!(storage.get(0, b"foo").await.unwrap(), Some(b"bar".to_vec()));

    storage.set(1, b"k", b"v").await.unwrap();
    assert_eq!(storage.get(1, b"k").await.unwrap(), Some(b"v".to_vec()));
    assert!(storage.get(0, b"foo").await.unwrap().is_some());
}

#[tokio::test]
async fn test_aidb_mget_wrongtype_nil() {
    use std::collections::HashMap;

    let dir = TempDir::new().unwrap();
    let storage = adapter(&dir);
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
    assert_eq!(vals[0], Some(b"hello".to_vec()));
    assert_eq!(vals[1], None);
    assert_eq!(vals[2], None);
}

#[tokio::test]
async fn test_aidb_restart_survives() {
    let dir = TempDir::new().unwrap();
    {
        let storage = adapter(&dir);
        storage.set(0, b"persist", b"yes").await.unwrap();
        storage
            .set_typed(
                0,
                b"hash",
                StoredValue {
                    value: ValueType::Hash(std::collections::HashMap::from([(
                        b"f".to_vec(),
                        b"v".to_vec(),
                    )])),
                    expires_at: None,
                },
            )
            .await
            .unwrap();
    }

    let storage = adapter(&dir);
    assert_eq!(
        storage.get(0, b"persist").await.unwrap(),
        Some(b"yes".to_vec())
    );
    let typed = storage.get_typed(0, b"hash").await.unwrap().expect("hash");
    match typed.value {
        ValueType::Hash(h) => assert_eq!(h.get(b"f" as &[u8]), Some(&b"v".to_vec())),
        _ => panic!("expected hash"),
    }
}

#[tokio::test]
async fn test_aidb_adapter_list_and_flushdb() {
    use aikv::command::CommandRouter;
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    {
        let storage = adapter(&dir);
        let router = CommandRouter::new(storage.clone());
        let mut db = 0;

        router
            .execute(
                "LPUSH",
                &[Bytes::from("listk"), Bytes::from("a"), Bytes::from("b")],
                &mut db,
            )
            .await
            .unwrap();
        let resp = router
            .execute(
                "LRANGE",
                &[Bytes::from("listk"), Bytes::from("0"), Bytes::from("-1")],
                &mut db,
            )
            .await
            .unwrap();
        let aikv::protocol::RespValue::Array(Some(items)) = resp else {
            panic!("expected array");
        };
        assert_eq!(items.len(), 2);

        storage.set(0, b"other", b"x").await.unwrap();
        storage.clear(0).await.unwrap();
        assert!(storage.get(0, b"other").await.unwrap().is_none());
    }

    let storage2 = adapter(&dir);
    assert!(storage2.get(0, b"other").await.unwrap().is_none());
}

#[tokio::test]
async fn test_aidb_dump_restore_roundtrip() {
    use aikv::command::CommandRouter;
    use aikv::protocol::RespValue;
    use bytes::Bytes;

    fn b(s: &str) -> Bytes {
        Bytes::from(s.to_string())
    }

    let dir = TempDir::new().unwrap();
    {
        let storage = adapter(&dir);
        let router = CommandRouter::new(storage.clone());
        let mut db = 0;

        router
            .execute("SET", &[b("k"), b("payload")], &mut db)
            .await
            .unwrap();
        let RespValue::BulkString(Some(dump)) =
            router.execute("DUMP", &[b("k")], &mut db).await.unwrap()
        else {
            panic!("expected bulk dump");
        };
        assert!(!dump.is_empty());

        router.execute("DEL", &[b("k")], &mut db).await.unwrap();
        assert!(storage.get(0, b"k").await.unwrap().is_none());

        router
            .execute("RESTORE", &[b("k"), b("0"), dump], &mut db)
            .await
            .unwrap();
        let resp = router.execute("GET", &[b("k")], &mut db).await.unwrap();
        assert_eq!(resp, RespValue::BulkString(Some(b("payload"))));
    }

    let storage = adapter(&dir);
    assert_eq!(
        storage.get(0, b"k").await.unwrap(),
        Some(b"payload".to_vec())
    );
}

#[tokio::test]
async fn test_aidb_json_roundtrip() {
    use aikv::command::CommandRouter;
    use aikv::protocol::RespValue;
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    let doc = r#"{"persist":true,"n":42}"#;
    {
        let storage = adapter(&dir);
        let router = CommandRouter::new(storage);
        let mut db = 0;
        router
            .execute(
                "JSON.SET",
                &[Bytes::from("jsonk"), Bytes::from("$"), Bytes::from(doc)],
                &mut db,
            )
            .await
            .unwrap();
    }

    let storage = adapter(&dir);
    let router = CommandRouter::new(storage);
    let mut db = 0;
    let resp = router
        .execute(
            "JSON.GET",
            &[Bytes::from("jsonk"), Bytes::from("$")],
            &mut db,
        )
        .await
        .unwrap();
    let RespValue::BulkString(Some(data)) = resp else {
        panic!("expected bulk");
    };
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("persist"));
    assert!(text.contains("42"));
}

#[tokio::test]
async fn test_aidb_script_roundtrip() {
    use aikv::command::CommandRouter;
    use aikv::protocol::RespValue;
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    let script = "redis.call('SET', KEYS[1], ARGV[1]); return redis.call('GET', KEYS[1])";
    {
        let storage = adapter(&dir);
        let router = CommandRouter::new(storage);
        let mut db = 0;
        let resp = router
            .execute(
                "EVAL",
                &[
                    Bytes::from(script),
                    Bytes::from("1"),
                    Bytes::from("scriptk"),
                    Bytes::from("persisted"),
                ],
                &mut db,
            )
            .await
            .unwrap();
        assert_eq!(resp, RespValue::BulkString(Some(Bytes::from("persisted"))));
    }

    let storage = adapter(&dir);
    let router = CommandRouter::new(storage);
    let mut db = 0;
    let resp = router
        .execute("GET", &[Bytes::from("scriptk")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(Bytes::from("persisted"))));
}
