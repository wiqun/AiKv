//! Atom 原子类型扩展命令族测试 (ATOM.SWAP / ATOM.CAS / ATOM.ADD 等)
//! @component aikv-command

use std::net::SocketAddr;
use std::time::Duration;

use aikv::protocol::{RespParser, RespValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;

use super::helpers::start_ephemeral_server;

/// 解析 RESP 响应字节为 RespValue
fn parse_response(data: &[u8]) -> RespValue {
    let mut parser = RespParser::new();
    parser.feed(data);
    parser
        .parse()
        .expect("parse error")
        .expect("incomplete response")
}

/// 在同一个连接上发送多个命令,返回所有响应字节
struct TestConn {
    stream: TcpStream,
}

impl TestConn {
    async fn connect(addr: SocketAddr) -> Self {
        let stream = time::timeout(Duration::from_secs(3), TcpStream::connect(addr))
            .await
            .expect("connect timeout")
            .expect("connect failed");
        let mut this = Self { stream };
        // 裸 TCP 测试未协商协议版本,显式切换到 RESP2 以保持测试断言兼容
        let hello_resp = this.send("HELLO", &["2"]).await;
        assert!(
            hello_resp.starts_with(b"*"),
            "HELLO 2 应返回 RESP2 flat array"
        );
        this
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

        let mut buf = vec![0u8; 4096];
        let n = time::timeout(Duration::from_secs(3), self.stream.read(&mut buf))
            .await
            .expect("read timeout")
            .expect("read failed");
        buf.truncate(n);
        buf
    }
}

async fn wait_for_blocked_client(addr: SocketAddr) {
    let mut conn = TestConn::connect(addr).await;
    let mut last_response = Vec::new();
    time::timeout(Duration::from_secs(3), async {
        loop {
            let resp = conn.send("INFO", &["clients"]).await;
            if resp
                .windows(b"blocked_clients:1".len())
                .any(|window| window == b"blocked_clients:1")
            {
                break;
            }
            last_response = resp;
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "wait for blocked client timeout; last INFO response: {}",
            String::from_utf8_lossy(&last_response)
        )
    });
}

// ─── 测试 ─────────────────────────────────────────────────────

/// Issue #78: EXEC 中的运行时错误不得 rollback, 后续命令仍须按顺序执行.
#[tokio::test]
async fn issue_78_multi_exec_runtime_error_does_not_stop_following_commands() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    assert_eq!(
        parse_response(&conn.send("SET", &["runtime-key", "string"]).await),
        RespValue::SimpleString("OK".into())
    );
    let _ = conn.send("MULTI", &[]).await;
    let _ = conn.send("LPOP", &["runtime-key"]).await;
    let _ = conn.send("SET", &["after-error", "ok"]).await;

    let response = parse_response(&conn.send("EXEC", &[]).await);
    let RespValue::Array(Some(items)) = response else {
        panic!("expected EXEC array response");
    };
    assert_eq!(items.len(), 2);
    assert!(matches!(
        &items[0],
        RespValue::Error(message) if message.contains("WRONGTYPE")
    ));
    assert_eq!(items[1], RespValue::SimpleString("OK".into()));
    assert_eq!(
        parse_response(&conn.send("GET", &["after-error"]).await),
        RespValue::BulkString(Some("ok".into()))
    );
}

/// Issue #78: MULTI/EXEC 必须整笔拒绝 blocking 命令, 且不得挂住后续请求.
#[tokio::test]
async fn issue_78_multi_exec_rejects_blocking_commands_without_hanging_server() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let _ = conn.send("MULTI", &[]).await;
    let _ = conn.send("BLPOP", &["never-ready", "0"]).await;
    let response = time::timeout(Duration::from_secs(3), conn.send("EXEC", &[]))
        .await
        .expect("EXEC blocked");
    assert!(matches!(
        parse_response(&response),
        RespValue::Error(message) if message.contains("blocking commands")
    ));
    assert_eq!(
        parse_response(&conn.send("PING", &[]).await),
        RespValue::SimpleString("PONG".into())
    );
}

