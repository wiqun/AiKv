//! Cluster node implementation wrapping AiDb's MultiRaftNode.
//!
//! This module provides a thin wrapper around AiDb's Multi-Raft implementation
//! for Redis Cluster compatibility.

use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;

/// Node ID type alias
pub type NodeId = u64;

/// Group ID type alias (for Raft Groups)
#[cfg(feature = "cluster")]
pub type GroupId = u64;

/// Configuration for cluster node initialization
#[cfg(feature = "cluster")]
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Node ID
    pub node_id: NodeId,

    /// Data directory path
    pub data_dir: PathBuf,

    /// Node's bind address (ip:port) for Redis protocol
    pub bind_address: String,

    /// Node's Raft RPC address (ip:port) for gRPC
    pub raft_address: String,

    /// Number of Raft groups for data sharding
    pub num_groups: usize,

    /// Whether this is the bootstrap node (first node in cluster)
    pub is_bootstrap: bool,

    /// Initial cluster members as (node_id, raft_address) pairs
    /// Only used by bootstrap node
    pub initial_members: Vec<(NodeId, String)>,
}

#[cfg(feature = "cluster")]
impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            node_id: 1,
            data_dir: PathBuf::from("./data"),
            bind_address: "127.0.0.1:6379".to_string(),
            raft_address: "127.0.0.1:50051".to_string(),
            num_groups: 4,
            is_bootstrap: false,
            initial_members: vec![],
        }
    }
}

/// Cluster node that wraps AiDb's MultiRaftNode.
///
/// This is a minimal wrapper that exposes AiDb's Multi-Raft functionality
/// for Redis Cluster protocol compatibility.
#[cfg(feature = "cluster")]
pub struct ClusterNode {
    /// Configuration
    config: ClusterConfig,

    /// AiDb's MultiRaftNode
    multi_raft: Option<Arc<aidb::cluster::MultiRaftNode>>,

    /// AiDb's MetaRaftNode (accessed through MultiRaftNode)
    meta_raft: Option<Arc<aidb::cluster::MetaRaftNode>>,

    /// AiDb's Router
    router: Option<Arc<aidb::cluster::Router>>,
}

#[cfg(feature = "cluster")]
impl ClusterNode {
    /// Create a new ClusterNode with the given configuration.
    pub fn new(config: ClusterConfig) -> Self {
        Self {
            config,
            multi_raft: None,
            meta_raft: None,
            router: None,
        }
    }

    /// Initialize the cluster node.
    ///
    /// This creates the underlying MultiRaftNode and initializes MetaRaft.
    pub async fn initialize(&mut self) -> Result<()> {
        use aidb::cluster::MultiRaftNode;
        use openraft::Config as RaftConfig;

        // Create Raft configuration
        let raft_config = RaftConfig::default();

        // Create MultiRaftNode
        let mut multi_raft = MultiRaftNode::new(
            self.config.node_id,
            &self.config.data_dir,
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

        // If bootstrap node, initialize MetaRaft cluster
        if self.config.is_bootstrap {
            multi_raft
                .initialize_meta_cluster(self.config.initial_members.clone())
                .await
                .map_err(|e| {
                    crate::error::AikvError::Internal(format!(
                        "Failed to bootstrap MetaRaft: {}",
                        e
                    ))
                })?;
        }

        // Wrap in Arc after initialization
        let multi_raft = Arc::new(multi_raft);

        // Get MetaRaftNode reference
        let meta_raft = multi_raft.meta_raft().ok_or_else(|| {
            crate::error::AikvError::Internal("MetaRaft not initialized".to_string())
        })?;

        // Get initial cluster metadata from MetaRaft
        let cluster_meta = meta_raft.get_cluster_meta();

        // Initialize Router with cluster metadata
        let router = Arc::new(aidb::cluster::Router::new(cluster_meta));

        self.multi_raft = Some(multi_raft.clone());
        self.meta_raft = Some(meta_raft.clone());
        self.router = Some(router);

        Ok(())
    }

    /// Get reference to MultiRaftNode
    pub fn multi_raft(&self) -> Option<&Arc<aidb::cluster::MultiRaftNode>> {
        self.multi_raft.as_ref()
    }

    /// Get reference to MetaRaftNode
    pub fn meta_raft(&self) -> Option<&Arc<aidb::cluster::MetaRaftNode>> {
        self.meta_raft.as_ref()
    }

    /// Get reference to Router
    pub fn router(&self) -> Option<&Arc<aidb::cluster::Router>> {
        self.router.as_ref()
    }

    /// Get node ID
    pub fn node_id(&self) -> NodeId {
        self.config.node_id
    }

    /// Create Raft groups for data sharding.
    ///
    /// This should be called after the cluster is formed and all nodes have joined.
    pub async fn create_data_groups(&self) -> Result<()> {
        let multi_raft = self.multi_raft.as_ref().ok_or_else(|| {
            crate::error::AikvError::Internal("MultiRaftNode not initialized".to_string())
        })?;

        for group_id in 1..=self.config.num_groups as u64 {
            // Create group with single replica (this node)
            let replicas = vec![self.config.node_id];

            multi_raft
                .create_raft_group(group_id, replicas)
                .await
                .map_err(|e| {
                    crate::error::AikvError::Internal(format!(
                        "Failed to create group {}: {}",
                        group_id, e
                    ))
                })?;
        }

        Ok(())
    }

    /// Shutdown the cluster node.
    pub async fn shutdown(&self) -> Result<()> {
        if let Some(multi_raft) = &self.multi_raft {
            multi_raft.shutdown().await.map_err(|e| {
                crate::error::AikvError::Internal(format!("Failed to shutdown: {}", e))
            })?;
        }
        Ok(())
    }
}

/// Placeholder struct when cluster feature is disabled
#[cfg(not(feature = "cluster"))]
pub struct ClusterNode;

#[cfg(not(feature = "cluster"))]
impl ClusterNode {
    pub fn new(_node_id: NodeId, _bind_address: String, _cluster_port: u16) -> Self {
        Self
    }
}
