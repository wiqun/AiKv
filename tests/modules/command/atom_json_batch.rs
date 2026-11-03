//! @component aikv-command

use std::net::SocketAddr;
use std::time::Duration;

use aikv::protocol::{RespParser, RespValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;

use super::helpers::start_ephemeral_server;

fn parse_response(data: &[u8]) -> RespValue {
    let mut parser = RespParser::new();
    parser.feed(data);
    parser
        .parse()
        .expect("parse error")
        .expect("incomplete response")
}

struct TestConn {
    stream: TcpStream,
}

impl TestConn {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = time::timeout(Duration::from_secs(3), TcpStream::connect(addr))
            .await
            .expect("connect timeout")
            .expect("connect failed");
        Self { stream }
    }

    async fn send(&mut self, cmd: &str, args: &[&str]) -> Vec<u8> {
        let mut frame = format!("*{}\r\n", 1 + args.len());
        frame.push_str(&format!("${}\r\n{}\r\n", cmd.len(), cmd));
        for arg in args {
            frame.push_str(&format!("${}\r\n{}\r\n", arg.len(), arg));
        }
        self.stream
            .write_all(frame.as_bytes())
            .await
            .expect("write failed");

        let mut buf = vec![0u8; 8192];
        let n = time::timeout(Duration::from_secs(3), self.stream.read(&mut buf))
            .await
            .expect("read timeout")
            .expect("read failed");
        buf.truncate(n);
        buf
    }
}

async fn key_exists(conn: &mut TestConn, key: &str) -> bool {
    let resp = conn.send("EXISTS", &[key]).await;
    matches!(parse_response(&resp), RespValue::Integer(1))
}

#[tokio::test]
async fn test_atom_exec_json_single_json_set() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let batch = r#"[["JSON.SET","jb1","$","{\"a\":1}"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::SimpleString("OK".into()));
        }
        other => panic!("expected Array, got {:?}", other),
    }

    let resp = conn.send("JSON.GET", &["jb1", "$"]).await;
    let RespValue::BulkString(Some(data)) = parse_response(&resp) else {
        panic!("expected JSON document");
    };
    assert!(data.windows(3).any(|w| w == b"\"a\""));
}

#[tokio::test]
async fn test_atom_exec_json_rollback_new_key() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let batch = r#"[["JSON.SET","jb_new","$","{\"a\":1}"],["NOSUCHCMD","jb_new"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(msg) => assert!(msg.contains("unknown command")),
        other => panic!("expected Error, got {:?}", other),
    }
    assert!(!key_exists(&mut conn, "jb_new").await);
}

#[tokio::test]
async fn test_atom_exec_json_rollback_overwrite() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let seed = r#"[["JSON.SET","jb_old","$","{\"a\":1}"]]"#;
    let _ = conn.send("ATOM.EXEC", &[seed]).await;

    let batch = r#"[["JSON.SET","jb_old","$","{\"b\":2}"],["NOSUCHCMD","x"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(_) => {}
        other => panic!("expected Error, got {:?}", other),
    }

    let resp = conn.send("JSON.GET", &["jb_old", "$"]).await;
    let RespValue::BulkString(Some(data)) = parse_response(&resp) else {
        panic!("expected JSON document");
    };
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("\"a\""));
    assert!(!text.contains("\"b\""));
}

#[tokio::test]
async fn test_atom_exec_json_update_where_fail_rollback() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let seed = r#"[["JSON.SET","jb_upd","$","{\"items\":[{\"v\":1}],\"tag\":\"orig\"}"]]"#;
    let _ = conn.send("ATOM.EXEC", &[seed]).await;

    let batch = r#"[["JSON.SET","jb_upd2","$","{\"x\":1}"],["JSON.UPDATE","jb_upd","$.items[?(@.v == 99)]","$.tag","\"bad\"",""]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(msg) => assert!(msg.contains("No elements match")),
        other => panic!("expected Error, got {:?}", other),
    }
    assert!(!key_exists(&mut conn, "jb_upd2").await);

    let resp = conn.send("JSON.GET", &["jb_upd", "$"]).await;
    let RespValue::BulkString(Some(data)) = parse_response(&resp) else {
        panic!("expected JSON document");
    };
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("orig") || text.contains("\"tag\":\"orig\""));
}

