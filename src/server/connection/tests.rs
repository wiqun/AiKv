use super::*;

#[test]
fn hello_map_uses_bulkstring_for_all_values() {
    // 对齐 Redis 7 原生: RESP3 Map 中 proto/id 是 Integer, 其余是 BulkString.

    // 测试 RESP3 Map 响应
    let map = hello_map(42, ProtocolVersion::Resp3);
    let RespValue::Map(pairs) = map else {
        panic!("RESP3 HELLO 应返回 Map");
    };
    for (key, value) in &pairs {
        assert!(
            matches!(key, RespValue::BulkString(Some(_))),
            "Map key 必须是 BulkString，得到: {key:?}"
        );
        let RespValue::BulkString(Some(key_bytes)) = key else {
            unreachable!()
        };
        let key_str = String::from_utf8_lossy(key_bytes);
        match key_str.as_ref() {
            "proto" | "id" => {
                assert!(
                    matches!(value, RespValue::Integer(_)),
                    "字段 '{key_str}' 必须是 Integer (Redis 7 兼容), 得到: {value:?}"
                );
            }
            _ => {
                assert!(
                    matches!(value, RespValue::BulkString(Some(_))),
                    "字段 '{key_str}' 必须是 BulkString, 得到: {value:?}"
                );
            }
        }
    }

    // 测试 RESP2 Array 响应: 全部 BulkString (RESP2 无原生 Integer 语义)
    let arr = hello_map(42, ProtocolVersion::Resp2);
    let RespValue::Array(Some(items)) = arr else {
        panic!("RESP2 HELLO 应返回 Array");
    };
    assert_eq!(items.len() % 2, 0, "Array 项数应为偶数 (交替 key-value)");
    for item in &items {
        assert!(
            matches!(item, RespValue::BulkString(Some(_))),
            "RESP2 Array 中的每一项必须是 BulkString，得到: {item:?}"
        );
    }
}

#[tokio::test]
async fn hello_3_rejected_when_resp3_disabled() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    let storage = crate::storage::MemoryEngine::new(16);
    let cfg = crate::server::ConnectionConfig {
        resp3_enabled: false,
        ..Default::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = ServerSharedState::new(cfg, storage, addr.port());

    let handle = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        let _ = Connection::handle(stream, peer, state).await;
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"*2\r\n$5\r\nHELLO\r\n$1\r\n3\r\n")
        .await
        .unwrap();
    client.shutdown().await.unwrap();

    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.unwrap();
    let out = String::from_utf8_lossy(&buf);
    assert!(out.contains("ERR RESP3 is disabled"), "got: {out}");
    let _ = handle.await;
}
