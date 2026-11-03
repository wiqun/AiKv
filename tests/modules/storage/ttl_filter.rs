//! @component aikv-storage

//! TTL compaction filter 集成测试.
//!
//! 验证: 过期 key 在 compaction 后被自动清理.

use std::sync::Arc;

use aikv::storage::{
    now_ms, AiDbEngine, KvStorage, KvStorageAdapter, StoredValue, TtlExpireFilter, ValueType,
};
use tempfile::TempDir;

fn mk_storage(dir: &TempDir) -> (Arc<dyn KvStorage>, Arc<aidb::DB>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let db = engine.db.clone();
    // 注入 TTL filter
    db.set_compaction_filter(Some(Arc::new(TtlExpireFilter)));
    (KvStorageAdapter::new(engine), db)
}

/// 写入已过期的 key → flush + compaction → key 应消失.
#[tokio::test]
async fn test_ttl_filter_removes_expired_key() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = mk_storage(&dir);

    let past_ms = now_ms() - 10000; // 10 秒前过期
    storage
        .set_typed(
            0,
            b"expired_key",
            StoredValue {
                value: ValueType::String(b"should_be_gone".to_vec()),
                expires_at: Some(past_ms),
            },
        )
        .await
        .unwrap();

    // 写入未过期的 key
    storage.set(0, b"live_key", b"alive").await.unwrap();

    // 触发 flush + compaction
    db.flush().unwrap();
    // 写入额外 key 以触发 L0 compaction
    for i in 0..5u8 {
        db.put(&[b'p', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 验证: 过期 key 消失, 未过期 key 保留
    let val = storage.get(0, b"expired_key").await.unwrap();
    assert_eq!(val, None, "expired key should be removed after compaction");

    let val = storage.get(0, b"live_key").await.unwrap();
    assert_eq!(val, Some(b"alive".to_vec()), "live key should survive");

    db.close().unwrap();
}

/// 过期 key 在 reopen 后仍然不可见 (持久化清理).
#[tokio::test]
async fn test_ttl_filter_persistent_cleanup() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().to_path_buf();

    let past_ms = now_ms() - 5000;
    {
        let engine = AiDbEngine::open_for_testing(&db_path).unwrap();
        let db = engine.db.clone();
        db.set_compaction_filter(Some(Arc::new(TtlExpireFilter)));
        let storage = KvStorageAdapter::new(engine);

        storage
            .set_typed(
                0,
                b"will_expire",
                StoredValue {
                    value: ValueType::String(b"gone".to_vec()),
                    expires_at: Some(past_ms),
                },
            )
            .await
            .unwrap();

        storage.set(0, b"perm", b"keep").await.unwrap();

        db.flush().unwrap();
        for i in 0..5u8 {
            db.put(&[b'p', i], &[i]).unwrap();
            db.flush().unwrap();
        }
        db.drain_compactions().unwrap();
        db.close().unwrap();
    }

    // 重新打开 (不带 filter), 验证清理已持久化
    {
        let engine = AiDbEngine::open_for_testing(&db_path).unwrap();
        let storage = KvStorageAdapter::new(engine);

        let val = storage.get(0, b"will_expire").await.unwrap();
        assert_eq!(val, None, "expired key should be gone after reopen");

        let val = storage.get(0, b"perm").await.unwrap();
        assert_eq!(
            val,
            Some(b"keep".to_vec()),
            "non-expired key should persist"
        );
    }
}

/// 批量过期 key: 随机混合过期/未过期, 验证 compaction 后一致性.
#[tokio::test]
async fn test_ttl_filter_batch_mixed_expiry() {
    let dir = TempDir::new().unwrap();

    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let db = engine.db.clone();
    db.set_compaction_filter(Some(Arc::new(TtlExpireFilter)));
    let storage = KvStorageAdapter::new(engine);

    let past_ms = now_ms() - 1000;
    let future_ms = now_ms() + 3600000; // 1 小时后

    // 交替写入过期和未过期 key
    for i in 0..10u8 {
        let expires_at = if i % 2 == 0 {
            Some(past_ms)
        } else {
            Some(future_ms)
        };
        storage
            .set_typed(
                0,
                &[i],
                StoredValue {
                    value: ValueType::String(vec![i]),
                    expires_at,
                },
            )
            .await
            .unwrap();
    }

    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'f', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    for i in 0..10u8 {
        let val = storage.get(0, &[i]).await.unwrap();
        if i % 2 == 0 {
            assert_eq!(val, None, "expired key {} should be gone", i);
        } else {
            assert_eq!(val, Some(vec![i]), "non-expired key {} should survive", i);
        }
    }

    db.close().unwrap();
}

/// TTL filter 不影响不含 expires_at 的 key.
#[tokio::test]
async fn test_ttl_filter_ignores_no_expiry() {
    let dir = TempDir::new().unwrap();

    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let db = engine.db.clone();
    db.set_compaction_filter(Some(Arc::new(TtlExpireFilter)));
    let storage = KvStorageAdapter::new(engine);

    // 写入不含 expires_at 的普通 key
    storage.set(0, b"plain", b"data").await.unwrap();
    // 写入 subkey 格式的 key (hash field)
    storage
        .set_typed(
            0,
            b"myhash",
            StoredValue {
                value: ValueType::String(b"hash_val".to_vec()),
                expires_at: None,
            },
        )
        .await
        .unwrap();

    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'g', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    assert_eq!(
        storage.get(0, b"plain").await.unwrap(),
        Some(b"data".to_vec())
    );
    assert_eq!(
        storage.get(0, b"myhash").await.unwrap(),
        Some(b"hash_val".to_vec())
    );

    db.close().unwrap();
}

/// 当复合集合的 header 被删除成为孤儿 subkey 时, compactionFilter 兜底回收孤儿 subkey.
#[tokio::test]
async fn test_ttl_filter_reclaims_orphan_subkeys_when_header_deleted() {
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};

    let dir = TempDir::new().unwrap();
    let (_storage, db) = mk_storage(&dir);

    let user_key = b"orphan_hash";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);

    // 1. 模拟遗留的孤儿 subkeys (Header 根本不存在或已被物理删除)
    for i in 0..5 {
        let field = format!("f{i}").into_bytes();
        let sk = encode_hash_field_key(&encoded_user_key, &field);
        db.put(&sk, b"orphan_val").unwrap();
    }

    // 2. 触发 flush 与 compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'k', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 3. 验证 5 个孤儿 subkey 必须已被 compaction 彻底回收
    let prefix = hash_subkey_prefix(&encoded_user_key);
    let end = AiDbEngine::prefix_end(&prefix);
    let iter = db.scan(Some(&prefix), end.as_deref()).unwrap();
    let remaining: Vec<_> = iter.collect();
    assert_eq!(remaining.len(), 0, "孤儿 subkey 必须在 compaction 中被回收");

    db.close().unwrap();
}

