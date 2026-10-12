//! 版本化 subkey 的 compaction 回收 (真实 aidb).

use std::collections::HashMap;
use std::sync::Arc;

use aikv::storage::compaction_filter::AikvCompactionFilter;
use aikv::storage::subkey::subkey_db_prefix;
use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter, StoredValue, ValueType};
use tempfile::TempDir;

fn open(dir: &TempDir, grace_us: u64) -> (Arc<dyn KvStorage>, Arc<aidb::DB>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let db = engine.db.clone();
    db.set_compaction_filter(Some(Arc::new(
        AikvCompactionFilter::single_node().with_grace_us(grace_us),
    )));
    (KvStorageAdapter::new(engine), db)
}

fn big_hash(n: usize) -> HashMap<Vec<u8>, Vec<u8>> {
    (0..n)
        .map(|i| (format!("f{i}").into_bytes(), b"v".to_vec()))
        .collect()
}

fn compact(db: &aidb::DB) {
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'p', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();
}

fn subkey_count(db: &aidb::DB) -> usize {
    let prefix = subkey_db_prefix(0);
    let mut end = prefix.clone();
    *end.last_mut().unwrap() += 1;
    db.scan(Some(&prefix), Some(&end)).unwrap().count()
}

/// 删除集合后 compaction 必须回收其全部 subkey.
#[tokio::test]
async fn deleted_collection_subkeys_are_collected() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(big_hash(100)),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    storage.delete(0, b"h").await.unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 0);
    db.close().unwrap();
}

/// 仍在使用的集合 subkey 不得被回收.
#[tokio::test]
async fn live_collection_subkeys_survive() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(big_hash(100)),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 100);
    let got = storage.get_expanded(0, b"h").await.unwrap().unwrap();
    assert_eq!(got.value, ValueType::Hash(big_hash(100)));
    db.close().unwrap();
}

/// 同名集合被整体替换后, 旧版本的 subkey 必须被回收, 只留新版本.
#[tokio::test]
async fn replaced_version_subkeys_are_collected() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    for _ in 0..2 {
        storage
            .put_value(
                0,
                b"h",
                StoredValue {
                    value: ValueType::Hash(big_hash(100)),
                    expires_at: None,
                },
            )
            .await
            .unwrap();
    }
    compact(&db);
    assert_eq!(subkey_count(&db), 100);
    db.close().unwrap();
}

/// 已过期的集合: header 过期即视为不存在, subkey 应被回收.
#[tokio::test]
async fn expired_collection_subkeys_are_collected() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    let past = aikv::storage::now_ms() - 1000;
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(big_hash(100)),
                expires_at: Some(past),
            },
        )
        .await
        .unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 0);
    db.close().unwrap();
}

/// 宽限期内 (版本刚生成) 的孤儿 subkey 必须保留, 给暂存写切换 header 留时间.
#[tokio::test]
async fn orphan_subkeys_inside_grace_period_survive() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 3_600_000_000);
    storage
        .put_value(
            0,
            b"h",
            StoredValue {
                value: ValueType::Hash(big_hash(100)),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    storage.delete(0, b"h").await.unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 100);
    db.close().unwrap();
}
