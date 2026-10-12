//! 集群数据面存储适配器: 将用户数据的读写路由到数据面 Raft group, 保证 primary
//! failover 后新 leader 能读到旧数据. 包裹本地引擎 (`StorageAdapter`), 当
//! `CLUSTER_STATE_MGR` 已初始化且 key 路由到已分配 slot 的 group 时:
//! - 写 (`set`/`delete`/`write_batch`): 经 `propose_group` 提交到对应 group 的 Raft,
//!   由 Raft 复制并 apply 到各节点的 group 状态机.
//! - 读 (`get`/`exists`/`scan_prefix`): 直接读本地 group 状态机.
//!
//! 集群未初始化 (单机模式) 或 slot 未分配时回退本地引擎.
//!
//! # Invariant
//!
//! - 已分配 slot 的写**必须** `propose_group`, **禁止** 写本地引擎 fallback, 避免
//!   SET 成功但 GET 读 Raft 为空; 仅 `Unallocated` 或未初始化 cluster 写 local.
//! - 惰性 TTL 删除仅当本节点是 key 所在 data group 的 Raft leader 时才允许
//!   (`allow_lazy_expire_delete` 覆盖, 经 `is_local_group_leader` 判断); 只读副本上
//!   惰性过期发现的 key 直接跳过物理删除, 留给 leader 或后续写连接清理.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;

use aidb::cluster::router::key_to_slot;
use aidb::cluster::{
    MigOp, Request, Response, ShardedStorage, SlotMigrationState, SlotStatus, ThinWriteBatch,
};
use aidb::error::ClusterError as AidbClusterError;
use aidb::Checkpoint;

use super::cluster_batcher::{submit_write_op, submit_write_ops, GroupSetBatcher};
use crate::cluster::router::{migration_phase_for_slot, MigrationRoutePhase, TRYAGAIN_MIGRATION};
use crate::cluster::state::{ClusterStateManager, CLUSTER_STATE_MGR};
use crate::error::{Error, Result};
use crate::storage::adapter::{AdapterWriteOp, StorageAdapter};
use crate::storage::aidb::AiDbEngine;
use crate::storage::subkey;
use crate::storage::types::StorageEngineKind;

const GROUP_READY_MAX_ATTEMPTS: u32 = 20;
const GROUP_READY_RETRY_DELAY: Duration = Duration::from_millis(250);
const ERR_DATA_GROUP_NOT_READY: &str = "CLUSTERDOWN data group not ready";

/// F-056-A1 读路由结果: `Single` 为老路径 (本地 group 直读, 无需合并);
/// `Merge` 为 Frozen/Copying 期合并读, 需依次查 target tombstone → target
/// 值 → source 值 (见 `ClusterDataAdapter::merge_read`).
enum ReadRoute {
    Single(u64),
    Merge {
        target_group: u64,
        source_group: u64,
        epoch: u64,
    },
}

/// F-056-A1 写路由结果: `Plain` 为老路径 (`Request::Put`/`Delete`/`WriteBatch`);
/// `Migration` 为 Copying (Prepare/Migrating) 期写, 必须走
/// `Request::MigrationWrite` 记录 tombstone (关闭"不存在则跳过 propose"短路).
enum WriteRoute {
    Plain(u64),
    Migration {
        gid: u64,
        source_group: u64,
        epoch: u64,
    },
}

/// 数据面感知的存储适配器.
pub struct ClusterDataAdapter {
    local: Arc<dyn StorageAdapter>,
    set_batchers: Mutex<HashMap<u64, Arc<GroupSetBatcher>>>,
    eager_flush: usize,
}

impl ClusterDataAdapter {
    /// `eager_flush` 的默认值.
    /// 值 = `SET_BATCH_MAX_OPS / 10.67`, 保持与原始 128/12 相同的比例.
    pub const DEFAULT_EAGER_FLUSH: usize = 48;

    pub fn new(local: Arc<dyn StorageAdapter>, eager_flush: usize) -> Arc<Self> {
        Arc::new(Self {
            local,
            set_batchers: Mutex::new(HashMap::new()),
            eager_flush,
        })
    }