/// Issue #78: 已注册的 BLPOP waiter 不得插入 MULTI/EXEC 的命令序列.
#[tokio::test]
async fn issue_78_blocking_waiter_cannot_interleave_multi_exec() {
    let (addr, _server) = start_ephemeral_server().await;
    let waiter = tokio::spawn(async move {
        let mut conn = TestConn::connect(addr).await;
        parse_response(&conn.send("BLPOP", &["tx-list", "2"]).await)
    });
    wait_for_blocked_client(addr).await;

    let mut conn = TestConn::connect(addr).await;
    let _ = conn.send("MULTI", &[]).await;
    let _ = conn.send("LPUSH", &["tx-list", "value"]).await;
    let _ = conn.send("LLEN", &["tx-list"]).await;
    assert_eq!(
        parse_response(&conn.send("EXEC", &[]).await),
        RespValue::Array(Some(vec![RespValue::Integer(1), RespValue::Integer(1)]))
    );
    assert_eq!(
        waiter.await.expect("BLPOP task failed"),
        RespValue::Array(Some(vec![
            RespValue::BulkString(Some("tx-list".into())),
            RespValue::BulkString(Some("value".into())),
        ]))
    );
}

/// Issue #78: 已注册的 BLMOVE waiter 不得插入 MULTI/EXEC 的命令序列.
#[tokio::test]
async fn issue_78_blmove_waiter_cannot_interleave_multi_exec() {
    let (addr, _server) = start_ephemeral_server().await;
    let waiter = tokio::spawn(async move {
        let mut conn = TestConn::connect(addr).await;
        parse_response(
            &conn
                .send("BLMOVE", &["source", "dest", "LEFT", "RIGHT", "2"])
                .await,
        )
    });
    wait_for_blocked_client(addr).await;

    let mut conn = TestConn::connect(addr).await;
    let _ = conn.send("MULTI", &[]).await;
    let _ = conn.send("LPUSH", &["source", "value"]).await;
    let _ = conn.send("LLEN", &["source"]).await;
    assert_eq!(
        parse_response(&conn.send("EXEC", &[]).await),
        RespValue::Array(Some(vec![RespValue::Integer(1), RespValue::Integer(1)]))
    );
    assert_eq!(
        waiter.await.expect("BLMOVE task failed"),
        RespValue::BulkString(Some("value".into()))
    );
}

/// Issue #78: 已注册的 BZPOPMIN waiter 不得插入 MULTI/EXEC 的命令序列.
#[tokio::test]
async fn issue_78_bzpop_waiter_cannot_interleave_multi_exec() {
    let (addr, _server) = start_ephemeral_server().await;
    let waiter = tokio::spawn(async move {
        let mut conn = TestConn::connect(addr).await;
        parse_response(&conn.send("BZPOPMIN", &["z", "2"]).await)
    });
    wait_for_blocked_client(addr).await;

    let mut conn = TestConn::connect(addr).await;
    let _ = conn.send("MULTI", &[]).await;
    let _ = conn.send("ZADD", &["z", "1", "member"]).await;
    let _ = conn.send("ZCARD", &["z"]).await;
    assert_eq!(
        parse_response(&conn.send("EXEC", &[]).await),
        RespValue::Array(Some(vec![RespValue::Integer(1), RespValue::Integer(1)]))
    );
    assert_eq!(
        waiter.await.expect("BZPOPMIN task failed"),
        RespValue::Array(Some(vec![
            RespValue::BulkString(Some("z".into())),
            RespValue::BulkString(Some("member".into())),
            RespValue::BulkString(Some("1".into())),
        ]))
    );
}

