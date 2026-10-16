use std::collections::HashMap;

use crate::cluster::state::CLUSTER_STATE_MGR;
use crate::error::Error;
use crate::protocol::RespValue;

use super::bytes_to_str;
use super::map_propose_error;
use super::migration::run_pending_migration_to_completion;
use super::parse_cluster_node_id;
use super::parse_int;

// ---------------------------------------------------------------------------
// CLUSTER ADDSLOTS
// ---------------------------------------------------------------------------
pub async fn cluster_add_slots(
    slots: &[u16],
    target_node_id: Option<u64>,
) -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;

    let effective_node = target_node_id.unwrap_or(mgr.node_id);
    let group_id = {
        let meta = mgr.meta_raft.get_cluster_meta();
        match meta
            .groups
            .iter()
            .find(|(_, g)| g.replicas.iter().any(|r| r.node_id == effective_node))
            .map(|(id, _)| *id)
        {
            Some(gid) => gid,
            None => {
                // 自动创建分片组: node_id 作为 group_id
                let gid = effective_node;
                mgr.meta_raft
                    .propose(aidb::cluster::MetaRequest::CreateGroup {
                        group_id: gid,
                        initial_replicas: vec![(effective_node, true)],
                    })
                    .await
                    .map_err(map_propose_error)?;
                gid
            }
        }
    };

    mgr.meta_raft
        .propose(aidb::cluster::MetaRequest::AssignSlots {
            group_id,
            slots: slots.to_vec(),
        })
        .await
        .map_err(map_propose_error)?;

    // Promote target node to Voter so CLUSTER NODES shows correct
    // "master" role instead of "slave" (nodes join as Learner via MEET).
    mgr.meta_raft
        .propose(aidb::cluster::MetaRequest::ChangeNodeRole {
            node_id: effective_node,
            role: aidb::cluster::NodeRole::Voter,
        })
        .await
        .map_err(map_propose_error)?;

    // Immediately refresh router cache so subsequent routing decisions
    // use the updated slot table instead of stale Unallocated entries.
    let meta = mgr.meta_raft.get_cluster_meta();
    let slot_table = mgr.meta_raft.get_slot_table();
    let mut group_nodes: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut node_addrs: HashMap<u64, String> = HashMap::new();
    for (gid, group) in &meta.groups {
        group_nodes.insert(*gid, group.replicas.iter().map(|r| r.node_id).collect());
    }
    for (nid, node) in &meta.nodes {
        let addr = node
            .client_addr
            .clone()
            .unwrap_or_else(|| node.rpc_addr.clone());
        node_addrs.insert(*nid, addr);
    }
    let group_leaders: HashMap<u64, u64> = meta
        .groups
        .iter()
        .filter_map(|(gid, g)| {
            g.replicas
                .iter()
                .find(|r| r.is_leader)
                .map(|r| (*gid, r.node_id))
        })
        .collect();
    mgr.router
        .refresh_from_data(slot_table, group_nodes, node_addrs, group_leaders);

    Ok("OK".to_string())
}

// ---------------------------------------------------------------------------
// CLUSTER DELSLOTS
// ---------------------------------------------------------------------------
pub async fn cluster_del_slots(slots: &[u16]) -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    mgr.meta_raft
        .propose(aidb::cluster::MetaRequest::UnassignSlots {
            slots: slots.to_vec(),
        })
        .await
        .map_err(map_propose_error)?;
    Ok("OK".to_string())
}

