//! @component aikv-storage

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

#[tokio::test]
async fn test_aidb_write_batch_delete_range_atomic() {
    use aikv::storage::adapter::{AdapterWriteOp, StorageAdapter};

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");

    for i in 0..5 {
        engine
            .set(format!("k{i:02}").into_bytes(), b"val".to_vec())
            .await
            .unwrap();
    }

    // 提交原子批处理: 包含 Put, Delete 和 DeleteRange
    let ops = vec![
        AdapterWriteOp::Put {
            key: b"k99".to_vec(),
            value: b"new".to_vec(),
        },
        AdapterWriteOp::DeleteRange {
            start: b"k01".to_vec(),
            end: b"k04".to_vec(),
        },
    ];
    engine.write_batch(ops).await.unwrap();

    assert_eq!(
        engine.get(b"k00".to_vec()).await.unwrap(),
        Some(b"val".to_vec())
    );
    assert_eq!(engine.get(b"k01".to_vec()).await.unwrap(), None);
    assert_eq!(engine.get(b"k02".to_vec()).await.unwrap(), None);
    assert_eq!(engine.get(b"k03".to_vec()).await.unwrap(), None);
    assert_eq!(
        engine.get(b"k04".to_vec()).await.unwrap(),
        Some(b"val".to_vec())
    );
    assert_eq!(
        engine.get(b"k99".to_vec()).await.unwrap(),
        Some(b"new".to_vec())
    );
}

#[tokio::test]
async fn test_aidb_cascade_delete_hash_subkeys() {
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};
    use aikv::storage::types::{CollectionKind, StoredValue, ValueType};

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());

    let user_key = b"my_hash";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);

    // 1. 写入 Header
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 10,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();

    // 2. 写入 10 个 subkeys
    for i in 0..10 {
        let field = format!("f{i:02}").into_bytes();
        let subkey = encode_hash_field_key(&encoded_user_key, &field);
        engine
            .set(subkey, format!("v{i:02}").into_bytes())
            .await
            .unwrap();
    }

    // 确认底层确实能查到 subkey
    let first_subkey = encode_hash_field_key(&encoded_user_key, b"f00");
    assert!(engine.get(first_subkey.clone()).await.unwrap().is_some());

    // 3. 执行 delete
    let deleted = storage.delete(0, user_key).await.unwrap();
    assert!(deleted);

    // 4. 验证 Header 已经被删除
    assert!(storage.get_typed(0, user_key).await.unwrap().is_none());

    // 5. 关键验证点: 底层所有的 subkeys 必须已被级联清除!
    let prefix = hash_subkey_prefix(&encoded_user_key);
    let subkeys = engine.scan_prefix(&prefix).await.unwrap();
    assert_eq!(
        subkeys.len(),
        0,
        "Hash subkeys 必须被级联删除, 但仍然残留了 {} 条",
        subkeys.len()
    );
}

#[tokio::test]
async fn test_aidb_cascade_delete_set_subkeys() {
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::{encode_set_member_key, set_subkey_prefix};
    use aikv::storage::types::{CollectionKind, StoredValue, ValueType};

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());

    let user_key = b"my_set";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);

    // 1. 写入 Set Header
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Set,
                    count: 10,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();

    // 2. 写入 10 个 subkeys
    for i in 0..10 {
        let member = format!("m{i:02}").into_bytes();
        let subkey = encode_set_member_key(&encoded_user_key, &member);
        engine.set(subkey, Vec::new()).await.unwrap();
    }

    // 3. 执行 delete
    let deleted = storage.delete(0, user_key).await.unwrap();
    assert!(deleted);

    // 4. 验证 Header 已删除
    assert!(storage.get_typed(0, user_key).await.unwrap().is_none());

    // 5. 验证底层 Set subkeys 全部被级联清除!
    let prefix = set_subkey_prefix(&encoded_user_key);
    let subkeys = engine.scan_prefix(&prefix).await.unwrap();
    assert_eq!(
        subkeys.len(),
        0,
        "Set subkeys 必须被级联删除, 但仍然残留了 {} 条",
        subkeys.len()
    );
}

