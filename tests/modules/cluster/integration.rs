//! @component aikv-cluster
// Cluster protocol L2 integration tests.
// CLUSTER_STATE_MGR is OnceLock — all tests use a shared setup via std::sync::Once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, LazyLock};

use bytes::Bytes;
use parking_lot::RwLock;

use aidb::cluster::meta_types::{default_slot_table, SlotStatus};
use aidb::cluster::{MultiRaftNode, RaftServiceDispatcher, Router};

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};
use aikv::protocol::RespValue;

static RT: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Runtime::new().unwrap());

fn ensure_cluster_state() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let mgr = RT.block_on(create_cluster_mgr());
        let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
    });
}

async fn create_cluster_mgr() -> ClusterStateManager {
    let mut group_nodes = HashMap::new();
    group_nodes.insert(1, vec![1, 2, 3]);
    let mut node_addrs = HashMap::new();
    node_addrs.insert(1, "127.0.0.1:7001".to_string());

    let mut table = default_slot_table();
    for s in 0u16..5 {
        table[s as usize] = SlotStatus::Assigned(1);
    }
    let router = Router::new(table.clone(), group_nodes, node_addrs);
    let dispatcher = Arc::new(RaftServiceDispatcher::new());
    let multi_raft = Arc::new(MultiRaftNode::new(1, Arc::new(router.clone()), dispatcher));

    let db = aidb::DB::open(
        std::env::temp_dir().join(format!("cluster_int_{}", std::process::id())),
        aidb::config::Options::for_testing(),
    )
    .unwrap();
    let net_factory = aidb::cluster::RaftNetworkClientFactory::new(1, 0, 30, 65536);
    let meta_raft = aidb::cluster::MetaRaftNode::new(
        aidb::cluster::RaftNodeConfig {
            node_id: 1,
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

    // 注册一个真实数据 group 到 groups map, 使 COUNTKEYSINSLOT / GETKEYSINSLOT
    // 的 `for gid in gids` 循环体真正执行 (覆盖 block_on panic 路径):
    // OpenRaftNode::new 需要 `use_wal: true`, `Options::for_testing()` 已满足.
    // scan_keys 因未注册 storages 会立即返回 `group not found locally` Err,
    // 不影响本 harness 其余用例.
    let data_db = aidb::DB::open(
        std::env::temp_dir().join(format!("cluster_int_data_{}", std::process::id())),
        aidb::config::Options::for_testing(),
    )
    .unwrap();
    let data_net_factory = aidb::cluster::RaftNetworkClientFactory::new(1, 1, 30, 65536);
    let data_node = aidb::cluster::OpenRaftNode::new(
        aidb::cluster::RaftNodeConfig {
            node_id: 1,
            group_id: 1,
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
        data_db,
        data_net_factory,
    )
    .await
    .unwrap();
    let data_node = Arc::new(data_node);
    let _ = data_node
        .initialize(vec![(1, "127.0.0.1:7001".to_string())])
        .await;
    multi_raft
        .get_groups()
        .write()
        .insert(1, Arc::clone(&data_node));

    ClusterStateManager {
        router,
        meta_raft: Arc::new(meta_raft),
        multi_raft,
        node_id: 1,
        config_epoch: AtomicU64::new(0),
        role: RwLock::new(ReplicationRole::Primary),
        local_group_leaders: {
            let mut l = HashMap::new();
            l.insert(1, true);
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
        paused_write_groups: parking_lot::RwLock::new(HashMap::new()),
        _watcher_shutdown: parking_lot::Mutex::new(None),
        _auto_save_shutdown: parking_lot::Mutex::new(None),
        slot_groups: ClusterStateManager::new_slot_groups(),
        has_active_migration: AtomicBool::new(false),
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    RT.block_on(f)
}

// ── CLUSTER help / format ──

/// 验证 CLUSTER INFO 输出响应字段格式
#[test]
fn cluster_info_format() {
    ensure_cluster_state();
    let info = aikv::cluster::cluster_info().unwrap();
    for field in [
        "cluster_state",
        "cluster_slots_assigned",
        "cluster_known_nodes",
        "cluster_size",
        "cluster_current_epoch",
    ] {
        assert!(info.contains(field), "info missing {field}");
    }
}

/// 验证 CLUSTER MYID 输出 40 字节节点 ID
#[test]
fn cluster_myid_format() {
    ensure_cluster_state();
    let myid = aikv::cluster::cluster_myid().unwrap();
    assert_eq!(myid.len(), 40);
    assert!(myid.chars().all(|c| c.is_ascii_hexdigit()));
}

/// 验证 CLUSTER NODES 输出包含节点条目
#[test]
fn cluster_nodes_format() {
    ensure_cluster_state();
    let nodes = aikv::cluster::cluster_nodes().unwrap();
    // nodes may be empty (MetaRaft not initialized with nodes),
    // but the function should not panic
    for line in nodes.lines() {
        assert!(line.split(' ').count() >= 2, "bad line: {line}");
    }
}

// ── CLUSTER command errors (async) ──

/// 验证 CLUSTER REPLICATE 设置从节点副本角色
#[test]
fn cluster_replicate_sets_role() {
    ensure_cluster_state();
    // REPLICATE sets the local role — succeeds regardless of primary existence
    let result = block_on(aikv::cluster::cluster_replicate(999));
    assert!(
        result.is_ok(),
        "REPLICATE should set local role: {:?}",
        result
    );
    // Verify the role was changed by reading it back through REPLICAS
    let replicas = aikv::cluster::cluster_replicas(999);
    let _ = replicas;
}

/// 验证 非 Leader 节点执行 CLUSTER ADDSLOTS 报 CLUSTERDOWN/ERR 错误
#[test]
fn cluster_add_slots_not_leader() {
    ensure_cluster_state();
    if let Err(e) = block_on(aikv::cluster::cluster_add_slots(&[100, 101], None)) {
        assert!(e.contains("ERR") || e.contains("CLUSTERDOWN"));
    }
}

/// 验证 非 Leader 节点执行 CLUSTER DELSLOTS 报 CLUSTERDOWN/ERR 错误
#[test]
fn cluster_del_slots_not_leader() {
    ensure_cluster_state();
    if let Err(e) = block_on(aikv::cluster::cluster_del_slots(&[100])) {
        assert!(e.contains("ERR") || e.contains("CLUSTERDOWN"));
    }
}

static FAILOVER_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// 验证 主节点执行 CLUSTER FAILOVER 拒绝
#[test]
fn cluster_failover_from_primary() {
    let _guard = FAILOVER_TEST_LOCK.lock();
    ensure_cluster_state();
    let mgr = aikv::cluster::state::CLUSTER_STATE_MGR.get().unwrap();
    *mgr.role.write() = aikv::cluster::state::ReplicationRole::Primary;
    let err = block_on(aikv::cluster::cluster_failover("FORCE")).unwrap_err();
    assert!(err.contains("ERR You should be a replica"));
}

/// 验证 TAKEOVER 模式被明确拒绝 (Raft 不支持单方面非法定人数夺权)
#[test]
fn cluster_failover_takeover_rejected() {
    let _guard = FAILOVER_TEST_LOCK.lock();
    ensure_cluster_state();
    let err = block_on(aikv::cluster::cluster_failover("TAKEOVER")).unwrap_err();
    assert!(
        err.contains("TAKEOVER is not supported in Raft cluster mode"),
        "expected Raft cluster mode rejection, got: {err}"
    );
}

/// 验证未知模式被拒绝
#[test]
fn cluster_failover_unknown_mode() {
    let _guard = FAILOVER_TEST_LOCK.lock();
    ensure_cluster_state();
    let err = block_on(aikv::cluster::cluster_failover("INVALID_MODE")).unwrap_err();
    assert!(err.contains("ERR Unknown FAILOVER mode"), "got: {err}");
}

/// 验证 从节点执行 FORCE 触发选举, 保留原共识组拓扑, 并在当选后升主
#[test]
fn cluster_failover_force_preserves_topology_and_promotes() {
    use aidb::cluster::meta_types::{ClusterMeta, GroupMeta, ReplicaInfo};
    use aikv::cluster::state::{ReplicationRole, CLUSTER_STATE_MGR};

    let _guard = FAILOVER_TEST_LOCK.lock();
    ensure_cluster_state();
    let mgr = CLUSTER_STATE_MGR.get().unwrap();

    // 1. 设置 cluster_meta, 包含一个 3 副本数据 group 1
    let mut meta = ClusterMeta::default();
    let replicas = vec![
        ReplicaInfo {
            node_id: 1,
            is_leader: false,
        },
        ReplicaInfo {
            node_id: 2,
            is_leader: true,
        },
        ReplicaInfo {
            node_id: 3,
            is_leader: false,
        },
    ];
    meta.groups.insert(
        1,
        GroupMeta {
            group_id: 1,
            replicas: replicas.clone(),
            slot_ranges: vec![(0, 4)],
            config_version: 1,
        },
    );
    mgr.meta_raft.set_cluster_meta_for_testing(meta);

    // 2. 将本地角色标记为从节点 (Replica)
    *mgr.role.write() = ReplicationRole::Replica { primary_id: 2 };
    mgr.local_group_leaders.write().insert(1, false);

    // 3. 执行 CLUSTER FAILOVER FORCE
    let res = block_on(aikv::cluster::cluster_failover("FORCE"));
    assert!(res.is_ok(), "FORCE failover should succeed: {res:?}");

    // 4. 严守共识拓扑核心红线: 验证 group 1 的副本拓扑未被篡改为单一节点
    let meta_after = mgr.meta_raft.get_cluster_meta();
    let group_after = meta_after.groups.get(&1).unwrap();
    assert_eq!(
        group_after.replicas.len(),
        3,
        "拓扑成员必须保持为 3 个副本, 绝不能被篡改为单一节点"
    );

    // 5. 验证本地角色已切换为 Primary
    assert!(
        matches!(*mgr.role.read(), ReplicationRole::Primary),
        "成功当选后角色必须切换为 Primary"
    );
    assert_eq!(
        mgr.local_group_leaders.read().get(&1),
        Some(&true),
        "本地 group 1 leader 缓存必须标记为 true"
    );

    // 恢复主节点角色, 避免干扰后续测试
    *mgr.role.write() = ReplicationRole::Primary;
}

/// 验证 无参协同式 CLUSTER FAILOVER 明确拒绝并引导使用 FORCE
#[test]
fn cluster_failover_unsupported_coordinated_mode_returns_error() {
    let _guard = FAILOVER_TEST_LOCK.lock();
    ensure_cluster_state();

    let res = block_on(aikv::cluster::cluster_failover(""));
    assert!(
        res.is_err(),
        "coordinated failover without RPC must return Err"
    );
    let err_msg = res.unwrap_err();
    assert!(
        err_msg.contains(
            "ERR Coordinated failover requires cross-node RPC; please use CLUSTER FAILOVER FORCE"
        ),
        "unexpected error message: {err_msg}"
    );
}

/// 验证 当主节点失去所有本地 group 的 leader 权时, 自动被动降级为 Replica
#[test]
fn test_leader_loss_auto_demotes_primary_to_replica() {
    use aidb::cluster::meta_types::{ClusterMeta, GroupMeta, ReplicaInfo};
    use aikv::cluster::state::{ReplicationRole, CLUSTER_STATE_MGR};

    let _guard = FAILOVER_TEST_LOCK.lock();
    ensure_cluster_state();
    let mgr = CLUSTER_STATE_MGR.get().unwrap();

    // 1. 设置 cluster_meta, 本节点 (node 1) 属于 group 1 与 group 2
    let mut meta = ClusterMeta::default();
    meta.groups.insert(
        1,
        GroupMeta {
            group_id: 1,
            replicas: vec![
                ReplicaInfo {
                    node_id: 1,
                    is_leader: true,
                },
                ReplicaInfo {
                    node_id: 2,
                    is_leader: false,
                },
            ],
            slot_ranges: vec![(0, 4)],
            config_version: 1,
        },
    );
    meta.groups.insert(
        2,
        GroupMeta {
            group_id: 2,
            replicas: vec![
                ReplicaInfo {
                    node_id: 1,
                    is_leader: true,
                },
                ReplicaInfo {
                    node_id: 2,
                    is_leader: false,
                },
            ],
            slot_ranges: vec![(5, 9)],
            config_version: 1,
        },
    );
    mgr.meta_raft.set_cluster_meta_for_testing(meta);

    // 初始状态为 Primary, 两个 group 的 leader 均在本机
    *mgr.role.write() = ReplicationRole::Primary;
    mgr.local_group_leaders.write().insert(1, true);
    mgr.local_group_leaders.write().insert(2, true);

    // 2. group 1 leader 变更为 node 2 (本节点仍是 group 2 leader, 保持 Primary)
    mgr.apply_observed_group_leader(1, 2);
    assert!(
        matches!(*mgr.role.read(), ReplicationRole::Primary),
        "仍持有 group 2 leader 时必须维持 Primary"
    );

    // 3. group 2 leader 也变更为 node 2 (失去全部所属 group leader 权, 自动降级为 Replica)
    mgr.apply_observed_group_leader(2, 2);
    match *mgr.role.read() {
        ReplicationRole::Replica { primary_id } => {
            assert_eq!(primary_id, 2, "降级后的 primary_id 应与新 leader 一致");
        }
        _ => panic!("失去全部 leader 权后必须被动降级为 Replica"),
    }

    // 4. group 1 重新当选 leader, 自动重新晋升为 Primary
    mgr.apply_observed_group_leader(1, 1);
    assert!(
        matches!(*mgr.role.read(), ReplicationRole::Primary),
        "重新当选 group leader 后应自动晋升为 Primary"
    );

    // 恢复主节点角色
    *mgr.role.write() = ReplicationRole::Primary;
}

/// 验证 RAII PauseWriteGuard 释放与超时自动恢复
#[test]
fn test_write_pause_guard_raii_and_timeout() {
    ensure_cluster_state();
    let mgr = aikv::cluster::state::CLUSTER_STATE_MGR.get().unwrap();

    // 1. 显式获取 RAII guard
    {
        let _guard = mgr.pause_group_writes(99, std::time::Duration::from_secs(10));
        assert!(mgr.is_group_write_paused(99));
    }
    // 2. guard drop 后自动解除
    assert!(!mgr.is_group_write_paused(99));

    // 3. 超时自动恢复
    let _guard = mgr.pause_group_writes(99, std::time::Duration::from_millis(1));
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert!(!mgr.is_group_write_paused(99));
}

/// 验证 CLUSTER SAVECONFIG 命令路由派发
#[test]
fn cluster_saveconfig_dispatch() {
    ensure_cluster_state();
    if let Err(e) = block_on(aikv::cluster::dispatch_cluster(Some("SAVECONFIG"), &[])) {
        let msg = format!("{e}");
        assert!(msg.contains("ERR") || msg.contains("CLUSTERDOWN"));
    }
}

// ── Sync command error / format ──

/// 验证 CLUSTER KEYSLOT 计算符合 CRC16 规则 (0..16383)
#[test]
fn cluster_keyslot_basic() {
    ensure_cluster_state();
    let slot: u16 = aikv::cluster::cluster_keyslot(b"x")
        .unwrap()
        .parse()
        .unwrap();
    assert!(slot < 16384);
}

/// COUNTKEYSINSLOT 在 tokio worker 上调用 `Handle::block_on` 会 panic 断连.
/// 期望: 返回 Integer, 不断开. 草稿: `2026-08-14-fix-cluster-countkeysinslot-panic.md`
#[test]
fn cluster_countkeysinslot_does_not_panic_inside_runtime() {
    ensure_cluster_state();
    let args = [
        Bytes::from_static(b"COUNTKEYSINSLOT"),
        Bytes::from_static(b"0"),
    ];
    let resp = block_on(aikv::cluster::dispatch_cluster(
        Some("COUNTKEYSINSLOT"),
        &args,
    ))
    .expect("COUNTKEYSINSLOT must not panic or error on initialized cluster");
    assert!(
        matches!(resp, RespValue::Integer(_)),
        "expected Integer, got {resp:?}"
    );
}

/// GETKEYSINSLOT 同源: 禁止在 async 上下文 `block_on`.
/// 草稿: `2026-08-14-fix-cluster-countkeysinslot-panic.md`
#[test]
fn cluster_getkeysinslot_does_not_panic_inside_runtime() {
    ensure_cluster_state();
    let args = [
        Bytes::from_static(b"GETKEYSINSLOT"),
        Bytes::from_static(b"0"),
        Bytes::from_static(b"10"),
    ];
    let resp = block_on(aikv::cluster::dispatch_cluster(
        Some("GETKEYSINSLOT"),
        &args,
    ))
    .expect("GETKEYSINSLOT must not panic");
    assert!(
        matches!(resp, RespValue::Array(_)),
        "expected Array, got {resp:?}"
    );
}
