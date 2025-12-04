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
use crate::cluster::{ClusterCommands, ClusterState, NodeInfo};
#[cfg(feature = "cluster")]
use std::sync::RwLock;

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
    cluster_state: Arc<RwLock<ClusterState>>,
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
        let (node_id, cluster_state) = {
            // Generate a unique node ID for this cluster node
            let node_id = ClusterCommands::generate_node_id();

            // Create shared cluster state
            let cluster_state = Arc::new(RwLock::new(ClusterState::new()));

            // Register this node in the cluster state
            // Use the actual bind address for the node's address
            // For "0.0.0.0" we use "127.0.0.1" as the external address
            let node_addr = if addr.starts_with("0.0.0.0:") {
                format!("127.0.0.1:{}", port)
            } else {
                addr.clone()
            };

            {
                let mut state_guard = cluster_state.write().unwrap();
                let node_info = NodeInfo::new(node_id, node_addr);
                state_guard.nodes.insert(node_id, node_info);
            }

            info!(
                "Cluster mode enabled: node_id={:040x}, port={}",
                node_id, port
            );

            (node_id, cluster_state)
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
            cluster_state,
        }
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

                    #[cfg(feature = "cluster")]
                    let executor = CommandExecutor::with_shared_cluster_state(
                        self.storage.clone(),
                        self.port,
                        self.node_id,
                        Arc::clone(&self.cluster_state),
                    );

                    #[cfg(not(feature = "cluster"))]
                    let executor = CommandExecutor::with_port(self.storage.clone(), self.port);

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