#[tokio::test]
async fn test_standard_multi_exec_alias() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let resp = conn.send("MULTI", &[]).await;
    assert_eq!(parse_response(&resp), RespValue::SimpleString("OK".into()));

    let resp = conn.send("SET", &["sk", "1"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::SimpleString("QUEUED".into())
    );

    let resp = conn.send("EXEC", &[]).await;
    match parse_response(&resp) {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::SimpleString("OK".into()));
        }
        other => panic!("expected Array, got {:?}", other),
    }

    let resp = conn.send("GET", &["sk"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(Some("1".into()))
    );
}

#[tokio::test]
async fn test_standard_watch_conflict() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    let _ = conn1.send("SET", &["wk", "v"]).await;

    let resp = conn2.send("WATCH", &["wk"]).await;
    assert_eq!(parse_response(&resp), RespValue::SimpleString("OK".into()));

    let _ = conn1.send("SET", &["wk", "v2"]).await;

    let _ = conn2.send("MULTI", &[]).await;
    let _ = conn2.send("SET", &["wk", "v3"]).await;
    let resp = conn2.send("EXEC", &[]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(None),
        "WATCH conflict should return nil"
    );
}

#[tokio::test]
async fn test_atom_multi_exec_basic() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    // SET a 1, GET a, INCR a — in transaction
    let resp = conn.send("ATOM.MULTI", &[]).await;
    assert_eq!(parse_response(&resp), RespValue::SimpleString("OK".into()));

    let resp = conn.send("SET", &["a", "1"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::SimpleString("QUEUED".into())
    );

    let resp = conn.send("GET", &["a"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::SimpleString("QUEUED".into())
    );

    let resp = conn.send("INCR", &["a"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::SimpleString("QUEUED".into())
    );

    // EXEC
    let resp = conn.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    match parsed {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 3, "expected 3 results");
            assert_eq!(items[0], RespValue::SimpleString("OK".into()));
            assert_eq!(items[1], RespValue::BulkString(Some("1".into())));
            assert_eq!(items[2], RespValue::Integer(2));
        }
        other => panic!("expected Array, got {:?}", other),
    }

    // Verify the value is 2 (INCR 1 → 2)
    let resp = conn.send("GET", &["a"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(Some("2".into()))
    );
}

#[tokio::test]
async fn test_atom_discard() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    // MULTI → SET a 1 → DISCARD
    let _ = conn.send("ATOM.MULTI", &[]).await;
    let _ = conn.send("SET", &["a", "1"]).await;
    let resp = conn.send("ATOM.DISCARD", &[]).await;
    assert_eq!(parse_response(&resp), RespValue::SimpleString("OK".into()));

    // After DISCARD, SET a should not have been applied
    let resp = conn.send("GET", &["a"]).await;
    assert_eq!(parse_response(&resp), RespValue::BulkString(None));
}

#[tokio::test]
async fn test_atom_multi_nesting_error() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let _ = conn.send("ATOM.MULTI", &[]).await;
    let resp = conn.send("ATOM.MULTI", &[]).await;
    let parsed = parse_response(&resp);
    assert!(
        matches!(parsed, RespValue::Error(ref msg) if msg.contains("MULTI calls can not be nested")),
        "expected MULTI nested error, got {:?}",
        parsed
    );
}

#[tokio::test]
async fn test_atom_exec_empty() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let _ = conn.send("ATOM.MULTI", &[]).await;
    let resp = conn.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    assert_eq!(parsed, RespValue::Array(Some(vec![])));
}

#[tokio::test]
async fn test_atom_exec_without_multi() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let resp = conn.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    assert!(
        matches!(parsed, RespValue::Error(ref msg) if msg.contains("EXEC without MULTI")),
        "expected EXEC without MULTI error, got {:?}",
        parsed
    );
}

#[tokio::test]
async fn test_atom_discard_without_multi() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn = TestConn::connect(addr).await;

    let resp = conn.send("ATOM.DISCARD", &[]).await;
    let parsed = parse_response(&resp);
    assert!(
        matches!(parsed, RespValue::Error(ref msg) if msg.contains("DISCARD without MULTI")),
        "expected DISCARD without MULTI error, got {:?}",
        parsed
    );
}