// ---------------------------------------------------------------------------
// CLUSTER SETSLOT
// ---------------------------------------------------------------------------
pub async fn cluster_set_slot(
    slot: u16,
    sub: &str,
    node_id: Option<u64>,
) -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    match sub.to_uppercase().as_str() {
        "MIGRATING" => {
            let target_nid = node_id.ok_or_else(|| "ERR wrong number of arguments".to_string())?;
            let (source_gid, _) = mgr
                .router
                .route_slot(slot)
                .map_err(|e| format!("ERR {e}"))?;
            // 数据拷贝必须在 source group 的 data Raft Leader 上执行.
            // MetaRaft Leader 未必持有该 group — failover 后尤甚.
            if !mgr.is_local_group_leader(source_gid) {
                let leader_id = mgr.router.get_group_leader(source_gid);
                if let Some(lid) = leader_id {
                    if let Some(addr) = mgr.announce_resolver.redirect_addr(lid, &mgr.router) {
                        return Err(format!("MOVED 0 {addr}"));
                    }
                }
                return Err(format!(
                    "不是 Leader (当前 Leader: {leader_id:?}, 地址: None)"
                ));
            }
            let meta = mgr.meta_raft.get_cluster_meta();
            let target_gid = meta
                .groups
                .iter()
                .find(|(_, g)| g.replicas.iter().any(|r| r.node_id == target_nid))
                .map(|(id, _)| *id)
                .ok_or_else(|| "ERR Target node not in any group".to_string())?;
            let sm = mgr
                .slot_migration_manager
                .as_ref()
                .ok_or_else(|| "ERR SlotMigrationManager not initialized".to_string())?;
            let migration_id = sm
                .start_migration(source_gid, target_gid, vec![slot])
                .await
                .map_err(|e| format!("ERR {e}"))?;
            if let Err(e) = run_pending_migration_to_completion(sm, migration_id).await {
                // Begin 已进 MetaRaft: 拷贝失败必须 Cancel, 否则 slots_migrating 残留,
                // 后续 SETSLOT 会报 migration already in progress.
                let base = if e.starts_with("ERR ") {
                    e
                } else {
                    format!("ERR {e}")
                };
                return Err(match sm.cancel_migration().await {
                    Ok(()) => base,
                    Err(cancel_err) => {
                        tracing::warn!(
                            slot,
                            error = %cancel_err,
                            "SETSLOT MIGRATING failed and cancel_migration also failed"
                        );
                        format!("{base}; cancel_migration also failed: {cancel_err}")
                    }
                });
            }
            Ok("OK".to_string())
        }
        "IMPORTING" => {
            let source_nid = node_id.ok_or_else(|| "ERR wrong number of arguments".to_string())?;
            mgr.importing_slots.write().insert(slot, source_nid);
            Ok("OK".to_string())
        }
        "NODE" => {
            let target_nid = node_id.ok_or_else(|| "ERR wrong number of arguments".to_string())?;
            let meta = mgr.meta_raft.get_cluster_meta();
            let target_gid = meta
                .groups
                .iter()
                .find(|(_, g)| g.replicas.iter().any(|r| r.node_id == target_nid))
                .map(|(id, _)| *id)
                .ok_or_else(|| "ERR Target node not in any group".to_string())?;
            mgr.meta_raft
                .propose(aidb::cluster::MetaRequest::AssignSlots {
                    group_id: target_gid,
                    slots: vec![slot],
                })
                .await
                .map_err(map_propose_error)?;
            Ok("OK".to_string())
        }
        "STABLE" => {
            // Clear local importing slot tracking
            mgr.importing_slots.write().remove(&slot);
            // F-056: 走完整收尾链 (freeze → quiesce → final_verify → mark_ready → commit).
            // 无活跃迁移时 finish_migration 会失败; 仅清 importing 仍返回 OK.
            if mgr.meta_raft.get_migration_state().is_some() {
                let sm = mgr
                    .slot_migration_manager
                    .as_ref()
                    .ok_or_else(|| "ERR SlotMigrationManager not initialized".to_string())?;
                sm.finish_migration()
                    .await
                    .map_err(|e| format!("ERR finish_migration: {e}"))?;
            }
            Ok("OK".to_string())
        }
        "CANCEL" => {
            mgr.importing_slots.write().remove(&slot);
            if mgr.meta_raft.get_migration_state().is_none() {
                return Ok("OK".to_string());
            }
            let sm = mgr
                .slot_migration_manager
                .as_ref()
                .ok_or_else(|| "ERR SlotMigrationManager not initialized".to_string())?;
            sm.cancel_migration()
                .await
                .map_err(|e| format!("ERR cancel_migration: {e}"))?;
            Ok("OK".to_string())
        }
        _ => Err("ERR Unknown SETSLOT subcommand".to_string()),
    }
}

pub(super) async fn handle_addslots(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    // Optional "NODE <id>" prefix for assigning slots to a specific node.
    let (target_node_id, slot_start) = if args.len() > 2 {
        match bytes_to_str(&args[1]) {
            Ok(s) if s.eq_ignore_ascii_case("node") => {
                let nid = bytes_to_str(&args[2])
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok());
                (nid, 3usize)
            }
            _ => (None, 1usize),
        }
    } else {
        (None, 1usize)
    };
    let slots: Vec<u16> = args[slot_start..]
        .iter()
        .filter_map(|a| parse_int(a))
        .collect();
    let msg = cluster_add_slots(&slots, target_node_id)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}

pub(super) async fn handle_delslots(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let slots: Vec<u16> = args[1..].iter().filter_map(|a| parse_int(a)).collect();
    let msg = cluster_del_slots(&slots).await.map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}

pub(super) async fn handle_setslot(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let slot: u16 = args
        .get(1)
        .and_then(|a| parse_int(a))
        .ok_or_else(|| Error::Command("ERR invalid slot".into()))?;
    let sub = match args.get(2).map(|a| bytes_to_str(a)) {
        Some(Ok(s)) => s,
        _ => "",
    };
    let node_id = args
        .get(3)
        .and_then(|a| bytes_to_str(a).ok())
        .and_then(|s| parse_cluster_node_id(s).ok());
    let msg = cluster_set_slot(slot, sub, node_id)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}
