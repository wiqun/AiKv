use tempfile::TempDir;

use aikv::storage::types::{now_ms, KvStorage};
use aikv::storage::{AiDbEngine, KvStorageAdapter};

#[tokio::test]
async fn reopen_expires_starts_at_zero() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    {
        let engine = AiDbEngine::open_for_testing(&path).expect("open aidb");
        let storage = KvStorageAdapter::open(engine, None).await.unwrap();
        storage
            .set_with_ttl(0, b"k", b"v", now_ms() + 60_000)
            .await
            .unwrap();
        assert_eq!(storage.expires(0).await.unwrap(), 1);
        storage.flush_engine().await.unwrap();
    }
    let engine = AiDbEngine::open_for_testing(&path).expect("reopen");
    let storage = KvStorageAdapter::open(engine, None).await.unwrap();
    assert_eq!(storage.expires(0).await.unwrap(), 0);
}
