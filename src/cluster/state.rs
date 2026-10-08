//! 集群全局状态: 全局单例 `CLUSTER_STATE_MGR` 与 `ClusterStateManager` 管理器.
//! 聚合 aidb 的 MetaRaft / MultiRaft / Router 引用与本地缓存 (group leader 缓存、
//! `importing_slots`、`role`、迁移目标 leader), 供同步路由决策与 CLUSTER 子命令读取.
//!
//! # 职责
//!
//! - 全局单例 `CLUSTER_STATE_MGR` (`OnceLock`), `init_cluster` 装配成功后才 `set`.
//! - `refresh` / `apply_observed_group_leader`: 从 MetaRaft / MultiRaft 观测刷新本地 leader 缓存.
//! - `migration_target_leader`: ASK 重定向目标 (覆盖 Prepare / Migrating / Frozen / ReadyToCommit).
//! - `migration_epoch`: 活跃迁移 epoch, 供 `Request::MigrationWrite` / tombstone 定位.
//!
//! # Invariant
//!
//! - `CLUSTER_STATE_MGR` 未 `set` 时 `cluster_route` 跳过 (非 cluster 单机行为).
//! - `ClusterRouter::decide` 只读本管理器缓存 + MetaRaft 快照, 不 await OpenRaft.
//! - 权威拓扑始终来自 MetaRaft; 本模块只维护本地视图缓存.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use parking_lot::RwLock;

use crate::cluster::announce::AnnounceResolver;
use crate::config::AnnounceMode;
use aidb::cluster::meta_types::{ClusterMeta, SlotStatus, SlotTable};
use aidb::cluster::slot_migration::SlotMigrationManager;
use aidb::cluster::{MembershipCoordinator, MetaRaftNode, MultiRaftNode, Router};

/// 默认数据面端口偏移 (与 Redis Cluster @cport 约定一致).
pub const DEFAULT_DATA_PORT_OFFSET: u16 = 10000;

/// 收集 slot table 中负责 slot 的 group 集合 (Assigned / Migrating).
pub(crate) fn groups_with_slots(slot_table: &SlotTable) -> HashSet<u64> {
    slot_table
        .iter()
        .filter_map(|status| match status {
            SlotStatus::Assigned(gid) | SlotStatus::Migrating(gid) => Some(*gid),
            SlotStatus::Unallocated => None,
        })
        .collect()
}

/// 本机视角集群健康派生 (纯函数): 负责 slot 的 group 中, 若有本节点作为
/// leader 的 group 已失去 quorum (探活判定失败), 则视为不健康.
///
/// 规则:
/// - 对每个负责 slot 的 group: 若 `quorum_status.get(gid) == Some(false)` → false.
/// - `quorum_status` 无该 group 记录 (本机非其 leader) → 不判定 (由 MetaRaft
///   `is_leader` 正常维护, 分区时 leader 在多数派侧转移).
/// - slot 指向的 group 不在 `meta.groups` (孤儿 group) → false, 与
///   `compute_cluster_state` 的 `slots_map_to_known_groups` 判定对齐, 避免
///   router 门开放而 CLUSTER INFO 报 fail 的分裂.
/// - 本机不负责任何 group → true.
pub fn derive_cluster_ok(
    quorum_status: &HashMap<u64, bool>,
    slot_table: &SlotTable,
    meta: &ClusterMeta,
) -> bool {
    groups_with_slots(slot_table).iter().all(|gid| {
        if !meta.groups.contains_key(gid) {
            return false;
        }
        quorum_status.get(gid) != Some(&false)
    })
}

/// 本节点角色
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationRole {
    Primary,
    Replica { primary_id: u64 },
}

