//! 流水线按序执行与保序写回测试
//! @component aikv-server

use bytes::Bytes;
use std::sync::Arc;
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let client = TcpStream::connect(addr).await.expect("connect client");
    let (server, _) = listener.accept().await.expect("accept server");
    (client, server)
}

#[tokio::test]
async fn test_pipeline_in_order_delivery() {
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
        commands.push(("GET".to_string(), vec![Bytes::from(format!("k_{i:03}"))]));
    }

    // 执行流水线
    conn.execute_pipeline(commands)
        .await
        .expect("pipeline execution failed");

    // 从客户端读取所有响应并解析
    let mut reader = client_stream;
    let mut parser = RespParser::new();
    let mut responses = Vec::new();
    let mut buf = vec![0u8; 4096];

    while responses.len() < 100 {
        let n = reader
            .read(&mut buf)
            .await
            .expect("read from client socket");
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
async fn test_pipeline_mixed_success_and_errors_order() {
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
        (
            "SET".to_string(),
            vec![Bytes::from("a"), Bytes::from("val_a")],
        ),
        ("UNKNOWN_CMD_XYZ".to_string(), vec![]),
        (
            "SET".to_string(),
            vec![Bytes::from("b"), Bytes::from("val_b")],
        ),
        ("GET".to_string(), vec![Bytes::from("a")]),
        ("GET".to_string(), vec![Bytes::from("non_existent_key")]),
        ("GET".to_string(), vec![Bytes::from("b")]),
    ];

    conn.execute_pipeline(commands)
        .await
        .expect("pipeline execution failed");

    let mut reader = client_stream;
    let mut parser = RespParser::new();
    let mut responses = Vec::new();
    let mut buf = vec![0u8; 1024];

    while responses.len() < 6 {
        let n = reader
            .read(&mut buf)
            .await
            .expect("read from client socket");
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
        RespValue::Error(msg) => {
            assert!(msg.contains("unknown command"), "预期未知命令错误: {msg}")
        }
        other => panic!("预期未知命令错误, 实际为: {other:?}"),
    }
    assert_eq!(responses[2], RespValue::SimpleString("OK".to_string()));
    assert_eq!(
        responses[3],
        RespValue::BulkString(Some(Bytes::from("val_a")))
    );
    assert_eq!(responses[4], RespValue::BulkString(None));
    assert_eq!(
        responses[5],
        RespValue::BulkString(Some(Bytes::from("val_b")))
    );
}

async fn read_responses(client: &mut TcpStream, want: usize) -> Vec<RespValue> {
    let mut parser = RespParser::new();
    let mut responses = Vec::with_capacity(want);
    let mut buf = vec![0u8; 8192];
    while responses.len() < want {
        let n = client
            .read(&mut buf)
            .await
            .expect("read from client socket");
        if n == 0 {
            break;
        }
        parser.feed(&buf[..n]);
        while let Ok(Some(resp)) = parser.parse() {
            responses.push(resp);
        }
    }
    responses
}

/// 回归 (集群流水线不按序): 同一连接流水线 `SET k v_i` 紧跟 `GET k`, GET 必须读到紧邻前一条 SET 的值,
/// 且同一 key 连续 SET 的最终值必须是最后一条. 旧实现为每条命令 `spawn` 并发执行, 线上实测
/// `SET k new` + `GET k` 3000/3000 次读到旧值, 连续 `SET k 0..19` 300/300 次最终值不是最后一条.
/// 期望: 流水线内命令按到达顺序生效.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_cluster_pipeline_preserves_execution_order() {
    const ROUNDS: usize = 150;
    const PAIRS: usize = 8;
    let state = create_test_state();
    let remote = "127.0.0.1:6379".parse().unwrap();

    for round in 0..ROUNDS {
        let (mut client, server) = create_connected_tcp_pair().await;
        let mut conn = Connection::new_for_test(server, remote, state.clone());
        let key = format!("order_{round}");

        // SET v0, GET, SET v1, GET, ..., 最后再连续 3 次写同一个 key, 再 GET.
        let mut commands = Vec::new();
        for j in 0..PAIRS {
            commands.push((
                "SET".to_string(),
                vec![Bytes::from(key.clone()), Bytes::from(format!("v{j}"))],
            ));
            commands.push(("GET".to_string(), vec![Bytes::from(key.clone())]));
        }
        for tail in ["a", "b", "c"] {
            commands.push((
                "SET".to_string(),
                vec![Bytes::from(key.clone()), Bytes::from(tail)],
            ));
        }
        commands.push(("GET".to_string(), vec![Bytes::from(key.clone())]));
        let total = commands.len();

        conn.execute_pipeline(commands).await.expect("pipeline");
        let responses = read_responses(&mut client, total).await;
        assert_eq!(responses.len(), total);

        for j in 0..PAIRS {
            assert_eq!(
                responses[2 * j + 1],
                RespValue::BulkString(Some(Bytes::from(format!("v{j}")))),
                "round {round}: 第 {j} 个 GET 必须读到紧邻前一条 SET 的值"
            );
        }
        assert_eq!(
            responses[total - 1],
            RespValue::BulkString(Some(Bytes::from("c"))),
            "round {round}: 连续 SET 的最终值必须是最后一条"
        );
    }
}
