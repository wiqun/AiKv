use std::time::Duration;

use crate::cluster::state::CLUSTER_STATE_MGR;
use crate::error::Error;
use crate::protocol::RespValue;
use aidb::cluster::membership_coordinator;
use aidb::cluster::types::ClusterError;
use aidb::Error as AidbError;
use bytes::Bytes;
use tokio::time::sleep;

use super::bytes_to_str;
use super::map_propose_error;
use super::parse_hex_node_id;
use super::parse_int;

// ---------------------------------------------------------------------------
// CLUSTER MEET
// ---------------------------------------------------------------------------
pub async fn cluster_meet(
    addr: &str,
    port: u16,
    client_port: Option<u16>,
    client_host: Option<&str>,
) -> Result<String, String> {
    let mut last_err = String::new();
    let mut delay_ms = 100u64;
    for attempt in 1u32..=10 {
        let mgr = CLUSTER_STATE_MGR
            .get()
            .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
        let coordinator = mgr
            .membership_coordinator
            .as_ref()
            .ok_or_else(|| "ERR Membership coordinator not available".to_string())?;
        let meta = mgr.meta_raft.get_cluster_meta();
        let next_id = meta.nodes.keys().max().unwrap_or(&0) + 1;
        // client_addr 使用独立的外部可达 host, 与内部 RPC 地址分离
        let client_host = client_host.unwrap_or(addr);
        let client_addr = client_port.map(|cp| format!("{client_host}:{cp}"));
        match coordinator
            .add_node(membership_coordinator::NodeJoinContext {
                node_id: next_id,
                rpc_addr: format!("{addr}:{port}"),
                client_addr,
                join_method: membership_coordinator::JoinMethod::Empty,
            })
            .await
        {
            Ok(()) => return Ok("OK".to_string()),
            Err(e) => {
                let is_not_leader =
                    matches!(&e, AidbError::Cluster(ClusterError::NotLeader { .. }));
                let msg = e.to_string();
                // 兼容尚未归一的 ForwardToLeader Display 串.
                let looks_forward = msg.contains("has to forward request to")
                    || msg.contains("NotLeader")
                    || msg.contains("不是 Leader");
                last_err = map_propose_error(e);
                if (is_not_leader || looks_forward) && attempt < 10 {
                    sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = delay_ms.saturating_mul(2).min(2000);
                    continue;
                }
                return Err(last_err);
            }
        }
    }
    Err(last_err)
}

// ---------------------------------------------------------------------------
// CLUSTER FORGET
// ---------------------------------------------------------------------------
pub async fn cluster_forget(hex_node_id: &str, force: bool) -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let coordinator = mgr
        .membership_coordinator
        .as_ref()
        .ok_or_else(|| "ERR Membership coordinator not available".to_string())?;
    let node_id = parse_hex_node_id(hex_node_id)?;
    coordinator
        .remove_node(membership_coordinator::NodeLeaveContext { node_id, force })
        .await
        .map_err(map_propose_error)?;
    Ok("OK".to_string())
}

