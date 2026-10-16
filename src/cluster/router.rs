//! 同步集群路由决策: `ClusterRouter::decide` 依据 MetaRaft 迁移相位与 Router 缓存,
//! 决定 key 请求是本地执行 (`Execute`) 还是重定向 (`Moved` / `Ask` / `TryAgain` /
//! `ClusterDown`), 由 `CommandRouter::cluster_route` 调用.
//!
//! # 决策树
//!
//! ```text
//! decide(key, cmd_type, asking, readonly)
//!   ├─ CLUSTER_STATE_MGR 未 set → ClusterDown
//!   ├─ migration_phase_for_slot 命中 (F-056 / F-056-A1):
//!   │    ├─ Frozen / ReadyToCommit: Write → TryAgain
//!   │    │                          Read  → 导向 target group (合并读/纯 target 由 adapter 区分)
//!   │    └─ Copying (Prepare/Migrating): Read 且 target 本地 + (asking|importing) → Execute
//!   │                                   Read 否则 → decide_target_read (合并读导向 target)
//!   │                                   Write 继续落到下方 v7 逻辑
//!   ├─ router.route_key 失败 → 刷新缓存重试一次 → 仍失败 → ClusterDown
//!   ├─ Write + importing_slots 含 slot → Execute (IMPORTING 窗口, 不含 Frozen/Ready)
//!   └─ SlotStatus:
//!        ├─ Assigned:   本地可执行 (group leader 或 readonly 读)? Execute
//!       │                否则刷新后再判 → Moved (group leader)
//!        ├─ Migrating:  本机 source → Read: Execute / Write: Ask(target) / Admin: Execute
//!       │                本机 target → asking|importing: Execute, 否则 Moved(source)
//!       │                其他节点   → ClusterDown (migration in progress)
//!        └─ Unallocated: 刷新重试; 新 Assign 且本地 leader → Execute, 否则 Moved / ClusterDown
//! ```
//!
//! # Invariant
//!
//! - `decide` 同步: 只读缓存 + MetaRaft 快照, 不 await OpenRaft.
//! - readonly replica 读: `READONLY` + `Read` + 本地 group → 本地读; 写仍 Moved 到 leader.
//! - `check_cross_slot`: 多 key 命令须同 slot, 否则 CROSSSLOT (MSET key 在偶数下标).
//! - `scan_tryagain_if_migrating`: SCAN 族在任意活跃迁移期间 TRYAGAIN (admin 白名单短路前拦截).

use std::collections::HashMap;

use aidb::cluster::meta_types::SlotMigrationState;
use aidb::cluster::router::key_to_slot;
use aidb::cluster::SlotStatus;

use crate::cluster::state::{ClusterStateManager, CLUSTER_STATE_MGR};

/// Frozen / ReadyToCommit 写冻结时返回给客户端的 RESP 错误正文.
pub const TRYAGAIN_MIGRATION: &str = "TRYAGAIN Migration in progress, retry later";

/// 集群路由决策结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    Execute,
    Moved {
        slot: u16,
        node_id: u64,
        addr: String,
    },
    Ask {
        slot: u16,
        node_id: u64,
        addr: String,
    },
    /// F-056: Frozen / ReadyToCommit 期间客户端写冻结.
    TryAgain {
        reason: String,
    },
    ClusterDown(String),
}

/// 命令读/写类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandType {
    Read,
    Write,
    Admin,
}

/// F-056 / F-056-A1 迁移相位对客户端路由的影响 (`decide` 与 `ClusterDataAdapter` 共用).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationRoutePhase {
    /// Prepare / Migrating: 写 ASK target (v7); 读为合并读 (A1, 导向 target).
    Copying {
        source_group: u64,
        target_group: u64,
    },
    /// 写 TRYAGAIN; 读为合并读 (A1 覆盖 A2 的"仍走 source").
    Frozen {
        source_group: u64,
        target_group: u64,
    },
    /// 写 TRYAGAIN; 读必须切 target, 纯 target 无 source fallback (A2 起点).
    ReadyToCommit {
        source_group: u64,
        target_group: u64,
    },
}