#[tokio::test]
async fn test_aidb_lazy_expire_cascade_delete_subkeys() {
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};
    use aikv::storage::types::{CollectionKind, StoredValue, ValueType};

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());

    let user_key = b"expired_hash";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);

    // 写入一个已过期的 Header (expires_at 为过去的时间戳 1000)
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 10,
                },
                expires_at: Some(1000),
            },
        )
        .await
        .unwrap();

    for i in 0..10 {
        let field = format!("f{i:02}").into_bytes();
        let subkey = encode_hash_field_key(&encoded_user_key, &field);
        engine
            .set(subkey, format!("v{i:02}").into_bytes())
            .await
            .unwrap();
    }

    // 读触发惰性过期
    let val = storage.get_typed(0, user_key).await.unwrap();
    assert!(val.is_none());

    // 验证底层 Subkey 是否已被惰性级联清理
    let prefix = hash_subkey_prefix(&encoded_user_key);
    let subkeys = engine.scan_prefix(&prefix).await.unwrap();
    assert_eq!(
        subkeys.len(),
        0,
        "惰性过期必须级联删除 subkeys, 但仍然残留了 {} 条",
        subkeys.len()
    );
}

#[tokio::test]
async fn test_aidb_cascade_delete_does_not_harm_other_keys_with_x01() {
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};
    use aikv::storage::types::{CollectionKind, StoredValue, ValueType};

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());

    // 1. 写入一个用户自己命名的包含 \x01 的独立 key
    let special_key = b"tenant\x01profile";
    storage
        .set(0, special_key, b"important_data")
        .await
        .unwrap();

    // 2. 写入一个名字叫 "tenant" 的复合 Hash (其 subkey 前缀为 "tenant\x01H")
    let hash_key = b"tenant";
    let encoded_hash_key = AiDbEngine::encode_key(0, hash_key);
    storage
        .set_typed(
            0,
            hash_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 5,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();

    for i in 0..5 {
        let field = format!("f{i}").into_bytes();
        let subkey = encode_hash_field_key(&encoded_hash_key, &field);
        engine.set(subkey, b"hash_val".to_vec()).await.unwrap();
    }

    // 3. 执行删除复合 Hash
    let deleted = storage.delete(0, hash_key).await.unwrap();
    assert!(deleted);

    // 4. 验证 Hash 及其 subkeys 全部被清理
    assert!(storage.get_typed(0, hash_key).await.unwrap().is_none());
    let prefix = hash_subkey_prefix(&encoded_hash_key);
    assert_eq!(engine.scan_prefix(&prefix).await.unwrap().len(), 0);

    // 5. 反向红线断言: 包含 \x01 的独立用户 key 绝对未受任何波及!
    let special_val = storage.get(0, special_key).await.unwrap();
    assert_eq!(
        special_val,
        Some(b"important_data".to_vec()),
        "包含 \\x01 的合法独立 key 不得被误删!"
    );
}

