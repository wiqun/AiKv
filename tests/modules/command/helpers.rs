use std::sync::Arc;
use std::time::Duration;

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{KvStorage, MemoryEngine};
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;

pub fn router() -> CommandRouter {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    CommandRouter::new(storage)
}

pub fn router_with_script_workers(threads: usize) -> CommandRouter {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    CommandRouter::new(storage).with_script_worker_threads(threads)
}

pub fn shared_state() -> Arc<ServerSharedState> {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    ServerSharedState::new(ConnectionConfig::default(), storage, 6379)
}

pub fn router_with_shared() -> (Arc<CommandRouter>, Arc<ServerSharedState>) {
    let shared = shared_state();
    (shared.router(), shared)
}

/// 启动 ephemeral TCP server (127.0.0.1:随机端口), 供 MIGRATE 集成测使用.
pub async fn start_ephemeral_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
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
    let handle = tokio::spawn(async move {
        let _ = Server::run_with_listener(listener, state).await;
    });
    time::sleep(Duration::from_millis(50)).await;
    (addr, handle)
}

pub async fn tcp_get(addr: std::net::SocketAddr, key: &str) -> Vec<u8> {
    let mut stream = time::timeout(Duration::from_secs(2), TcpStream::connect(addr))
        .await
        .expect("connect timeout")
        .expect("connect failed");
    let frame = format!("*2\r\n$3\r\nGET\r\n${}\r\n{key}\r\n", key.len());
    stream.write_all(frame.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let n = time::timeout(Duration::from_secs(2), stream.read(&mut buf))
        .await
        .expect("read timeout")
        .expect("read failed");
    buf.truncate(n);
    buf
}

pub fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

pub fn assert_ok(resp: RespValue) {
    assert_eq!(resp, RespValue::SimpleString("OK".into()));
}

pub fn assert_int(resp: RespValue, n: i64) {
    assert_eq!(resp, RespValue::Integer(n));
}

pub fn assert_nil(resp: RespValue) {
    assert_eq!(resp, RespValue::BulkString(None));
}

pub fn assert_err_contains(err: aikv::error::Error, needle: &str) {
    assert!(
        err.to_string().contains(needle),
        "expected error containing {needle:?}, got {err}"
    );
}
