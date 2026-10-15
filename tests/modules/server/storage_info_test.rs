use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time;

use super::helpers::{connect, read_response, write_cmd};
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter, StorageAdapter};

#[tokio::test]
async fn test_info_storage_all_whitelist_metrics_rendered() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let stats = engine.aidb_statistics().expect("must have stats");

    // 注入测试数值
    stats.wal_written_bytes.store(101, Ordering::Relaxed);
    stats.flush_written_bytes.store(202, Ordering::Relaxed);
    stats.compaction_written_bytes.store(303, Ordering::Relaxed);
    stats.logical_write_bytes.store(404, Ordering::Relaxed);
    stats.block_read_bytes.store(505, Ordering::Relaxed);
    stats.logical_read_bytes.store(606, Ordering::Relaxed);
    stats.compaction_read_bytes.store(707, Ordering::Relaxed);
    stats.bloom_useful.store(808, Ordering::Relaxed);
    stats.compaction_pending_bytes.store(909, Ordering::Relaxed);

    // 注入 write stall: 2 个 kind, 2 + 3 = 5 次请求
    stats.write_stall_requests[0].store(2, Ordering::Relaxed);
    stats.write_stall_requests[2].store(3, Ordering::Relaxed);
    stats.write_stall_durations[0].record(1500);
    stats.write_stall_durations[2].record(2500);
    stats
        .write_stall_max_duration_us
        .store(5000, Ordering::Relaxed);

    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let state = ServerSharedState::new(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
            script_worker_threads: 2,
        },
        storage,
        0,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _handle = tokio::spawn(async move {
        let _ = Server::run_with_listener(listener, state).await;
    });
    time::sleep(Duration::from_millis(50)).await;

    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"INFO", b"storage"]).await;
    let resp = read_response(&mut stream).await;
    let info = String::from_utf8_lossy(&resp);

    assert!(info.contains("storage_engine:aidb"), "info output: {info}");
    assert!(
        info.contains("aidb_wal_written_bytes:101"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_flush_written_bytes:202"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_compaction_written_bytes:303"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_logical_write_bytes:404"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_block_read_bytes:505"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_logical_read_bytes:606"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_compaction_read_bytes:707"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_bloom_useful:808"),
        "info output: {info}"
    );
    assert!(
        info.contains("aidb_compaction_pending_bytes:909"),
        "info output: {info}"
    );

    // write stall 汇总断言: 2 + 3 = 5
    assert!(
        info.contains("aidb_write_stall_requests:5"),
        "info output: {info}"
    );
    // stall duration sum: 1500 + 2500 = 4000
    assert!(
        info.contains("aidb_write_stall_duration_us:4000"),
        "info output: {info}"
    );
    // stall max duration us: 5000
    assert!(
        info.contains("aidb_write_stall_max_duration_us:5000"),
        "info output: {info}"
    );
}