#[tokio::test]
async fn test_aidb_dump_restore_large_hash_subkeys() {
    use aikv::command::key::KeyCommands;
    use aikv::command::KeyLock;
    use aikv::protocol::RespValue;
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};
    use aikv::storage::types::{CollectionKind, KvStorage, StoredValue, ValueType};
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());
    let key_lock = Arc::new(KeyLock::new(256));
    let key_cmd = KeyCommands::new(storage.clone(), key_lock);

    // 1. 构造一个包含 80 个 field 的大 Hash (超过 64 阈值, 存储为 Subkey)
    let user_key = b"large_hash";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 80,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();

    for i in 0..80 {
        let field = format!("field_{i:03}").into_bytes();
        let val = format!("val_{i:03}").into_bytes();
        let subkey = encode_hash_field_key(&encoded_user_key, &field);
        engine.set(subkey, val).await.unwrap();
    }

    // 2. 执行 DUMP
    let dump_resp = key_cmd
        .dump(0, &[Bytes::from_static(user_key)])
        .await
        .expect("dump should succeed");
    let payload = match dump_resp {
        RespValue::BulkString(Some(b)) => b,
        other => panic!("expected bulk string, got {:?}", other),
    };

    // 3. 执行 RESTORE 到新 key
    let new_key = b"large_hash_restored";
    key_cmd
        .restore(
            0,
            &[
                Bytes::from_static(new_key),
                Bytes::from_static(b"0"),
                payload,
            ],
        )
        .await
        .expect("restore should succeed");

    // 4. 验证新 key 在底层是否自动转为 Subkey 存储 (CollectionHeader { kind: Hash, count: 80 })
    let restored_meta = storage.get_typed(0, new_key).await.unwrap().unwrap();
    match restored_meta.value {
        ValueType::CollectionHeader { kind, count } => {
            assert_eq!(kind, CollectionKind::Hash);
            assert_eq!(count, 80);
        }
        ValueType::Hash(_) => {
            panic!("RESTORE 大 Hash 不得以内联形式存储, 必须自动拆分为 Subkey 格式");
        }
        other => panic!("unexpected restored value type: {:?}", other),
    }

    // 5. 验证新 key 底层的 80 个 subkey 全部完整存在且正确
    let encoded_new_key = AiDbEngine::encode_key(0, new_key);
    let prefix = hash_subkey_prefix(&encoded_new_key);
    let subkeys = engine.scan_prefix(&prefix).await.unwrap();
    assert_eq!(subkeys.len(), 80, "底层应存在 80 个 hash subkeys");
}

#[tokio::test]
async fn test_aidb_dump_restore_large_set_subkeys() {
    use aikv::command::key::KeyCommands;
    use aikv::command::KeyLock;
    use aikv::protocol::RespValue;
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::{encode_set_member_key, set_subkey_prefix};
    use aikv::storage::types::{CollectionKind, KvStorage, StoredValue, ValueType};
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());
    let key_lock = Arc::new(KeyLock::new(256));
    let key_cmd = KeyCommands::new(storage.clone(), key_lock);

    let user_key = b"large_set";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Set,
                    count: 80,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();

    for i in 0..80 {
        let member = format!("mem_{i:03}").into_bytes();
        let subkey = encode_set_member_key(&encoded_user_key, &member);
        engine.set(subkey, vec![]).await.unwrap();
    }

    let dump_resp = key_cmd
        .dump(0, &[Bytes::from_static(user_key)])
        .await
        .expect("dump should succeed");
    let payload = match dump_resp {
        RespValue::BulkString(Some(b)) => b,
        other => panic!("expected bulk string, got {:?}", other),
    };

    let new_key = b"large_set_restored";
    key_cmd
        .restore(
            0,
            &[
                Bytes::from_static(new_key),
                Bytes::from_static(b"0"),
                payload,
            ],
        )
        .await
        .expect("restore should succeed");

    let restored_meta = storage.get_typed(0, new_key).await.unwrap().unwrap();
    match restored_meta.value {
        ValueType::CollectionHeader { kind, count } => {
            assert_eq!(kind, CollectionKind::Set);
            assert_eq!(count, 80);
        }
        ValueType::Set(_) => {
            panic!("RESTORE 大 Set 不得以内联形式存储, 必须自动拆分为 Subkey 格式");
        }
        other => panic!("unexpected restored value type: {:?}", other),
    }

    let encoded_new_key = AiDbEngine::encode_key(0, new_key);
    let prefix = set_subkey_prefix(&encoded_new_key);
    let subkeys = engine.scan_prefix(&prefix).await.unwrap();
    assert_eq!(subkeys.len(), 80, "底层应存在 80 个 set subkeys");
}

