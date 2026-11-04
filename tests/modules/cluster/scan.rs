//! @component aikv-cluster
//! 集群多 Group 全局扫描 (SCAN / KEYS) 与多路归并分页集成测试.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use aidb::cluster::multi_raft_node::LifecycleConfig;
use aidb::cluster::types::ThinWriteBatch;
use aidb::cluster::{
    MetaRaftNode, MetaRequest, MultiRaftNode, RaftNetworkClientFactory, RaftNodeConfig,
    RaftServiceDispatcher, Request, Router, SlotMigrationState, SlotStatus,
};
use aidb::config::Options;
use aidb::DB;
use tempfile::TempDir;

use aikv::cluster::announce::AnnounceResolver;
use aikv::cluster::state::{
    ClusterStateManager, ReplicationRole, CLUSTER_STATE_MGR, DEFAULT_DATA_PORT_OFFSET,
};
use aikv::storage::adapter::{KvStorageAdapter, StorageAdapter};
use aikv::storage::aidb::AiDbEngine;
use aikv::storage::cluster_adapter::ClusterDataAdapter;
use aikv::storage::types::{KvStorage, StoredValue};

struct MetaRaftProv(Arc<MetaRaftNode>);
impl aidb::cluster::lifecycle_manager::MetaRaftProvider for MetaRaftProv {
    fn get_cluster_meta(&self) -> aidb::cluster::ClusterMeta {
        self.0.get_cluster_meta()
    }
    fn get_slot_table(&self) -> aidb::cluster::SlotTable {
        self.0.get_slot_table()
    }
    fn get_migration_state(&self) -> Option<SlotMigrationState> {
        self.0.get_migration_state()
    }
}