    /// 本节点作为迁槽目标时的本地 data group (IMPORTING / MIGRATE RESTORE).
    fn local_migration_target_group(mgr: &ClusterStateManager) -> Option<u64> {
        if let Some(state) = mgr.meta_raft.get_migration_state() {
            let target = match &state {
                SlotMigrationState::Prepare { target_group, .. }
                | SlotMigrationState::Migrating { target_group, .. }
                | SlotMigrationState::Frozen { target_group, .. }
                | SlotMigrationState::ReadyToCommit { target_group, .. } => *target_group,
            };
            // 仅当 target group 确实在本节点 self.groups 中时才使用.
            if mgr.multi_raft.get_groups().read().contains_key(&target) {
                return Some(target);
            }
        }
        // 回退: 使用本节点的任意本地 group.
        mgr.multi_raft.local_group_ids().into_iter().next()
    }

    /// IMPORTING 窗口内写操作应落到本节点承载的目标 data group, 而非 router 仍显示的源 group.
    /// Frozen / ReadyToCommit 写冻结由 `write_frozen_err` 拦截, 此处不放行.
    fn importing_write_group(mgr: &ClusterStateManager, slot: u16) -> Option<u64> {
        if !mgr.importing_slots.read().contains_key(&slot) {
            return None;
        }
        if let Some(phase) = migration_phase_for_slot(mgr, slot) {
            if phase.writes_frozen() {
                return None;
            }
        }
        Self::local_migration_target_group(mgr)
    }

    fn write_frozen_err() -> Error {
        Error::Command(TRYAGAIN_MIGRATION.into())
    }

    /// F-056: Frozen/Ready 写冻结 — 与 `ClusterRouter::decide` 共用相位.
    fn reject_if_write_frozen(mgr: &ClusterStateManager, routing_key: &[u8]) -> Result<()> {
        let slot = key_to_slot(routing_key);
        if let Some(phase) = migration_phase_for_slot(mgr, slot) {
            if phase.writes_frozen() {
                return Err(Self::write_frozen_err());
            }
        }
        Ok(())
    }

    /// F-056-A1 读路由: ReadyToCommit → 纯 target (仅本地时走快路径, 语义不变);
    /// Frozen/Copying → 合并读 (`ReadRoute::Merge`), 不要求 target/source 本地
    /// —— `merge_read` 内部按需经 `get_key_from_group_remote` 远程读取.
    fn migration_read_route(mgr: &ClusterStateManager, slot: u16) -> Result<Option<ReadRoute>> {
        let Some(phase) = migration_phase_for_slot(mgr, slot) else {
            return Ok(None);
        };
        match phase {
            MigrationRoutePhase::ReadyToCommit { target_group, .. } => Ok(mgr
                .multi_raft
                .get_groups()
                .read()
                .contains_key(&target_group)
                .then_some(ReadRoute::Single(target_group))),
            MigrationRoutePhase::Frozen {
                source_group,
                target_group,
            }
            | MigrationRoutePhase::Copying {
                source_group,
                target_group,
            } => {
                let epoch = mgr
                    .migration_epoch()
                    .ok_or_else(|| Error::Command(TRYAGAIN_MIGRATION.into()))?;
                Ok(Some(ReadRoute::Merge {
                    target_group,
                    source_group,
                    epoch,
                }))
            }
        }
    }

    /// FIX-0056-A1 合并读线性点 (算法/合并读): 先查 target 上该 epoch 内 key 的
    /// tombstone —— 最后一次是 Del 则直接返回 None (禁止 source 复活); 否则读
    /// target 值, 命中即返回 (target-wins); target miss 时才回落 source (数据
    /// 尚未拷贝到 target 的情形). 任一步 RPC 失败/超时都必须返回 TRYAGAIN,
    /// 禁止静默 fallback 到可能陈旧的本地视图 (读导向 点 3).
    async fn merge_read(
        mgr: &ClusterStateManager,
        target_group: u64,
        source_group: u64,
        epoch: u64,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let tombstone = mgr
            .multi_raft
            .get_migration_tombstone_remote(target_group, epoch, key)
            .await
            .map_err(Self::merge_read_err)?;
        if matches!(tombstone, Some(MigOp::Del)) {
            return Ok(None);
        }
        if let Some(v) = mgr
            .multi_raft
            .get_key_from_group_remote(target_group, key)
            .await
            .map_err(Self::merge_read_err)?
        {
            return Ok(Some(v));
        }
        mgr.multi_raft
            .get_key_from_group_remote(source_group, key)
            .await
            .map_err(Self::merge_read_err)
    }

