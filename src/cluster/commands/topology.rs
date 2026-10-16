use crate::cluster::state::groups_with_slots;
use crate::cluster::state::ClusterStateManager;
use crate::cluster::state::CLUSTER_STATE_MGR;
use crate::cluster::state::DEFAULT_DATA_PORT_OFFSET;
use crate::error::Error;
use crate::protocol::RespValue;
use aidb::cluster::meta_types::ClusterMeta;
use aidb::cluster::meta_types::NodeStatus;
use aidb::cluster::meta_types::SlotMigrationState;
use aidb::cluster::meta_types::SlotStatus;
use aidb::cluster::meta_types::SlotTable;
use aidb::cluster::meta_types::SLOT_COUNT;
use aidb::cluster::router::key_to_slot;
use bytes::Bytes;
use openraft::rt::watch::WatchReceiver;

use super::parse_int;

// ---------------------------------------------------------------------------
// CLUSTER KEYSLOT
// ---------------------------------------------------------------------------
pub fn cluster_keyslot(key: &[u8]) -> Result<String, String> {
    let slot = key_to_slot(key);
    Ok(slot.to_string())
}

// ---------------------------------------------------------------------------
// CLUSTER MYID
// ---------------------------------------------------------------------------
pub fn cluster_myid() -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    Ok(format!("{:040x}", mgr.node_id))
}

// ---------------------------------------------------------------------------
// CLUSTER INFO
// ---------------------------------------------------------------------------

/// 解析 group leader (CLUSTER INFO 健康判定; 不用 Router 的 first-node 回退).
pub(super) fn resolve_group_leader_for_info(
    mgr: &ClusterStateManager,
    meta: &ClusterMeta,
    group_id: u64,
) -> Option<u64> {
    let leader = meta
        .groups
        .get(&group_id)
        .and_then(|g| g.replicas.iter().find(|r| r.is_leader))
        .map(|replica| replica.node_id);
    let leader = leader.or_else(|| {
        if let Some(node) = mgr.multi_raft.get_groups().read().get(&group_id) {
            node.raft().metrics().borrow_watched().current_leader
        } else {
            None
        }
    });
    // 本节点作为该 group leader 且已失去 quorum (探活判定) → 视为无 leader,
    // 驱动 CLUSTER INFO 报 cluster_state:fail (隔离的少数派旧 leader 视角).
    apply_leader_quorum_gate(leader, mgr.node_id, mgr.group_quorum_ok(group_id))
}

/// quorum 门控 (纯函数): 本节点作为 leader 且已失去 quorum → 视为无 leader.
pub(super) fn apply_leader_quorum_gate(
    leader: Option<u64>,
    self_id: u64,
    group_quorum_ok: bool,
) -> Option<u64> {
    if leader == Some(self_id) && !group_quorum_ok {
        None
    } else {
        leader
    }
}

/// 动态 `cluster_state:ok` / `fail` (对齐 oldmain: slot 满 + leader + 映射一致).
pub(super) fn compute_cluster_state<F>(
    slot_table: &SlotTable,
    meta: &ClusterMeta,
    resolve_leader: F,
) -> &'static str
where
    F: Fn(u64) -> Option<u64>,
{
    let covered = slot_table
        .iter()
        .filter(|s| !matches!(s, SlotStatus::Unallocated))
        .count();
    if covered != SLOT_COUNT {
        return "fail";
    }

    let slots_map_to_known_groups = slot_table.iter().all(|status| match status {
        SlotStatus::Unallocated => true,
        SlotStatus::Assigned(gid) | SlotStatus::Migrating(gid) => meta.groups.contains_key(gid),
    });
    if !slots_map_to_known_groups {
        return "fail";
    }

    let groups_with_slots = groups_with_slots(slot_table);

    if !groups_with_slots
        .iter()
        .all(|gid| resolve_leader(*gid).is_some())
    {
        return "fail";
    }

    "ok"
}

