//! @component aikv-cluster
//! 集群副本懒过期回归: 过期 key 读应返回 nil, 不得因清理 delete 失败变成集群错误.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use parking_lot::RwLock;

use aidb::cluster::meta_types::default_slot_table;
use aidb::cluster::MultiRaftNode;
use aidb::cluster::RaftServiceDispatcher;
use aidb::cluster::Router;

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};
use aikv::storage::adapter::StorageAdapter;
use aikv::storage::aidb::AiDbEngine;
use aikv::storage::cluster_adapter::ClusterDataAdapter;

/// group 1 -> nodes [1,2,3] (self = node 1), all slots Assigned(1).
async fn build_mgr(tmp_name: &str) -> Arc<ClusterStateManager> {
    let mut group_nodes = HashMap::new();
    group_nodes.insert(1u64, vec![1u64, 2u64, 3u64]);
    let mut node_addrs = HashMap::new();
    node_addrs.insert(1u64, "127.0.0.1:7001".to_string());
    node_addrs.insert(2u64, "127.0.0.1:7002".to_string());

    let table = default_slot_table();
    let router = Router::new(table.clone(), group_nodes.clone(), node_addrs.clone());

    let dispatcher = Arc::new(RaftServiceDispatcher::new());
    let multi_raft = Arc::new(MultiRaftNode::new(1, Arc::new(router.clone()), dispatcher));

    let raft_config = aidb::cluster::RaftNodeConfig {
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
    };
    let db = aidb::DB::open(
        std::env::temp_dir().join(tmp_name),
        aidb::config::Options::for_testing(),
    )
    .unwrap();
    let net_factory = aidb::cluster::RaftNetworkClientFactory::new(1, 0, 30, 65536);
    let meta_raft = aidb::cluster::MetaRaftNode::new(raft_config, db, net_factory)
        .await
        .unwrap();
    meta_raft.set_slot_table(table);

    let metrics = Arc::new(aikv::server::metrics::ServerMetrics::default());
    let mgr = ClusterStateManager {
        router,
        meta_raft: Arc::new(meta_raft),
        multi_raft,
        node_id: 1,
        config_epoch: AtomicU64::new(0),
        role: RwLock::new(ReplicationRole::Primary),
        local_group_leaders: {
            let mut l = HashMap::new();
            l.insert(1u64, true);
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
        metrics: Some(Arc::clone(&metrics)),
        _watcher_shutdown: parking_lot::Mutex::new(None),
        _auto_save_shutdown: parking_lot::Mutex::new(None),
        slot_groups: ClusterStateManager::new_slot_groups(),
        has_active_migration: AtomicBool::new(false),
    };
    mgr.multi_raft.override_group_local(1);
    let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
    CLUSTER_STATE_MGR.get().unwrap().clone()
}

/// Make `is_local_group_leader(1)` report false without touching the
/// underlying (still genuinely self-elected) single-node Raft group:
/// point the router's group-1 leader at node 2 so the "own-node fallback"
/// in `ClusterStateManager::is_local_group_leader` can't short-circuit
/// back to true, then flip the local leader-cache bit too.
fn mark_as_replica(mgr: &ClusterStateManager) {
    let mut all_assigned_to_1 = default_slot_table();
    for slot in all_assigned_to_1.iter_mut() {
        *slot = aidb::cluster::SlotStatus::Assigned(1);
    }
    let mut group_nodes = HashMap::new();
    group_nodes.insert(1u64, vec![1u64, 2u64, 3u64]);
    let mut node_addrs = HashMap::new();
    node_addrs.insert(1u64, "127.0.0.1:7001".to_string());
    node_addrs.insert(2u64, "127.0.0.1:7002".to_string());
    let mut group_leaders = HashMap::new();
    group_leaders.insert(1u64, 2u64);
    mgr.router
        .refresh_from_data(all_assigned_to_1, group_nodes, node_addrs, group_leaders);
    mgr.local_group_leaders.write().insert(1, false);
}

#[tokio::test]
async fn lazy_expire_delete_allowed_on_leader_denied_on_replica() {
    let mgr = build_mgr("aikv_lazy_expire_test").await;
    let local =
        AiDbEngine::open_for_testing(std::env::temp_dir().join("aikv_lazy_expire_local")).unwrap();
    let adapter = ClusterDataAdapter::new(local, ClusterDataAdapter::DEFAULT_EAGER_FLUSH);

    let key = AiDbEngine::encode_key(0, b"expiring-key");

    // Leader: lazy expiration cleanup is allowed to attempt a real delete.
    mgr.local_group_leaders.write().insert(1, true);
    assert!(
        adapter.allow_lazy_expire_delete(&key),
        "leader should be allowed to run lazy-expire delete"
    );

    // Replica (not leader, but group still local): must not attempt the
    // delete — a propose from a follower always fails with NotLeader and
    // used to bubble up as a hard error on plain reads.
    mark_as_replica(&mgr);
    assert!(
        !adapter.allow_lazy_expire_delete(&key),
        "replica must not attempt lazy-expire delete (would hit NotLeader)"
    );
}