/// 集群状态管理器 — 全局单例, 缓存路由信息供同步的 decide() 使用.
///
/// Router 是值类型但内部使用 Arc<RwLock<...>> (AiDb Router 实现了 Clone,
/// 克隆只拷贝内部 Arc 指针, 与 LifecycleManager 等共享同一份状态).
pub struct ClusterStateManager {
    pub router: Router,
    pub meta_raft: std::sync::Arc<MetaRaftNode>,
    pub multi_raft: std::sync::Arc<MultiRaftNode>,
    pub node_id: u64,
    pub config_epoch: AtomicU64,
    pub role: RwLock<ReplicationRole>,
    /// 本地 Group Leader 缓存 {group_id → is_leader}
    pub local_group_leaders: RwLock<HashMap<u64, bool>>,
    /// 本节点作为 leader 的 group → 是否仍保有 quorum (LeaderChangeWatcher 探活注入).
    /// 无记录 (非 leader 或探活未 tick) → 视为有效.
    pub group_quorum_ok: RwLock<HashMap<u64, bool>>,
    /// 本机视角集群健康标志: 由 `derive_cluster_ok` 从探活状态派生.
    /// 初始 true (探活未注入前不误伤).
    pub cluster_state_ok: AtomicBool,
    /// 成员协调器 (CLUSTER MEET/FORGET/REPLICATE 使用)
    pub membership_coordinator: Option<std::sync::Arc<MembershipCoordinator>>,
    /// 槽迁移管理器 (CLUSTER SETSLOT 使用)
    pub slot_migration_manager: Option<std::sync::Arc<SlotMigrationManager>>,
    /// 本地导入中的 slots (用于 `redis-cli --cluster check` 兼容).
    /// maps slot → source_node_id that initiated the import.
    pub importing_slots: parking_lot::RwLock<std::collections::HashMap<u16, u64>>,
    /// 数据目录 (用于 CONFIG REWRITE 持久化到 nodes.conf)
    pub data_dir: Option<std::path::PathBuf>,
    /// 数据面总线端口偏移.
    pub data_port_offset: u16,
    /// 客户端地址通告 (CLUSTER SLOTS / MOVED; 仅进程 env, 不持久化).
    pub announce_resolver: AnnounceResolver,
    /// Metrics (注入用于 gossip/failover 计数, 初始化时可 None).
    pub metrics: Option<std::sync::Arc<crate::server::metrics::ServerMetrics>>,
    /// Shutdown signal for LeaderChangeWatcher (held to keep sender alive).
    pub _watcher_shutdown: parking_lot::Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    /// Shutdown signal for ConfigAutoSave (held to keep sender alive).
    pub _auto_save_shutdown: parking_lot::Mutex<Option<tokio::sync::watch::Sender<bool>>>,
}

impl ClusterStateManager {
    pub fn new(
        router: Router,
        meta_raft: std::sync::Arc<MetaRaftNode>,
        multi_raft: std::sync::Arc<MultiRaftNode>,
        node_id: u64,
        announce_mode: AnnounceMode,
    ) -> Self {
        let mgr = Self {
            router,
            meta_raft,
            multi_raft,
            node_id,
            config_epoch: AtomicU64::new(0),
            role: RwLock::new(ReplicationRole::Primary),
            local_group_leaders: RwLock::new(HashMap::new()),
            group_quorum_ok: RwLock::new(HashMap::new()),
            cluster_state_ok: AtomicBool::new(true),
            membership_coordinator: None,
            slot_migration_manager: None,
            data_dir: None,
            data_port_offset: DEFAULT_DATA_PORT_OFFSET,
            announce_resolver: AnnounceResolver::new(announce_mode),
            importing_slots: parking_lot::RwLock::new(std::collections::HashMap::new()),
            metrics: None,
            _watcher_shutdown: parking_lot::Mutex::new(None),
            _auto_save_shutdown: parking_lot::Mutex::new(None),
        };
        mgr.refresh();
        mgr
    }

    /// 注入 MembershipCoordinator (在集群初始化完成后调用).
    pub fn set_membership_coordinator(&mut self, mc: std::sync::Arc<MembershipCoordinator>) {
        self.membership_coordinator = Some(mc);
    }

    /// 注入 SlotMigrationManager (在集群初始化完成后调用).
    pub fn set_slot_migration_manager(&mut self, sm: std::sync::Arc<SlotMigrationManager>) {
        self.slot_migration_manager = Some(sm);
    }

    /// 从 MetaRaft 刷新本地 Leader 缓存.
    pub fn refresh(&self) {
        let meta = self.meta_raft.get_cluster_meta();
        if meta.groups.is_empty() {
            return;
        }
        let mut leaders = self.local_group_leaders.write();
        leaders.clear();
        for (gid, group) in &meta.groups {
            let is_leader = group
                .replicas
                .iter()
                .any(|r| r.node_id == self.node_id && r.is_leader);
            leaders.insert(*gid, is_leader);
        }
    }

    pub fn is_local_group_leader(&self, group_id: u64) -> bool {
        if self.router.get_group_leader(group_id) == Some(self.node_id)
            && self.multi_raft.is_group_local(group_id)
        {
            return true;
        }
        self.local_group_leaders
            .read()
            .get(&group_id)
            .copied()
            .unwrap_or(false)
    }

    /// 获取当前迁移目标 group 的 leader 信息, 用于 ASK 重定向.
    ///
    /// 返回 `(target_group, leader_node_id, client_addr)` 或 None.
    /// 覆盖 Prepare / Migrating / Frozen / ReadyToCommit 全部活跃相位.
    pub fn migration_target_leader(&self) -> Option<(u64, u64, String)> {
        let mig_state = self.meta_raft.get_migration_state();
        let target_group = match &mig_state {
            Some(aidb::cluster::SlotMigrationState::Prepare { target_group, .. })
            | Some(aidb::cluster::SlotMigrationState::Migrating { target_group, .. })
            | Some(aidb::cluster::SlotMigrationState::Frozen { target_group, .. })
            | Some(aidb::cluster::SlotMigrationState::ReadyToCommit { target_group, .. }) => {
                *target_group
            }
            None => return None,
        };
        let leader = self.router.get_group_leader(target_group)?;
        let addr = self.announce_resolver.redirect_addr(leader, &self.router)?;
        Some((target_group, leader, addr))
    }

