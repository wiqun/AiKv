use crate::cluster::state::CLUSTER_STATE_MGR;
use crate::error::Error;
use crate::protocol::RespValue;
use aidb::cluster::meta_types::SlotMigrationState;
use aidb::cluster::ReplicaAllocator;
#[cfg(feature = "cluster-test-util")]
use aidb::cluster::{failpoint_registry, FailPoint};
use bytes::Bytes;

#[cfg(feature = "cluster-test-util")]
use super::bytes_to_str;

// ---------------------------------------------------------------------------
// CLUSTER FAILPOINT (cluster-test-util feature only)
// ---------------------------------------------------------------------------

/// 故障注入管理.
///
/// CLUSTER FAILPOINT ARM <name> [once]
/// CLUSTER FAILPOINT RELEASE <name>
/// CLUSTER FAILPOINT STATUS
#[cfg(feature = "cluster-test-util")]
fn cluster_failpoint(args: &[Bytes]) -> Result<String, String> {
    let sub = args
        .get(1)
        .ok_or_else(|| "ERR wrong number of arguments".to_string())?;
    let sub_str = bytes_to_str(sub).map_err(|e| e.to_string())?;

    match sub_str.to_uppercase().as_str() {
        "ARM" => {
            let name = args
                .get(2)
                .ok_or_else(|| "ERR wrong number of arguments for ARM".to_string())?;
            let name_str = bytes_to_str(name).map_err(|e| e.to_string())?;
            let fp = FailPoint::from_str(name_str)
                .ok_or_else(|| format!("ERR unknown failpoint: {name_str}"))?;
            if args.get(3).is_some_and(|a| a.eq_ignore_ascii_case(b"once")) {
                failpoint_registry().arm_once(fp);
                Ok(format!("armed {} (once)", fp.display_name()))
            } else {
                failpoint_registry().arm(fp);
                Ok(format!("armed {}", fp.display_name()))
            }
        }
        "RELEASE" => {
            let name = args
                .get(2)
                .ok_or_else(|| "ERR wrong number of arguments for RELEASE".to_string())?;
            let name_str = bytes_to_str(name).map_err(|e| e.to_string())?;
            let fp = FailPoint::from_str(name_str)
                .ok_or_else(|| format!("ERR unknown failpoint: {name_str}"))?;
            failpoint_registry().release(fp);
            Ok(format!("released {}", fp.display_name()))
        }
        "STATUS" => Ok(failpoint_registry().status()),
        _ => Err(format!("ERR unknown FAILPOINT subcommand: {sub_str}")),
    }
}
// ---------------------------------------------------------------------------
// CLUSTER REBALANCE
// ---------------------------------------------------------------------------
/// 从 MetaRaft 迁移状态构造 `ActiveMigration` 并驱动 `run_pending_migration`,
/// 使迁移真正执行数据拷贝. 若无此步, 迁移停留在 `Prepare` 状态,
/// 后续 `finish_migration` (STABLE) 会被 aidb 防呆拒绝.
/// `CLUSTER SETSLOT MIGRATING` 与 `CLUSTER REBALANCE` 共用.
pub(super) async fn run_pending_migration_to_completion(
    sm: &aidb::cluster::SlotMigrationManager,
    migration_id: u64,
) -> Result<(), String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;
    let migration_state = mgr
        .meta_raft
        .get_migration_state()
        .ok_or_else(|| "ERR migration state lost".to_string())?;
    let (src, dst, slots) = match &migration_state {
        SlotMigrationState::Prepare {
            source_group,
            target_group,
            slots,
            ..
        } => (*source_group, *target_group, slots.clone()),
        _ => return Err("ERR unexpected migration state".to_string()),
    };
    let active = aidb::cluster::slot_migration::ActiveMigration {
        migration_id,
        source_group: src,
        target_group: dst,
        slots,
        checkpoint: Vec::new(),
    };
    let result = sm
        .run_pending_migration(active)
        .await
        .map_err(|e| format!("run_migration: {e}"))?;
    if !result.is_completed {
        return Err(format!(
            "migration incomplete: {} keys migrated",
            result.migrated_count
        ));
    }
    Ok(())
}

