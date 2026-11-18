//! Phase 3.5 安全基线与运行时 QoS 集成测试
//! @component aikv-server

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use aikv::config::{resolve_from_parts, CliOverrides};
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{KvStorage, MemoryEngine};

use super::helpers::{connect, read_response, write_cmd};

async fn start_server_with_toml(
    toml_raw: &str,
) -> (
    std::net::SocketAddr,
    Arc<ServerSharedState>,
    tokio::task::JoinHandle<()>,
) {
    let (settings, _warnings) = resolve_from_parts(
        Some(toml_raw),
        std::iter::empty::<(String, String)>(),
        &CliOverrides::default(),
    )
    .expect("resolve config");

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(settings.server_ext.databases);
    let conn_cfg = ConnectionConfig {
        read_timeout: (settings.server_ext.read_timeout_ms > 0)
            .then(|| Duration::from_millis(settings.server_ext.read_timeout_ms)),
        idle_timeout: (settings.server_ext.idle_timeout_ms > 0)
            .then(|| Duration::from_millis(settings.server_ext.idle_timeout_ms)),
        max_clients: settings.max_clients,
        script_worker_threads: settings.script_worker_threads,
        ..Default::default()
    };
    let state = ServerSharedState::new(conn_cfg, storage, 0);
    state.apply_resolved_settings(&settings);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let run_state = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        let _ = Server::run_with_listener(listener, run_state).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, state, handle)
}