pub fn cluster_info() -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let meta = mgr.meta_raft.get_cluster_meta();
    let slot_table = mgr.meta_raft.get_slot_table();

    let assigned = slot_table
        .iter()
        .filter(|s| matches!(s, SlotStatus::Assigned(_)))
        .count();
    let migrating = slot_table
        .iter()
        .filter(|s| matches!(s, SlotStatus::Migrating(_)))
        .count();
    let ok_count = assigned;
    let known_nodes = meta.nodes.len();
    let group_count = meta.groups.len();
    let epoch = meta.version;
    let (msgs_sent, msgs_received) = mgr
        .metrics
        .as_ref()
        .map(|m| (m.cluster_messages_sent(), m.cluster_messages_received()))
        .unwrap_or((0, 0));
    let cluster_state = compute_cluster_state(&slot_table, &meta, |gid| {
        resolve_group_leader_for_info(mgr, &meta, gid)
    });

    Ok(format!(
        "cluster_state:{cluster_state}\n\
         cluster_slots_assigned:{assigned}\n\
         cluster_slots_ok:{ok_count}\n\
         cluster_slots_pfail:0\n\
         cluster_slots_fail:0\n\
         cluster_slots_migrating:{migrating}\n\
         cluster_known_nodes:{known_nodes}\n\
         cluster_size:{group_count}\n\
         cluster_current_epoch:{epoch}\n\
         cluster_my_epoch:{epoch}\n\
         cluster_stats_messages_sent:{msgs_sent}\n\
         cluster_stats_messages_received:{msgs_received}\n\
         total_cluster_links_buffer_limit_exceeded:0\n\
         cluster_slot_migration_active_tasks:0\n\
         cluster_slot_migration_active_trim_running:0\n\
         cluster_slot_migration_active_trim_current_job_keys:0\n\
         cluster_slot_migration_active_trim_current_job_trimmed:0\n\
         cluster_slot_migration_stats_active_trim_started:0\n\
         cluster_slot_migration_stats_active_trim_completed:0\n\
         cluster_slot_migration_stats_active_trim_cancelled:0\n",
    ))
}
// ---------------------------------------------------------------------------
// CLUSTER NODES
// ---------------------------------------------------------------------------

/// Redis CLUSTER NODES link-state from MetaRaft node status.
pub(super) fn cluster_nodes_link_state(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Offline => "disconnected",
        NodeStatus::Online | NodeStatus::Draining => "connected",
    }
}

/// Redis CLUSTER NODES 的 master/slave 标签 (与 primary_id 一致, 看分片 group 而非 MetaRaft NodeRole).
///
/// 副本在 ADD_REPLICA 后常为 Voter (参与 MultiRaft), 若用 `NodeRole::Voter` 会误显示为 master.
pub(crate) fn cluster_node_role_label(
    meta: &aidb::cluster::meta_types::ClusterMeta,
    nid: u64,
) -> &'static str {
    let in_group = meta
        .groups
        .values()
        .any(|g| g.replicas.iter().any(|r| r.node_id == nid));
    let is_leader = meta
        .groups
        .values()
        .any(|g| g.replicas.iter().any(|r| r.node_id == nid && r.is_leader));
    if is_leader || !in_group {
        "master"
    } else {
        "slave"
    }
}

