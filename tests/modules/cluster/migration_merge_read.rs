//! @component aikv-cluster
//! FIX-0056-A1 aikv 侧合并读 / 迁移期写 集成测试.
//!
//! 使用真实的单节点 `MetaRaftNode` + 双 group (source=1, target=2) 的
//! `MultiRaftNode` (lifecycle 驱动本地 Raft group 创建), 通过
//! `ClusterDataAdapter` (`StorageAdapter` trait) 驱动 GET/SET/DEL, 验证:
//!
//! 1. Prepare/Migrating (Copying) 期 SET 未拷贝完的 key, GET 立即可见新值.
//! 2. target miss 的 DEL 仍一律 propose 并记录 tombstone, 之后 GET 不复活.
//! 3. DEL 后模拟 bulk-copy `PutConditional` 不复活; ReadyToCommit 后仍 miss.
//! 9. Frozen 合并读回归: 不再纯 source, 能看到只落在 target 的新写.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use aidb::cluster::multi_raft_node::LifecycleConfig;
use aidb::cluster::router::key_to_slot;
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
use aikv::storage::adapter::StorageAdapter;
use aikv::storage::aidb::AiDbEngine;
use aikv::storage::cluster_adapter::ClusterDataAdapter;

/// 所有测试 key 共用 hash tag `{m}`, 确保落在同一个被迁移的 slot.
const PREPARE_KEY: &[u8] = b"prepare_key{m}";
const DEL_KEY: &[u8] = b"del_key{m}";

