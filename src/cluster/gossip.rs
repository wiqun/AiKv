//! 拓扑后台刷新 (轻量 gossip): 周期性从 MetaRaft 刷新本地 group leader 路由缓存,
//! 并通过异步 TCP 探活感知 peer 节点在线状态, 递增 gossip metrics.

use std::sync::Arc;

/// 启动集群拓扑后台刷新循环.
///
/// 从 MetaRaft 同步本地 leader 路由缓存, 对集群内 peer 节点进行异步 TCP 探活,
/// 并更新 CLUSTER INFO gossip metrics.
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

            // 异步探测 peer 节点连通性 (连接其 RPC 端口, 500ms 超时)
            for (&peer_id, peer_info) in &meta.nodes {
                if peer_id == state.node_id {
                    continue;
                }
                let rpc_addr = peer_info.rpc_addr.clone();
                let state_clone = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Ok(Ok(_)) = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        tokio::net::TcpStream::connect(&rpc_addr),
                    )
                    .await
                    {
                        state_clone.mark_peer_online(peer_id);
                    }
                });
            }

            tracing::trace!(known_nodes = meta.nodes.len(), "topology refresh tick");
        }
    })
}