    fn merge_read_err<E: std::fmt::Display>(e: E) -> Error {
        tracing::warn!(error = %e, "merge_read failed, mapping to TRYAGAIN");
        Error::Command(TRYAGAIN_MIGRATION.into())
    }

    /// 若 `slot` 处于 Copying (Prepare/Migrating), 返回 `(source_group, epoch)`
    /// 供写路径构造 `Request::MigrationWrite`. Frozen/ReadyToCommit 写已在
    /// `reject_if_write_frozen` 拦截, 不会到达这里. epoch 缺失属于不应发生的
    /// 内部不一致 (`BeginSlotMigration` 与 `migration_state` 同一 apply 内落地),
    /// 保守返回 TRYAGAIN 而非静默退化为非迁移写 (会漏记 tombstone).
    fn copying_migration_route(mgr: &ClusterStateManager, slot: u16) -> Result<Option<(u64, u64)>> {
        match migration_phase_for_slot(mgr, slot) {
            Some(MigrationRoutePhase::Copying { source_group, .. }) => {
                let epoch = mgr
                    .migration_epoch()
                    .ok_or_else(|| Error::Command(TRYAGAIN_MIGRATION.into()))?;
                Ok(Some((source_group, epoch)))
            }
            _ => Ok(None),
        }
    }

    /// 读路径: 按 router 已分配 slot 路由到本地 group, 或 F-056-A1 合并读.
    /// ReadyToCommit 读切 target (纯 target, 仅本地时走快路径); Frozen/Copying
    /// 读为合并读 (可能涉及远程 RPC, 由调用方 `merge_read` 处理).
    fn route_read(key: &[u8]) -> Result<Option<(Arc<ClusterStateManager>, ReadRoute)>> {
        let Some(mgr) = CLUSTER_STATE_MGR.get().cloned() else {
            return Ok(None);
        };
        let Some(routing_key) = subkey::routing_user_key(key) else {
            return Ok(None);
        };
        let routing_key = routing_key.as_slice();
        let slot = key_to_slot(routing_key);
        if let Some(route) = Self::migration_read_route(&mgr, slot)? {
            return Ok(Some((mgr, route)));
        }
        Ok(match mgr.router.route_key(routing_key) {
            Ok((gid, SlotStatus::Assigned(_))) => Some((mgr, ReadRoute::Single(gid))),
            Ok((gid, SlotStatus::Migrating(_)))
                if mgr.multi_raft.get_groups().read().contains_key(&gid) =>
            {
                Some((mgr, ReadRoute::Single(gid)))
            }
            _ => None,
        })
    }

    /// 写路径: IMPORTING 时优先写入本节点目标 group.
    /// 返回 `Err(TRYAGAIN)` 当 Frozen/Ready 写冻结, 或 Copying 期 epoch 缺失;
    /// `Ok(None)` 表示无本地 group.
    fn route_write(key: &[u8]) -> Result<Option<(Arc<ClusterStateManager>, WriteRoute)>> {
        let Some(mgr) = CLUSTER_STATE_MGR.get().cloned() else {
            return Ok(None);
        };
        let Some(routing_key) = subkey::routing_user_key(key) else {
            return Ok(None);
        };
        let routing_key = routing_key.as_slice();
        Self::reject_if_write_frozen(&mgr, routing_key)?;
        let slot = key_to_slot(routing_key);
        let migration = Self::copying_migration_route(&mgr, slot)?;
        let gid = if let Some(gid) = Self::importing_write_group(&mgr, slot) {
            Some(gid)
        } else {
            match mgr.router.route_key(routing_key) {
                Ok((gid, SlotStatus::Assigned(_))) => {
                    if mgr.multi_raft.get_groups().read().contains_key(&gid) {
                        Some(gid)
                    } else {
                        // decide() 已在 IMPORTING 窗口放行本地写, 但 router 仍指向源 group.
                        Self::local_migration_target_group(&mgr)
                    }
                }
                Ok((gid, SlotStatus::Migrating(_))) => {
                    if mgr.multi_raft.get_groups().read().contains_key(&gid) {
                        Some(gid)
                    } else {
                        Self::local_migration_target_group(&mgr)
                    }
                }
                _ => None,
            }
        };
        Ok(gid.map(|gid| {
            let route = match migration {
                Some((source_group, epoch)) => WriteRoute::Migration {
                    gid,
                    source_group,
                    epoch,
                },
                None => WriteRoute::Plain(gid),
            };
            (mgr, route)
        }))
    }

