//! @component aikv-cluster
//! 回归测试: ATOM.EXEC/EXEC JSON batch (TL.KvDoc 事务提交协议) 在集群拓扑
//! 下必须把内部快照阶段命中的 MOVED/ASK 原样透传给客户端, 而不能包裹成
//! 一个通用的 "ERR internal error during batch snapshot: ..." 错误.
//!
//! 背景: `cmd_atom_exec_json_batch` 在真正执行每条写命令前, 先用内部
//! `routed_command("DUMP", key)` 给要写的 key 做快照 (用于失败回滚).
//! 8bfac2f (移除进程内透明转发 forward_command, 让客户端自行按 -c 重定向)
//! 之后, 若该 key 的 slot 不在本节点, 这次内部 DUMP 会先拿到一个
//! `RespValue::Error("MOVED ...")`, `batch_load_key_snapshot` 把它转成
//! `Error::Command("MOVED ...")` 向上冒泡, 最终被 `cmd_atom_exec_json_batch`
//! 用 `format!("ERR internal error during batch snapshot: {e}")` 包了一层,
//! 产出类似 `ERR internal error during batch snapshot: 命令错误: MOVED 3821
//! 192.168.1.112:6379` 的顶层响应.这不是标准 `-MOVED slot addr` 格式,
//! StackExchange.Redis 等集群感知客户端无法识别并自动重定向重试整个
//! batch, 导致 `KvDocTransaction.CommitAsync()` 直接抛出
//! `RedisServerException`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Once};
use std::time::Duration;

use parking_lot::RwLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;

use aidb::cluster::meta_types::{default_slot_table, SlotStatus};
use aidb::cluster::router::key_to_slot;
use aidb::cluster::{MultiRaftNode, RaftServiceDispatcher, Router};

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};
use aikv::protocol::{RespParser, RespValue};
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::{KvStorage, MemoryEngine};

static RT: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| tokio::runtime::Runtime::new().unwrap());

/// slot 0 分配给远端 group 1 (本节点 id=2 未在本进程内起对应 raft group,
/// 因此对本节点而言 group 1 不是 local), 与 `cluster_routing.rs` /
/// `cluster_redirect_metrics.rs` 中已验证的 `b"\x00\x00" → slot 0` 映射保持一致.
fn ensure_remote_slot0_cluster_state() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mgr = RT.block_on(async {
            let mut group_nodes = HashMap::new();
            group_nodes.insert(1u64, vec![1u64, 2u64, 3u64]);
            let mut node_addrs = HashMap::new();
            node_addrs.insert(1u64, "192.168.1.112:6379".to_string());

            let mut table = default_slot_table();
            for s in 0u16..5 {
                table[s as usize] = SlotStatus::Assigned(1);
            }
            let router = Router::new(table.clone(), group_nodes, node_addrs);

            let dispatcher = Arc::new(RaftServiceDispatcher::new());
            let multi_raft = Arc::new(MultiRaftNode::new(2, Arc::new(router.clone()), dispatcher));

            let db = aidb::DB::open(
                std::env::temp_dir().join(format!(
                    "atom_exec_json_batch_redirect_{}",
                    std::process::id()
                )),
                aidb::config::Options::for_testing(),
            )
            .unwrap();
            let net_factory = aidb::cluster::RaftNetworkClientFactory::new(2, 0, 30, 65536);
            let meta_raft = aidb::cluster::MetaRaftNode::new(
                aidb::cluster::RaftNodeConfig {
                    node_id: 2,
                    group_id: 0,
                    election_timeout_min: 2000,
                    election_timeout_max: 4000,
                    heartbeat_interval: 100,
                    max_payload_entries: 100,
                    snapshot_logs_since_last: 1000,
                    max_entry_size: 8192,
                    rpc_timeout_ms: 500,
                    grpc_max_message_size: 65536,
                    snapshot_size_threshold: None,
                    linearizable_read: false,
                    log_committer_config: None,
                },
                db,
                net_factory,
            )
            .await
            .unwrap();
            meta_raft.set_slot_table(table);

            ClusterStateManager {
                router,
                meta_raft: Arc::new(meta_raft),
                multi_raft,
                node_id: 2,
                config_epoch: AtomicU64::new(0),
                role: RwLock::new(ReplicationRole::Replica { primary_id: 1 }),
                local_group_leaders: {
                    let mut l = HashMap::new();
                    l.insert(1u64, false);
                    RwLock::new(l)
                },
                group_quorum_ok: RwLock::new(HashMap::new()),
                cluster_state_ok: AtomicBool::new(true),
                membership_coordinator: None,
                slot_migration_manager: None,
                data_dir: None,
                importing_slots: RwLock::new(HashMap::new()),
                data_port_offset: DEFAULT_DATA_PORT_OFFSET,
                announce_resolver: AnnounceResolver::default(),
                metrics: None,
                _watcher_shutdown: parking_lot::Mutex::new(None),
                _auto_save_shutdown: parking_lot::Mutex::new(None),
            }
        });
        let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
    });
}

async fn start_server() -> (SocketAddr, Arc<MemoryEngine>) {
    let storage = MemoryEngine::new(16);
    let shared = ServerSharedState::new(
        ConnectionConfig {
            read_timeout: None,
            idle_timeout: None,
            max_clients: 0,
            script_worker_threads: 2,
        },
        storage.clone(),
        6379,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = Server::run_with_listener(listener, shared).await;
    });
    time::sleep(Duration::from_millis(50)).await;
    (addr, storage)
}

