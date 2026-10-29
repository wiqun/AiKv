//! 集群模式流水线并发分流与保序写回测试
//! @component aikv-server

use std::sync::Arc;
use bytes::Bytes;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use aikv::protocol::{RespParser, RespValue};
use aikv::server::{Connection, ConnectionConfig, ServerSharedState};
use aikv::storage::{KvStorage, MemoryEngine};

fn create_test_state() -> Arc<ServerSharedState> {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    ServerSharedState::new(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
            script_worker_threads: 2,
        },
        storage,
        0,
    )
}

async fn create_connected_tcp_pair() -> (TcpStream, TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let client = TcpStream::connect(addr).await.expect("connect client");
    let (server, _) = listener.accept().await.expect("accept server");
    (client, server)
}

#[tokio::test]
async fn test_cluster_concurrent_pipeline_in_order_delivery() {
    let (client_stream, server_stream) = create_connected_tcp_pair().await;
    let remote = "127.0.0.1:6379".parse().unwrap();
    let state = create_test_state();
    let mut conn = Connection::new_for_test(server_stream, remote, state);

    // 构造 100 条流水线命令: 50 条 SET + 50 条 GET
    let mut commands = Vec::with_capacity(100);
    for i in 0..50 {
        commands.push((
            "SET".to_string(),
            vec![
                Bytes::from(format!("k_{i:03}")),
                Bytes::from(format!("v_{i:03}")),
            ],
        ));
    }
    for i in 0..50 {
        commands.push((
            "GET".to_string(),
            vec![Bytes::from(format!("k_{i:03}"))],
        ));
    }

    // 执行集群并发流水线分发
    conn.execute_cluster_concurrent_pipeline(commands)
        .await
        .expect("pipeline execution failed");

    // 从客户端读取所有响应并解析
    let mut reader = client_stream;
    let mut parser = RespParser::new();
    let mut responses = Vec::new();
    let mut buf = vec![0u8; 4096];

    while responses.len() < 100 {
        let n = reader.read(&mut buf).await.expect("read from client socket");
        if n == 0 {
            break;
        }
        parser.feed(&buf[..n]);
        while let Ok(Some(resp)) = parser.parse() {
            responses.push(resp);
        }
    }

    assert_eq!(responses.len(), 100, "必须接收到完整的 100 条响应");

    // 验证前 50 条 SET 响应严格全为 +OK
    for (i, resp) in responses.iter().take(50).enumerate() {
        match resp {
            RespValue::SimpleString(s) => assert_eq!(s, "OK", "第 {i} 条 SET 响应必须为 OK"),
            other => panic!("第 {i} 条 SET 响应预期 SimpleString(OK), 实际为: {other:?}"),
        }
    }

    // 验证后 50 条 GET 响应严格按照原始下标顺序返回对应 value
    for i in 0..50 {
        let expected_val = format!("v_{i:03}");
        match &responses[50 + i] {
            RespValue::BulkString(Some(bytes)) => {
                assert_eq!(
                    bytes,
                    expected_val.as_bytes(),
                    "第 {} 条 GET 响应顺序或内容不匹配",
                    50 + i
                );
            }
            other => panic!(
                "第 {} 条 GET 响应预期 BulkString({expected_val}), 实际为: {other:?}",
                50 + i
            ),
        }
    }
}

#[tokio::test]
async fn test_cluster_concurrent_pipeline_mixed_success_and_errors_order() {
    let (client_stream, server_stream) = create_connected_tcp_pair().await;
    let remote = "127.0.0.1:6379".parse().unwrap();
    let state = create_test_state();
    let mut conn = Connection::new_for_test(server_stream, remote, state);

    // 构造包含正常写入、错误命令、点查的混合批次:
    // 0: SET a val_a
    // 1: UNKNOWN_CMD_XYZ
    // 2: SET b val_b
    // 3: GET a
    // 4: GET non_existent_key
    // 5: GET b
    let commands = vec![
        ("SET".to_string(), vec![Bytes::from("a"), Bytes::from("val_a")]),
        ("UNKNOWN_CMD_XYZ".to_string(), vec![]),
        ("SET".to_string(), vec![Bytes::from("b"), Bytes::from("val_b")]),
        ("GET".to_string(), vec![Bytes::from("a")]),
        ("GET".to_string(), vec![Bytes::from("non_existent_key")]),
        ("GET".to_string(), vec![Bytes::from("b")]),
    ];

    conn.execute_cluster_concurrent_pipeline(commands)
        .await
        .expect("pipeline execution failed");

    let mut reader = client_stream;
    let mut parser = RespParser::new();
    let mut responses = Vec::new();
    let mut buf = vec![0u8; 1024];

    while responses.len() < 6 {
        let n = reader.read(&mut buf).await.expect("read from client socket");
        if n == 0 {
            break;
        }
        parser.feed(&buf[..n]);
        while let Ok(Some(resp)) = parser.parse() {
            responses.push(resp);
        }
    }

    assert_eq!(responses.len(), 6);
    assert_eq!(responses[0], RespValue::SimpleString("OK".to_string()));
    match &responses[1] {
        RespValue::Error(msg) => assert!(msg.contains("unknown command"), "预期未知命令错误: {msg}"),
        other => panic!("预期未知命令错误, 实际为: {other:?}"),
    }
    assert_eq!(responses[2], RespValue::SimpleString("OK".to_string()));
    assert_eq!(responses[3], RespValue::BulkString(Some(Bytes::from("val_a"))));
    assert_eq!(responses[4], RespValue::BulkString(None));
    assert_eq!(responses[5], RespValue::BulkString(Some(Bytes::from("val_b"))));
}