pub fn cluster_nodes() -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let meta = mgr.meta_raft.get_cluster_meta();
    let migration_state = mgr.meta_raft.get_migration_state();
    let mut lines = Vec::new();

    for (nid, node) in &meta.nodes {
        let role = cluster_node_role_label(&meta, *nid);
        let base_flags = if *nid == mgr.node_id {
            format!("myself,{}", role)
        } else {
            role.to_string()
        };

        // Append migration flags if this node is source or target of active migration
        let migration_flags = match &migration_state {
            Some(state) => {
                let (src_gid, dst_gid) = match state {
                    aidb::cluster::meta_types::SlotMigrationState::Prepare {
                        source_group,
                        target_group,
                        ..
                    }
                    | aidb::cluster::meta_types::SlotMigrationState::Migrating {
                        source_group,
                        target_group,
                        ..
                    }
                    | aidb::cluster::meta_types::SlotMigrationState::Frozen {
                        source_group,
                        target_group,
                        ..
                    }
                    | aidb::cluster::meta_types::SlotMigrationState::ReadyToCommit {
                        source_group,
                        target_group,
                        ..
                    } => (*source_group, *target_group),
                };
                let in_source = meta
                    .groups
                    .get(&src_gid)
                    .map(|g| g.replicas.iter().any(|r| r.node_id == *nid))
                    .unwrap_or(false);
                let in_target = meta
                    .groups
                    .get(&dst_gid)
                    .map(|g| g.replicas.iter().any(|r| r.node_id == *nid))
                    .unwrap_or(false);
                match (in_source, in_target) {
                    (true, _) => ",migrating",
                    (_, true) => ",importing",
                    _ => "",
                }
            }
            None => "",
        };
        let flags = format!("{}{}", base_flags, migration_flags);

        // 从 group 元数据推导 primary_id: leader 显示 "-", replica 显示 leader 的 node_id.
        let display_primary_id = {
            let node_group = meta
                .groups
                .iter()
                .find(|(_, g)| g.replicas.iter().any(|r| r.node_id == *nid));
            match node_group {
                Some((_, group)) => {
                    let is_leader = group
                        .replicas
                        .iter()
                        .any(|r| r.node_id == *nid && r.is_leader);
                    if is_leader {
                        "-".to_string()
                    } else {
                        group
                            .replicas
                            .iter()
                            .find(|r| r.is_leader)
                            .map(|r| format!("{:040x}", r.node_id))
                            .unwrap_or_else(|| "-".to_string())
                    }
                }
                None => "-".to_string(),
            }
        };

        // 从 group 元数据推导 node 的 slot 范围 (仅 leader 显示).
        let slot_ranges_str = {
            let node_group = meta
                .groups
                .iter()
                .find(|(_, g)| g.replicas.iter().any(|r| r.node_id == *nid));
            match node_group {
                Some((_, group)) if !group.slot_ranges.is_empty() => {
                    let is_leader = group
                        .replicas
                        .iter()
                        .any(|r| r.node_id == *nid && r.is_leader);
                    if is_leader {
                        let ranges: Vec<String> = group
                            .slot_ranges
                            .iter()
                            .map(|(s, e)| format!("{s}-{e}"))
                            .collect();
                        format!(" {}", ranges.join(" "))
                    } else {
                        String::new()
                    }
                }
                _ => String::new(),
            }
        };

        let offset = CLUSTER_STATE_MGR
            .get()
            .map_or(DEFAULT_DATA_PORT_OFFSET, |m| m.data_port_offset);
        let cport = match mgr.router.get_node_addr(*nid) {
            Some(ref addr) => addr
                .rsplit(':')
                .next()
                .and_then(|port| port.parse::<u16>().ok())
                .map(|p| p + offset)
                .unwrap_or(0),
            None => 0,
        };
        // Note: rsplit(':') handles both IPv4 ("127.0.0.1:7000") and
        // IPv6 ("[::1]:7000") by taking the last colon segment as port.

        // Use client_addr for the display address (host:port that clients
        // connect to), falling back to rpc_addr if not set.  Without this,
        // CLUSTER NODES shows the gRPC/RPC port and clients that discover
        // topology via CLUSTER NODES will fail to connect.
        let addr = mgr
            .router
            .get_node_addr(*nid)
            .unwrap_or_else(|| node.rpc_addr.clone());
        let link_state = cluster_nodes_link_state(&node.status);
        let line = format!(
            "{:040x} {}@{} {} {} 0 0 {} {}{}\n",
            nid, addr, cport, flags, display_primary_id, meta.version, link_state, slot_ranges_str,
        );
        lines.push(line);
    }

    Ok(lines.join(""))
}

// ---------------------------------------------------------------------------
// CLUSTER SLOTS
// ---------------------------------------------------------------------------

/// 节点端点信息 (用于 CLUSTER SLOTS / SHARDS 响应).
#[derive(Debug, Clone)]
pub struct NodeEndpoint {
    pub host: String,
    pub port: u16,
    pub node_id: u64,
}

/// 槽范围信息, 包含 master 和 replicas.
#[derive(Debug, Clone)]
pub struct SlotRangeInfo {
    pub start: u16,
    pub end: u16,
    pub master: NodeEndpoint,
    pub replicas: Vec<NodeEndpoint>,
}