/// Parse optional `FORCE` flag for `CLUSTER FORGET <node-id> [FORCE]`.
pub fn parse_forget_force(arg: Option<&[u8]>) -> Result<bool, String> {
    match arg {
        None => Ok(false),
        Some(raw) => {
            let flag = std::str::from_utf8(raw)
                .map_err(|_| "ERR invalid FORGET option".to_string())?
                .trim();
            if flag.eq_ignore_ascii_case("force") {
                Ok(true)
            } else {
                Err(format!("ERR unknown FORGET option '{flag}'"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CLUSTER SAVECONFIG — 持久化集群配置到文件
// ---------------------------------------------------------------------------

/// Persist the current cluster config to nodes.conf.
/// Returns Ok(()) on success, or an error string.
pub fn save_nodes_conf(
    meta_raft: &aidb::cluster::MetaRaftNode,
    data_dir: &std::path::Path,
) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("ERR cannot create data dir: {e}"))?;

    let meta = meta_raft.get_cluster_meta();
    let slot_table = meta_raft.get_slot_table();

    let config_path = data_dir.join("nodes.conf");
    // Serialize cluster config as JSON for inspectability
    let config_json = serde_json::to_string_pretty(&serde_json::json!({
      "version": meta.version,
      "nodes": meta.nodes.iter().map(|(id, n)| serde_json::json!({
        "id": id,
        "rpc_addr": n.rpc_addr,
        "client_addr": n.client_addr,
        "status": format!("{:?}", n.status),
        "role": format!("{:?}", n.role),
      })).collect::<Vec<_>>(),
      "groups": meta.groups.iter().map(|(gid, g)| serde_json::json!({
        "group_id": gid,
        "config_version": g.config_version,
        "replicas": g.replicas.iter().map(|r| serde_json::json!({
          "node_id": r.node_id,
          "is_leader": r.is_leader,
        })).collect::<Vec<_>>(),
      })).collect::<Vec<_>>(),
      "slot_table": slot_table.iter().enumerate().filter_map(|(i, s)| {
        match s {
          aidb::cluster::meta_types::SlotStatus::Assigned(gid) => {
            Some(serde_json::json!({"slot": i, "group_id": gid}))
          }
          _ => None,
        }
      }).collect::<Vec<_>>(),
    }))
    .map_err(|e| format!("ERR serialization: {e}"))?;

    // Atomic write: write to temp -> fsync -> rename
    let tmp_path = config_path.with_extension("tmp");
    std::fs::write(&tmp_path, &config_json).map_err(|e| format!("ERR write config: {e}"))?;
    std::fs::rename(&tmp_path, &config_path).map_err(|e| format!("ERR rename config: {e}"))?;
    // fsync the directory to ensure the rename is durable
    if let Some(parent) = config_path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    tracing::info!(path = %config_path.display(), "cluster config saved");
    Ok(())
}

pub(super) fn cluster_saveconfig() -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let dir = mgr
        .data_dir
        .as_ref()
        .ok_or_else(|| "ERR data_dir not set".to_string())?;
    save_nodes_conf(&mgr.meta_raft, dir)?;
    Ok("OK".to_string())
}
// ---------------------------------------------------------------------------
// CLUSTER FAILOVER
// ---------------------------------------------------------------------------
pub async fn cluster_failover(mode: &str) -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    match mode.to_uppercase().as_str() {
        "FORCE" | "TAKEOVER" => {
            // Read role into local before any .await (parking_lot lock held across await is risky).
            let is_primary = matches!(
                *mgr.role.read(),
                crate::cluster::state::ReplicationRole::Primary
            );
            if is_primary {
                return Err("ERR You should be a replica".to_string());
            }
            let meta = mgr.meta_raft.get_cluster_meta();
            for (gid, group) in &meta.groups {
                if group.replicas.iter().any(|r| r.node_id == mgr.node_id) {
                    mgr.multi_raft
                        .change_group_membership(*gid, vec![mgr.node_id])
                        .await
                        .map_err(|e| format!("ERR {e}"))?;
                    *mgr.role.write() = crate::cluster::state::ReplicationRole::Primary;
                    if let Some(m) = &mgr.metrics {
                        m.on_failover();
                    }
                    return Ok("OK".to_string());
                }
            }
            Err("ERR Node not found in any group".to_string())
        }
        _ => Err("ERR Unknown FAILOVER mode".to_string()),
    }
}

// ---------------------------------------------------------------------------
// CLUSTER REPLICATE
// ---------------------------------------------------------------------------
//
// 设置本节点为指定 primary 的副本 (仅元数据层面).
// 实际的 MultiRaft 成员变更受限于 group 在本地才可操作,
// 当前版本 replicas 不服务数据读取, 这是已知限制.
pub async fn cluster_replicate(primary_id: u64) -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    *mgr.role.write() = crate::cluster::state::ReplicationRole::Replica { primary_id };
    Ok("OK".to_string())
}
// ---------------------------------------------------------------------------
// CLUSTER REPLICAS
// ---------------------------------------------------------------------------
pub fn cluster_replicas(node_id: u64) -> Result<Vec<String>, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let meta = mgr.meta_raft.get_cluster_meta();
    let replicas: Vec<String> = meta
        .groups
        .iter()
        .filter(|(_, g)| {
            g.replicas
                .iter()
                .any(|r| r.node_id == node_id && r.is_leader)
        })
        .flat_map(|(_, g)| &g.replicas)
        .filter(|r| !r.is_leader)
        .map(|r| format!("{:040x}", r.node_id))
        .collect();
    Ok(replicas)
}