    pub(super) fn map_err<E: std::fmt::Display>(e: E) -> Error {
        Error::Cluster(crate::error::ClusterError::Aidb(
            AidbClusterError::Internal(e.to_string()),
        ))
    }

    pub(super) fn check_response(resp: Response) -> Result<()> {
        match resp {
            Response::Ok => Ok(()),
            Response::Error(msg) => Err(Error::Cluster(crate::error::ClusterError::Aidb(
                AidbClusterError::Internal(msg),
            ))),
            other => Err(Error::Cluster(crate::error::ClusterError::Aidb(
                AidbClusterError::Internal(format!(
                    "unexpected response for data write: {other:?}"
                )),
            ))),
        }
    }

    fn data_group_not_ready_err() -> Error {
        Error::Cluster(crate::error::ClusterError::Aidb(
            AidbClusterError::InvalidState(ERR_DATA_GROUP_NOT_READY.into()),
        ))
    }

    fn key_slot_status(key: &[u8]) -> Option<SlotStatus> {
        let mgr = CLUSTER_STATE_MGR.get()?;
        let user_key = subkey::routing_user_key(key)?;
        mgr.router
            .route_key(&user_key)
            .ok()
            .map(|(_, status)| status)
    }

    /// 仅单机或未分配 slot 时允许写本地引擎; 已分配 slot 必须走 Raft.
    fn should_use_local_engine(key: &[u8]) -> bool {
        match Self::key_slot_status(key) {
            None => true,
            Some(SlotStatus::Unallocated) => true,
            Some(_) => false,
        }
    }

    fn is_transient_cluster_err(err: &Error) -> bool {
        match err {
            Error::Cluster(crate::error::ClusterError::Aidb(ref e)) => {
                matches!(
                    e,
                    AidbClusterError::Raft(_)
                        | AidbClusterError::NotLeader { .. }
                        | AidbClusterError::Timeout(_)
                        | AidbClusterError::InvalidState(_)
                        | AidbClusterError::Internal(_)
                )
            }
            _ => false,
        }
    }

    pub(super) async fn propose_group_with_retry(
        mgr: &ClusterStateManager,
        gid: u64,
        request: Request,
    ) -> Result<Response> {
        let mut last_err = Self::data_group_not_ready_err();
        let started = std::time::Instant::now();
        let note = || {
            if let Some(metrics) = &mgr.metrics {
                metrics.note_propose_group(gid, started.elapsed().as_micros() as u64, true);
            }
        };
        for attempt in 0..GROUP_READY_MAX_ATTEMPTS {
            match mgr.multi_raft.propose_group(gid, request.clone()).await {
                Ok(resp) => {
                    note();
                    return Ok(resp);
                }
                Err(e) => {
                    last_err = Self::map_err(e);
                    if Self::is_transient_cluster_err(&last_err)
                        && attempt + 1 < GROUP_READY_MAX_ATTEMPTS
                    {
                        tracing::warn!(gid, attempt, "forward retry");
                        tokio::time::sleep(GROUP_READY_RETRY_DELAY).await;
                        continue;
                    }
                    note();
                    return Err(last_err);
                }
            }
        }
        note();
        Err(last_err)
    }
}

/// Checkpoint every local data-group DB under `dest/group_{id}/`.
pub(crate) fn checkpoint_group_storages(
    storages: &HashMap<u64, ShardedStorage>,
    dest: &Path,
) -> Result<PathBuf> {
    if storages.is_empty() {
        return Err(Error::Command(
            "ERR no local data groups to checkpoint".into(),
        ));
    }
    std::fs::create_dir_all(dest).map_err(|e| Error::Storage(e.to_string()))?;
    for (gid, storage) in storages {
        storage
            .db()
            .flush()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let group_dest = dest.join(format!("group_{gid}"));
        Checkpoint::create(storage.db().as_ref(), &group_dest)
            .map_err(|e| Error::Storage(e.to_string()))?;
    }
    Ok(dest.to_path_buf())
}

fn flush_group_storages(storages: &HashMap<u64, ShardedStorage>) -> Result<()> {
    for storage in storages.values() {
        storage
            .db()
            .flush()
            .map_err(|e| Error::Storage(e.to_string()))?;
    }
    Ok(())
}

