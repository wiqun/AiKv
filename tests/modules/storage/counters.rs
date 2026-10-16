use aidb::engine::compaction::{CompactionFilter, FilterDecision};
use aikv::storage::adapter::KvStorageAdapter;
use aikv::storage::ttl_filter::TtlExpireFilter;
use aikv::storage::types::StoredValue;
use aikv::storage::types::{now_ms, ExpiresCounters, KvStorage};
use aikv::storage::AiDbEngine;
use tempfile::tempdir;

fn make_temp_engine() -> (tempfile::TempDir, std::sync::Arc<AiDbEngine>) {
    let dir = tempdir().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    (dir, engine)
}

#[tokio::test]
async fn expires_tracks_process_local_ttl() {
    let (_dir, engine) = make_temp_engine();
    let storage = KvStorageAdapter::open(engine, None).await.unwrap();

    assert_eq!(storage.expires(0).await.unwrap(), 0);
    storage
        .set_with_ttl(0, b"k", b"v", now_ms() + 60_000)
        .await
        .unwrap();
    assert_eq!(storage.expires(0).await.unwrap(), 1);
    storage
        .set_with_ttl(0, b"k", b"v2", now_ms() + 60_000)
        .await
        .unwrap();
    assert_eq!(storage.expires(0).await.unwrap(), 2);
    assert!(storage.delete(0, b"k").await.unwrap());
    assert_eq!(storage.expires(0).await.unwrap(), 1);
}

#[tokio::test]
async fn lazy_expire_decrements_expires_not_len() {
    let (_dir, engine) = make_temp_engine();
    let storage = KvStorageAdapter::open(engine, None).await.unwrap();

    storage
        .set_with_ttl(0, b"gone", b"v", now_ms().saturating_sub(1_000))
        .await
        .unwrap();
    assert_eq!(storage.expires(0).await.unwrap(), 1);
    assert!(storage.get(0, b"gone").await.unwrap().is_none());
    assert_eq!(storage.expires(0).await.unwrap(), 0);
}

#[tokio::test]
async fn delete_without_ttl_leaves_expires_at_zero() {
    let (_dir, engine) = make_temp_engine();
    let storage = KvStorageAdapter::open(engine, None).await.unwrap();

    storage.set(0, b"plain", b"v").await.unwrap();
    assert!(storage.delete(0, b"plain").await.unwrap());
    assert_eq!(storage.expires(0).await.unwrap(), 0);
}

#[test]
fn ttl_expire_filter_does_not_touch_expires_counters() {
    let counters = ExpiresCounters::new();
    counters.on_ttl_write(0);
    let stored = StoredValue {
        value: aikv::storage::types::ValueType::String(b"v".to_vec()),
        expires_at: Some(1),
    };
    let bytes = postcard::to_allocvec(&stored).unwrap();
    assert_eq!(
        TtlExpireFilter.filter(0, b"k", &bytes),
        FilterDecision::Remove
    );
    assert_eq!(counters.get(0), 1);
}
