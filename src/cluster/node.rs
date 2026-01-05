//! Cluster node implementation wrapping AiDb's MultiRaftNode.
//!
//! This module provides a thin wrapper around AiDb's Multi-Raft implementation
//! for Redis Cluster compatibility.

use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "cluster")]
use openraft::BasicNode;

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

        // Check if the cluster is already initialized by checking Raft metrics
        // If there's already a committed vote or log entries, the cluster was previously initialized
        let already_initialized = {
            if let Some(meta_raft) = multi_raft.meta_raft() {
                let raft = meta_raft.raft();
                let metrics = raft.metrics().borrow().clone();
                
                // Check if there are any voters in the membership (excluding empty membership)
                let has_voters = !metrics.membership_config.membership().voter_ids().collect::<Vec<_>>().is_empty();
                
                // Check if there's any committed log
                let has_committed_log = metrics.last_applied.is_some();
                
                has_voters || has_committed_log
            } else {
                false
            }
        };

        // If bootstrap node and not already initialized, initialize MetaRaft cluster
        if self.config.is_bootstrap && !already_initialized {
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

    /// Add a node as a learner to the MetaRaft cluster.
    ///
    /// This is the first step in adding a new node to the MetaRaft voting cluster.
    /// The node will receive log updates but cannot vote in elections.
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to add
    /// * `addr` - Raft address of the node (ip:port for gRPC)
    ///
    /// # Returns
    ///
    /// `Ok(())` on success
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Add node 2 as a learner
    /// cluster_node.add_meta_learner(2, "127.0.0.1:50052".to_string()).await?;
    /// ```
    pub async fn add_meta_learner(&self, node_id: NodeId, addr: String) -> Result<()> {
        let meta_raft = self.meta_raft.as_ref().ok_or_else(|| {
            crate::error::AikvError::Internal("MetaRaft not initialized".to_string())
        })?;

        let node = BasicNode { addr };

        meta_raft
            .add_learner(node_id, node)
            .await
            .map_err(|e| {
                crate::error::AikvError::Internal(format!("Failed to add MetaRaft learner: {}", e))
            })?;

        Ok(())
    }

    /// Promote a learner to a voting member in the MetaRaft cluster.
    ///
    /// After a learner has caught up with the log, this promotes it to a voter.
    /// The new voters list must include all desired voting members (including the promoted one).
    ///
    /// # Arguments
    ///
    /// * `voters` - Complete list of node IDs that should be voters
    ///
    /// # Returns
    ///
    /// `Ok(())` on success
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Promote node 2 to voter (assuming node 1 is already a voter)
    /// cluster_node.promote_meta_voter(vec![1, 2]).await?;
    /// ```
    pub async fn promote_meta_voter(&self, voters: Vec<NodeId>) -> Result<()> {
        let meta_raft = self.meta_raft.as_ref().ok_or_else(|| {
            crate::error::AikvError::Internal("MetaRaft not initialized".to_string())
        })?;

        use std::collections::BTreeSet;
        let members: BTreeSet<NodeId> = voters.into_iter().collect();

        meta_raft
            .change_membership(members, true)
            .await
            .map_err(|e| {
                crate::error::AikvError::Internal(format!(
                    "Failed to promote MetaRaft voter: {}",
                    e
                ))
            })?;

        Ok(())
    }

    /// Change the MetaRaft cluster membership.
    ///
    /// This is a low-level API that directly calls OpenRaft's change_membership.
    /// Use add_meta_learner and promote_meta_voter for the recommended workflow.
    ///
    /// # Arguments
    ///
    /// * `voters` - Complete list of node IDs that should be voters
    /// * `retain_learners` - Whether to keep existing learners
    ///
    /// # Returns
    ///
    /// `Ok(())` on success
    pub async fn change_meta_membership(
        &self,
        voters: Vec<NodeId>,
        retain_learners: bool,
    ) -> Result<()> {
        let meta_raft = self.meta_raft.as_ref().ok_or_else(|| {
            crate::error::AikvError::Internal("MetaRaft not initialized".to_string())
        })?;

        use std::collections::BTreeSet;
        let members: BTreeSet<NodeId> = voters.into_iter().collect();

        meta_raft
            .change_membership(members, retain_learners)
            .await
            .map_err(|e| {
                crate::error::AikvError::Internal(format!("Failed to change membership: {}", e))
            })?;

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