#[tokio::test]
async fn test_aidb_dump_restore_large_zset_subkeys() {
    use aikv::command::key::KeyCommands;
    use aikv::command::KeyLock;
    use aikv::protocol::RespValue;
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::types::{CollectionKind, KvStorage, ValueType};
    use aikv::storage::zset_subkey;
    use bytes::Bytes;
    use std::collections::BTreeMap;

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());
    let key_lock = Arc::new(KeyLock::new(256));
    let key_cmd = KeyCommands::new(storage.clone(), key_lock);

    let user_key = b"large_zset";
    let mut map = BTreeMap::new();
    for i in 0..80 {
        map.insert(format!("m_{i:03}").into_bytes(), i as f64);
    }
    let ops = zset_subkey::replace_ops(0, user_key, &map, None);
    storage.write_batch(0, ops).await.unwrap();

    let dump_resp = key_cmd
        .dump(0, &[Bytes::from_static(user_key)])
        .await
        .expect("dump should succeed");
    let payload = match dump_resp {
        RespValue::BulkString(Some(b)) => b,
        other => panic!("expected bulk string, got {:?}", other),
    };

    let new_key = b"large_zset_restored";
    key_cmd
        .restore(
            0,
            &[
                Bytes::from_static(new_key),
                Bytes::from_static(b"0"),
                payload,
            ],
        )
        .await
        .expect("restore should succeed");

    let restored_meta = storage.get_typed(0, new_key).await.unwrap().unwrap();
    match restored_meta.value {
        ValueType::CollectionHeader { kind, count } => {
            assert_eq!(kind, CollectionKind::ZSet);
            assert_eq!(count, 80);
        }
        ValueType::ZSet(_) => {
            panic!("RESTORE 大 ZSet 不得以内联形式存储, 必须自动拆分为 Subkey 格式");
        }
        other => panic!("unexpected restored value type: {:?}", other),
    }

    let encoded_new_key = AiDbEngine::encode_key(0, new_key);
    let member_prefix = zset_subkey::member_prefix(&encoded_new_key);
    let members = engine.scan_prefix(&member_prefix).await.unwrap();
    assert_eq!(members.len(), 80, "底层应存在 80 个 zset member subkeys");

    let score_prefix = zset_subkey::score_prefix(&encoded_new_key);
    let scores = engine.scan_prefix(&score_prefix).await.unwrap();
    assert_eq!(
        scores.len(),
        80,
        "底层应存在 80 个 zset score index subkeys"
    );
}

#[tokio::test]
async fn test_aidb_dump_exceed_32mb_aborts_without_deleting_source() {
    use aikv::command::key::KeyCommands;
    use aikv::command::KeyLock;
    use aikv::storage::adapter::StorageAdapter;
    use aikv::storage::subkey::encode_hash_field_key;
    use aikv::storage::types::{CollectionKind, KvStorage, StoredValue, ValueType};
    use bytes::Bytes;

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage = KvStorageAdapter::new(engine.clone());
    let key_lock = Arc::new(KeyLock::new(256));
    let key_cmd = KeyCommands::new(storage.clone(), key_lock);

    // 写入一个由多个大 subkey 组成、总大小超过 32MB 的 Hash
    let user_key = b"huge_hash";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 33,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();

    let chunk = vec![b'x'; 1024 * 1024 + 10]; // 1MB+
    for i in 0..33 {
        let field = format!("f_{i:03}").into_bytes();
        let sk = encode_hash_field_key(&encoded_user_key, &field);
        engine.set(sk, chunk.clone()).await.unwrap();
    }

    // 尝试 DUMP, 预期因为超过 32MB 而报错中止
    let res = key_cmd.dump(0, &[Bytes::from_static(user_key)]).await;
    assert!(res.is_err(), "超过 32MB 的 DUMP 必须报错返回 Err");
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("32MB") || err_msg.contains("exceeds maximum allowed size"),
        "错误信息需明确指明超过 32MB: {err_msg}"
    );

    // 验证源端数据完好无损
    let src_meta = storage.get_typed(0, user_key).await.unwrap();
    assert!(src_meta.is_some(), "DUMP 失败源端数据不得丢失!");

    // 同样验证 MIGRATE 在超限时立即报错且绝不删除源端
    let mig_res = key_cmd
        .migrate(
            0,
            &[
                Bytes::from_static(b"127.0.0.1"),
                Bytes::from_static(b"9999"),
                Bytes::from_static(user_key),
                Bytes::from_static(b"0"),
                Bytes::from_static(b"5000"),
            ],
        )
        .await;
    assert!(mig_res.is_err(), "超过 32MB 的 MIGRATE 必须报错返回 Err");
    assert!(
        storage.get_typed(0, user_key).await.unwrap().is_some(),
        "MIGRATE 失败源端数据不得丢失!"
    );
}