    /// FIX-0056-A1: 当前活跃迁移的 epoch (= `BeginSlotMigration` 时的
    /// `cluster_meta.version`), 用于 `Request::MigrationWrite` /
    /// `get_migration_tombstone_remote` 定位 target group 上的 oplog 前缀.
    /// 无活跃迁移时为 `None`.
    pub fn migration_epoch(&self) -> Option<u64> {
        self.meta_raft.get_migration_epoch()
    }

    /// 应用本地 MultiRaft 观测到的 group leader, 立即刷新路由缓存.
    pub fn apply_observed_group_leader(&self, group_id: u64, leader_id: u64) {
        self.router.update_group_leader(group_id, leader_id);
        let is_local = leader_id == self.node_id;
        self.local_group_leaders.write().insert(group_id, is_local);
    }

    /// 应用探活观测的 group quorum 状态, 并用 `derive_cluster_ok` 派生集群健康.
    /// 参数化 (status 全量覆盖 + 注入 slot_table/meta), 便于纯函数单测.
    pub fn apply_observed_group_quorum(
        &self,
        status: HashMap<u64, bool>,
        slot_table: SlotTable,
        meta: ClusterMeta,
    ) {
        let ok = derive_cluster_ok(&status, &slot_table, &meta);
        *self.group_quorum_ok.write() = status;
        self.cluster_state_ok.store(ok, Ordering::Relaxed);
    }

    /// 本节点作为某 group leader 是否仍保有 quorum (无记录默认有效).
    pub fn group_quorum_ok(&self, group_id: u64) -> bool {
        self.group_quorum_ok
            .read()
            .get(&group_id)
            .copied()
            .unwrap_or(true)
    }

    /// 本机视角集群是否健康 (fail 时路由层拒绝读写).
    pub fn cluster_state_ok(&self) -> bool {
        self.cluster_state_ok.load(Ordering::Relaxed)
    }
}

/// 全局集群状态管理器
pub static CLUSTER_STATE_MGR: OnceLock<std::sync::Arc<ClusterStateManager>> = OnceLock::new();

fn unheard_secs(group: u64) -> f64 {
    use std::time::Instant;
    static FIRST: std::sync::LazyLock<parking_lot::Mutex<HashMap<u64, Instant>>> =
        std::sync::LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));
    let mut map = FIRST.lock();
    map.entry(group)
        .or_insert_with(Instant::now)
        .elapsed()
        .as_secs_f64()
}

/// 导出时读每个本地 data group 的 OpenRaft metrics. 没有集群时是空.
pub fn raft_export_views() -> Vec<(String, crate::server::export::RaftExportView)> {
    use crate::server::export::RaftExportView;
    use openraft::rt::watch::WatchReceiver;
    use openraft::ServerState;

    let Some(mgr) = CLUSTER_STATE_MGR.get() else {
        return Vec::new();
    };
    let groups = mgr.multi_raft.get_groups();
    let guard = groups.read();
    let mut out = Vec::with_capacity(guard.len());
    for (gid, node) in guard.iter() {
        let receiver = node.raft().metrics();
        let metrics = receiver.borrow_watched();
        let role = match metrics.state {
            ServerState::Leader => "leader",
            ServerState::Follower => "follower",
            ServerState::Candidate => "candidate",
            ServerState::Learner => "learner",
            ServerState::Shutdown => "shutdown",
        };
        let is_leader = metrics.state == ServerState::Leader;
        let min_voter_matched = if is_leader {
            metrics
                .membership_config
                .voter_ids()
                .filter(|voter| *voter != metrics.id)
                .map(|voter| {
                    metrics
                        .replication
                        .as_ref()
                        .and_then(|map| map.get(&voter))
                        .and_then(|id| id.as_ref().map(|log| log.index))
                        .unwrap_or(0)
                })
                .min()
                .unwrap_or(0)
        } else {
            0
        };
        let heard = aidb::cluster::contact::leader_contact_age_secs(*gid);
        let leader_contact_age_secs =
            crate::server::export::contact_age_for_export(is_leader, heard, unheard_secs(*gid));
        out.push((
            gid.to_string(),
            RaftExportView {
                commit_index: metrics
                    .local_committed
                    .as_ref()
                    .map(|id| id.index)
                    .unwrap_or(0),
                applied_index: metrics
                    .last_applied
                    .as_ref()
                    .map(|id| id.index)
                    .unwrap_or(0),
                last_log_index: metrics.last_log_index.unwrap_or(0),
                min_voter_matched,
                is_leader,
                leader_contact_age_secs,
                role,
            },
        ));
    }
    out
}