fn parse_response(data: &[u8]) -> RespValue {
    let mut parser = RespParser::new();
    parser.feed(data);
    parser
        .parse()
        .expect("parse error")
        .expect("incomplete response")
}

async fn send(stream: &mut TcpStream, cmd: &str, args: &[&str]) -> Vec<u8> {
    let byte_args: Vec<&[u8]> = args.iter().map(|arg| arg.as_bytes()).collect();
    send_mixed(stream, cmd, &byte_args).await
}

async fn send_mixed(stream: &mut TcpStream, cmd: &str, args: &[&[u8]]) -> Vec<u8> {
    let mut header = format!("*{}\r\n", 1 + args.len());
    header.push_str(&format!("${}\r\n{}\r\n", cmd.len(), cmd));
    stream.write_all(header.as_bytes()).await.unwrap();
    for arg in args {
        stream
            .write_all(format!("${}\r\n", arg.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(arg).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
    }
    let mut buf = vec![0u8; 8192];
    let n = time::timeout(Duration::from_secs(3), stream.read(&mut buf))
        .await
        .expect("read timeout")
        .expect("read failed");
    buf.truncate(n);
    buf
}

/// JSON 转义后的 `\x00\x00` (CRC16 == 0 → slot 0), 交给 batch 内部的
/// DUMP 快照后应命中远端 slot 并触发 MOVED.
const REMOTE_SLOT_KEY_JSON_ESCAPED: &str = r#"\u0000\u0000"#;

#[test]
fn atom_exec_json_batch_snapshot_moved_is_passed_through_cleanly() {
    ensure_remote_slot0_cluster_state();
    RT.block_on(async {
        let (addr, _storage) = start_server().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let batch = format!(r#"[["SET","{REMOTE_SLOT_KEY_JSON_ESCAPED}","v"]]"#);
        let resp = send(&mut stream, "EXEC", &[&batch]).await;
        match parse_response(&resp) {
            RespValue::Error(msg) => {
                assert!(
                    msg.starts_with("MOVED "),
                    "batch 快照阶段命中非本地 slot 时应直接透传顶层 MOVED, \
                     而不能包裹成内部错误 (回归 8bfac2f 之后的行为), got: {msg}"
                );
            }
            other => panic!("expected top-level MOVED error, got {other:?}"),
        }
    });
}

fn crossslot_key_pair() -> (&'static [u8], &'static [u8]) {
    let candidates: &[&[u8]] = &[
        b"cross-a{one}",
        b"cross-b{two}",
        b"cross-c{thr}",
        b"cross-d{fou}",
    ];
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            if key_to_slot(candidates[i]) != key_to_slot(candidates[j]) {
                return (candidates[i], candidates[j]);
            }
        }
    }
    panic!("failed to find two keys with different slots");
}

/// Issue #78: 标准 MULTI/EXEC 跨 slot 必须在执行前整笔 CROSSSLOT, 不得部分写入.
#[test]
fn issue_78_standard_multi_exec_crossslot_rejects_before_any_write() {
    ensure_remote_slot0_cluster_state();
    let (key_a, key_b) = crossslot_key_pair();
    RT.block_on(async {
        let (addr, storage) = start_server().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let _ = send_mixed(&mut stream, "MULTI", &[]).await;
        let _ = send_mixed(&mut stream, "SET", &[key_a, b"v1"]).await;
        let _ = send_mixed(&mut stream, "SET", &[key_b, b"v2"]).await;
        let resp = send_mixed(&mut stream, "EXEC", &[]).await;

        match parse_response(&resp) {
            RespValue::Error(msg) => {
                assert!(
                    msg.starts_with("CROSSSLOT "),
                    "expected top-level CROSSSLOT, got {msg}"
                );
            }
            other => panic!("expected top-level CROSSSLOT error, got {other:?}"),
        }

        assert!(
            storage.get(0, key_a).await.unwrap().is_none(),
            "first queued key must not be written on CROSSSLOT"
        );
        assert!(
            storage.get(0, key_b).await.unwrap().is_none(),
            "second queued key must not be written on CROSSSLOT"
        );
    });
}

/// slot-0 二进制 key (CRC16 == 0), 标准 EXEC 应返回顶层 MOVED 而非数组元素.
const REMOTE_SLOT0_KEY: &[u8] = b"\0\0";

/// Issue #78: 标准 MULTI/EXEC 命中远端 slot 时必须整笔 MOVED, 不能先执行本地写.
#[test]
fn issue_78_standard_multi_exec_moved_is_top_level() {
    ensure_remote_slot0_cluster_state();
    RT.block_on(async {
        let (addr, _storage) = start_server().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();

        let _ = send_mixed(&mut stream, "MULTI", &[]).await;
        let _ = send_mixed(&mut stream, "SET", &[REMOTE_SLOT0_KEY, b"value"]).await;
        let resp = send_mixed(&mut stream, "EXEC", &[]).await;

        match parse_response(&resp) {
            RespValue::Error(msg) => {
                assert!(
                    msg.starts_with("MOVED "),
                    "expected top-level MOVED for standard EXEC, got {msg}"
                );
            }
            other => panic!("expected top-level MOVED error, got {other:?}"),
        }
    });
}