#[tokio::test]
async fn test_atom_exec_json_set_xe_conflict() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let seed = r#"[["JSON.SET","jb_xe","$","{\"a\":1}"]]"#;
    let _ = conn.send("ATOM.EXEC", &[seed]).await;

    let batch =
        r#"[["JSON.SET","jb_xe2","$","{\"z\":1}"],["JSON.SET","jb_xe","$","{\"b\":2}","0","XE"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(msg) => assert!(msg.contains("XE")),
        other => panic!("expected Error, got {:?}", other),
    }
    assert!(!key_exists(&mut conn, "jb_xe2").await);
}

#[tokio::test]
async fn test_atom_exec_json_mset_partial_fail() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let batch =
        r#"[["JSON.MSET","jb_m1","$","{\"a\":1}","jb_m2","$","{\"a\":2}"],["NOSUCHCMD","x"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(_) => {}
        other => panic!("expected Error, got {:?}", other),
    }
    assert!(!key_exists(&mut conn, "jb_m1").await);
    assert!(!key_exists(&mut conn, "jb_m2").await);
}

#[tokio::test]
async fn test_atom_exec_json_update_nn_noop() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let seed = r#"[["JSON.SET","jb_nn","$","{\"items\":[{\"v\":1}],\"tag\":\"keep\"}"]]"#;
    let _ = conn.send("ATOM.EXEC", &[seed]).await;

    let batch = r#"[["JSON.UPDATE","jb_nn","$.items[?(@.v == 99)]","$.tag","\"bad\"","NN"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::SimpleString("OK".into()));
        }
        other => panic!("expected Array, got {:?}", other),
    }

    let resp = conn.send("JSON.GET", &["jb_nn", "$.tag"]).await;
    let RespValue::BulkString(Some(data)) = parse_response(&resp) else {
        panic!("expected value");
    };
    let tag = String::from_utf8_lossy(&data);
    assert!(tag == "keep" || tag == "\"keep\"");
}

#[tokio::test]
async fn test_atom_exec_json_duplicate_key() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let batch = r#"[["JSON.SET","jb_dup","$","{\"a\":1}"],["JSON.SET","jb_dup","$","{\"a\":2}"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(msg) => assert!(msg.contains("duplicate key")),
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_exec_json_alias_same_as_atom_exec() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let batch = r#"[["JSON.SET","jb_alias","$","{\"v\":1}"]]"#;
    let resp = conn.send("EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::SimpleString("OK".into()));
        }
        other => panic!("expected Array, got {:?}", other),
    }
}

/// 失败 batch 在从未成功写入时不应 RESTORE 旧 snapshot, 以免覆盖并发事务的提交.
#[tokio::test]
async fn test_atom_exec_json_failed_rollback_does_not_clobber_concurrent_write() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut setup = TestConn::connect(addr).await;
    let seed = r#"[["JSON.SET","jb_race","$","{\"v\":\"orig\"}"]]"#;
    let _ = setup.send("ATOM.EXEC", &[seed]).await;

    for _ in 0..32 {
        let reset = r#"[["JSON.SET","jb_race","$","{\"v\":\"orig\"}"]]"#;
        let _ = setup.send("ATOM.EXEC", &[reset]).await;

        let winner = tokio::spawn(async move {
            let mut conn = TestConn::connect(addr).await;
            let batch = r#"[["JSON.SET","jb_race","$","{\"v\":\"winner\"}"]]"#;
            conn.send("ATOM.EXEC", &[batch]).await
        });

        let loser = tokio::spawn(async move {
            let mut conn = TestConn::connect(addr).await;
            let batch =
                r#"[["JSON.UPDATE","jb_race","$.x","$.v","\"stale\"","",""],["NOSUCHCMD","x"]]"#;
            conn.send("ATOM.EXEC", &[batch]).await
        });

        let (winner_resp, loser_resp) = tokio::join!(winner, loser);
        let _ = winner_resp.expect("winner task");
        let _ = loser_resp.expect("loser task");

        let resp = setup.send("JSON.GET", &["jb_race", "$.v"]).await;
        let RespValue::BulkString(Some(data)) = parse_response(&resp) else {
            panic!("expected value");
        };
        let value = String::from_utf8_lossy(&data);
        assert!(
            value.contains("winner"),
            "concurrent winner write must not be rolled back to orig, got {value}"
        );
    }
}

#[tokio::test]
async fn test_atom_exec_json_inside_multi_rejected() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let _ = conn.send("MULTI", &[]).await;
    let batch = r#"[["JSON.SET","jb_multi","$","{}"]]"#;
    let resp = conn.send("ATOM.EXEC", &[batch]).await;
    match parse_response(&resp) {
        RespValue::Error(msg) => assert!(msg.contains("EXEC inside MULTI")),
        other => panic!("expected Error, got {:?}", other),
    }
}
