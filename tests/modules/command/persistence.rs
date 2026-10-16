use std::sync::Arc;
use std::time::Duration;

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{AiDbEngine, KvStorage, KvStorageAdapter, StorageEngineKind};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time;

use super::helpers::{assert_err_contains, b, router_with_shared};

fn aidb_router(dir: &TempDir) -> (Arc<CommandRouter>, Arc<ServerSharedState>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let shared = ServerSharedState::new_with_engine(
        ConnectionConfig::default(),
        storage,
        6379,
        StorageEngineKind::AiDb,
        Some(dir.path().to_path_buf()),
    );
    (shared.router(), shared)
}

async fn tcp_cmd(addr: std::net::SocketAddr, parts: &[&[u8]]) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut frame = format!("*{}\r\n", parts.len());
    for part in parts {
        frame.push_str(&format!("${}\r\n", part.len()));
        frame.push_str(std::str::from_utf8(part).unwrap());
        frame.push_str("\r\n");
    }
    stream.write_all(frame.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap();
    buf.truncate(n);
    buf
}

#[tokio::test]
async fn test_config_get_appendonly_no() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("CONFIG", &[b("GET"), b("appendonly")], &mut db)
        .await
        .unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
    let RespValue::BulkString(Some(val)) = &items[1] else {
        panic!("expected bulk");
    };
    assert_eq!(String::from_utf8_lossy(val), "no");
}

#[tokio::test]
async fn test_config_set_appendonly_rejected() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let err = router
        .execute("CONFIG", &[b("SET"), b("appendonly"), b("yes")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "appendonly");
}

#[tokio::test]
async fn test_save_memory_err() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let err = router.execute("SAVE", &[], &mut db).await.unwrap_err();
    assert_err_contains(err, "memory engine");
}

#[tokio::test]
async fn test_lastsave_memory_zero() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router.execute("LASTSAVE", &[], &mut db).await.unwrap();
    assert_eq!(resp, RespValue::Integer(0));
}

#[tokio::test]
async fn test_bgsave_memory_err() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let err = router.execute("BGSAVE", &[], &mut db).await.unwrap_err();
    assert_err_contains(err, "memory engine");
}

#[tokio::test]
async fn test_shutdown_memory_save_err() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let err = router
        .execute("SHUTDOWN", &[b("SAVE")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "memory engine");
}

#[tokio::test]
async fn test_bgsave_persistent_engine() {
    let dir = TempDir::new().unwrap();
    let (router, shared) = aidb_router(&dir);
    let mut db = 0;
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    let resp = router.execute("SAVE", &[], &mut db).await.unwrap();
    assert_eq!(resp, RespValue::SimpleString("OK".into()));
    assert!(shared.backup_dir.exists());
}

#[tokio::test]
async fn test_bgsave_in_progress() {
    let dir = TempDir::new().unwrap();
    let (router, _shared) = aidb_router(&dir);
    let mut db = 0;
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    let _ = router.execute("BGSAVE", &[], &mut db).await.unwrap();
    let err = router.execute("BGSAVE", &[], &mut db).await.unwrap_err();
    assert_err_contains(err, "already in progress");
    time::sleep(Duration::from_millis(500)).await;
}

#[tokio::test]
async fn test_info_persistence_section() {
    let dir = TempDir::new().unwrap();
    let (router, _shared) = aidb_router(&dir);
    let mut db = 0;
    let resp = router
        .execute("INFO", &[b("persistence")], &mut db)
        .await
        .unwrap();
    let RespValue::BulkString(Some(text)) = resp else {
        panic!("expected bulk");
    };
    let s = String::from_utf8_lossy(&text);
    assert!(s.contains("# Persistence"));
    assert!(s.contains("rdb_bgsave_in_progress:"));
}

#[tokio::test]
async fn test_shutdown_nosave_exits_server() {
    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(engine);
    let state = ServerSharedState::new_with_engine(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
            script_worker_threads: 2,
        },
        storage,
        0,
        StorageEngineKind::AiDb,
        Some(dir.path().to_path_buf()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = state.shutdown.clone();
    let handle = tokio::spawn(async move {
        Server::run_with_listener(listener, state).await.unwrap();
    });
    time::sleep(Duration::from_millis(50)).await;

    let resp = tcp_cmd(addr, &[b"SHUTDOWN", b"NOSAVE"]).await;
    assert!(String::from_utf8_lossy(&resp).contains("OK"));

    time::timeout(Duration::from_secs(2), async {
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("shutdown not triggered");
    let _ = time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn test_bgsave_restore() {
    let dir = TempDir::new().unwrap();
    let (router, shared) = aidb_router(&dir);
    let mut db = 0;
    router
        .execute("SET", &[b("persist"), b("yes")], &mut db)
        .await
        .unwrap();
    router.execute("BGSAVE", &[], &mut db).await.unwrap();
    time::timeout(Duration::from_secs(5), async {
        while shared
            .bgsave_in_progress
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("bgsave did not finish");

    let restored = AiDbEngine::open_for_testing(&shared.backup_dir).unwrap();
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(restored);
    let resp = storage.get(0, b"persist").await.unwrap();
    assert_eq!(resp, Some(b"yes".to_vec()));
}

#[tokio::test]
async fn test_bgsave_restore_smoke() {
    let dir = TempDir::new().unwrap();
    let (router, shared) = aidb_router(&dir);
    let mut db = 0;
    router
        .execute("SET", &[b("persist"), b("yes")], &mut db)
        .await
        .unwrap();
    router.execute("SAVE", &[], &mut db).await.unwrap();

    let restored = AiDbEngine::open_for_testing(&shared.backup_dir).unwrap();
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(restored);
    let resp = storage.get(0, b"persist").await.unwrap();
    assert_eq!(resp, Some(b"yes".to_vec()));
}
