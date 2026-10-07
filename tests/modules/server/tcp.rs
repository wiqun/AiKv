//! TCP 内联命令与网络交互集成测试
//! @component aikv-server

use aikv::protocol::RespParser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::helpers::{connect, read_all_available, read_response, start_server, write_cmd};

/// 验证 TCP 协议直连发送 PING 返回 PONG
#[tokio::test]
async fn test_tcp_ping() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"PING"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"+PONG\r\n");
}

#[tokio::test]
async fn test_tcp_echo() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"ECHO", b"hello"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"$5\r\nhello\r\n");
}

#[tokio::test]
async fn test_tcp_hello_resp2() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"HELLO", b"2"]).await;
    let resp = read_response(&mut stream).await;
    // RESP2 HELLO 返回 flat array,交替 key-value 对
    let mut parser = RespParser::new();
    parser.feed(&resp);
    let value = parser.parse().unwrap().expect("hello response");
    let aikv::protocol::RespValue::Array(Some(items)) = value else {
        panic!("RESP2 HELLO 应返回 flat array，非 RESP3 Map");
    };
    // items 交替排列: [k1, v1, k2, v2, ...],至少 4 对 (8 项)
    assert!(items.len() >= 8, "RESP2 HELLO 响应至少包含 4 对 key-value");
    assert_eq!(items.len() % 2, 0, "key-value 应对称");
}

#[tokio::test]
async fn test_tcp_hello_resp3() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"HELLO", b"3"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"%"));
    assert!(resp.windows(5).any(|w| w == b"proto"));
}

#[tokio::test]
async fn test_tcp_hello_no_args() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"HELLO"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"%"));
}

#[tokio::test]
async fn test_tcp_empty_array() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    stream.write_all(b"*0\r\n").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut buf = [0u8; 16];
    match stream.try_read(&mut buf) {
        Ok(0) => {}
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(n) => panic!("unexpected response bytes: {n}"),
        Err(e) => panic!("try_read failed: {e}"),
    }
}

#[tokio::test]
async fn test_tcp_unknown_command() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"FOOBAR"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"-ERR unknown command"));
}

#[tokio::test]
async fn test_tcp_set_get() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"foo", b"bar"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"+OK\r\n");
    write_cmd(&mut stream, &[b"GET", b"foo"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"$3\r\nbar\r\n");
}

#[tokio::test]
async fn test_tcp_set_with_ex() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"k", b"v", b"EX", b"60"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    write_cmd(&mut stream, &[b"TTL", b"k"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b":"));
}

#[tokio::test]
async fn test_tcp_mget_mset() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"MSET", b"a", b"1", b"b", b"2"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    write_cmd(&mut stream, &[b"MGET", b"a", b"b", b"c"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"*3"));
}

#[tokio::test]
async fn test_tcp_expire_ttl() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"k", b"v"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"EXPIRE", b"k", b"120"]).await;
    assert_eq!(read_response(&mut stream).await, b":1\r\n");
    write_cmd(&mut stream, &[b"TTL", b"k"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b":"));
}

#[tokio::test]
async fn test_tcp_select_dbsize() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"k", b"v"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"SELECT", b"1"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    write_cmd(&mut stream, &[b"DBSIZE"]).await;
    let selected = read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"SELECT", b"0"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"DBSIZE"]).await;
    let original = read_response(&mut stream).await;
    assert_eq!(selected, original);
    assert_ne!(selected, b":0\r\n");
    assert!(selected.starts_with(b":"));
}

#[tokio::test]
async fn test_tcp_hash_commands() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"HSET", b"h", b"f", b"v"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"HGET", b"h", b"f"]).await;
    assert_eq!(read_response(&mut stream).await, b"$1\r\nv\r\n");
}

#[tokio::test]
async fn test_tcp_wrong_type_error() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"s", b"v"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"HGET", b"s", b"f"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"-WRONGTYPE"));
}

#[tokio::test]
async fn test_tcp_pipeline_commands() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    stream
        .write_all(b"*3\r\n$3\r\nSET\r\n$1\r\na\r\n$1\r\n1\r\n*2\r\n$3\r\nGET\r\n$1\r\na\r\n")
        .await
        .unwrap();
    let resp = read_all_available(&mut stream).await;
    assert!(resp.windows(5).any(|w| w == b"+OK\r\n"));
    assert!(resp.windows(7).any(|w| w == b"$1\r\n1\r\n"));
}

