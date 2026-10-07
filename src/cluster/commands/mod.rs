//! CLUSTER 子命令实现与统一分发: `dispatch_cluster` 是全部 CLUSTER 命令的入口,
//! 覆盖 Redis 标准子命令 (MEET / FORGET / INFO / NODES / SLOTS / SHARDS / ADDSLOTS /
//! DELSLOTS / SETSLOT / FAILOVER / REPLICATE / REPLICAS / KEYSLOT / MYID 等) 与
//! AiKv 扩展 (CREATEGROUP / ADD_REPLICA / DEL_REPLICA / REBALANCE / GROUPSTATUS),
//! 均通过 `CLUSTER_STATE_MGR` 访问 aidb 的 MetaRaft / MultiRaft / migration manager.
//!
//! # 分发流程
//!
//! ```text
//! CLUSTER <sub> ... ── dispatch_cluster
//!   ├─ 只读: INFO/NODES/SLOTS/SHARDS/MYID/KEYSLOT/REPLICAS/GROUPSTATUS
//!   │        直接读 MetaRaft 快照 (get_cluster_meta / get_slot_table) 构造 RESP
//!   ├─ 成员: MEET/FORGET ── MembershipCoordinator (MEET NotLeader 指数退避重试)
//!   ├─ slot: ADDSLOTS/DELSLOTS/SETSLOT NODE ── MetaRaft propose
//!   │        (ADDSLOTS 无 group 时自动 CreateGroup + 升 Voter)
//!   ├─ 迁移: SETSLOT MIGRATING → start_migration + run_pending_migration
//!   │        (拷贝失败自动 Cancel, 避免 slots_migrating 残留)
//!   │        SETSLOT IMPORTING → 本地 importing_slots
//!   │        SETSLOT STABLE    → finish_migration (freeze→quiesce→final_verify→mark_ready→commit)
//!   │        SETSLOT CANCEL    → cancel_migration (清半截迁移)
//!   │        REBALANCE         → 贪心搬槽 + run_pending + finish (失败同样 Cancel)
//!   ├─ failover/replica: FAILOVER → change_group_membership 升主; REPLICATE → 仅本地 role
//!   └─ propose 出错统一 map_propose_error → MOVED 0 <addr> / CLUSTERDOWN
//! ```
//!
//! # Invariant
//!
//! - `CLUSTER_STATE_MGR` 门控: 各子命令先 `get()`, 未初始化返回 `CLUSTERDOWN`.
//! - 迁移状态语义: MIGRATING / STABLE 走 `SlotMigrationManager` 完整收尾链, 失败返回 ERR 不静默跳过.
//! - NotLeader propose → `MOVED 0 <addr>` 或 CLUSTERDOWN (leader unknown).
//! - `CLUSTER INFO` 的 `cluster_state` 动态判定: slot 全覆盖 + 映射到已知 group + 均有 leader → `ok`, 否则 `fail`.
//! - `CLUSTER RESET` 不支持: 返回明确 ERR (MetaRaft 共识, 停服清 data_dir 重搭).

mod group;
mod membership;
mod migration;
mod slots;
#[cfg(test)]
mod tests;
mod topology;

pub(crate) use topology::cluster_node_role_label;
pub use topology::{
    cluster_count_keys_in_slot, cluster_get_keys_in_slot, cluster_info, cluster_keyslot,
    cluster_myid, cluster_myshardid, cluster_nodes, cluster_shards, cluster_slots, NodeEndpoint,
    SlotRangeInfo,
};

pub use membership::{
    cluster_failover, cluster_forget, cluster_meet, cluster_replicas, cluster_replicate,
    parse_forget_force, save_nodes_conf,
};

pub use slots::{cluster_add_slots, cluster_del_slots, cluster_set_slot};

pub use group::{
    cluster_add_replica, cluster_create_group, cluster_del_replica, cluster_groupstatus,
};

pub use migration::cluster_rebalance;

use crate::cluster::state::CLUSTER_STATE_MGR;
use crate::error::Error;

/// Parse an integer from bytes (used by CLUSTER command argument parsing).
pub fn parse_int<T: std::str::FromStr>(bytes: &[u8]) -> Option<T> {
    let s = std::str::from_utf8(bytes).ok()?;
    s.parse().ok()
}