impl MigrationRoutePhase {
    pub fn source_group(self) -> u64 {
        match self {
            Self::Copying { source_group, .. }
            | Self::Frozen { source_group, .. }
            | Self::ReadyToCommit { source_group, .. } => source_group,
        }
    }

    pub fn target_group(self) -> u64 {
        match self {
            Self::Copying { target_group, .. }
            | Self::Frozen { target_group, .. }
            | Self::ReadyToCommit { target_group, .. } => target_group,
        }
    }

    pub fn writes_frozen(self) -> bool {
        matches!(self, Self::Frozen { .. } | Self::ReadyToCommit { .. })
    }
}

/// 若 `slot` 属于当前活跃迁移, 返回相位决策; 否则 None.
pub fn migration_phase_for_slot(
    mgr: &ClusterStateManager,
    slot: u16,
) -> Option<MigrationRoutePhase> {
    migration_phase_from_state(mgr.meta_raft.get_migration_state().as_ref(), slot)
}

/// 纯函数版: 从 `migration_state` 解析 slot 相位 (供 decide / adapter / 单测共用).
pub fn migration_phase_from_state(
    state: Option<&SlotMigrationState>,
    slot: u16,
) -> Option<MigrationRoutePhase> {
    let ms = state?;
    let (phase, slots) = match ms {
        SlotMigrationState::Prepare {
            source_group,
            target_group,
            slots,
        } => (
            MigrationRoutePhase::Copying {
                source_group: *source_group,
                target_group: *target_group,
            },
            slots,
        ),
        SlotMigrationState::Migrating {
            source_group,
            target_group,
            slots,
            ..
        } => (
            MigrationRoutePhase::Copying {
                source_group: *source_group,
                target_group: *target_group,
            },
            slots,
        ),
        SlotMigrationState::Frozen {
            source_group,
            target_group,
            slots,
        } => (
            MigrationRoutePhase::Frozen {
                source_group: *source_group,
                target_group: *target_group,
            },
            slots,
        ),
        SlotMigrationState::ReadyToCommit {
            source_group,
            target_group,
            slots,
        } => (
            MigrationRoutePhase::ReadyToCommit {
                source_group: *source_group,
                target_group: *target_group,
            },
            slots,
        ),
    };
    if slots.contains(&slot) {
        Some(phase)
    } else {
        None
    }
}

/// 从 Raft 状态机刷新路由缓存.
fn refresh_router_cache(mgr: &ClusterStateManager) {
    let meta = mgr.meta_raft.get_cluster_meta();
    let slot_table = mgr.meta_raft.get_slot_table();
    let mut group_nodes: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut group_leaders: HashMap<u64, u64> = HashMap::new();
    let mut node_addrs: HashMap<u64, String> = HashMap::new();
    for (gid, group) in &meta.groups {
        group_nodes.insert(*gid, group.replicas.iter().map(|r| r.node_id).collect());
        if let Some(leader) = group.replicas.iter().find(|r| r.is_leader) {
            group_leaders.insert(*gid, leader.node_id);
        }
    }
    for (nid, node) in &meta.nodes {
        let addr = node
            .client_addr
            .clone()
            .unwrap_or_else(|| node.rpc_addr.clone());
        node_addrs.insert(*nid, addr);
    }
    mgr.router
        .refresh_from_data(slot_table, group_nodes, node_addrs, group_leaders);
}

/// 集群路由决策器
pub struct ClusterRouter;