/// 当复合集合的 header 过期时, compaction 自动清理 header 与全部 subkeys.
#[tokio::test]
async fn test_ttl_filter_reclaims_orphan_subkeys_when_header_expired() {
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};
    use aikv::storage::types::CollectionKind;

    let dir = TempDir::new().unwrap();
    let (storage, db) = mk_storage(&dir);

    let user_key = b"expired_hash";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);

    // 1. 写入已过期的 Header
    let past_ms = now_ms() - 10000;
    storage
        .set_typed(
            0,
            user_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 5,
                },
                expires_at: Some(past_ms),
            },
        )
        .await
        .unwrap();

    // 写入对应的 subkeys
    for i in 0..5 {
        let field = format!("f{i}").into_bytes();
        let sk = encode_hash_field_key(&encoded_user_key, &field);
        db.put(&sk, b"hash_val").unwrap();
    }

    // 2. 触发 flush 与 compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'k', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 3. 验证 Header 与 subkey 均被清理
    let header_val = db.get(&encoded_user_key).unwrap();
    assert_eq!(header_val, None, "已过期 Header 必须被清理");

    let prefix = hash_subkey_prefix(&encoded_user_key);
    let end = AiDbEngine::prefix_end(&prefix);
    let iter = db.scan(Some(&prefix), end.as_deref()).unwrap();
    let remaining: Vec<_> = iter.collect();
    assert_eq!(remaining.len(), 0, "过期 Hash 的 subkeys 必须全部被清理");

    db.close().unwrap();
}