#[tokio::test]
async fn test_tcp_pipeline() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    let mut payload = String::new();
    for _ in 0..100 {
        payload.push_str("*1\r\n$4\r\nPING\r\n");
    }
    stream.write_all(payload.as_bytes()).await.unwrap();
    let mut resp = vec![0u8; 100 * 7];
    stream.read_exact(&mut resp).await.unwrap();
    assert_eq!(resp, b"+PONG\r\n".repeat(100));
}

#[tokio::test]
async fn test_tcp_concurrent_connections() {
    let (addr, _handle) = start_server().await;
    let mut handles = Vec::new();
    for _ in 0..8 {
        handles.push(tokio::spawn(async move {
            let mut stream = connect(addr).await;
            write_cmd(&mut stream, &[b"PING"]).await;
            let resp = read_response(&mut stream).await;
            assert_eq!(resp, b"+PONG\r\n");
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn test_tcp_hello_version_switch_in_pipeline() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    stream
        .write_all(b"*2\r\n$5\r\nHELLO\r\n$1\r\n3\r\n*1\r\n$4\r\nPING\r\n")
        .await
        .unwrap();
    let resp = read_all_available(&mut stream).await;
    assert!(resp.starts_with(b"%"));
    assert!(std::str::from_utf8(&resp).unwrap().contains("+PONG"));
}

#[tokio::test]
#[ignore = "stress: TCP slow send (1 byte per ms)"]
async fn test_tcp_malicious_slow_send() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    for byte in b"*1\r\n$4\r\nPING\r\n" {
        stream.write_all(&[*byte]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"+PONG\r\n");
}

#[tokio::test]
#[ignore = "stress: large TCP pipeline buffer (1000 PINGs)"]
async fn test_tcp_pipeline_large_buffer() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    let payload = "*1\r\n$4\r\nPING\r\n".repeat(1000);
    stream.write_all(payload.as_bytes()).await.unwrap();
    let mut resp = vec![0u8; 7000];
    stream.read_exact(&mut resp).await.unwrap();
    assert_eq!(resp.len(), 7000);
}

#[tokio::test]
async fn test_tcp_monitor() {
    let (addr, _handle) = start_server().await;

    let mut monitor = connect(addr).await;
    write_cmd(&mut monitor, &[b"MONITOR"]).await;
    assert_eq!(read_response(&mut monitor).await, b"+OK\r\n");

    let mut client = connect(addr).await;
    write_cmd(&mut client, &[b"SET", b"mk", b"mv"]).await;
    let _ = read_response(&mut client).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let line = read_response(&mut monitor).await;
    let text = String::from_utf8_lossy(&line);
    assert!(text.starts_with('+'));
    assert!(text.contains("\"SET\""));
    assert!(text.contains("\"mk\""));
    assert!(text.contains("\"mv\""));
}

/// Pipeline 中阻塞命令前必须先刷出同批已排队响应
#[tokio::test]
async fn test_tcp_pipeline_flush_before_blocking_blpop() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;

    // SET + PING + BLPOP(空 list, timeout=0.2) 同批发送:
    // SET/PING 必须在 BLPOP 阻塞等待期间就能被客户端读到
    let mut pipeline_req = Vec::new();
    pipeline_req.extend_from_slice(b"*3\r\n$3\r\nSET\r\n$4\r\nblk1\r\n$1\r\nv\r\n");
    pipeline_req.extend_from_slice(b"*1\r\n$4\r\nPING\r\n");
    pipeline_req.extend_from_slice(b"*3\r\n$5\r\nBLPOP\r\n$9\r\nempty_blk\r\n$3\r\n0.2\r\n");
    stream.write_all(&pipeline_req).await.unwrap();

    let mut parser = RespParser::new();
    let mut responses = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(150);
    while responses.len() < 2 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut chunk = vec![0u8; 1024];
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => {
                parser.feed(&chunk[..n]);
                while let Ok(Some(resp)) = parser.parse() {
                    responses.push(resp);
                }
            }
            _ => break,
        }
    }

    assert!(
        responses.len() >= 2,
        "阻塞前应已写出 SET/PING 响应, 实际只收到 {}",
        responses.len()
    );
    assert_eq!(
        responses[0],
        aikv::protocol::RespValue::SimpleString("OK".into())
    );
    assert_eq!(
        responses[1],
        aikv::protocol::RespValue::SimpleString("PONG".into())
    );

    // 收完 BLPOP 超时 nil, 避免污染后续测试连接
    while responses.len() < 3 {
        let mut chunk = vec![0u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read(&mut chunk))
            .await
            .expect("BLPOP 超时响应")
            .unwrap();
        if n == 0 {
            break;
        }
        parser.feed(&chunk[..n]);
        while let Ok(Some(resp)) = parser.parse() {
            responses.push(resp);
        }
    }
    assert_eq!(responses.len(), 3);
}

