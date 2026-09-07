use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time;

use super::helpers::{connect, read_response, write_cmd};
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter, StorageAdapter};

#[tokio::test]
async fn test_info_threads_and_storage_recovery_fields() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let stats = engine.aidb_statistics().expect("must have stats");

    // 注入底层 4 项 recovery 指标
    stats
        .recovery_wal_replay_duration_us
        .store(12500, Ordering::Relaxed);
    stats
        .recovery_wal_replayed_bytes
        .store(67108864, Ordering::Relaxed);
    stats
        .recovery_manifest_duration_us
        .store(3200, Ordering::Relaxed);
    stats
        .recovery_sstable_open_duration_us
        .store(8100, Ordering::Relaxed);

    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let state = ServerSharedState::new(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
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

    // 1. 测试 INFO threads 精确格式与字段
    write_cmd(&mut stream, &[b"INFO", b"threads"]).await;
    let resp = read_response(&mut stream).await;
    let s = String::from_utf8_lossy(&resp);
    assert!(s.contains("# Threads\r\n"), "must contain # Threads header");
    #[cfg(target_os = "linux")]
    {
        // Linux 环境下 process_threads 应大于 0，且包含两项上下文切换计数
        assert!(s.contains("\r\nprocess_threads:"));
        assert!(s.contains("\r\ncontext_switches_voluntary:"));
        assert!(s.contains("\r\ncontext_switches_nonvoluntary:"));
    }

    // 2. 测试 INFO storage 恢复字段精确字符串
    write_cmd(&mut stream, &[b"INFO", b"storage"]).await;
    let resp = read_response(&mut stream).await;
    let s = String::from_utf8_lossy(&resp);
    assert!(
        s.contains("aidb_recovery_wal_replay_duration_us:12500\r\n"),
        "missing aidb_recovery_wal_replay_duration_us in: {s}"
    );
    assert!(
        s.contains("aidb_recovery_wal_replayed_bytes:67108864\r\n"),
        "missing aidb_recovery_wal_replayed_bytes in: {s}"
    );
    assert!(
        s.contains("aidb_recovery_manifest_duration_us:3200\r\n"),
        "missing aidb_recovery_manifest_duration_us in: {s}"
    );
    assert!(
        s.contains("aidb_recovery_sstable_open_duration_us:8100\r\n"),
        "missing aidb_recovery_sstable_open_duration_us in: {s}"
    );
    assert!(
        s.contains("aikv_recovery_rebuild_counters_duration_us:0\r\n"),
        "missing aikv_recovery_rebuild_counters_duration_us in: {s}"
    );

    // 3. 测试 INFO all 包含 # Threads (通过循环读取完整 bulk string)
    write_cmd(&mut stream, &[b"INFO", b"all"]).await;
    let resp = read_full_bulk(&mut stream).await;
    let s = String::from_utf8_lossy(&resp);
    assert!(
        s.contains("# Threads\r\n"),
        "INFO all must contain # Threads header"
    );
}

async fn read_full_bulk(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut expected_total: Option<usize> = None;
    loop {
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if expected_total.is_none() {
            if let Some(pos) = buf.windows(2).position(|w| w == b"\r\n") {
                let header = std::str::from_utf8(&buf[..pos]).unwrap();
                if let Some(len_str) = header.strip_prefix('$') {
                    let body_len: usize = len_str.parse().unwrap();
                    expected_total = Some(pos + 2 + body_len + 2);
                }
            }
        }
        if let Some(total) = expected_total {
            if buf.len() >= total {
                break;
            }
        }
    }
    buf
}