/// 当同名 key 被重建为其他类型 (如 String) 时, 旧集合的孤儿 subkey 必须被回收, 新 String 完好保留.
#[tokio::test]
async fn test_ttl_filter_reclaims_orphan_subkeys_when_header_type_mismatched() {
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};

    let dir = TempDir::new().unwrap();
    let (storage, db) = mk_storage(&dir);

    let user_key = b"rebuilt_key";
    let encoded_user_key = AiDbEngine::encode_key(0, user_key);

    // 1. 写入旧 Hash 的 5 个孤儿 subkey
    for i in 0..5 {
        let field = format!("f{i}").into_bytes();
        let sk = encode_hash_field_key(&encoded_user_key, &field);
        db.put(&sk, b"old_hash_val").unwrap();
    }

    // 2. 同名 key 重建为 String 类型
    storage.set(0, user_key, b"new_string_value").await.unwrap();

    // 3. 触发 flush 与 compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'k', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 4. 验证 String 完好保留
    let val = storage.get(0, user_key).await.unwrap();
    assert_eq!(val, Some(b"new_string_value".to_vec()));

    // 5. 验证旧 Hash 的 5 个 subkey 被清理
    let prefix = hash_subkey_prefix(&encoded_user_key);
    let end = AiDbEngine::prefix_end(&prefix);
    let iter = db.scan(Some(&prefix), end.as_deref()).unwrap();
    let remaining: Vec<_> = iter.collect();
    assert_eq!(remaining.len(), 0, "类型不匹配的孤儿 subkey 必须被清理");

    db.close().unwrap();
}

/// 反向红线测试: 合法且未过期的集合 subkey 绝对不被误删; 用户 key 包含 \x01 也绝不被误删.
#[tokio::test]
async fn test_ttl_filter_preserves_valid_subkeys_and_keys_with_x01() {
    use aikv::storage::subkey::{encode_hash_field_key, hash_subkey_prefix};
    use aikv::storage::types::CollectionKind;

    let dir = TempDir::new().unwrap();
    let (storage, db) = mk_storage(&dir);

    // 1. 写入合法的未过期 Hash
    let hash_key = b"valid_hash";
    let encoded_hash_key = AiDbEngine::encode_key(0, hash_key);
    storage
        .set_typed(
            0,
            hash_key,
            StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: 3,
                },
                expires_at: None,
            },
        )
        .await
        .unwrap();
    for i in 0..3 {
        let field = format!("f{i}").into_bytes();
        let sk = encode_hash_field_key(&encoded_hash_key, &field);
        db.put(&sk, b"valid_val").unwrap();
    }

    // 2. 写入包含 \x01 的合法独立普通 key
    let special_key = b"user\x01profile";
    storage
        .set(0, special_key, b"my_profile_data")
        .await
        .unwrap();

    // 3. 触发 flush 与 compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'k', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 4. 验证 Hash subkeys 完好无损 (反向红线)
    let prefix = hash_subkey_prefix(&encoded_hash_key);
    let end = AiDbEngine::prefix_end(&prefix);
    let iter = db.scan(Some(&prefix), end.as_deref()).unwrap();
    let remaining: Vec<_> = iter.collect();
    assert_eq!(remaining.len(), 3, "合法未过期的 subkeys 绝对不能被误删");

    // 5. 验证包含 \x01 的独立普通 key 完好无损
    let val = storage.get(0, special_key).await.unwrap();
    assert_eq!(val, Some(b"my_profile_data".to_vec()));

    db.close().unwrap();
}

/// 重现缺陷: 用户普通 key 若形如 `foo\x01Hbar` 且不存在 `foo` 时, 会被误判为孤儿 subkey 惨遭误删!
#[tokio::test]
async fn test_ttl_filter_bug_reproduce_foo_x01_hbar() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = mk_storage(&dir);

    // 写入一个合法的独立用户 key, 名字恰好包含 \x01H
    let user_key = b"foo\x01Hbar";
    storage.set(0, user_key, b"precious_data").await.unwrap();

    // 触发 flush 与 compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'k', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 断言该用户 key 是否幸存
    let val = storage.get(0, user_key).await.unwrap();
    assert_eq!(
        val,
        Some(b"precious_data".to_vec()),
        "用户独立 key `foo\\x01Hbar` 绝不能被当作孤儿 subkey 误删!"
    );

    db.close().unwrap();
}