/// MONITOR 内 QUIT 必须先写出 +OK 再断连 (write_buf 聚合路径回归)
#[tokio::test]
async fn test_tcp_monitor_quit_flushes_ok() {
    let (addr, _handle) = start_server().await;
    let mut monitor = connect(addr).await;
    write_cmd(&mut monitor, &[b"MONITOR"]).await;
    assert_eq!(read_response(&mut monitor).await, b"+OK\r\n");

    write_cmd(&mut monitor, &[b"QUIT"]).await;
    assert_eq!(read_response(&mut monitor).await, b"+OK\r\n");
}

#[tokio::test]
async fn test_tcp_lpush_lrange() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"LPUSH", b"l", b"a", b"b"]).await;
    assert_eq!(read_response(&mut stream).await, b":2\r\n");
    write_cmd(&mut stream, &[b"LRANGE", b"l", b"0", b"-1"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"*2"));
}

#[tokio::test]
async fn test_tcp_set_smembers() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SADD", b"s", b"x"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"SMEMBERS", b"s"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"*1"));
}

#[tokio::test]
async fn test_tcp_zadd_zrange() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"ZADD", b"z", b"1", b"m"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"ZRANGE", b"z", b"0", b"-1"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"*1"));
}

#[tokio::test]
async fn test_tcp_rename() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"old", b"v"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"RENAME", b"old", b"new"]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    write_cmd(&mut stream, &[b"GET", b"new"]).await;
    assert_eq!(read_response(&mut stream).await, b"$1\r\nv\r\n");
}

#[tokio::test]
async fn test_tcp_keys_scan() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"SET", b"foo", b"1"]).await;
    read_response(&mut stream).await;
    write_cmd(&mut stream, &[b"KEYS", b"f*"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"*1"));
    write_cmd(&mut stream, &[b"SCAN", b"0"]).await;
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"*2"));
}

#[tokio::test]
async fn test_tcp_info_time() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"INFO", b"server"]).await;
    let info = read_response(&mut stream).await;
    let text = String::from_utf8_lossy(&info);
    assert!(text.contains("redis_version"));
    write_cmd(&mut stream, &[b"TIME"]).await;
    let time_resp = read_response(&mut stream).await;
    assert!(time_resp.starts_with(b"*2"));
}

#[tokio::test]
async fn test_tcp_json_set_get() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    let doc = r#"{"name":"John","age":30}"#;
    write_cmd(&mut stream, &[b"JSON.SET", b"jkey", b"$", doc.as_bytes()]).await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    write_cmd(&mut stream, &[b"JSON.GET", b"jkey", b"$"]).await;
    let resp = read_response(&mut stream).await;
    let text = String::from_utf8_lossy(&resp);
    assert!(text.contains("John"));
    assert!(text.contains("30"));
}

#[tokio::test]
async fn test_tcp_json_expire() {
    use tokio::time::{sleep, Duration};

    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(
        &mut stream,
        &[b"JSON.SET", b"jexp", b"$", br#"{"v":1}"#, b"1"],
    )
    .await;
    assert_eq!(read_response(&mut stream).await, b"+OK\r\n");
    sleep(Duration::from_secs(2)).await;
    write_cmd(&mut stream, &[b"JSON.GET", b"jexp", b"$"]).await;
    let resp = read_response(&mut stream).await;
    // 裸 TCP 未发送 HELLO,协议未协商,null 仍为 RESP2 格式 `$-1\r\n`
    assert_eq!(resp, b"$-1\r\n");
}

#[tokio::test]
async fn test_tcp_eval_basic() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    write_cmd(&mut stream, &[b"EVAL", b"return 1+1", b"0"]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b":2\r\n");
}

/// 非数组内联 PING\r\n 必须被拒绝 (非 +PONG), 且连接仍可发数组 PING.
#[tokio::test]
async fn test_tcp_rejects_inline_ping() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    stream.write_all(b"PING\r\n").await.unwrap();
    let resp = read_response(&mut stream).await;
    assert!(resp.starts_with(b"-"), "expected error, got {resp:?}");
    assert!(
        !resp.windows(5).any(|w| w == b"+PONG"),
        "telnet inline PING must not succeed"
    );
    let body = String::from_utf8_lossy(&resp);
    assert!(
        body.to_ascii_lowercase().contains("unknown type marker"),
        "expected unknown type marker, got: {body}"
    );
    write_cmd(&mut stream, &[b"PING"]).await;
    let pong = read_response(&mut stream).await;
    assert_eq!(pong, b"+PONG\r\n");
}

