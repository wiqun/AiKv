//! @component aikv-cluster
//! CLUSTER INFO 输出 Redis 8.8 字段存在性 golden 测试 (独立 binary).
//!
//! 独立成 binary 的原因: 本测试需要 `CLUSTER_STATE_MGR.set(...)` (OnceLock,
//! 一次性不可逆). 若留在 `tests/commands.rs` binary 内, 会污染同进程
//! 所有后续命令测试 — 临时服务器会被误判为集群模式, 报
//! `slot N is not allocated to any group` (MIGRATE/JSON 8 测曾因此失败).
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use aidb::cluster::meta_types::{default_slot_table, SlotStatus};
use aidb::cluster::{MultiRaftNode, RaftServiceDispatcher, Router};
use parking_lot::RwLock;

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::cluster_info;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};

const CLUSTER_FIELDS: &str = include_str!("../../fixtures/redis88_cluster_info_fields.txt");

#[test]
fn cluster_info_redis88_fields_present() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
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
            let multi_raft = Arc::new(MultiRaftNode::new(1, Arc::new(router.clone()), dispatcher));
            let db = aidb::DB::open(
                std::env::temp_dir().join(format!("info_golden_cluster_{}", std::process::id())),
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

            let mgr = ClusterStateManager {
                router,
                meta_raft: Arc::new(meta_raft),
                multi_raft,
                node_id: 1,
                config_epoch: AtomicU64::new(0),
                role: RwLock::new(ReplicationRole::Primary),
                local_group_leaders: RwLock::new(HashMap::from([(1u64, true)])),
                group_quorum_ok: RwLock::new(HashMap::new()),
                cluster_state_ok: AtomicBool::new(true),
                membership_coordinator: None,
                slot_migration_manager: None,
                data_dir: None,
                importing_slots: RwLock::new(HashMap::new()),
                data_port_offset: DEFAULT_DATA_PORT_OFFSET,
                announce_resolver: AnnounceResolver::default(),
                metrics: Some(Arc::new(aikv::server::metrics::ServerMetrics::default())),
                paused_write_groups: parking_lot::RwLock::new(HashMap::new()),
                _watcher_shutdown: parking_lot::Mutex::new(None),
                _auto_save_shutdown: parking_lot::Mutex::new(None),
                slot_groups: ClusterStateManager::new_slot_groups(),
                has_active_migration: AtomicBool::new(false),
            };
            let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
        });
    });

    let info = cluster_info().expect("cluster initialized");
    let fields: Vec<String> = CLUSTER_FIELDS
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| line.trim().to_string())
        .collect();
    let missing: Vec<_> = fields
        .iter()
        .filter(|field| {
            let prefix = format!("{field}:");
            !info.lines().any(|line| line.starts_with(&prefix))
        })
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "CLUSTER INFO missing Redis 8.8 fields: {}",
        missing.join(", ")
    );
}
