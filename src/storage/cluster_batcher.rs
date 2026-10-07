//! 数据面写凑批 actor: 按 data group 将 PUT/DELETE 聚合成 `WriteBatch` 后 propose.
//! 由 `ClusterDataAdapter` 按 group 懒创建.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use aidb::cluster::{Request, ThinWriteBatch};
use aidb::error::ClusterError as AidbClusterError;

use crate::cluster::state::ClusterStateManager;
use crate::error::{Error, Result};
use crate::storage::cluster_adapter::ClusterDataAdapter;

const SET_BATCH_MAX_OPS: usize = 512;
/// 凑批等待上限. 1ms 平衡吞吐与延迟: 集群 50c 负载下 items 到达快, 等 1ms 足矣.
#[allow(dead_code)]
const SET_BATCH_MAX_DELAY: Duration = Duration::from_millis(1);

pub(super) struct GroupSetBatcher {
    tx: mpsc::Sender<WriteBatchItem>,
}

pub(super) enum WriteBatchItem {
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        ack: oneshot::Sender<std::result::Result<bool, String>>,
    },
    Delete {
        key: Vec<u8>,
        ack: oneshot::Sender<std::result::Result<bool, String>>,
    },
}

struct AckMap {
    ack: oneshot::Sender<std::result::Result<bool, String>>,
    /// `None` = reverse-dedup 丢弃的中间 op → ack `false`
    effect_index: Option<usize>,
}

const MIN_BATCH_TARGET: usize = 16;
const MAX_MICRO_WAIT_US: u64 = 50;

pub(super) fn get_or_spawn_set_batcher(
    batchers: &parking_lot::Mutex<HashMap<u64, Arc<GroupSetBatcher>>>,
    mgr: Arc<ClusterStateManager>,
    gid: u64,
    eager_flush: usize,
) -> Arc<GroupSetBatcher> {
    let mut batchers = batchers.lock();
    if let Some(existing) = batchers.get(&gid) {
        return existing.clone();
    }

    let (tx, rx) = mpsc::channel(4096);
    let batcher = Arc::new(GroupSetBatcher { tx });
    tokio::spawn(run_set_batcher(mgr, gid, rx, eager_flush));
    batchers.insert(gid, batcher.clone());
    batcher
}

/// 将多笔 PUT/DELETE 全部送进 batcher 后再等 ack, 使同一请求的用户写与 watch meta
/// 落入同一批 Raft propose (Issue #83).
pub(super) async fn submit_write_ops(
    batchers: &parking_lot::Mutex<HashMap<u64, Arc<GroupSetBatcher>>>,
    eager_flush: usize,
    mgr: Arc<ClusterStateManager>,
    gid: u64,
    ops: Vec<(Vec<u8>, Option<Vec<u8>>)>,
) -> Result<Vec<bool>> {
    enum Slot {
        Ready(bool),
        Wait(oneshot::Receiver<std::result::Result<bool, String>>),
    }

    if ops.is_empty() {
        return Ok(Vec::new());
    }

    let mut prepared: Vec<(Vec<u8>, Option<Vec<u8>>, bool)> = Vec::with_capacity(ops.len());
    for (key, value) in ops {
        if value.is_none() {
            let existed = mgr
                .multi_raft
                .get_local(gid, &key)
                .await
                .map_err(|e| {
                    tracing::warn!(gid = gid, error = %e, "get_local failed in submit_write_ops");
                    ClusterDataAdapter::map_err(e)
                })?
                .is_some();
            if !existed {
                prepared.push((key, value, false));
                continue;
            }
        }
        prepared.push((key, value, true));
    }

    let batcher = get_or_spawn_set_batcher(batchers, mgr, gid, eager_flush);
    let mut slots: Vec<Slot> = Vec::with_capacity(prepared.len());
    for (key, value, send) in prepared {
        if !send {
            slots.push(Slot::Ready(false));
            continue;
        }
        let (ack, wait) = oneshot::channel();
        let item = match value {
            Some(v) => WriteBatchItem::Put { key, value: v, ack },
            None => WriteBatchItem::Delete { key, ack },
        };
        batcher.tx.send(item).await.map_err(|_| {
            Error::Cluster(crate::error::ClusterError::Aidb(
                AidbClusterError::Internal("data group write batcher stopped".into()),
            ))
        })?;
        slots.push(Slot::Wait(wait));
    }

    let mut out = Vec::with_capacity(slots.len());
    for slot in slots {
        match slot {
            Slot::Ready(v) => out.push(v),
            Slot::Wait(wait) => {
                let flag = wait
                    .await
                    .map_err(|_| {
                        Error::Cluster(crate::error::ClusterError::Aidb(
                            AidbClusterError::Internal(
                                "data group write batcher dropped response".into(),
                            ),
                        ))
                    })?
                    .map_err(|e| {
                        Error::Cluster(crate::error::ClusterError::Aidb(
                            AidbClusterError::Internal(e.to_string()),
                        ))
                    })?;
                out.push(flag);
            }
        }
    }
    Ok(out)
}