#[async_trait]
impl StorageAdapter for ClusterDataAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        match Self::route_read(&key)? {
            Some((mgr, ReadRoute::Single(gid))) => {
                mgr.multi_raft.get_local(gid, &key).await.map_err(|e| {
                    tracing::warn!(gid = gid, error = %e, "get_local failed");
                    Self::map_err(e)
                })
            }
            Some((
                mgr,
                ReadRoute::Merge {
                    target_group,
                    source_group,
                    epoch,
                },
            )) => Self::merge_read(&mgr, target_group, source_group, epoch, &key).await,
            None if Self::should_use_local_engine(&key) => self.local.get(key).await,
            None => Err(Self::data_group_not_ready_err()),
        }
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        match Self::route_write(&key)? {
            Some((mgr, WriteRoute::Migration { gid, epoch, .. })) => {
                let mut ops = ThinWriteBatch::new();
                ops.put(key, value);
                let resp = Self::propose_group_with_retry(
                    &mgr,
                    gid,
                    Request::MigrationWrite { epoch, ops },
                )
                .await?;
                Self::check_response(resp)
            }
            Some((mgr, WriteRoute::Plain(gid))) => {
                submit_write_op(
                    &self.set_batchers,
                    self.eager_flush,
                    mgr,
                    gid,
                    key,
                    Some(value),
                )
                .await?;
                Ok(())
            }
            None if Self::should_use_local_engine(&key) => self.local.set(key, value).await,
            None => Err(Self::data_group_not_ready_err()),
        }
    }

    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut by_gid: HashMap<u64, (Arc<ClusterStateManager>, Vec<(Vec<u8>, Option<Vec<u8>>)>)> =
            HashMap::new();
        let mut by_mig: HashMap<
            u64,
            (
                Arc<ClusterStateManager>,
                u64,
                Vec<(Vec<u8>, Option<Vec<u8>>)>,
            ),
        > = HashMap::new();
        let mut fallback: Vec<AdapterWriteOp> = Vec::new();

        for op in ops {
            let key_ref = match &op {
                AdapterWriteOp::Put { key, .. } | AdapterWriteOp::Delete { key } => key.as_slice(),
            };
            match Self::route_write(key_ref)? {
                Some((mgr, WriteRoute::Plain(gid))) => {
                    let tuple = match op {
                        AdapterWriteOp::Put { key, value } => (key, Some(value)),
                        AdapterWriteOp::Delete { key } => (key, None),
                    };
                    by_gid
                        .entry(gid)
                        .or_insert_with(|| (mgr, Vec::new()))
                        .1
                        .push(tuple);
                }
                Some((mgr, WriteRoute::Migration { gid, epoch, .. })) => {
                    let tuple = match op {
                        AdapterWriteOp::Put { key, value } => (key, Some(value)),
                        AdapterWriteOp::Delete { key } => (key, None),
                    };
                    by_mig
                        .entry(gid)
                        .or_insert_with(|| (mgr, epoch, Vec::new()))
                        .2
                        .push(tuple);
                }
                _ => fallback.push(op),
            }
        }

        for (gid, (mgr, items)) in by_gid {
            submit_write_ops(&self.set_batchers, self.eager_flush, mgr, gid, items).await?;
        }

        for (gid, (mgr, epoch, items)) in by_mig {
            let mut tb = ThinWriteBatch::new();
            for (key, value) in &items {
                match value {
                    Some(v) => tb.put(key.clone(), v.clone()),
                    None => tb.delete(key.clone()),
                }
            }
            let resp = Self::propose_group_with_retry(
                &mgr,
                gid,
                Request::MigrationWrite { epoch, ops: tb },
            )
            .await?;
            Self::check_response(resp)?;
        }

        for op in fallback {
            match op {
                AdapterWriteOp::Put { key, value } => self.set(key, value).await?,
                AdapterWriteOp::Delete { key } => {
                    self.delete(key).await?;
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
        match Self::route_write(&key)? {
            Some((
                mgr,
                WriteRoute::Migration {
                    gid,
                    source_group,
                    epoch,
                },
            )) => {
                // FIX-0056-A1: 迁移期一律 propose (禁止"不存在则跳过"短路),
                // existed 仅用于 DEL 返回值, 经合并读判断 (target 或 source 命中即算存在).
                let existed = Self::merge_read(&mgr, gid, source_group, epoch, &key)
                    .await?
                    .is_some();
                let mut ops = ThinWriteBatch::new();
                ops.delete(key);
                let resp = Self::propose_group_with_retry(
                    &mgr,
                    gid,
                    Request::MigrationWrite { epoch, ops },
                )
                .await?;
                Self::check_response(resp)?;
                Ok(existed)
            }
            Some((mgr, WriteRoute::Plain(gid))) => {
                submit_write_op(&self.set_batchers, self.eager_flush, mgr, gid, key, None).await
            }
            None if Self::should_use_local_engine(&key) => self.local.delete(key).await,
            None => Err(Self::data_group_not_ready_err()),
        }
    }

    async fn exists(&self, key: Vec<u8>) -> Result<bool> {
        match Self::route_read(&key)? {
            Some((mgr, ReadRoute::Single(gid))) => {
                let v = mgr.multi_raft.get_local(gid, &key).await.map_err(|e| {
                    tracing::warn!(gid = gid, error = %e, "get_local failed in exists");
                    Self::map_err(e)
                })?;
                Ok(v.is_some())
            }
            Some((
                mgr,
                ReadRoute::Merge {
                    target_group,
                    source_group,
                    epoch,
                },
            )) => Ok(
                Self::merge_read(&mgr, target_group, source_group, epoch, &key)
                    .await?
                    .is_some(),
            ),
            None if Self::should_use_local_engine(&key) => self.local.exists(key).await,
            None => Err(Self::data_group_not_ready_err()),
        }
    }

    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        // 按 group 分组后逐组提交, 避免跨 group 的单次提案.
        // F-056-A1: Copying 期写额外按 (gid, epoch) 分组走 MigrationWrite.
        // inserted/deleted 一律来自 propose 后 WriteStats (禁止 propose 前 exists).
        let mut plain_by_group: HashMap<u64, (Arc<ClusterStateManager>, ThinWriteBatch)> =
            HashMap::new();
        let mut migration_by_group: HashMap<u64, (Arc<ClusterStateManager>, u64, ThinWriteBatch)> =
            HashMap::new();
        let mut local_ops: Vec<AdapterWriteOp> = Vec::new();
        for op in batch {
            let key = match &op {
                AdapterWriteOp::Put { key, .. } => key.as_slice(),
                AdapterWriteOp::Delete { key } => key.as_slice(),
            };
            match Self::route_write(key)? {
                Some((mgr, WriteRoute::Migration { gid, epoch, .. })) => {
                    let entry = migration_by_group
                        .entry(gid)
                        .or_insert_with(|| (mgr, epoch, ThinWriteBatch::new()));
                    match op {
                        AdapterWriteOp::Put { key, value } => entry.2.put(key, value),
                        AdapterWriteOp::Delete { key } => entry.2.delete(key),
                    }
                }
                Some((mgr, WriteRoute::Plain(gid))) => {
                    let entry = plain_by_group
                        .entry(gid)
                        .or_insert_with(|| (mgr, ThinWriteBatch::new()));
                    match op {
                        AdapterWriteOp::Put { key, value } => entry.1.put(key, value),
                        AdapterWriteOp::Delete { key } => entry.1.delete(key),
                    }
                }
                None if Self::should_use_local_engine(key) => local_ops.push(op),
                None => return Err(Self::data_group_not_ready_err()),
            }
        }

        for (gid, (mgr, tb)) in plain_by_group {
            let resp = Self::propose_group_with_retry(&mgr, gid, Request::WriteBatch(tb)).await?;
            Self::check_response(resp)?;
        }
        for (gid, (mgr, epoch, ops)) in migration_by_group {
            let resp =
                Self::propose_group_with_retry(&mgr, gid, Request::MigrationWrite { epoch, ops })
                    .await?;
            Self::check_response(resp)?;
        }
        if !local_ops.is_empty() {
            self.local.write_batch(local_ops).await?;
        }
        Ok(())
    }

    async fn visit_range(
        &self,
        start: Vec<u8>,
        end: Option<Vec<u8>>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<bool> + Send>,
    ) -> Result<()> {
        match CLUSTER_STATE_MGR.get() {
            Some(mgr) => merge_local_ranges(mgr, start, end, &mut f).await,
            None => self.local.visit_range(start, end, f).await,
        }
    }

    async fn for_each_prefix(
        &self,
        prefix: Vec<u8>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        match CLUSTER_STATE_MGR.get() {
            Some(mgr) => {
                let end = AiDbEngine::prefix_end(&prefix);
                for gid in mgr.multi_raft.local_group_ids() {
                    let pairs = mgr
                        .multi_raft
                        .scan_local_pairs_in_range(gid, prefix.clone(), end.clone())
                        .await
                        .map_err(Self::map_err)?;
                    for (k, v) in pairs {
                        f(k, v)?;
                    }
                }
                Ok(())
            }
            None => self.local.for_each_prefix(prefix, f).await,
        }
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        match CLUSTER_STATE_MGR.get() {
            Some(mgr) => {
                for gid in mgr.multi_raft.local_group_ids() {
                    let keys = mgr
                        .multi_raft
                        .scan_keys(gid, Some((start.clone(), end.clone())))
                        .await
                        .map_err(Self::map_err)?;
                    let mut tb = ThinWriteBatch::new();
                    for key in keys {
                        tb.delete(key);
                    }
                    if !tb.ops.is_empty() {
                        let resp = mgr
                            .multi_raft
                            .propose_group(gid, Request::WriteBatch(tb))
                            .await
                            .map_err(Self::map_err)?;
                        Self::check_response(resp)?;
                    }
                }
                Ok(())
            }
            None => self.local.delete_range(start, end).await,
        }
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.estimate_entry_count().await? as usize)
    }

    async fn estimate_entry_count(&self) -> Result<u64> {
        match CLUSTER_STATE_MGR.get() {
            Some(mgr) => Ok(mgr.multi_raft.estimate_entry_count()),
            None => self.local.estimate_entry_count().await,
        }
    }

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.len().await? == 0)
    }

    async fn clear(&self) -> Result<()> {
        match CLUSTER_STATE_MGR.get() {
            Some(mgr) => {
                for gid in mgr.multi_raft.local_group_ids() {
                    let pairs = mgr
                        .multi_raft
                        .scan_local_pairs(gid)
                        .await
                        .map_err(Self::map_err)?;
                    let mut tb = ThinWriteBatch::new();
                    for (k, _) in pairs {
                        tb.delete(k);
                    }
                    if !tb.ops.is_empty() {
                        let resp = mgr
                            .multi_raft
                            .propose_group(gid, Request::WriteBatch(tb))
                            .await
                            .map_err(Self::map_err)?;
                        Self::check_response(resp)?;
                    }
                }
                Ok(())
            }
            None => self.local.clear().await,
        }
    }

    async fn flush(&self) -> Result<()> {
        if let Some(mgr) = CLUSTER_STATE_MGR.get() {
            let storages = mgr.multi_raft.get_storages().read().clone();
            if !storages.is_empty() {
                let storages = storages;
                tokio::task::spawn_blocking(move || flush_group_storages(&storages))
                    .await
                    .map_err(|e| Error::Storage(e.to_string()))??;
            }
        }
        self.local.flush().await
    }

    async fn create_checkpoint(&self, dest: &Path) -> Result<PathBuf> {
        match CLUSTER_STATE_MGR.get() {
            Some(mgr) => {
                let storages = mgr.multi_raft.get_storages().read().clone();
                if storages.is_empty() {
                    return self.local.create_checkpoint(dest).await;
                }
                let dest = dest.to_path_buf();
                tokio::task::spawn_blocking(move || checkpoint_group_storages(&storages, &dest))
                    .await
                    .map_err(|e| Error::Storage(e.to_string()))?
            }
            None => self.local.create_checkpoint(dest).await,
        }
    }

    async fn close(&self) -> Result<()> {
        self.local.close().await
    }

    fn engine_kind(&self) -> StorageEngineKind {
        self.local.engine_kind()
    }

    fn approximate_memory_bytes(&self) -> Option<u64> {
        self.local.approximate_memory_bytes()
    }

    fn aidb_statistics(&self) -> Option<std::sync::Arc<aidb::Statistics>> {
        self.local.aidb_statistics()
    }

    fn allow_lazy_expire_delete(&self, key: &[u8]) -> bool {
        match Self::route_read(key) {
            // 只有本节点是该 group 的 Raft leader 时, 惰性过期才值得发起 propose;
            // 副本上 propose 必然以 NotLeader 失败 (见 node.rs::propose 不做网络转发),
            // 交给 leader 自己的读路径或后续写连接去清理即可.
            Ok(Some((mgr, ReadRoute::Single(gid)))) => mgr.is_local_group_leader(gid),
            // 合并读期 (Frozen/Copying): 迁移期写一律落 target group, 惰性过期同理.
            Ok(Some((mgr, ReadRoute::Merge { target_group, .. }))) => {
                mgr.is_local_group_leader(target_group)
            }
            // 未路由到 data group (单机模式或 slot 未分配): 走本地引擎, 始终允许.
            Ok(None) => true,
            // 路由本身失败 (如迁移 epoch 缺失): 保守放弃, 交由下次读/写连接清理.
            Err(_) => false,
        }
    }
}