/// 畸形长度类输入 ($-2\r\n) 是不可恢复协议错误, 服务端必须断连,
/// 不能对同一 buffer 死循环写错误 (ISSUE-026).
#[tokio::test]
async fn test_tcp_fatal_protocol_error_closes_connection() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;
    stream.write_all(b"$-2\r\n").await.unwrap();
    // 修复前: 服务端对同一 buffer 无限写错误; 修复后: 断连, 收到 EOF.
    let resp = read_all_available(&mut stream).await;
    assert!(
        resp.len() < 1024,
        "不可恢复协议错误应关闭连接而非堆积错误, got {} bytes",
        resp.len()
    );
}

/// Pipeline 混合命令 (成功与错误交替) 返回必须严格保持 1:1 顺序
#[tokio::test]
async fn test_tcp_pipeline_mixed_success_and_errors_order() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;

    // 一次性写入: SET k v -> UNKNOWN_CMD -> GET k -> UNKNOWN2 -> PING
    let mut pipeline_req = Vec::new();
    pipeline_req.extend_from_slice(b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$5\r\nhello\r\n");
    pipeline_req.extend_from_slice(b"*1\r\n$11\r\nUNKNOWN_CMD\r\n");
    pipeline_req.extend_from_slice(b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n");
    pipeline_req.extend_from_slice(b"*1\r\n$8\r\nUNKNOWN2\r\n");
    pipeline_req.extend_from_slice(b"*1\r\n$4\r\nPING\r\n");

    stream.write_all(&pipeline_req).await.unwrap();

    let mut parser = RespParser::new();
    let mut responses = Vec::new();

    while responses.len() < 5 {
        let mut chunk = vec![0u8; 1024];
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        parser.feed(&chunk[..n]);
        while let Ok(Some(resp)) = parser.parse() {
            responses.push(resp);
        }
    }

    assert_eq!(responses.len(), 5);
    assert_eq!(
        responses[0],
        aikv::protocol::RespValue::SimpleString("OK".into())
    );
    assert!(
        matches!(&responses[1], aikv::protocol::RespValue::Error(msg) if msg.contains("ERR unknown command"))
    );
    assert_eq!(
        responses[2],
        aikv::protocol::RespValue::BulkString(Some(bytes::Bytes::from_static(b"hello")))
    );
    assert!(
        matches!(&responses[3], aikv::protocol::RespValue::Error(msg) if msg.contains("ERR unknown command"))
    );
    assert_eq!(
        responses[4],
        aikv::protocol::RespValue::SimpleString("PONG".into())
    );
}

/// 大 Payload (>100 KiB) 响应写出后连接仍可处理后续小命令 (覆盖大写路径)
#[tokio::test]
async fn test_tcp_large_payload_then_small_command() {
    let (addr, _handle) = start_server().await;
    let mut stream = connect(addr).await;

    let large_val = vec![b'x'; 128 * 1024]; // 128 KiB > MAX_WRITE_BUF_RETAIN_CAPACITY (64 KiB)
    write_cmd(&mut stream, &[b"SET", b"large_key", &large_val]).await;
    let resp = read_response(&mut stream).await;
    assert_eq!(resp, b"+OK\r\n");

    write_cmd(&mut stream, &[b"GET", b"large_key"]).await;
    let mut parser = RespParser::new();
    let value = loop {
        let mut chunk = vec![0u8; 16384];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "GET large_key 连接在读完完整 RESP 帧前关闭");
        parser.feed(&chunk[..n]);
        if let Ok(Some(v)) = parser.parse() {
            break v;
        }
    };
    match value {
        aikv::protocol::RespValue::BulkString(Some(b)) => {
            assert_eq!(b.len(), large_val.len());
            assert!(b.iter().all(|&c| c == b'x'));
        }
        other => panic!("期望 BulkString, 得到 {other:?}"),
    }

    // 完整读完大帧后再发小命令, 验证连接仍可用
    write_cmd(&mut stream, &[b"PING"]).await;
    let pong = read_response(&mut stream).await;
    assert_eq!(pong, b"+PONG\r\n");
}