/// Convert a aidb propose error into a Redis response string.
/// - NotLeader with leader address → MOVED redirect
/// - NotLeader without address → CLUSTERDOWN (leader unknown)
/// - Other errors → ERR prefix
pub(super) fn map_propose_error(e: aidb::Error) -> String {
    if let aidb::Error::Cluster(aidb::cluster::types::ClusterError::NotLeader {
        leader,
        leader_addr,
        ..
    }) = e
    {
        if let Some(mgr) = CLUSTER_STATE_MGR.get() {
            if let Some(leader_id) = leader {
                if let Some(addr) = mgr.announce_resolver.redirect_addr(leader_id, &mgr.router) {
                    return format!("MOVED 0 {addr}");
                }
            }
            if let Some(addr) = leader_addr.as_deref() {
                if let Some(redirect) = mgr.announce_resolver.redirect_from_addr_str(addr) {
                    return format!("MOVED 0 {redirect}");
                }
            }
        }
        return match leader_addr {
            Some(addr) => format!("MOVED 0 {addr}"),
            None => "CLUSTERDOWN The cluster is not available".to_string(),
        };
    }
    format!("ERR {e}")
}

pub(super) fn bytes_to_str(bytes: &[u8]) -> std::result::Result<&str, Error> {
    std::str::from_utf8(bytes).map_err(|_| Error::Command("ERR invalid utf8".into()))
}

// ---------------------------------------------------------------------------
// Helper: parse 40-char hex node_id into u64
// ---------------------------------------------------------------------------
pub fn parse_hex_node_id(hex: &str) -> Result<u64, String> {
    let hex = hex.trim_start_matches("0x");
    u64::from_str_radix(hex, 16).map_err(|_| "ERR invalid node id".to_string())
}

/// Accept decimal node ids (factory/bootstrap) or 40-char hex ids (redis-cli style).
pub fn parse_cluster_node_id(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("ERR invalid node id".to_string());
    }
    if raw.chars().all(|c| c.is_ascii_digit()) {
        raw.parse::<u64>()
            .map_err(|_| "ERR invalid node id".to_string())
    } else {
        parse_hex_node_id(raw)
    }
}

// ---------------------------------------------------------------------------
// CLUSTER subcommand dispatch
// ---------------------------------------------------------------------------
/// CLUSTER 子命令分发器. 所有 CLUSTER 子命令统一入口,
/// 避免 execute_inner 的 match 进一步膨胀.
pub async fn dispatch_cluster(
    sub: Option<&str>,
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    match sub {
        Some("keyslot") | Some("KEYSLOT") => topology::handle_keyslot(args),
        Some("myid") | Some("MYID") => topology::handle_myid(args),
        Some("info") | Some("INFO") => topology::handle_info(args),
        Some("nodes") | Some("NODES") => topology::handle_nodes(args),
        Some("slots") | Some("SLOTS") => topology::handle_slots(args),
        Some("shards") | Some("SHARDS") => topology::handle_shards(args),
        Some("myshardid") | Some("MYSHARDID") => topology::handle_myshardid(args),
        Some("countkeysinslot") | Some("COUNTKEYSINSLOT") => {
            topology::handle_countkeysinslot(args).await
        }
        Some("getkeysinslot") | Some("GETKEYSINSLOT") => topology::handle_getkeysinslot(args).await,
        Some("groupstatus") | Some("GROUPSTATUS") => group::handle_groupstatus(args),
        Some("meet") | Some("MEET") => membership::handle_meet(args).await,
        Some("forget") | Some("FORGET") => membership::handle_forget(args).await,
        Some("failover") | Some("FAILOVER") => membership::handle_failover(args).await,
        Some("replicate") | Some("REPLICATE") => membership::handle_replicate(args).await,
        Some("replicas") | Some("REPLICAS") => membership::handle_replicas(args),
        Some("saveconfig") | Some("SAVECONFIG") => membership::handle_saveconfig(args),
        Some("bumpepoch") | Some("BUMPEPOCH") => membership::handle_bumpepoch(args).await,
        Some("set-config-epoch") | Some("SET-CONFIG-EPOCH") => {
            membership::handle_set_config_epoch(args)
        }
        Some("count-failure-reports") | Some("COUNT-FAILURE-REPORTS") => {
            membership::handle_count_failure_reports(args)
        }
        Some("reset") | Some("RESET") => membership::handle_reset(args),
        Some("addslots") | Some("ADDSLOTS") => slots::handle_addslots(args).await,
        Some("delslots") | Some("DELSLOTS") => slots::handle_delslots(args).await,
        Some("setslot") | Some("SETSLOT") => slots::handle_setslot(args).await,
        Some("creategroup") | Some("CREATEGROUP") => group::handle_creategroup(args).await,
        Some("add_replica") | Some("ADD_REPLICA") => group::handle_add_replica(args).await,
        Some("del_replica") | Some("DEL_REPLICA") => group::handle_del_replica(args).await,
        Some("rebalance") | Some("REBALANCE") => migration::handle_rebalance(args).await,
        #[cfg(feature = "cluster-test-util")]
        Some("failpoint") | Some("FAILPOINT") => migration::handle_failpoint(args),
        _ => Err(crate::error::Error::Command(
            "ERR unknown CLUSTER subcommand".into(),
        )),
    }
}