async fn send_pipeline(stream: &mut TcpStream, cmds: &[&[&[u8]]]) -> Vec<u8> {
    let mut payload = String::new();
    for parts in cmds {
        payload.push_str(&format!("*{}\r\n", parts.len()));
        for p in *parts {
            payload.push_str(&format!("${}\r\n", p.len()));
            payload.push_str(std::str::from_utf8(p).unwrap());
            payload.push_str("\r\n");
        }
    }
    stream.write_all(payload.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut buf = vec![0u8; 8192];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
        .await
        .expect("read timeout")
        .expect("read failed");
    buf.truncate(n);
    buf
}

#[tokio::test]
async fn test_requirepass_auth_flow_and_pipeline() {
    let toml = r#"
[server]
requirepass = "s3cr3t"
"#;
    let (addr, _state, handle) = start_server_with_toml(toml).await;
    let mut stream = connect(addr).await;

    // 1. 未认证拦截 PING / GET / MULTI / 无 AUTH 的 HELLO
    write_cmd(&mut stream, &[b"PING"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.starts_with("-NOAUTH"), "got: {resp}");

    write_cmd(&mut stream, &[b"MULTI"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.starts_with("-NOAUTH"), "got: {resp}");

    write_cmd(&mut stream, &[b"HELLO", b"3"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.starts_with("-NOAUTH"), "got: {resp}");

    // 2. 错误密码返回 WRONGPASS
    write_cmd(&mut stream, &[b"AUTH", b"wrong"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.starts_with("-WRONGPASS"), "got: {resp}");

    // 3. 单参数 AUTH 成功放行
    write_cmd(&mut stream, &[b"AUTH", b"s3cr3t"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"+OK\r\n");

    write_cmd(&mut stream, &[b"PING"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"+PONG\r\n");

    // 4. 新连接使用 AUTH default <pw> 与 HELLO 3 AUTH default <pw>
    let mut s2 = connect(addr).await;
    write_cmd(&mut s2, &[b"AUTH", b"default", b"s3cr3t"]).await;
    assert_eq!(read_response(&mut s2).await, b"+OK\r\n");

    let mut s3 = connect(addr).await;
    write_cmd(&mut s3, &[b"HELLO", b"3", b"AUTH", b"default", b"s3cr3t"]).await;
    let hello_resp = read_response(&mut s3).await;
    assert!(
        hello_resp.starts_with(b"%"),
        "expected RESP3 map, got: {:?}",
        hello_resp
    );

    // 5. Pipeline 中 AUTH 屏障后紧跟 SET/GET
    let mut s4 = connect(addr).await;
    let pipe_resp = send_pipeline(
        &mut s4,
        &[
            &[b"AUTH", b"s3cr3t"],
            &[b"SET", b"pk", b"pv"],
            &[b"GET", b"pk"],
        ],
    )
    .await;
    let text = String::from_utf8_lossy(&pipe_resp);
    assert!(text.contains("+OK\r\n"), "got: {text}");
    assert!(text.contains("$2\r\npv\r\n"), "got: {text}");

    // 6. CONFIG SET requirepass "" 动态关闭密码
    write_cmd(&mut stream, &[b"CONFIG", b"SET", b"requirepass", b""]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");

    let mut s5 = connect(addr).await;
    write_cmd(&mut s5, &[b"PING"]).await;
    assert_eq!(read_response(&mut s5).await, b"+PONG\r\n");

    handle.abort();
}

#[tokio::test]
async fn test_rename_command_and_conflict_validation() {
    // 1. 冲突配置在解析阶段报错
    let conflict_toml = r#"
[server.rename_command]
KEYS = "SAFE_CMD"
FLUSHALL = "SAFE_CMD"
"#;
    assert!(resolve_from_parts(
        Some(conflict_toml),
        std::iter::empty::<(String, String)>(),
        &CliOverrides::default(),
    )
    .is_err());

    let builtin_conflict_toml = r#"
[server.rename_command]
KEYS = "GET"
"#;
    assert!(resolve_from_parts(
        Some(builtin_conflict_toml),
        std::iter::empty::<(String, String)>(),
        &CliOverrides::default(),
    )
    .is_err());

    // 2. 正常重命名与禁用
    let toml = r#"
[server.rename_command]
keys = "SAFE_KEYS"
flushall = ""
"#;
    let (addr, _state, handle) = start_server_with_toml(toml).await;
    let mut stream = connect(addr).await;

    // 写入测试 key
    write_cmd(&mut stream, &[b"SET", b"rk1", b"v1"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");

    // 原名 KEYS 与禁用 FLUSHALL 均返回 unknown command
    write_cmd(&mut stream, &[b"KEYS", b"*"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.contains("unknown command"), "got: {resp}");

    write_cmd(&mut stream, &[b"FLUSHALL"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.contains("unknown command"), "got: {resp}");

    // 新名 SAFE_KEYS 可正常执行
    write_cmd(&mut stream, &[b"SAFE_KEYS", b"*"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.contains("rk1"), "got: {resp}");

    // MULTI 入队禁用命令应被拒绝
    write_cmd(&mut stream, &[b"MULTI"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    write_cmd(&mut stream, &[b"FLUSHALL"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(resp.contains("unknown command"), "got: {resp}");
    write_cmd(&mut stream, &[b"DISCARD"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");

    // Pipeline 中包含禁用命令返回错误
    let pipe = send_pipeline(&mut stream, &[&[b"KEYS", b"*"], &[b"SAFE_KEYS", b"*"]]).await;
    let text = String::from_utf8_lossy(&pipe);
    assert!(text.contains("unknown command"), "got: {text}");
    assert!(text.contains("rk1"), "got: {text}");

    // COMMAND INFO 对原名返回 nil, 对新名返回信息
    write_cmd(&mut stream, &[b"COMMAND", b"INFO", b"KEYS", b"SAFE_KEYS"]).await;
    let resp = String::from_utf8_lossy(&read_response(&mut stream).await).into_owned();
    assert!(
        resp.contains("_\r\n") || resp.contains("$-1\r\n"),
        "expected nil for KEYS, got: {resp}"
    );
    assert!(
        resp.contains("safe_keys") || resp.contains("SAFE_KEYS"),
        "got: {resp}"
    );

    handle.abort();
}

#[tokio::test]
async fn test_client_output_buffer_limit_hard_and_soft() {
    let toml = r#"
[server.client_output_buffer_limit.normal]
hard_limit_bytes = 4096
soft_limit_bytes = 2048
soft_seconds = 1
"#;
    let (addr, _state, handle) = start_server_with_toml(toml).await;

    // 1. 准备一个 8KB 的 value
    let mut setup = connect(addr).await;
    let big_val = vec![b'x'; 8192];
    write_cmd(&mut setup, &[b"SET", b"big_ob_key", &big_val]).await;
    assert_eq!(read_response(&mut setup).await, b"+OK\r\n");

    // 2. 新连接读取 big_ob_key 触发 hard_limit_bytes=4096，服务端应主动断开而不回写超限响应
    let mut victim = connect(addr).await;
    write_cmd(&mut victim, &[b"GET", b"big_ob_key"]).await;
    let mut buf = vec![0u8; 2048];
    let read_res = tokio::time::timeout(Duration::from_secs(2), victim.read(&mut buf))
        .await
        .expect("read timeout");
    assert!(
        matches!(read_res, Ok(0) | Err(_)),
        "expected connection closed by output buffer limit, got: {read_res:?}"
    );

    // 3. INFO stats 中 client_output_buffer_limit_disconnections >= 1
    write_cmd(&mut setup, &[b"INFO", b"stats"]).await;
    let info = String::from_utf8_lossy(&read_response(&mut setup).await).into_owned();
    assert!(
        info.contains("client_output_buffer_limit_disconnections:1"),
        "expected disconnections=1 in INFO, got: {info}"
    );

    // 4. CONFIG SET client-output-buffer-limit 热放开限制后可正常读取
    write_cmd(
        &mut setup,
        &[
            b"CONFIG",
            b"SET",
            b"client-output-buffer-limit",
            b"normal 0 0 0",
        ],
    )
    .await;
    assert_eq!(read_response(&mut setup).await, b"+OK\r\n");

    let mut ok_conn = connect(addr).await;
    write_cmd(&mut ok_conn, &[b"GET", b"big_ob_key"]).await;
    let resp = read_response(&mut ok_conn).await;
    assert!(resp.starts_with(b"$8192\r\n"), "expected full 8192B reply");

    handle.abort();
}