/// 验证集群模式下 (以 `\\x01sm/{gid}/` 为 key 前缀) CompactionFilter 的孤儿 subkey 兜底回收与未过期保留.
#[tokio::test]
async fn test_ttl_filter_cluster_mode_orphan_reclaim_and_preservation() {
    use aikv::storage::subkey::encode_hash_field_key;
    use aikv::storage::types::{CollectionKind, StoredValue, ValueType};

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let db = engine.db.clone();
    db.set_compaction_filter(Some(Arc::new(TtlExpireFilter)));

    let gid = 1u64;
    let gid_bytes = gid.to_be_bytes();
    let sm_prefix = [b"\x01sm/", gid_bytes.as_slice(), b"/"].concat();

    // 1. 模拟一个 Header 已经消失的集群孤儿 Hash subkey
    let orphan_user_key = b"cluster_orphan_hash";
    let encoded_orphan_user = AiDbEngine::encode_key(0, orphan_user_key);
    let orphan_sk = encode_hash_field_key(&encoded_orphan_user, b"f01");
    let orphan_db_key = [sm_prefix.as_slice(), orphan_sk.as_slice()].concat();
    db.put(&orphan_db_key, b"orphan_payload").unwrap();

    // 2. 模拟一个合法的未过期集群 Hash subkey
    let valid_user_key = b"cluster_valid_hash";
    let encoded_valid_user = AiDbEngine::encode_key(0, valid_user_key);
    let valid_header_db_key = [sm_prefix.as_slice(), encoded_valid_user.as_slice()].concat();
    let header_val = StoredValue {
        value: ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count: 1,
        },
        expires_at: None,
    };
    db.put(
        &valid_header_db_key,
        &postcard::to_allocvec(&header_val).unwrap(),
    )
    .unwrap();

    let valid_sk = encode_hash_field_key(&encoded_valid_user, b"f01");
    let valid_db_key = [sm_prefix.as_slice(), valid_sk.as_slice()].concat();
    db.put(&valid_db_key, b"valid_payload").unwrap();

    // 3. 触发 flush 与 compaction
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'k', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    // 4. 验证集群孤儿 subkey 必须被清理
    let orphan_res = db.get(&orphan_db_key).unwrap();
    assert_eq!(
        orphan_res, None,
        "集群模式下的孤儿 subkey 必须被 compaction 兜底回收"
    );

    // 5. 验证集群合法 subkey 完好保留
    let valid_res = db.get(&valid_db_key).unwrap();
    assert_eq!(
        valid_res,
        Some(b"valid_payload".to_vec()),
        "集群模式下的合法 subkey 必须保留"
    );

    db.close().unwrap();
}

#[test]
fn test_split_cluster_sm_prefix() {
    use aikv::storage::ttl_filter::split_cluster_sm_prefix;

    // 非集群 key
    let raw = b"my_normal_key";
    let (prefix, user_key) = split_cluster_sm_prefix(raw);
    assert_eq!(prefix, b"");
    assert_eq!(user_key, b"my_normal_key");

    // 格式不完整的 key
    let malformed = b"\x01sm/123";
    let (prefix, user_key) = split_cluster_sm_prefix(malformed);
    assert_eq!(prefix, b"");
    assert_eq!(user_key, malformed);

    // 标准集群 key: \x01sm/ (4B) + gid:8B + / (1B) + user_key
    let gid = 42u64.to_be_bytes();
    let cluster_key = [b"\x01sm/", gid.as_slice(), b"/my_cluster_user_key"].concat();
    let (prefix, user_key) = split_cluster_sm_prefix(&cluster_key);
    assert_eq!(prefix.len(), 13);
    assert_eq!(prefix, [b"\x01sm/", gid.as_slice(), b"/"].concat());
    assert_eq!(user_key, b"my_cluster_user_key");
}