/// 搭建单节点、两个真实数据 group (1=source, 2=target) 的最小集群, 返回
/// 就绪的 `ClusterStateManager` (已注册进 `CLUSTER_STATE_MGR`).
async fn build_mgr() -> Arc<ClusterStateManager> {
    let slot = key_to_slot(PREPARE_KEY);
    assert_eq!(
        slot,
        key_to_slot(DEL_KEY),
        "test keys must share a slot (hash tag)"
    );

    let meta_dir = TempDir::new().unwrap();
    let meta_db = DB::open(meta_dir.path().join("meta"), Options::for_testing()).unwrap();
    let meta_factory =
        RaftNetworkClientFactory::new(1, aidb::cluster::METARAFT_GROUP_ID, 30, 65 * 1024 * 1024);
    let meta_cfg = RaftNodeConfig {
        node_id: 1,
        group_id: aidb::cluster::METARAFT_GROUP_ID,
        election_timeout_min: 150,
        election_timeout_max: 300,
        heartbeat_interval: 30,
        rpc_timeout_ms: 30,
        snapshot_logs_since_last: 200,
        ..Default::default()
    };
    let meta_raft = Arc::new(
        MetaRaftNode::new(meta_cfg, meta_db, meta_factory)
            .await
            .unwrap(),
    );
    meta_raft
        .initialize(vec![(1, "http://127.0.0.1:1".into())])
        .await
        .unwrap();
    for _ in 0..50 {
        if meta_raft.is_leader().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        meta_raft.is_leader().await,
        "meta raft should elect itself leader"
    );

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
    meta_raft
        .propose(MetaRequest::AssignSlots {
            group_id: 1,
            slots: vec![slot],
        })
        .await
        .unwrap();

    let mut group_nodes = HashMap::new();
    group_nodes.insert(1u64, vec![1u64]);
    group_nodes.insert(2u64, vec![1u64]);
    let mut node_addrs = HashMap::new();
    node_addrs.insert(1u64, "127.0.0.1:17001".to_string());
    let mut table = aidb::cluster::default_slot_table();
    table[slot as usize] = SlotStatus::Assigned(1);
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
    let _shutdown_rx = multi_raft.start_lifecycle_with_data(LifecycleConfig {
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

    // 等两个 group 都在本地创建出来并选出 leader.
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
    assert_eq!(
        multi_raft.local_group_ids().len(),
        2,
        "both groups must be created locally"
    );

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
        _watcher_shutdown: parking_lot::Mutex::new(None),
        _auto_save_shutdown: parking_lot::Mutex::new(None),
        slot_groups: ClusterStateManager::new_slot_groups(),
        has_active_migration: AtomicBool::new(false),
    };
    let _ = CLUSTER_STATE_MGR.set(Arc::new(mgr));
    let _dirs = (meta_dir, data_dir);
    // 泄漏临时目录守卫的所有权到调用方生命周期之外是不安全的; 用 Box::leak
    // 把 TempDir 保留到进程退出 (测试进程本身很快结束, 可接受).
    std::mem::forget(_dirs);
    CLUSTER_STATE_MGR.get().unwrap().clone()
}

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

#[tokio::test]
async fn migration_merge_read_and_del_tombstone_lifecycle() {
    let mgr = build_mgr().await;
    let slot = key_to_slot(PREPARE_KEY);
    let local = AiDbEngine::open_for_testing(
        std::env::temp_dir().join(format!("aikv_merge_read_local_{}", std::process::id())),
    )
    .unwrap();
    let adapter = ClusterDataAdapter::new(local, ClusterDataAdapter::DEFAULT_EAGER_FLUSH);

    let prepare_key_enc = AiDbEngine::encode_key(0, PREPARE_KEY);
    let del_key_enc = AiDbEngine::encode_key(0, DEL_KEY);

    // ── Begin migration: source=1 → target=2 (Prepare 相位, Copying). ──
    mgr.meta_raft
        .propose(MetaRequest::BeginSlotMigration {
            source_group: 1,
            target_group: 2,
            slots: vec![slot],
        })
        .await
        .unwrap();
    assert!(matches!(
        mgr.meta_raft.get_migration_state(),
        Some(SlotMigrationState::Prepare { .. })
    ));
    let epoch = mgr
        .migration_epoch()
        .expect("BeginSlotMigration 后 migration_epoch 必须可读");

    // IMPORTING 窗口: 本节点同时是 source/target (单节点测试), 用
    // importing_slots 让写路径明确落到 target — 与生产环境 target 节点
    // `CLUSTER SETSLOT <slot> IMPORTING` 后的路由结果等价.
    mgr.importing_slots.write().insert(slot, 1);

    // ═══════════════════════════════════════════════════════════════
    // 测试计划 #1: Prepare 期 SET 未拷贝完的 key, GET 立即可见新值.
    // ═══════════════════════════════════════════════════════════════
    adapter
        .set(prepare_key_enc.clone(), b"v1".to_vec())
        .await
        .expect("set during Copying should succeed via MigrationWrite");
    // 必须落在 target (group 2), 走 Request::MigrationWrite.
    assert_eq!(
        mgr.multi_raft
            .get_key_from_group(2, &prepare_key_enc)
            .await
            .unwrap(),
        Some(b"v1".to_vec()),
        "MigrationWrite Put must land on target group"
    );
    let got = adapter.get(prepare_key_enc.clone()).await.unwrap();
    assert_eq!(
        got,
        Some(b"v1".to_vec()),
        "merge-read must see the just-written value on target"
    );

    // ═══════════════════════════════════════════════════════════════
    // 测试计划 #2: target miss 的 DEL 仍一律 propose, 记录 tombstone, 之后 GET miss.
    // ═══════════════════════════════════════════════════════════════
    // del_key 只存在于 source (模拟尚未被拷贝到 target 的旧数据).
    mgr.multi_raft
        .propose_group(
            1,
            Request::Put {
                key: del_key_enc.clone(),
                value: b"old-on-source".to_vec(),
            },
        )
        .await
        .unwrap();
    // 合并读: target miss → fallback source, 应看到 source 的值.
    let got = adapter.get(del_key_enc.clone()).await.unwrap();
    assert_eq!(
        got,
        Some(b"old-on-source".to_vec()),
        "merge-read must fall back to source when target has never seen the key"
    );

    // target 从未有过 del_key: 现网短路 (存在才 propose) 必须被关闭 —
    // DEL 仍必须一律 propose 并记录 tombstone.
    let existed = adapter
        .delete(del_key_enc.clone())
        .await
        .expect("delete during Copying should succeed via MigrationWrite");
    assert!(
        existed,
        "DEL return value should reflect merge-read existence (found on source)"
    );
    assert_eq!(
        mgr.multi_raft
            .get_key_from_group(2, &del_key_enc)
            .await
            .unwrap(),
        None,
        "target must not have the key after DEL (was never copied, nothing to delete on sm_key)"
    );
    let tombstone = mgr
        .multi_raft
        .get_migration_tombstone_remote(2, epoch, &del_key_enc)
        .await
        .unwrap();
    assert_eq!(
        tombstone,
        Some(aidb::cluster::MigOp::Del),
        "Del tombstone must be recorded on target even though target never had the key"
    );

    // 合并读必须看到 tombstone=Del, 不得回落 source (否则会复活 old-on-source).
    let got = adapter.get(del_key_enc.clone()).await.unwrap();
    assert_eq!(
        got, None,
        "merge-read must return miss after Del tombstone, not resurrect from source"
    );

    // ═══════════════════════════════════════════════════════════════
    // 测试计划 #3: DEL 后模拟 bulk-copy PutConditional 不复活; ReadyToCommit 后仍 miss.
    // ═══════════════════════════════════════════════════════════════
    mgr.multi_raft
        .propose_group(
            2,
            Request::PutConditional {
                key: del_key_enc.clone(),
                value: b"old-on-source".to_vec(),
                migration_epoch: Some(epoch),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        mgr.multi_raft
            .get_key_from_group(2, &del_key_enc)
            .await
            .unwrap(),
        None,
        "PutConditional must respect the Del tombstone and skip the copy"
    );
    let got = adapter.get(del_key_enc.clone()).await.unwrap();
    assert_eq!(
        got, None,
        "merge-read must still miss after a skipped PutConditional resurrection attempt"
    );

    // 收尾到 ReadyToCommit (纯 target 读, 无 source fallback): 仍应 miss.
    mgr.meta_raft
        .set_migration_state(Some(SlotMigrationState::ReadyToCommit {
            source_group: 1,
            target_group: 2,
            slots: vec![slot],
        }));
    let got = adapter.get(del_key_enc.clone()).await.unwrap();
    assert_eq!(
        got, None,
        "ReadyToCommit pure-target read must still miss del_key"
    );

    // ═══════════════════════════════════════════════════════════════
    // 测试计划 #9: Frozen 合并读回归 — 不再纯 source, 能看到只落在 target 的新写.
    // ═══════════════════════════════════════════════════════════════
    mgr.meta_raft
        .set_migration_state(Some(SlotMigrationState::Frozen {
            source_group: 1,
            target_group: 2,
            slots: vec![slot],
        }));
    // prepare_key 只存在于 target (从未 propose 到 source), 若 Frozen 读
    // 仍是 A2 的"纯 source", 这里会看到 miss — 必须是 Some(v1).
    assert_eq!(
        mgr.multi_raft
            .get_key_from_group(1, &prepare_key_enc)
            .await
            .unwrap(),
        None,
        "sanity: prepare_key must never have landed on source"
    );
    let got = adapter.get(prepare_key_enc.clone()).await.unwrap();
    assert_eq!(
        got,
        Some(b"v1".to_vec()),
        "Frozen merge-read must see target-only writes (A2 'source-only' assertion no longer holds)"
    );
}