type RangeVisitor = Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<bool> + Send>;
type RangeItem = std::result::Result<(Vec<u8>, Vec<u8>), String>;

async fn merge_local_ranges(
    mgr: &ClusterStateManager,
    start: Vec<u8>,
    end: Option<Vec<u8>>,
    f: &mut RangeVisitor,
) -> Result<()> {
    let gids = mgr.multi_raft.local_group_ids();
    let mut receivers = Vec::with_capacity(gids.len());
    for gid in gids {
        let (tx, rx) = tokio::sync::mpsc::channel::<RangeItem>(32);
        let raft = Arc::clone(&mgr.multi_raft);
        let start = start.clone();
        let end = end.clone();
        tokio::spawn(async move {
            let tx_pairs = tx.clone();
            let scanned = raft
                .visit_local_user_range(gid, start, end, move |key, value| {
                    tx_pairs.blocking_send(Ok((key, value))).is_ok()
                })
                .await;
            if let Err(err) = scanned {
                let _ = tx.blocking_send(Err(err.to_string()));
            }
        });
        receivers.push(rx);
    }

    let mut heads = Vec::with_capacity(receivers.len());
    for rx in &mut receivers {
        heads.push(recv_user_pair(rx).await?);
    }
    loop {
        let mut best: Option<usize> = None;
        for (index, head) in heads.iter().enumerate() {
            if let Some((key, _)) = head {
                let take = match best {
                    None => true,
                    Some(chosen) => key.as_slice() < heads[chosen].as_ref().unwrap().0.as_slice(),
                };
                if take {
                    best = Some(index);
                }
            }
        }
        let Some(index) = best else {
            break;
        };
        let (key, value) = heads[index].take().unwrap();
        if !f(key, value)? {
            break;
        }
        heads[index] = recv_user_pair(&mut receivers[index]).await?;
    }
    Ok(())
}