impl ClusterRouter {
    /// 决定如何处理 key 的请求.
    /// 同步调用, 所有状态来自 ClusterStateManager 缓存.
    pub fn decide(
        key: &[u8],
        cmd_type: CommandType,
        asking: bool,
        readonly: bool,
    ) -> RouteDecision {
        let Some(mgr) = CLUSTER_STATE_MGR.get() else {
            return RouteDecision::ClusterDown("CLUSTERDOWN Cluster not initialized".into());
        };

        // 本机视角 cluster_state:fail (探活判定本节点为 leader 的 slot 组已失去
        // quorum) → 拒绝所有读写 (Redis 语义). 置于迁移相位分支之前, 分区期间
        // 迁移本应中断; admin 命令 (CLUSTER INFO 等) 在 cluster_route 白名单绕过.
        if !mgr.cluster_state_ok() {
            return RouteDecision::ClusterDown("CLUSTERDOWN The cluster is down".into());
        }

        let slot = key_to_slot(key);

        // F-056 / F-056-A1: 最先查 migration_state 相位, 覆盖 ASKING / importing_slots 旁路.
        // A1: Frozen 覆盖 A2 —— 读不再纯 source, 与 Copying/ReadyToCommit 一样统一导向
        // target group (由 adapter 决定纯 target 读还是合并读, 见 相位总表).
        if let Some(phase) = migration_phase_for_slot(mgr, slot) {
            match phase {
                MigrationRoutePhase::Frozen { target_group, .. }
                | MigrationRoutePhase::ReadyToCommit { target_group, .. } => {
                    if cmd_type == CommandType::Write {
                        return try_again();
                    }
                    return decide_target_read(mgr, slot, target_group, &cmd_type, readonly);
                }
                MigrationRoutePhase::Copying { target_group, .. } => {
                    if cmd_type == CommandType::Read {
                        // IMPORTING 窗口: 本机是 target 且客户端已 ASKING (或该 slot
                        // 在本地 importing_slots 追踪中) 时直接信任 ASK 协议就地执行,
                        // 与写路径的 IMPORTING 放行语义对齐 (见下方 SlotStatus::Migrating
                        // 分支). 否则走合并读路由 (导向 target leader).
                        if mgr.multi_raft.is_group_local(target_group)
                            && (asking || mgr.importing_slots.read().contains_key(&slot))
                        {
                            return RouteDecision::Execute;
                        }
                        return decide_target_read(mgr, slot, target_group, &cmd_type, readonly);
                    }
                    // Write (ASK 到 target) / Admin: 继续下方 v7 逻辑.
                }
            }
        }

        let (group_id, status) = match mgr.router.route_key(key) {
            Ok(r) => r,
            Err(_) => {
                // Router cache may be stale (lifecycle tick hasn't caught up yet).
                // Refresh from Raft state machine and retry once.
                refresh_router_cache(mgr);
                match mgr.router.route_key(key) {
                    Ok(r) => r,
                    Err(e) => return RouteDecision::ClusterDown(e.to_string()),
                }
            }
        };

        // IMPORTING window: router cache may still show Assigned while the target
        // node tracks the slot locally via CLUSTER SETSLOT IMPORTING.
        // Frozen/Ready 已在上方拦截; 此处仅 Prepare/Migrating (或无 Meta 迁移态).
        if cmd_type == CommandType::Write && mgr.importing_slots.read().contains_key(&slot) {
            return RouteDecision::Execute;
        }

        match status {
            SlotStatus::Assigned(_) => {
                if should_execute_locally(mgr, group_id, &cmd_type, readonly) {
                    RouteDecision::Execute
                } else {
                    let has_group_meta = mgr
                        .meta_raft
                        .get_cluster_meta()
                        .groups
                        .contains_key(&group_id);
                    if has_group_meta {
                        // Leader 可能刚切换; 从 MetaRaft 刷新后再判一次.
                        mgr.refresh();
                        refresh_router_cache(mgr);
                        if should_execute_locally(mgr, group_id, &cmd_type, readonly) {
                            return RouteDecision::Execute;
                        }
                    }
                    leader_moved(mgr, group_id, slot)
                }
            }
            SlotStatus::Migrating(source_group) => {
                if mgr.multi_raft.is_group_local(source_group) {
                    // ASK-Redirect-Migrate (v7): reads stay on source
                    // (source keeps full data), writes are ASK-redirected to
                    // target so that post-commit the target holds the latest
                    // values. Per-key marking is unnecessary — the slot-level
                    // redirect eliminates every per-key TOCTOU race.
                    if !should_execute_locally(mgr, source_group, &cmd_type, readonly) {
                        return leader_moved(mgr, source_group, slot);
                    }
                    match cmd_type {
                        CommandType::Read => return RouteDecision::Execute,
                        CommandType::Write => return ask_target(mgr, slot),
                        CommandType::Admin => return RouteDecision::Execute,
                    }
                }
                let mig_state = mgr.meta_raft.get_migration_state();
                let target_group = match migration_phase_from_state(mig_state.as_ref(), slot) {
                    Some(phase) => phase.target_group(),
                    None => {
                        // Slot 标为 Migrating 但 Meta 无匹配相位: 仍尝试从任意活跃态取 target.
                        match &mig_state {
                            Some(SlotMigrationState::Prepare { target_group, .. })
                            | Some(SlotMigrationState::Migrating { target_group, .. })
                            | Some(SlotMigrationState::Frozen { target_group, .. })
                            | Some(SlotMigrationState::ReadyToCommit { target_group, .. }) => {
                                *target_group
                            }
                            None => {
                                return RouteDecision::ClusterDown(
                                    "CLUSTERDOWN migration state not found".into(),
                                )
                            }
                        }
                    }
                };
                if mgr.multi_raft.is_group_local(target_group) {
                    // IMPORTING node: accept one-shot writes (e.g. MIGRATE RESTORE) when ASKING
                    // or local importing_slots tracking from CLUSTER SETSLOT IMPORTING.
                    let is_importing = mgr.importing_slots.read().contains_key(&slot);
                    if asking || is_importing {
                        RouteDecision::Execute
                    } else {
                        leader_moved(mgr, source_group, slot)
                    }
                } else {
                    RouteDecision::ClusterDown("CLUSTERDOWN slot migration in progress".into())
                }
            }
            SlotStatus::Unallocated => {
                // Last-resort retry: refresh and check if slot was just assigned.
                refresh_router_cache(mgr);
                match mgr.router.route_key(key) {
                    Ok((group_id, SlotStatus::Assigned(_))) => {
                        if mgr.multi_raft.is_group_local(group_id)
                            && mgr.is_local_group_leader(group_id)
                        {
                            RouteDecision::Execute
                        } else {
                            leader_moved(mgr, group_id, slot)
                        }
                    }
                    _ => RouteDecision::ClusterDown("CLUSTERDOWN The cluster is down".into()),
                }
            }
        }
    }
}