#[tokio::test]
async fn test_atom_watch_conflict() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    // conn1: SET k v
    let _ = conn1.send("SET", &["k", "v"]).await;

    // conn2: WATCH k
    let resp = conn2.send("ATOM.WATCH", &["k"]).await;
    assert_eq!(parse_response(&resp), RespValue::SimpleString("OK".into()));

    // conn1: modify k (triggers version change)
    let _ = conn1.send("SET", &["k", "v2"]).await;

    // conn2: MULTI → SET k v3 → EXEC (should fail due to WATCH conflict)
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("SET", &["k", "v3"]).await;
    let resp = conn2.send("ATOM.EXEC", &[]).await;
    // Conflict → nil (transaction aborted)
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(None),
        "WATCH conflict should return nil"
    );

    // k should still be v2 (conn2's SET was not executed)
    let resp = conn1.send("GET", &["k"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(Some("v2".into()))
    );
}

#[tokio::test]
async fn test_atom_watch_no_conflict() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    // conn1: SET k v
    let _ = conn1.send("SET", &["k", "v"]).await;

    // conn2: WATCH k
    let _ = conn2.send("ATOM.WATCH", &["k"]).await;

    // conn2: MULTI → GET k → EXEC (no conflict, k not modified by others)
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("GET", &["k"]).await;
    let resp = conn2.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    match parsed {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::BulkString(Some("v".into())));
        }
        other => panic!("expected Array, got {:?}", other),
    }
}

#[tokio::test]
async fn test_atom_watch_auto_clear_after_exec() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    // conn1: SET k v
    let _ = conn1.send("SET", &["k", "v"]).await;

    // conn2: WATCH k → MULTI → GET k → EXEC (successful, clears watch)
    let _ = conn2.send("ATOM.WATCH", &["k"]).await;
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("GET", &["k"]).await;
    let _ = conn2.send("ATOM.EXEC", &[]).await;

    // conn1 modifies k (this should now NOT affect conn2's next EXEC)
    let _ = conn1.send("SET", &["k", "v3"]).await;

    // conn2: MULTI → GET k → EXEC (should succeed because WATCH was cleared)
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("GET", &["k"]).await;
    let resp = conn2.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    match parsed {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::BulkString(Some("v3".into())));
        }
        other => panic!("expected Array, got {:?}", other),
    }
}

#[tokio::test]
async fn test_atom_unwatch() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    // conn1: SET k v
    let _ = conn1.send("SET", &["k", "v"]).await;

    // conn2: WATCH k → UNWATCH
    let _ = conn2.send("ATOM.WATCH", &["k"]).await;
    let resp = conn2.send("ATOM.UNWATCH", &[]).await;
    assert_eq!(parse_response(&resp), RespValue::SimpleString("OK".into()));

    // conn1 modifies k
    let _ = conn1.send("SET", &["k", "v2"]).await;

    // conn2: MULTI → GET k → EXEC (should succeed because WATCH was removed)
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("GET", &["k"]).await;
    let resp = conn2.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    match parsed {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::BulkString(Some("v2".into())));
        }
        other => panic!("expected Array, got {:?}", other),
    }
}

#[tokio::test]
async fn test_atom_watch_multiple_keys() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    // conn2: WATCH k1 k2 k3
    let _ = conn2.send("ATOM.WATCH", &["k1", "k2", "k3"]).await;

    // conn1: modify k2 (one of the watched keys)
    let _ = conn1.send("SET", &["k2", "changed"]).await;

    // conn2: EXEC should fail
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("SET", &["k1", "x"]).await;
    let resp = conn2.send("ATOM.EXEC", &[]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(None),
        "WATCH on any key should abort transaction"
    );
}

