//! @component aikv-cluster
//! CLUSTER CREATEGROUP 集成测试.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

use aidb::cluster::meta_types::{default_slot_table, MetaRequest, SlotStatus};
use aidb::cluster::{MultiRaftNode, RaftServiceDispatcher, Router};

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};

static RT: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Runtime::new().unwrap());

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    RT.block_on(f)
}

async fn setup_cluster_mgr() -> ClusterStateManager {
    let mut group_nodes = HashMap::new();
    group_nodes.insert(1, vec![1]);
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
        std::env::temp_dir().join(format!("cluster_cg_{}", std::process::id())),
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
    meta_raft
        .initialize(vec![(1, "127.0.0.1:17001".to_string())])
        .await
        .unwrap();
    meta_raft.set_slot_table(table);

    ClusterStateManager {
        router,
        meta_raft: Arc::new(meta_raft),
        multi_raft,
        node_id: 1,
        config_epoch: AtomicU64::new(0),
        role: RwLock::new(ReplicationRole::Primary),
        local_group_leaders: RwLock::new(HashMap::new()),
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
        slot_groups: ClusterStateManager::new_slot_groups(),
        has_active_migration: AtomicBool::new(false),
    }
}

async fn register_node(mgr: &ClusterStateManager, node_id: u64) {
    mgr.meta_raft
        .propose(MetaRequest::RegisterNode {
            node_id,
            rpc_addr: format!("127.0.0.1:{}", 17000 + node_id),
            client_addr: Some(format!("127.0.0.1:{}", 7000 + node_id)),
            tags: HashMap::new(),
        })
        .await
        .expect("register node");
}

#[test]
fn cluster_create_group_cases() {
    let mgr = block_on(setup_cluster_mgr());
    block_on(register_node(&mgr, 7));
    block_on(register_node(&mgr, 8));
    let meta_raft = Arc::clone(&mgr.meta_raft);
    let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));

    let unknown = block_on(aikv::cluster::cluster_create_group(99, None)).unwrap_err();
    assert!(
        unknown.contains("ERR Node not known"),
        "unknown node: {unknown}"
    );

    let ok = block_on(aikv::cluster::cluster_create_group(7, None));
    assert_eq!(ok.as_deref(), Ok("OK"), "create_group failed: {ok:?}");

    let meta = meta_raft.get_cluster_meta();
    let group = meta.groups.get(&7).expect("group 7 should exist");
    assert_eq!(group.replicas.len(), 1);
    assert_eq!(group.replicas[0].node_id, 7);
    assert!(group.replicas[0].is_leader);

    let dup = block_on(aikv::cluster::cluster_create_group(7, None)).unwrap_err();
    assert!(
        dup.contains("ERR Node already in a data group"),
        "duplicate: {dup}"
    );

    let add_err = block_on(aikv::cluster::cluster_add_replica(8, 9)).unwrap_err();
    assert!(
        add_err.contains("ERR Primary is not a data group member"),
        "add_replica: {add_err}"
    );
}