fn try_again() -> RouteDecision {
    RouteDecision::TryAgain {
        reason: TRYAGAIN_MIGRATION.into(),
    }
}

/// F-056-A1: 任意活跃迁移期间 SCAN 族必须 TRYAGAIN.
///
/// 供 `command/router::cluster_route` 在 admin 白名单短路之前调用; 也供单测直接断言.
pub fn scan_tryagain_if_migrating(cmd: &str) -> Option<RouteDecision> {
    let lower = cmd.to_ascii_lowercase();
    if !matches!(lower.as_str(), "scan" | "hscan" | "sscan" | "zscan") {
        return None;
    }
    let mgr = CLUSTER_STATE_MGR.get()?;
    if mgr.meta_raft.get_migration_state().is_some() {
        Some(try_again())
    } else {
        None
    }
}

/// F-056-A1: Frozen / Copying (读) / ReadyToCommit 读统一导向 target group ——
/// 合并读 (Frozen/Copying) 或纯 target 读 (ReadyToCommit) 都必须在 target group
/// leader (或其 linearizable 等价路径) 上执行, 含 source 上 READONLY 副本也不
/// 得就地本地读 source (会看不到迁移期新写).
fn decide_target_read(
    mgr: &ClusterStateManager,
    slot: u16,
    target_group: u64,
    cmd_type: &CommandType,
    readonly: bool,
) -> RouteDecision {
    if should_execute_locally(mgr, target_group, cmd_type, readonly) {
        RouteDecision::Execute
    } else if mgr.multi_raft.is_group_local(target_group) {
        // 本地 target 副本但非可执行 (非 leader 且非 readonly): MOVED 到 target leader
        leader_moved(mgr, target_group, slot)
    } else {
        // 非 target 节点 (含 source / 第三方): ASK 到 target leader
        ask_target(mgr, slot)
    }
}