#[tokio::test]
async fn test_atom_watch_discard_clears() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    // conn2: WATCH k
    let _ = conn2.send("ATOM.WATCH", &["k"]).await;
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("SET", &["k", "x"]).await;

    // DISCARD clears both queue AND watched keys
    let _ = conn2.send("ATOM.DISCARD", &[]).await;

    // conn1 modifies k
    let _ = conn1.send("SET", &["k", "v1"]).await;

    // conn2: new MULTI → GET k → EXEC (should succeed, watched keys cleared by DISCARD)
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("GET", &["k"]).await;
    let resp = conn2.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    match parsed {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], RespValue::BulkString(Some("v1".into())));
        }
        other => panic!("expected Array, got {:?}", other),
    }
}

/// Issue #79: 单库模式下 SELECT 为垫片兼容，不同连接共享单一命名空间，跨 SELECT 写入同名 key 触发 WATCH 冲突.
#[tokio::test]
async fn issue_79_watch_db_isolation_same_key_name() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut conn1 = TestConn::connect(addr).await;
    let mut conn2 = TestConn::connect(addr).await;

    let _ = conn1.send("SELECT", &["1"]).await;
    let _ = conn1.send("SET", &["shared", "db1-val"]).await;

    let _ = conn2.send("SELECT", &["0"]).await;
    let _ = conn2.send("ATOM.WATCH", &["shared"]).await;
    let _ = conn2.send("ATOM.MULTI", &[]).await;
    let _ = conn2.send("GET", &["shared"]).await;

    // 单库模式下写入同名 key 影响被 WATCH 的 key
    let _ = conn1.send("SET", &["shared", "db1-changed"]).await;

    let resp = conn2.send("ATOM.EXEC", &[]).await;
    let parsed = parse_response(&resp);
    assert_eq!(
        parsed,
        RespValue::BulkString(None),
        "单库模式下同名 key 变更触发 WATCH 冲突返回 nil"
    );
}

/// Issue #79: 存储层 bump 后, 另一连接写入应使 EXEC 返回 nil.
#[tokio::test]
async fn issue_79_watch_conflict_from_other_connection_storage_version() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut writer = TestConn::connect(addr).await;
    let mut watcher = TestConn::connect(addr).await;

    let _ = writer.send("SET", &["k", "v1"]).await;
    let _ = watcher.send("ATOM.WATCH", &["k"]).await;
    let _ = writer.send("SET", &["k", "v2"]).await;

    let _ = watcher.send("ATOM.MULTI", &[]).await;
    let _ = watcher.send("SET", &["k", "v3"]).await;
    let resp = watcher.send("ATOM.EXEC", &[]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(None),
        "storage-backed WATCH must detect cross-connection write"
    );

    let resp = writer.send("GET", &["k"]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(Some("v2".into()))
    );
}

/// Issue #83: 一条连接 UNWATCH 不得清掉其它连接仍有效的 WATCH.
#[tokio::test]
async fn issue_83_unwatch_one_connection_keeps_other_watch() {
    let (addr, _server) = start_ephemeral_server().await;
    let mut writer = TestConn::connect(addr).await;
    let mut watcher_keep = TestConn::connect(addr).await;
    let mut watcher_drop = TestConn::connect(addr).await;

    let _ = writer.send("SET", &["k", "v1"]).await;
    let _ = watcher_keep.send("ATOM.WATCH", &["k"]).await;
    let _ = watcher_drop.send("ATOM.WATCH", &["k"]).await;
    let _ = watcher_drop.send("ATOM.UNWATCH", &[]).await;
    let _ = writer.send("SET", &["k", "v2"]).await;

    let _ = watcher_keep.send("ATOM.MULTI", &[]).await;
    let _ = watcher_keep.send("SET", &["k", "v3"]).await;
    let resp = watcher_keep.send("ATOM.EXEC", &[]).await;
    assert_eq!(
        parse_response(&resp),
        RespValue::BulkString(None),
        "remaining WATCH must still detect the write after another connection UNWATCH"
    );
}