pub(super) async fn handle_meet(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    // Write commands
    // Redis 标准: CLUSTER MEET <host> <client_port> [rpc_port] [client_host]
    // - Arg 2: client_port (Redis 标准, 用于 MOVED 重定向)
    // - Arg 3 (可选): rpc_port; 未提供时自动推导为 client_port + 10000
    // - Arg 4 (可选): client_host (Docker 等场景的外部可达地址)
    let addr = args
        .get(1)
        .ok_or_else(|| Error::Command("ERR wrong number of arguments".into()))?;
    let client_port_str = args
        .get(2)
        .ok_or_else(|| Error::Command("ERR wrong number of arguments".into()))?;
    let client_port: u16 = parse_int(client_port_str)
        .ok_or_else(|| Error::Command("ERR invalid client port".into()))?;
    let addr_str = bytes_to_str(addr)?;
    // 可选第 3 参数: RPC 端口; 默认 client_port + 10000 (Redis Cluster 惯例)
    let rpc_port: u16 = args
        .get(3)
        .and_then(|a| parse_int(a))
        .unwrap_or(client_port + 10000);
    // 可选第 4 参数: 客户端可达 host (Docker/容器场景)
    let client_host: Option<&str> = args.get(4).and_then(|a| bytes_to_str(a).ok());
    let msg = cluster_meet(addr_str, rpc_port, Some(client_port), client_host)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}

pub(super) async fn handle_forget(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let hex_id = args
        .get(1)
        .map(|a| bytes_to_str(a))
        .ok_or_else(|| Error::Command("ERR wrong number of arguments".into()))??;
    let force = parse_forget_force(args.get(2).map(|a| a.as_ref())).map_err(Error::Command)?;
    let msg = cluster_forget(hex_id, force)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}

pub(super) async fn handle_failover(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let mode = match args.get(1).map(|a| bytes_to_str(a)) {
        Some(Ok(s)) => s,
        _ => "FORCE",
    };
    let msg = cluster_failover(mode).await.map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}

pub(super) async fn handle_replicate(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let hex_id = args
        .get(1)
        .map(|a| bytes_to_str(a))
        .ok_or_else(|| Error::Command("ERR wrong number of arguments".into()))??;
    let primary_id = parse_hex_node_id(hex_id).map_err(Error::Command)?;
    let msg = cluster_replicate(primary_id)
        .await
        .map_err(Error::Command)?;
    Ok(RespValue::SimpleString(msg))
}

pub(super) fn handle_replicas(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let hex_id = args
        .get(1)
        .map(|a| bytes_to_str(a))
        .ok_or_else(|| Error::Command("ERR wrong number of arguments".into()))??;
    let node_id = parse_hex_node_id(hex_id).map_err(Error::Command)?;
    let replicas = cluster_replicas(node_id).map_err(Error::Command)?;
    Ok(RespValue::Array(Some(
        replicas
            .into_iter()
            .map(|r| RespValue::BulkString(Some(Bytes::from(r))))
            .collect(),
    )))
}

pub(super) fn handle_saveconfig(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    cluster_saveconfig().map_err(Error::Command)?;
    Ok(RespValue::SimpleString("OK".into()))
}

pub(super) async fn handle_bumpepoch(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| Error::Command("CLUSTERDOWN Cluster not initialized".into()))?;
    // Propose BumpEpoch through MetaRaft consensus
    use aidb::cluster::meta_types::MetaRequest;
    let _response = mgr
        .meta_raft
        .propose(MetaRequest::BumpEpoch)
        .await
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    // After consensus, read the new epoch from the state machine
    let new_epoch = mgr.meta_raft.get_cluster_meta().version;
    Ok(RespValue::BulkString(Some(Bytes::from(
        new_epoch.to_string(),
    ))))
}

pub(super) fn handle_set_config_epoch(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    Ok(RespValue::SimpleString("OK".into()))
}

pub(super) fn handle_count_failure_reports(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    Ok(RespValue::Integer(0))
}

pub(super) fn handle_reset(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    if let Some(mode) = args.get(1) {
        let mode_str = bytes_to_str(mode)?;
        if !mode_str.eq_ignore_ascii_case("soft") && !mode_str.eq_ignore_ascii_case("hard") {
            return Err(Error::Command(format!(
                "ERR unknown RESET option '{mode_str}'"
            )));
        }
    }
    Err(Error::Command(
                "ERR CLUSTER RESET is not supported; stop the node and clear data_dir (see docs/modules/cluster.md)".into(),
            ))
}