pub async fn cluster_rebalance() -> Result<String, String> {
    let mgr = CLUSTER_STATE_MGR
        .get()
        .ok_or_else(|| "CLUSTERDOWN Cluster not initialized".to_string())?;

    let slot_table = mgr.meta_raft.get_slot_table();

    // Count slots per group (only assigned slots)
    let mut group_slots: std::collections::HashMap<u64, Vec<u16>> =
        std::collections::HashMap::new();
    for (slot_idx, status) in slot_table.iter().enumerate() {
        if let aidb::cluster::meta_types::SlotStatus::Assigned(gid) = status {
            group_slots.entry(*gid).or_default().push(slot_idx as u16);
        }
    }

    let group_count = group_slots.len();
    if group_count <= 1 {
        return Ok("No rebalance needed (0 or 1 groups)".to_string());
    }

    // Check no migration in progress
    if mgr.meta_raft.get_migration_state().is_some() {
        return Err("ERR migration already in progress".to_string());
    }

    // Compute ideal slots per group (from ReplicaAllocator)
    let ideal_ranges = ReplicaAllocator::suggest_slot_allocation(group_count);
    let ideal_counts: Vec<usize> = ideal_ranges
        .iter()
        .map(|ranges| {
            ranges
                .iter()
                .map(|(start, end)| (end - start + 1) as usize)
                .sum()
        })
        .collect();

    // Map group_ids to a sorted list for matching against ideal_counts
    let mut sorted_gids: Vec<u64> = group_slots.keys().copied().collect();
    sorted_gids.sort();

    // Build surplus/deficit lists by comparing current vs ideal
    let per_group = 16384 / group_count;
    let mut deficits: Vec<(u64, usize)> = Vec::new();
    let mut surpluses: Vec<(u64, Vec<u16>)> = Vec::new();

    for (i, &gid) in sorted_gids.iter().enumerate() {
        let current = group_slots.get(&gid).map(|s| s.len()).unwrap_or(0);
        let ideal = ideal_counts.get(i).copied().unwrap_or(per_group);
        if current > ideal {
            let mut slots = group_slots[&gid].clone();
            slots.sort();
            let excess = current - ideal;
            surpluses.push((gid, slots[slots.len() - excess..].to_vec()));
        } else if current < ideal {
            deficits.push((gid, ideal - current));
        }
    }

    // Execute migrations greedily
    let sm = mgr
        .slot_migration_manager
        .as_ref()
        .ok_or_else(|| "ERR SlotMigrationManager not initialized".to_string())?;

    let mut total_migrated: u64 = 0;

    for (target_gid, mut needed) in deficits {
        while needed > 0 && !surpluses.is_empty() {
            let (src_gid, ref mut surplus_slots) = surpluses[0];
            let take = needed.min(surplus_slots.len());
            let migrate_slots: Vec<u16> = surplus_slots.drain(..take).collect();
            needed -= take;

            // Execute migration
            let migration_id = sm
                .start_migration(src_gid, target_gid, migrate_slots.clone())
                .await
                .map_err(|e| format!("ERR start_migration: {e}"))?;

            // Begin 已进 MetaRaft: 拷贝或收尾失败必须 Cancel, 与 SETSLOT MIGRATING 同语义.
            if let Err(e) = run_pending_migration_to_completion(sm, migration_id).await {
                let base = if e.starts_with("ERR ") {
                    e
                } else {
                    format!("ERR {e}")
                };
                return Err(match sm.cancel_migration().await {
                    Ok(()) => base,
                    Err(cancel_err) => {
                        tracing::warn!(
                            error = %cancel_err,
                            "REBALANCE run_pending failed and cancel_migration also failed"
                        );
                        format!("{base}; cancel_migration also failed: {cancel_err}")
                    }
                });
            }

            // F-056: 完整收尾链, 失败返回 ERR (不得静默跳过).
            if let Err(e) = sm.finish_migration().await {
                let base = format!("ERR finish_migration: {e}");
                return Err(match sm.cancel_migration().await {
                    Ok(()) => base,
                    Err(cancel_err) => {
                        tracing::warn!(
                            error = %cancel_err,
                            "REBALANCE finish failed and cancel_migration also failed"
                        );
                        format!("{base}; cancel_migration also failed: {cancel_err}")
                    }
                });
            }

            total_migrated += migrate_slots.len() as u64;

            if surplus_slots.is_empty() {
                surpluses.remove(0);
            }
        }
    }

    Ok(format!(
        "OK {} slots rebalanced across {} groups",
        total_migrated, group_count
    ))
}

pub(super) async fn handle_rebalance(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let _ = args;
    let msg = cluster_rebalance().await.map_err(Error::Command)?;
    Ok(RespValue::BulkString(Some(Bytes::from(msg))))
}

#[cfg(feature = "cluster-test-util")]
pub(super) fn handle_failpoint(
    args: &[bytes::Bytes],
) -> crate::error::Result<crate::protocol::RespValue> {
    let result = cluster_failpoint(args).map_err(Error::Command)?;
    Ok(RespValue::BulkString(Some(Bytes::from(result))))
}
