//! 测试辅助: 启动 ephemeral server

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time;

use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{KvStorage, MemoryEngine};

pub async fn start_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
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

pub async fn connect(addr: std::net::SocketAddr) -> TcpStream {
    time::timeout(Duration::from_secs(2), TcpStream::connect(addr))
        .await
        .expect("connect timeout")
        .expect("connect failed")
}

pub async fn read_response(stream: &mut TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 4096];
    let n = time::timeout(Duration::from_secs(2), stream.read(&mut buf))
        .await
        .expect("read timeout")
        .expect("read failed");
    buf.truncate(n);
    buf
}

pub async fn write_cmd(stream: &mut TcpStream, parts: &[&[u8]]) {
    let mut frame = format!("*{}\r\n", parts.len());
    for part in parts {
        frame.push_str(&format!("${}\r\n", part.len()));
        frame.push_str(std::str::from_utf8(part).expect("utf8 cmd"));
        frame.push_str("\r\n");
    }
    stream.write_all(frame.as_bytes()).await.unwrap();
}

use tokio::io::AsyncReadExt;

pub async fn read_all_available(stream: &mut TcpStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        match time::timeout(Duration::from_millis(200), stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
            Ok(Err(e)) => panic!("read failed: {e}"),
            Err(_) => break,
        }
    }
    out
}
