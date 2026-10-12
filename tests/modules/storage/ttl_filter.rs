//! TTL compaction filter 集成测试.
//!
//! 验证: 过期 key 在 compaction 后被自动清理.

use std::sync::Arc;

use aikv::storage::compaction_filter::AikvCompactionFilter;
use aikv::storage::{now_ms, AiDbEngine, KvStorage, KvStorageAdapter, StoredValue, ValueType};
use tempfile::TempDir;

fn mk_storage(dir: &TempDir) -> (Arc<dyn KvStorage>, Arc<aidb::DB>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let db = engine.db.clone();
    // 注入 TTL filter
    db.set_compaction_filter(Some(Arc::new(AikvCompactionFilter::single_node())));
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
        db.set_compaction_filter(Some(Arc::new(AikvCompactionFilter::single_node())));
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
    db.set_compaction_filter(Some(Arc::new(AikvCompactionFilter::single_node())));
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
    db.set_compaction_filter(Some(Arc::new(AikvCompactionFilter::single_node())));
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
