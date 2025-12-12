pub mod connection;
pub mod monitor;

pub use monitor::{MonitorBroadcaster, MonitorMessage};

use self::connection::Connection;
use crate::command::CommandExecutor;
use crate::error::Result;
use crate::observability::Metrics;
use crate::storage::StorageEngine;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

#[cfg(feature = "cluster")]
use crate::cluster::{ClusterCommands, MetaRaftNode, MultiRaftNode, Router};

/// AiKv server
pub struct Server {
    addr: String,
    port: u16,
    storage: StorageEngine,
    metrics: Arc<Metrics>,
    monitor_broadcaster: Arc<MonitorBroadcaster>,
    #[cfg(feature = "cluster")]
    node_id: u64,
    #[cfg(feature = "cluster")]
    meta_raft: Option<Arc<MetaRaftNode>>,
    #[cfg(feature = "cluster")]
    multi_raft: Option<Arc<MultiRaftNode>>,
    #[cfg(feature = "cluster")]
    router: Option<Arc<Router>>,
}

impl Server {
    /// Create a new server with the specified address and storage engine
    pub fn new(addr: String, storage: StorageEngine) -> Self {
        // Extract port from address string using proper SocketAddr parsing
        // This handles both IPv4 (127.0.0.1:6379) and IPv6 ([::1]:6379) formats
        let port = addr
            .parse::<SocketAddr>()
            .map(|a| a.port())
            .unwrap_or_else(|_| {
                // Fallback: try to extract port from the end after last ':'
                // This handles edge cases where the string isn't a valid SocketAddr
                addr.rsplit(':')
                    .next()
                    .and_then(|p| p.trim_end_matches(']').parse().ok())
                    .unwrap_or(6379)
            });

        #[cfg(feature = "cluster")]
        let node_id = {
            // Generate a unique node ID for this cluster node
            let node_id = ClusterCommands::generate_node_id();

            info!(
                "Cluster mode enabled: node_id={:040x}, port={}",
                node_id, port
            );

            node_id
        };

        Self {
            addr,
            port,
            storage,
            metrics: Arc::new(Metrics::new()),
            monitor_broadcaster: Arc::new(MonitorBroadcaster::new()),
            #[cfg(feature = "cluster")]
            node_id,
            #[cfg(feature = "cluster")]
            meta_raft: None,
            #[cfg(feature = "cluster")]
            multi_raft: None,
            #[cfg(feature = "cluster")]
            router: None,
        }
    }

    /// Initialize cluster components (cluster feature only)
    #[cfg(feature = "cluster")]
    pub async fn initialize_cluster(&mut self, data_dir: &str, raft_addr: &str, is_bootstrap: bool, peers: &[String]) -> Result<()> {
        use openraft::Config as RaftConfig;

        info!(
            "Initializing cluster: node_id={:040x}, raft_addr={}, bootstrap={}, peers={:?}",
            self.node_id, raft_addr, is_bootstrap, peers
        );

        let raft_config = RaftConfig::default();

        // Create MultiRaftNode
        let mut multi_raft = MultiRaftNode::new(
            self.node_id,
            std::path::Path::new(data_dir),
            raft_config.clone(),
        )
        .await
        .map_err(|e| crate::error::AikvError::Internal(format!("Failed to create MultiRaftNode: {}", e)))?;

        // Initialize MetaRaft
        multi_raft
            .init_meta_raft(raft_config)
            .await
            .map_err(|e| crate::error::AikvError::Internal(format!("Failed to init MetaRaft: {}", e)))?;

        // If bootstrap node, initialize MetaRaft cluster
        if is_bootstrap {
            // Build peer list for MetaRaft initialization
            let meta_peers = if !peers.is_empty() {
                // Use pre-configured peers for multi-master setup
                // Parse peers to extract or generate node IDs
                // For now, we use incremental node IDs based on port order
                let mut peer_nodes = Vec::new();
                for (_idx, peer_addr) in peers.iter().enumerate() {
                    // Generate consistent node ID based on peer address hash
                    let node_id = ClusterCommands::generate_node_id_from_addr(peer_addr);
                    peer_nodes.push((node_id, peer_addr.clone()));
                }
                info!("Multi-master bootstrap with {} peers: {:?}", peer_nodes.len(), peer_nodes);
                peer_nodes
            } else {
                // Single-node bootstrap (legacy behavior)
                info!("Single-node bootstrap (legacy mode)");
                vec![(self.node_id, raft_addr.to_string())]
            };

            multi_raft
                .initialize_meta_cluster(meta_peers)
                .await
                .map_err(|e| {
                    crate::error::AikvError::Internal(format!("Failed to bootstrap MetaRaft: {}", e))
                })?;
            
            info!("Cluster bootstrap complete");
        }

        // Wrap in Arc after initialization
        let multi_raft = Arc::new(multi_raft);

        // Get MetaRaftNode reference
        let meta_raft = multi_raft
            .meta_raft()
            .ok_or_else(|| crate::error::AikvError::Internal("MetaRaft not initialized".to_string()))?;

        // Get initial cluster metadata from MetaRaft
        let cluster_meta = meta_raft.get_cluster_meta();

        // Initialize Router with cluster metadata
        let router = Arc::new(Router::new(cluster_meta));

        self.meta_raft = Some(meta_raft.clone());
        self.multi_raft = Some(multi_raft);
        self.router = Some(router);

        info!("Cluster initialization complete");

        Ok(())
    }

    /// Get server metrics
    pub fn metrics(&self) -> Arc<Metrics> {
        Arc::clone(&self.metrics)
    }

    /// Get monitor broadcaster
    pub fn monitor_broadcaster(&self) -> Arc<MonitorBroadcaster> {
        Arc::clone(&self.monitor_broadcaster)
    }

    /// Run the server
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.addr).await?;
        info!("AiKv server listening on {}", self.addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("New connection from: {}", addr);

                    // Record connection metrics
                    self.metrics.connections.record_connection();

                    // Create executor with or without cluster commands
                    let mut executor = CommandExecutor::with_port(self.storage.clone(), self.port);

                    #[cfg(feature = "cluster")]
                    if let (Some(meta_raft), Some(multi_raft), Some(router)) = 
                        (&self.meta_raft, &self.multi_raft, &self.router) {
                        // Create ClusterCommands for this connection
                        let cluster_commands = ClusterCommands::new(
                            self.node_id,
                            Arc::clone(meta_raft),
                            Arc::clone(multi_raft),
                            Arc::clone(router),
                        );
                        executor.set_cluster_commands(cluster_commands);
                    }

                    let metrics = Arc::clone(&self.metrics);
                    let monitor_broadcaster = Arc::clone(&self.monitor_broadcaster);

                    tokio::spawn(async move {
                        let mut conn = Connection::new(
                            stream,
                            executor,
                            Some(metrics.clone()),
                            Some(monitor_broadcaster),
                        );

                        if let Err(e) = conn.handle().await {
                            error!("Connection error: {}", e);
                        }

                        // Record disconnection
                        metrics.connections.record_disconnection();
                        info!("Connection closed: {}", addr);
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }
}