fn should_execute_locally(
    mgr: &ClusterStateManager,
    group_id: u64,
    cmd_type: &CommandType,
    readonly: bool,
) -> bool {
    mgr.multi_raft.is_group_local(group_id)
        && (mgr.is_local_group_leader(group_id) || (*cmd_type == CommandType::Read && readonly))
}

fn leader_moved(mgr: &ClusterStateManager, group_id: u64, slot: u16) -> RouteDecision {
    match mgr.router.get_group_leader(group_id) {
        Some(leader) => match mgr.announce_resolver.redirect_addr(leader, &mgr.router) {
            Some(addr) => RouteDecision::Moved {
                slot,
                node_id: leader,
                addr,
            },
            None => RouteDecision::ClusterDown("CLUSTERDOWN unknown node address".into()),
        },
        None => RouteDecision::ClusterDown("CLUSTERDOWN no leader for group".into()),
    }
}

/// 构造 ASK 重定向到迁移目标 group 的 leader.
fn ask_target(mgr: &ClusterStateManager, slot: u16) -> RouteDecision {
    match mgr.migration_target_leader() {
        Some((_target_group, leader, addr)) => RouteDecision::Ask {
            slot,
            node_id: leader,
            addr,
        },
        None => RouteDecision::ClusterDown("CLUSTERDOWN migration target unknown".into()),
    }
}

/// 检查多个 key 是否在同一 slot
pub fn check_cross_slot(keys: &[&[u8]]) -> Result<(), String> {
    if keys.len() <= 1 {
        return Ok(());
    }
    let first_slot = key_to_slot(keys[0]);
    if keys[1..].iter().any(|k| key_to_slot(k) != first_slot) {
        return Err("CROSSSLOT Keys in request don't hash to the same slot".into());
    }
    Ok(())
}

#[cfg(test)]
mod phase_tests {
    use super::*;
    use aidb::cluster::meta_types::SlotMigrationState;

    #[test]
    fn migration_phase_covers_all_variants() {
        let prep = SlotMigrationState::Prepare {
            source_group: 1,
            target_group: 2,
            slots: vec![0, 1],
        };
        assert_eq!(
            migration_phase_from_state(Some(&prep), 0),
            Some(MigrationRoutePhase::Copying {
                source_group: 1,
                target_group: 2
            })
        );
        assert!(migration_phase_from_state(Some(&prep), 99).is_none());

        let mig = SlotMigrationState::Migrating {
            source_group: 1,
            target_group: 2,
            slots: vec![0],
            progress: 1,
            total: 10,
        };
        assert!(matches!(
            migration_phase_from_state(Some(&mig), 0),
            Some(MigrationRoutePhase::Copying { .. })
        ));

        let frozen = SlotMigrationState::Frozen {
            source_group: 1,
            target_group: 2,
            slots: vec![0],
        };
        let p = migration_phase_from_state(Some(&frozen), 0).unwrap();
        assert!(p.writes_frozen());
        assert_eq!(p.source_group(), 1);

        let ready = SlotMigrationState::ReadyToCommit {
            source_group: 1,
            target_group: 2,
            slots: vec![0],
        };
        let p = migration_phase_from_state(Some(&ready), 0).unwrap();
        assert!(p.writes_frozen());
        assert_eq!(p.target_group(), 2);
        assert!(migration_phase_from_state(None, 0).is_none());
    }
}