async fn recv_user_pair(
    rx: &mut tokio::sync::mpsc::Receiver<RangeItem>,
) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    match rx.recv().await {
        None => Ok(None),
        Some(Ok(pair)) => Ok(Some(pair)),
        Some(Err(err)) => Err(ClusterDataAdapter::map_err(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aidb::config::Options;
    use tempfile::tempdir;

    #[test]
    fn check_response_accepts_ok() {
        ClusterDataAdapter::check_response(Response::Ok).expect("Ok");
        assert!(ClusterDataAdapter::check_response(Response::Value(Some(vec![1]))).is_err());
        assert!(ClusterDataAdapter::check_response(Response::Error("x".into())).is_err());
    }

    #[test]
    fn checkpoint_group_storages_roundtrip() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let backup_dir = dir.path().join("backup");
        std::fs::create_dir_all(&data_dir).expect("data dir");

        let storage =
            ShardedStorage::open(&data_dir, 1, Options::for_testing()).expect("open group storage");
        storage.db().put(b"k1", b"v1").expect("put");
        storage.db().flush().expect("flush");

        let mut storages = HashMap::new();
        storages.insert(1, storage);

        let path = checkpoint_group_storages(&storages, &backup_dir).expect("checkpoint");
        assert_eq!(path, backup_dir);
        assert!(backup_dir.join("group_1").join("CURRENT").exists());

        let restored =
            aidb::DB::open(backup_dir.join("group_1"), Options::for_testing()).expect("restore");
        assert_eq!(restored.get(b"k1").expect("get"), Some(b"v1".to_vec()));
    }
}
