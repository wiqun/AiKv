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
            // Node ID will be set during initialize_cluster based on raft_address
            // This ensures consistent node IDs across restarts
            0
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
    pub async fn initialize_cluster(
        &mut self,
        data_dir: &str,
        raft_addr: &str,
        is_bootstrap: bool,
        peers: &[String],
    ) -> Result<()> {
        use openraft::Config as RaftConfig;

        // Generate consistent node ID from raft address
        // This ensures the same node always gets the same ID across restarts
        let node_id = ClusterCommands::generate_node_id_from_addr(raft_addr);
        self.node_id = node_id;

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
        .map_err(|e| {
            crate::error::AikvError::Internal(format!("Failed to create MultiRaftNode: {}", e))
        })?;

        // Initialize MetaRaft
        multi_raft.init_meta_raft(raft_config).await.map_err(|e| {
            crate::error::AikvError::Internal(format!("Failed to init MetaRaft: {}", e))
        })?;

        // Check if the cluster is already initialized by checking Raft metrics
        // If there's already a committed vote or log entries, the cluster was previously initialized
        let already_initialized = {
            if let Some(meta_raft) = multi_raft.meta_raft() {
                let raft = meta_raft.raft();
                let metrics = raft.metrics().borrow().clone();

                // Check if there are any voters in the membership (excluding empty membership)
                let has_voters = !metrics
                    .membership_config
                    .membership()
                    .voter_ids()
                    .collect::<Vec<_>>()
                    .is_empty();

                // Check if there's any committed log
                let has_committed_log = metrics.last_applied.is_some();

                if has_voters || has_committed_log {
                    info!(
                        "MetaRaft already initialized: has_voters={}, has_committed_log={}, membership={:?}",
                        has_voters, has_committed_log, metrics.membership_config.membership()
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        // If bootstrap node and not already initialized, initialize MetaRaft cluster
        if is_bootstrap && !already_initialized {
            // For multi-master setup with pre-configured peers, we need a different approach:
            // All nodes must start BEFORE bootstrap can add them as voters
            // For now, bootstrap as single node and peers will be added dynamically via CLUSTER MEET
            // TODO: Implement proper multi-node bootstrap once all nodes are confirmed running

            if !peers.is_empty() && peers.len() > 1 {
                // Multi-master mode: bootstrap with just this node first
                // Other peers will be added as MetaRaft voters when they join via CLUSTER MEET
                info!("Multi-master mode: Bootstrapping with this node only. Peers will be added when they join: {:?}", peers);
                warn!("Multi-node bootstrap requires all peers to be running. For now, bootstrapping as single node.");
                warn!("Use dynamic membership via CLUSTER MEET to add other masters as MetaRaft voters.");

                multi_raft
                    .initialize_meta_cluster(vec![(self.node_id, raft_addr.to_string())])
                    .await
                    .map_err(|e| {
                        crate::error::AikvError::Internal(format!(
                            "Failed to bootstrap MetaRaft: {}",
                            e
                        ))
                    })?;
            } else {
                // Single-node bootstrap (standard behavior)
                info!("Single-node bootstrap");
                multi_raft
                    .initialize_meta_cluster(vec![(self.node_id, raft_addr.to_string())])
                    .await
                    .map_err(|e| {
                        crate::error::AikvError::Internal(format!(
                            "Failed to bootstrap MetaRaft: {}",
                            e
                        ))
                    })?;
            }

            info!("Cluster bootstrap complete");
        } else if is_bootstrap && already_initialized {
            info!("Skipping cluster bootstrap - MetaRaft already initialized from persisted state");
        }

        // Wrap in Arc after initialization
        let multi_raft = Arc::new(multi_raft);

        // Start Raft network listener in background (gRPC server)
        let raft_addr_clone = raft_addr.to_string();
        let multi_raft_clone = multi_raft.clone();
        tokio::spawn(async move {
            info!("Starting Raft gRPC listener on {}", raft_addr_clone);

            // Extract port from raft address (which may be a hostname like "aikv1:50051")
            // and bind to 0.0.0.0:PORT to accept connections from all interfaces
            let port = raft_addr_clone
                .rsplit(':')
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(50051);

            let bind_addr: SocketAddr = format!("0.0.0.0:{}", port)
                .parse()
                .expect("Failed to create bind address");

            info!(
                "Binding Raft gRPC server to {} (advertised as {})",
                bind_addr, raft_addr_clone
            );

            // Build the Raft gRPC service that dispatches to the MultiRaftNode
            let svc =
                aidb::cluster::raft_network::raft_rpc::raft_service_server::RaftServiceServer::new(
                    crate::cluster::raft_service::MultiRaftService::new(multi_raft_clone),
                );

            if let Err(e) = tonic::transport::Server::builder()
                .add_service(svc)
                .serve(bind_addr)
                .await
            {
                error!("Raft listener failed: {}", e);
                std::process::exit(1);
            }
        });

        // Get MetaRaftNode reference
        let meta_raft = multi_raft.meta_raft().ok_or_else(|| {
            crate::error::AikvError::Internal("MetaRaft not initialized".to_string())
        })?;

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
                    let executor = CommandExecutor::with_port(self.storage.clone(), self.port);

                    #[cfg(feature = "cluster")]
                    if let (Some(meta_raft), Some(multi_raft), Some(router)) =
                        (&self.meta_raft, &self.multi_raft, &self.router)
                    {
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
