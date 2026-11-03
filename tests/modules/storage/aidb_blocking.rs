//! @component aikv-storage

//! 回归: AiDbEngine::get/set 须走 spawn_blocking, 避免阻塞 Tokio worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aikv::storage::{AiDbEngine, StorageAdapter};
use tempfile::TempDir;

#[tokio::test(flavor = "current_thread")]
async fn test_aidb_get_set_offload_to_blocking_pool() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    engine.set(b"k".to_vec(), b"v".to_vec()).await.unwrap();
    let key = b"k".to_vec();
    let expected_val = b"v".to_vec();

    let finished = Arc::new(AtomicBool::new(false));
    let flag = finished.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        flag.store(true, Ordering::SeqCst);
    });

    for _ in 0..32 {
        assert_eq!(
            engine.get(key.clone()).await.unwrap(),
            Some(expected_val.clone())
        );
    }

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        finished.load(Ordering::SeqCst),
        "timer task should run while get/set use spawn_blocking"
    );
}