async fn build_mgr() -> Arc<ClusterStateManager> {
    let meta_dir = TempDir::new().unwrap();
    let meta_db = DB::open(meta_dir.path(), Options::for_testing()).unwrap();
    let meta_net_factory = RaftNetworkClientFactory::new(1, 0, 30, 65536);
    let meta_raft = MetaRaftNode::new(
        RaftNodeConfig {
            node_id: 1,
            group_id: 0,
            election_timeout_min: 150,
            election_timeout_max: 300,
            heartbeat_interval: 30,
            snapshot_logs_since_last: 200,
            rpc_timeout_ms: 30,
            ..Default::default()
        },
        meta_db,
        meta_net_factory,
    )
    .await
    .unwrap();
    let meta_raft = Arc::new(meta_raft);
    let _ = meta_raft
        .initialize(vec![(1, "127.0.0.1:19100".into())])
        .await;

    meta_raft
        .propose(MetaRequest::RegisterNode {
            node_id: 1,
            rpc_addr: "http://127.0.0.1:19100".into(),
            client_addr: None,
            tags: HashMap::new(),
        })
        .await
        .unwrap();
    meta_raft
        .propose(MetaRequest::CreateGroup {
            group_id: 1,
            initial_replicas: vec![(1, true)],
        })
        .await
        .unwrap();
    meta_raft
        .propose(MetaRequest::CreateGroup {
            group_id: 2,
            initial_replicas: vec![(1, true)],
        })
        .await
        .unwrap();

    let mut group_nodes = HashMap::new();
    group_nodes.insert(1u64, vec![1u64]);
    group_nodes.insert(2u64, vec![1u64]);
    let mut node_addrs = HashMap::new();
    node_addrs.insert(1u64, "127.0.0.1:17001".to_string());
    let mut table = aidb::cluster::default_slot_table();
    for s in 0u16..8192 {
        table[s as usize] = SlotStatus::Assigned(1);
    }
    for s in 8192u16..16384 {
        table[s as usize] = SlotStatus::Assigned(2);
    }
    let router = Arc::new(Router::new(table, group_nodes, node_addrs));

    let dispatcher = Arc::new(RaftServiceDispatcher::new());
    let lifecycle = aidb::cluster::LifecycleManager::new(
        1,
        router.clone(),
        Arc::new(MetaRaftProv(meta_raft.clone())),
    )
    .with_tick_interval(Duration::from_millis(30));
    let multi_raft = Arc::new(MultiRaftNode::new_with_lifecycle(
        1,
        router.clone(),
        dispatcher,
        lifecycle,
    ));

    let data_dir = TempDir::new().unwrap();
    let shutdown_rx = multi_raft.start_lifecycle_with_data(LifecycleConfig {
        data_dir: data_dir.path().to_path_buf(),
        raft_node_config: RaftNodeConfig {
            node_id: 1,
            election_timeout_min: 150,
            election_timeout_max: 300,
            heartbeat_interval: 30,
            rpc_timeout_ms: 30,
            snapshot_logs_since_last: 200,
            ..Default::default()
        },
        options: Options::for_testing(),
        compaction_filter: None,
        compaction_removal_listener_factory: None,
    });

    for _ in 0..100 {
        let ids = multi_raft.local_group_ids();
        if ids.len() == 2
            && multi_raft.is_elected_leader_sync(1)
            && multi_raft.is_elected_leader_sync(2)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let mgr = ClusterStateManager {
        router: (*router).clone(),
        meta_raft,
        multi_raft,
        node_id: 1,
        config_epoch: AtomicU64::new(0),
        role: RwLock::new(ReplicationRole::Primary),
        local_group_leaders: {
            let mut l = HashMap::new();
            l.insert(1u64, true);
            l.insert(2u64, true);
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
    };
    let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
    std::mem::forget((meta_dir, data_dir, shutdown_rx));
    CLUSTER_STATE_MGR.get().unwrap().clone()
}

#[tokio::test]
async fn test_cluster_scan_across_groups_and_pagination() {
    let mgr = build_mgr().await;
    let local = AiDbEngine::open_for_testing(
        std::env::temp_dir().join(format!("aikv_scan_test_local_{}", std::process::id())),
    )
    .unwrap();
    let adapter = ClusterDataAdapter::new(local, ClusterDataAdapter::DEFAULT_EAGER_FLUSH);
    let kv_storage = KvStorageAdapter::new(adapter.clone());

    let val = StoredValue::string(b"val".to_vec());
    let raw_val = postcard::to_allocvec(&val).unwrap();

    // Group 1: 写入 apple, zebra
    let mut b1 = ThinWriteBatch::new();
    b1.put(b"apple".to_vec(), raw_val.clone());
    b1.put(b"zebra".to_vec(), raw_val.clone());
    mgr.multi_raft
        .propose_group(1, Request::WriteBatch(b1))
        .await
        .unwrap();

    // Group 2: 写入 banana, date
    let mut b2 = ThinWriteBatch::new();
    b2.put(b"banana".to_vec(), raw_val.clone());
    b2.put(b"date".to_vec(), raw_val.clone());
    mgr.multi_raft
        .propose_group(2, Request::WriteBatch(b2))
        .await
        .unwrap();

    // 1. 全量 SCAN 0 (count = 10): 跨 Group 多路归并，结果必须保持严格字典序递增
    let res = kv_storage.scan(0, 0, b"*", 10).await.unwrap();
    assert_eq!(res.cursor, 0);
    assert_eq!(
        res.keys,
        vec![
            b"apple".to_vec(),
            b"banana".to_vec(),
            b"date".to_vec(),
            b"zebra".to_vec()
        ]
    );

    // 2. 流式分页 SCAN (count = 2)
    let p1 = kv_storage.scan(0, 0, b"*", 2).await.unwrap();
    assert_eq!(p1.keys, vec![b"apple".to_vec(), b"banana".to_vec()]);
    assert_ne!(p1.cursor, 0);

    let p2 = kv_storage.scan(0, p1.cursor, b"*", 2).await.unwrap();
    assert_eq!(p2.keys, vec![b"date".to_vec(), b"zebra".to_vec()]);

    let p3 = kv_storage.scan(0, p2.cursor, b"*", 2).await.unwrap();
    assert!(p3.keys.is_empty());
    assert_eq!(p3.cursor, 0);

    // 3. KEYS *: for_each_prefix 跨 Group 收集
    let mut all_keys = kv_storage.keys(0, b"*").await.unwrap();
    all_keys.sort();
    assert_eq!(
        all_keys,
        vec![
            b"apple".to_vec(),
            b"banana".to_vec(),
            b"date".to_vec(),
            b"zebra".to_vec()
        ]
    );

    // 4. delete_range: 全节点范围删除清空
    adapter
        .delete_range(b"".to_vec(), b"".to_vec())
        .await
        .unwrap();
    let empty_keys = kv_storage.keys(0, b"*").await.unwrap();
    assert!(empty_keys.is_empty());
}