/// 统一写批入口: 将 PUT 或 DELETE 请求发送到对应 group 的 batcher.
///
/// PUT: 不前置 `get_local`. 成功只认 `Response::Ok`.
/// DELETE: key 不存在则短路跳过 propose, 返回 `Ok(false)`; 已存在并提交成功返回 `Ok(true)`.
pub(super) async fn submit_write_op(
    batchers: &parking_lot::Mutex<HashMap<u64, Arc<GroupSetBatcher>>>,
    eager_flush: usize,
    mgr: Arc<ClusterStateManager>,
    gid: u64,
    key: Vec<u8>,
    value: Option<Vec<u8>>,
) -> Result<bool> {
    let mut flags = submit_write_ops(batchers, eager_flush, mgr, gid, vec![(key, value)]).await?;
    flags.pop().ok_or_else(|| {
        Error::Cluster(crate::error::ClusterError::Aidb(
            AidbClusterError::Internal("submit_write_ops returned no flags".into()),
        ))
    })
}

pub(super) async fn run_set_batcher(
    mgr: Arc<ClusterStateManager>,
    gid: u64,
    mut rx: mpsc::Receiver<WriteBatchItem>,
    _eager_flush: usize,
) {
    while let Some(first) = rx.recv().await {
        let t_start = Instant::now();

        let mut items = Vec::with_capacity(SET_BATCH_MAX_OPS);
        items.push(first);

        // 第一阶段:快速非阻塞拉取
        while items.len() < SET_BATCH_MAX_OPS {
            match rx.try_recv() {
                Ok(item) => items.push(item),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        }

        // 第二阶段:防单打微退避 (仅在 items 数量小于 MIN_BATCH_TARGET 时触发 50us 避震)
        if items.len() < MIN_BATCH_TARGET {
            tokio::time::sleep(Duration::from_micros(MAX_MICRO_WAIT_US)).await;
            while items.len() < SET_BATCH_MAX_OPS {
                match rx.try_recv() {
                    Ok(item) => items.push(item),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        }

        let wait_us = t_start.elapsed().as_micros();

        // Build ThinWriteBatch with LWW reverse-dedup: 同 key 只保留时间序最后一笔.
        let item_count = items.len();
        let mut planned = Vec::with_capacity(item_count);
        let mut acks = Vec::with_capacity(item_count);
        for item in items {
            match item {
                WriteBatchItem::Put { key, value, ack } => {
                    planned.push(PlannedOp::Put { key, value });
                    acks.push(ack);
                }
                WriteBatchItem::Delete { key, ack } => {
                    planned.push(PlannedOp::Delete { key });
                    acks.push(ack);
                }
            }
        }
        let (tb, effect_indices) = plan_reverse_dedup(planned);
        let ack_maps: Vec<AckMap> = acks
            .into_iter()
            .zip(effect_indices)
            .map(|(ack, effect_index)| AckMap { ack, effect_index })
            .collect();

        let t_propose = Instant::now();
        let result = super::cluster_adapter::ClusterDataAdapter::propose_group_with_retry(
            &mgr,
            gid,
            Request::WriteBatch(tb),
        )
        .await
        .and_then(super::cluster_adapter::ClusterDataAdapter::check_response)
        .map_err(|e| e.to_string());

        let propose_us = t_propose.elapsed().as_micros();
        let total_us = t_start.elapsed().as_micros();

        tracing::info!(
            target: "perf",
            gid = gid,
            op_count = item_count,
            wait_us,
            propose_us,
            total_us,
            "batcher_batch_done"
        );

        match result {
            Ok(()) => {
                let indices: Vec<Option<usize>> = ack_maps.iter().map(|m| m.effect_index).collect();
                let bools = map_ack_included(&indices);
                for (m, v) in ack_maps.into_iter().zip(bools) {
                    let _ = m.ack.send(Ok(v));
                }
            }
            Err(e) => {
                for m in ack_maps {
                    let _ = m.ack.send(Err(e.clone()));
                }
            }
        }
    }
}

/// reverse-dedup 规划用的轻量 op (与 `WriteBatchItem` 同语义, 无 ack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PlannedOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// reverse + HashSet 按 key 做 last-write-wins: 同 key 只保留时间序最后一笔
/// (Put 或 Delete). 产出 `ThinWriteBatch` 与每个**时间序** item 对应的 `effects`
/// 下标 (`None` = 被丢掉的中间 op → ack false).
pub(super) fn plan_reverse_dedup(items: Vec<PlannedOp>) -> (ThinWriteBatch, Vec<Option<usize>>) {
    let mut tb = ThinWriteBatch::new();
    let mut indices_rev = Vec::with_capacity(items.len());
    let mut seen: HashSet<Vec<u8>> = HashSet::new();

    for item in items.into_iter().rev() {
        match item {
            PlannedOp::Put { key, value } => {
                if seen.insert(key.clone()) {
                    indices_rev.push(Some(tb.ops.len()));
                    tb.put(key, value);
                } else {
                    indices_rev.push(None);
                }
            }
            PlannedOp::Delete { key } => {
                if seen.insert(key.clone()) {
                    indices_rev.push(Some(tb.ops.len()));
                    tb.delete(key);
                } else {
                    indices_rev.push(None);
                }
            }
        }
    }
    indices_rev.reverse();
    (tb, indices_rev)
}

/// 实际提交的 op 在 `Response::Ok` 后 ack true. 被 reverse-dedup 丢掉的中间 op ack false.
pub(super) fn map_ack_included(effect_indices: &[Option<usize>]) -> Vec<bool> {
    effect_indices.iter().map(|opt| opt.is_some()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aidb::cluster::ThinWriteOp;

    #[test]
    fn map_acks_deduped_put_gets_inserted_false() {
        // 时间序: Put(k,v1), Put(k,v2), Put(m,w)
        // reverse-dedup 后 tb.ops: [Put(m,w), Put(k,v2)]; 丢掉的 Put(k,v1) → false
        let items = vec![
            PlannedOp::Put {
                key: b"k".to_vec(),
                value: b"v1".to_vec(),
            },
            PlannedOp::Put {
                key: b"k".to_vec(),
                value: b"v2".to_vec(),
            },
            PlannedOp::Put {
                key: b"m".to_vec(),
                value: b"w".to_vec(),
            },
        ];
        let (tb, indices) = plan_reverse_dedup(items);
        assert_eq!(tb.ops.len(), 2);
        match &tb.ops[0] {
            ThinWriteOp::Put { key, value } => {
                assert_eq!(key, b"m");
                assert_eq!(value, b"w");
            }
            other => panic!("ops[0] expected Put(m,w), got {other:?}"),
        }
        match &tb.ops[1] {
            ThinWriteOp::Put { key, value } => {
                assert_eq!(key, b"k");
                assert_eq!(value, b"v2");
            }
            other => panic!("ops[1] expected Put(k,v2), got {other:?}"),
        }
        assert_eq!(indices, vec![None, Some(1), Some(0)]);

        let acks = map_ack_included(&indices);
        assert_eq!(acks, vec![false, true, true]);
    }

    /// Put→Del→Put 必须 LWW 保留最后 Put; 旧实现保留全部 Delete 会变成 Put 后再 Del,
    /// 净效果 key 缺失.
    #[test]
    fn plan_reverse_dedup_put_del_put_keeps_last_put() {
        let items = vec![
            PlannedOp::Put {
                key: b"k".to_vec(),
                value: b"v1".to_vec(),
            },
            PlannedOp::Delete { key: b"k".to_vec() },
            PlannedOp::Put {
                key: b"k".to_vec(),
                value: b"v2".to_vec(),
            },
        ];
        let (tb, indices) = plan_reverse_dedup(items);
        assert_eq!(tb.ops.len(), 1);
        match &tb.ops[0] {
            ThinWriteOp::Put { key, value } => {
                assert_eq!(key, b"k");
                assert_eq!(value, b"v2");
            }
            other => panic!("expected Put(k,v2), got {other:?}"),
        }
        assert_eq!(indices, vec![None, None, Some(0)]);
        let acks = map_ack_included(&indices);
        assert_eq!(acks, vec![false, false, true]);
    }

    /// Put→Delete: 最后 Delete 胜出, 中间 Put 丢弃且 ack false.
    #[test]
    fn plan_reverse_dedup_put_then_delete_keeps_delete() {
        let items = vec![
            PlannedOp::Put {
                key: b"k".to_vec(),
                value: b"v1".to_vec(),
            },
            PlannedOp::Delete { key: b"k".to_vec() },
        ];
        let (tb, indices) = plan_reverse_dedup(items);
        assert_eq!(tb.ops.len(), 1);
        assert!(matches!(tb.ops[0], ThinWriteOp::Delete { .. }));
        assert_eq!(indices, vec![None, Some(0)]);
        let acks = map_ack_included(&indices);
        assert_eq!(acks, vec![false, true]);
    }

    /// Delete→Put: 最后 Put 胜出.
    #[test]
    fn plan_reverse_dedup_delete_then_put_keeps_put() {
        let items = vec![
            PlannedOp::Delete { key: b"k".to_vec() },
            PlannedOp::Put {
                key: b"k".to_vec(),
                value: b"v2".to_vec(),
            },
        ];
        let (tb, indices) = plan_reverse_dedup(items);
        assert_eq!(tb.ops.len(), 1);
        match &tb.ops[0] {
            ThinWriteOp::Put { value, .. } => assert_eq!(value, b"v2"),
            other => panic!("expected Put, got {other:?}"),
        }
        assert_eq!(indices, vec![None, Some(0)]);
    }
}
