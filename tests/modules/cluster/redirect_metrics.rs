//! @component aikv-cluster
//! MOVED/ASK 不应计入 commandstats (对齐 Redis 8.8 / redis_exporter).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Once};

use bytes::Bytes;
use parking_lot::RwLock;

use aidb::cluster::meta_types::{default_slot_table, SlotStatus};
use aidb::cluster::{MultiRaftNode, RaftServiceDispatcher, Router};

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::connection::ClusterConnectionState;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};
use aikv::protocol::RespValue;
use aikv::server::{ConnectionConfig, ServerSharedState};
use aikv::storage::{KvStorage, MemoryEngine};

static RT: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| tokio::runtime::Runtime::new().unwrap());

fn ensure_node2_cluster_state() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mgr = RT.block_on(async {
            let mut group_nodes = HashMap::new();
            group_nodes.insert(1u64, vec![1u64, 2u64, 3u64]);
            let mut node_addrs = HashMap::new();
            node_addrs.insert(1u64, "127.0.0.1:7001".to_string());

            let mut table = default_slot_table();
            for s in 0u16..5 {
                table[s as usize] = SlotStatus::Assigned(1);
            }
            let router = Router::new(table.clone(), group_nodes, node_addrs);

            let dispatcher = Arc::new(RaftServiceDispatcher::new());
            let multi_raft = Arc::new(MultiRaftNode::new(2, Arc::new(router.clone()), dispatcher));

            let db = aidb::DB::open(
                std::env::temp_dir().join(format!("redirect_metrics_{}", std::process::id())),
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
                paused_write_groups: parking_lot::RwLock::new(HashMap::new()),
                _watcher_shutdown: parking_lot::Mutex::new(None),
                _auto_save_shutdown: parking_lot::Mutex::new(None),
                slot_groups: ClusterStateManager::new_slot_groups(),
                has_active_migration: AtomicBool::new(false),
            }
        });
        let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
    });
}

#[test]
fn moved_does_not_increment_commandstats() {
    ensure_node2_cluster_state();

    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    let shared = ServerSharedState::new(ConnectionConfig::default(), storage, 6379);
    let router = shared.router();
    let cluster_state = ClusterConnectionState::default();

    let before = shared.metrics().command_ok_count("GET");
    let mut db = 0usize;
    let result = RT.block_on(async {
        router
            .execute_with_client(
                "GET",
                &[Bytes::from_static(b"\x00\x00")],
                &mut db,
                None,
                None,
                aikv::protocol::ProtocolVersion::Resp2,
                Some(&cluster_state),
            )
            .await
    });
    let after = shared.metrics().command_ok_count("GET");

    match result {
        Ok(RespValue::Error(msg)) => {
            assert!(msg.starts_with("MOVED "), "expected MOVED error, got {msg}");
        }
        other => panic!("expected MOVED error response, got {other:?}"),
    }
    assert_eq!(
        before, after,
        "GET commandstats must not increment on MOVED"
    );
}
