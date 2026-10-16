//! 拓扑后台刷新 (轻量 gossip): 周期性从 MetaRaft 刷新本地 group leader 路由缓存,
//! 并递增 gossip metrics. 无 Redis cluster bus PING/PONG; 成员/故障以 MetaRaft/Raft 为准.

use std::sync::Arc;

/// 启动集群拓扑后台刷新循环.
///
/// 从 MetaRaft 同步本地 leader 路由缓存, 并更新 CLUSTER INFO gossip metrics.
/// 不发送 Redis cluster bus PING/PONG; 成员与故障以 MetaRaft/Raft 为准.
/// `CLUSTER NODES` 直接读 MetaRaft (见 `cluster_nodes()`).
pub fn start_background_refresh(
    state: Arc<crate::cluster::state::ClusterStateManager>,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    let interval = std::time::Duration::from_secs(interval_secs);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let meta = state.meta_raft.get_cluster_meta();
            state.refresh();
            if let Some(ref m) = state.metrics {
                m.on_gossip_refresh(meta.nodes.len());
            }
            tracing::trace!(known_nodes = meta.nodes.len(), "topology refresh tick");
        }
    })
}