/// 从 Router + AnnounceResolver 解析 NodeEndpoint (CLUSTER SLOTS / SHARDS).
fn resolve_endpoint(mgr: &ClusterStateManager, node_id: u64) -> Option<NodeEndpoint> {
    let (host, port) = mgr
        .announce_resolver
        .endpoint_for_node(node_id, &mgr.router)?;
    Some(NodeEndpoint {
        host,
        port,
        node_id,
    })
}

pub fn cluster_slots() -> Result<Vec<SlotRangeInfo>, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let meta = mgr.meta_raft.get_cluster_meta();
    let slot_table = mgr.meta_raft.get_slot_table();
    let migration_state = mgr.meta_raft.get_migration_state();

    let mut ranges: Vec<SlotRangeInfo> = Vec::new();
    let mut i = 0usize;
    while i < slot_table.len() {
        let status = &slot_table[i];
        match status {
            SlotStatus::Assigned(gid) => {
                let start = i as u16;
                while i < slot_table.len() && slot_table[i] == SlotStatus::Assigned(*gid) {
                    i += 1;
                }
                let end = (i - 1) as u16;
                let group = meta.groups.get(gid);

                // 收集 master 和 replicas
                let mut master: Option<NodeEndpoint> = None;
                let mut replicas: Vec<NodeEndpoint> = Vec::new();
                if let Some(group) = group {
                    for r in &group.replicas {
                        if let Some(ep) = resolve_endpoint(mgr, r.node_id) {
                            if r.is_leader {
                                master = Some(ep);
                            } else {
                                replicas.push(ep);
                            }
                        }
                    }
                }

                if let Some(master) = master {
                    ranges.push(SlotRangeInfo {
                        start,
                        end,
                        master,
                        replicas,
                    });
                }
            }
            SlotStatus::Migrating(gid) => {
                // Build range for slots currently being migrated
                let start = i as u16;
                while i < slot_table.len() && slot_table[i] == SlotStatus::Migrating(*gid) {
                    i += 1;
                }
                let end = (i - 1) as u16;

                // Find source group (gid = source_group_id in Migrating status)
                let source_group = meta.groups.get(gid);
                // Get target group from migration state
                let target_gid = migration_state.as_ref().map(|ms| match ms {
                    SlotMigrationState::Prepare { target_group, .. }
                    | SlotMigrationState::Migrating { target_group, .. }
                    | SlotMigrationState::Frozen { target_group, .. }
                    | SlotMigrationState::ReadyToCommit { target_group, .. } => *target_group,
                });
                let target_group = target_gid.and_then(|tg| meta.groups.get(&tg));

                let mut source_master: Option<NodeEndpoint> = None;
                let mut target_master: Option<NodeEndpoint> = None;

                if let Some(g) = source_group {
                    for r in &g.replicas {
                        if r.is_leader {
                            source_master = resolve_endpoint(mgr, r.node_id);
                            break;
                        }
                    }
                }
                if let Some(g) = target_group {
                    for r in &g.replicas {
                        if r.is_leader {
                            target_master = resolve_endpoint(mgr, r.node_id);
                            break;
                        }
                    }
                }

                match (source_master, target_master) {
                    (Some(src), Some(dst)) => {
                        ranges.push(SlotRangeInfo {
                            start,
                            end,
                            master: src,
                            replicas: vec![NodeEndpoint {
                                host: dst.host.clone(),
                                port: dst.port,
                                node_id: dst.node_id,
                            }],
                        });
                    }
                    (Some(master), None) => {
                        ranges.push(SlotRangeInfo {
                            start,
                            end,
                            master,
                            replicas: vec![],
                        });
                    }
                    _ => {
                        // Neither node found, skip this range
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    Ok(ranges)
}

// ---------------------------------------------------------------------------
// CLUSTER SHARDS (Redis 7.0+)
// ---------------------------------------------------------------------------
pub fn cluster_shards() -> Result<RespValue, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let meta = mgr.meta_raft.get_cluster_meta();
    let slot_table = mgr.meta_raft.get_slot_table();

    // 按 Group 聚合槽范围
    let mut group_ranges: Vec<(u64, Vec<(u16, u16)>)> = Vec::new();
    let mut i = 0usize;
    while i < slot_table.len() {
        match slot_table[i] {
            SlotStatus::Assigned(gid) => {
                let start = i as u16;
                while i < slot_table.len() && slot_table[i] == SlotStatus::Assigned(gid) {
                    i += 1;
                }
                let end = (i - 1) as u16;
                match group_ranges.iter_mut().find(|(g, _)| *g == gid) {
                    Some((_, ranges)) => ranges.push((start, end)),
                    None => group_ranges.push((gid, vec![(start, end)])),
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    // 构建 RESP 响应: 每个 shard 是一个 map { "slots": [...], "nodes": [...] }
    let mut shards: Vec<RespValue> = Vec::new();
    for (gid, ranges) in &group_ranges {
        let group = meta.groups.get(gid);
        let primary = group.and_then(|g| g.replicas.iter().find(|r| r.is_leader));
        let primary_id = primary.map(|r| r.node_id);
        let endpoint = primary_id.and_then(|nid| resolve_endpoint(mgr, nid));

        // slots 数组: [[start, end], ...]
        let slots_array: Vec<RespValue> = ranges
            .iter()
            .map(|(s, e)| {
                RespValue::Array(Some(vec![
                    RespValue::Integer(*s as i64),
                    RespValue::Integer(*e as i64),
                ]))
            })
            .collect();

        // nodes 数组: 每个 node 是一个 map
        let mut nodes: Vec<RespValue> = Vec::new();
        if let (Some(id), Some(ep)) = (primary_id, endpoint) {
            let node_id_hex = format!("{:040x}", id);
            nodes.push(RespValue::Array(Some(vec![
                RespValue::BulkString(Some(Bytes::from("id"))),
                RespValue::BulkString(Some(Bytes::from(node_id_hex))),
                RespValue::BulkString(Some(Bytes::from("port"))),
                RespValue::Integer(ep.port as i64),
                RespValue::BulkString(Some(Bytes::from("ip"))),
                RespValue::BulkString(Some(Bytes::from(ep.host.clone()))),
                RespValue::BulkString(Some(Bytes::from("endpoint"))),
                RespValue::BulkString(Some(Bytes::from(ep.host))),
                RespValue::BulkString(Some(Bytes::from("role"))),
                RespValue::BulkString(Some(Bytes::from("master"))),
                RespValue::BulkString(Some(Bytes::from("replication-offset"))),
                RespValue::Integer(0),
                RespValue::BulkString(Some(Bytes::from("health"))),
                RespValue::BulkString(Some(Bytes::from("online"))),
            ])));
        }

        // shard map: "slots" → slot_ranges, "nodes" → nodes
        shards.push(RespValue::Array(Some(vec![
            RespValue::BulkString(Some(Bytes::from("slots"))),
            RespValue::Array(Some(slots_array)),
            RespValue::BulkString(Some(Bytes::from("nodes"))),
            RespValue::Array(Some(nodes)),
        ])));
    }

    Ok(RespValue::Array(Some(shards)))
}

// ---------------------------------------------------------------------------
// CLUSTER MYSHARDID
// ---------------------------------------------------------------------------
pub fn cluster_myshardid() -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let meta = mgr.meta_raft.get_cluster_meta();
    for (gid, group) in &meta.groups {
        if group.replicas.iter().any(|r| r.node_id == mgr.node_id) {
            return Ok(format!("{:040x}", gid));
        }
    }
    Err("CLUSTERDOWN Node not in any group".to_string())
}

// ---------------------------------------------------------------------------
// CLUSTER COUNTKEYSINSLOT
// ---------------------------------------------------------------------------
pub async fn cluster_count_keys_in_slot(slot: u16) -> Result<i64, String> {
    let mgr = match CLUSTER_STATE_MGR.get() {
        Some(m) => m,
        None => return Err("CLUSTERDOWN Cluster not initialized".to_string()),
    };
    let mut total = 0i64;
    let gids: Vec<_> = mgr.multi_raft.get_groups().read().keys().copied().collect();
    for gid in gids {
        if let Ok(keys) = mgr.multi_raft.scan_keys(gid, None).await {
            total += keys
                .iter()
                .filter(|k| aidb::cluster::router::key_to_slot(k) == slot)
                .count() as i64;
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// CLUSTER GETKEYSINSLOT
// ---------------------------------------------------------------------------
pub async fn cluster_get_keys_in_slot(slot: u16, count: usize) -> Result<Vec<Vec<u8>>, String> {
    let mgr = match CLUSTER_STATE_MGR.get() {
        Some(m) => m,
        None => return Err("CLUSTERDOWN Cluster not initialized".to_string()),
    };
    let mut keys = Vec::new();
    let gids: Vec<_> = mgr.multi_raft.get_groups().read().keys().copied().collect();
    for gid in gids {
        if keys.len() >= count {
            break;
        }
        if let Ok(found) = mgr.multi_raft.scan_keys(gid, None).await {
            for k in found {
                if aidb::cluster::router::key_to_slot(&k) == slot {
                    keys.push(k);
                    if keys.len() >= count {
                        break;
                    }
                }
            }
        }
    }
    Ok(keys)
}

pub(super) fn handle_keyslot(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let key = args
        .get(1)
        .ok_or_else(|| Error::Command("ERR wrong number of arguments".into()))?;
    let slot = cluster_keyslot(key).map_err(Error::Command)?;
    Ok(RespValue::Integer(
        slot.parse::<i64>()
            .map_err(|_| Error::Command("ERR invalid slot".into()))?,
    ))
}

pub(super) fn handle_myid(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let id = cluster_myid().map_err(Error::Command)?;
    Ok(RespValue::BulkString(Some(Bytes::from(id))))
}

pub(super) fn handle_info(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let info = cluster_info().map_err(Error::Command)?;
    Ok(RespValue::BulkString(Some(Bytes::from(info))))
}

pub(super) fn handle_nodes(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let nodes = cluster_nodes().map_err(Error::Command)?;
    Ok(RespValue::BulkString(Some(Bytes::from(nodes))))
}

pub(super) fn handle_slots(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let slots = cluster_slots().map_err(Error::Command)?;
    let array: Vec<RespValue> = slots
        .into_iter()
        .map(|range| {
            let mut entry = vec![
                RespValue::Integer(range.start as i64),
                RespValue::Integer(range.end as i64),
                RespValue::Array(Some(vec![
                    RespValue::BulkString(Some(Bytes::from(range.master.host))),
                    RespValue::Integer(range.master.port as i64),
                    RespValue::BulkString(Some(Bytes::from(format!(
                        "{:040x}",
                        range.master.node_id
                    )))),
                ])),
            ];
            // 追加 replica 条目
            for replica in &range.replicas {
                entry.push(RespValue::Array(Some(vec![
                    RespValue::BulkString(Some(Bytes::from(replica.host.clone()))),
                    RespValue::Integer(replica.port as i64),
                    RespValue::BulkString(Some(Bytes::from(format!("{:040x}", replica.node_id)))),
                ])));
            }
            RespValue::Array(Some(entry))
        })
        .collect();
    Ok(RespValue::Array(Some(array)))
}

pub(super) fn handle_shards(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    cluster_shards().map_err(Error::Command)
}

pub(super) fn handle_myshardid(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let id = cluster_myshardid().map_err(Error::Command)?;
    Ok(RespValue::BulkString(Some(Bytes::from(id))))
}

pub(super) async fn handle_countkeysinslot(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let slot: u16 = args
        .get(1)
        .and_then(|a| parse_int(a))
        .ok_or_else(|| Error::Command("ERR invalid slot".into()))?;
    let count = cluster_count_keys_in_slot(slot)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::Integer(count))
}

pub(super) async fn handle_getkeysinslot(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let slot: u16 = args
        .get(1)
        .and_then(|a| parse_int(a))
        .ok_or_else(|| Error::Command("ERR invalid slot".into()))?;
    let count: usize = args
        .get(2)
        .and_then(|a| parse_int(a))
        .ok_or_else(|| Error::Command("ERR invalid count".into()))?;
    let keys = cluster_get_keys_in_slot(slot, count)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::Array(Some(
        keys.into_iter()
            .map(|k| RespValue::BulkString(Some(Bytes::from(k))))
            .collect(),
    )))
}
