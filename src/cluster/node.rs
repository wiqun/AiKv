//! Cluster node implementation for AiKv
//!
//! This module provides the main `ClusterNode` type that wraps AiDb's Multi-Raft
//! functionality to provide a distributed AiKv node.

use std::sync::Arc;

use aidb::cluster::MultiRaftNode;
use openraft::Config as RaftConfig;
use tokio::sync::RwLock;

use super::config::{ClusterConfig, NodeId};
use super::router::SlotRouter;
use super::types::{ClusterState, NodeRole};
use crate::error::{AikvError, Result};

/// A cluster-enabled AiKv node
///
/// This wraps AiDb's Multi-Raft infrastructure to provide distributed
/// storage with automatic failover and data replication.
///
/// # Architecture
///
/// Each ClusterNode participates in:
/// - **MetaRaft** (Group 0): For cluster metadata management
/// - **Data Groups**: For actual key-value storage, sharded by slot
///
/// # Example
///
/// ```ignore
/// use aikv::cluster::{ClusterConfig, ClusterNode};
///
/// let config = ClusterConfig::new(1, "127.0.0.1:6379", "./data/node1")
///     .with_members(vec![
///         (1, "127.0.0.1:16379".to_string()),
///         (2, "127.0.0.1:16380".to_string()),
///         (3, "127.0.0.1:16381".to_string()),
///     ]);
///
/// let node = ClusterNode::new(config).await?;
/// node.bootstrap().await?;
/// ```
pub struct ClusterNode {
    /// Node configuration
    config: ClusterConfig,

    /// Multi-Raft node instance from AiDb
    multi_raft: Arc<RwLock<Option<MultiRaftNode>>>,

    /// Slot router for key-to-slot mapping
    router: SlotRouter,

    /// Current cluster state
    state: Arc<RwLock<ClusterState>>,
}

impl ClusterNode {
    /// Create a new cluster node with the given configuration
    ///
    /// This initializes the node but does not start the Raft consensus.
    /// Call `bootstrap()` on the first node or `join()` on subsequent nodes.
    pub async fn new(config: ClusterConfig) -> Result<Self> {
        // Create data directory
        std::fs::create_dir_all(&config.data_dir).map_err(|e| {
            AikvError::Storage(format!(
                "Failed to create data directory {:?}: {}",
                config.data_dir, e
            ))
        })?;

        // Create Raft configuration
        let raft_config = config.to_raft_config();

        // Create Multi-Raft node
        let multi_raft = MultiRaftNode::new(config.node_id, &config.data_dir, raft_config)
            .await
            .map_err(|e| AikvError::Storage(format!("Failed to create Multi-Raft node: {}", e)))?;

        Ok(Self {
            config,
            multi_raft: Arc::new(RwLock::new(Some(multi_raft))),
            router: SlotRouter::new(),
            state: Arc::new(RwLock::new(ClusterState::new())),
        })
    }

    /// Bootstrap a new cluster
    ///
    /// This should only be called on the first node of a new cluster.
    /// It initializes the MetaRaft group with the initial cluster members.
    ///
    /// # Arguments
    ///
    /// * `members` - Initial cluster members (node_id, cluster_bus_address)
    pub async fn bootstrap(&self) -> Result<()> {
        // Build MetaRaft config and members outside the lock
        let meta_config = RaftConfig {
            election_timeout_min: self.config.election_timeout.as_millis() as u64,
            election_timeout_max: (self.config.election_timeout.as_millis() * 2) as u64,
            heartbeat_interval: self.config.heartbeat_interval.as_millis() as u64,
            ..Default::default()
        };

        let members = if self.config.initial_members.is_empty() {
            // Single-node cluster
            vec![(
                self.config.node_id,
                format!("127.0.0.1:{}", self.config.cluster_port),
            )]
        } else {
            self.config.initial_members.clone()
        };

        // Acquire lock and perform async operations
        {
            let mut multi_raft = self.multi_raft.write().await;
            let node = multi_raft
                .as_mut()
                .ok_or_else(|| AikvError::Storage("Multi-Raft node not initialized".to_string()))?;

            // Initialize MetaRaft
            node.init_meta_raft(meta_config)
                .await
                .map_err(|e| AikvError::Storage(format!("Failed to initialize MetaRaft: {}", e)))?;

            // Bootstrap the MetaRaft cluster
            node.initialize_meta_cluster(members)
                .await
                .map_err(|e| AikvError::Storage(format!("Failed to bootstrap cluster: {}", e)))?;
        }

        // Update state
        {
            let mut state = self.state.write().await;
            state.role = NodeRole::Leader; // Bootstrap node starts as leader
            state.cluster_ok = true;
        }

        tracing::info!(
            node_id = self.config.node_id,
            "Cluster bootstrapped successfully"
        );

        Ok(())
    }

    /// Join an existing cluster
    ///
    /// This should be called on nodes joining an existing cluster.
    /// The node will connect to a seed node and sync cluster state.
    ///
    /// # Arguments
    ///
    /// * `seed_addr` - Address of an existing cluster member
    pub async fn join(&self, _seed_addr: &str) -> Result<()> {
        // TODO: Implement cluster join
        // 1. Connect to seed node
        // 2. Get cluster metadata
        // 3. Add self as learner
        // 4. Wait to be promoted to follower
        Err(AikvError::Storage(
            "Cluster join not yet implemented".to_string(),
        ))
    }

    /// Get the slot for a key
    pub fn key_slot(&self, key: &[u8]) -> u16 {
        self.router.key_slot(key)
    }

    /// Get current cluster state
    pub async fn state(&self) -> ClusterState {
        self.state.read().await.clone()
    }

    /// Get this node's ID
    pub fn node_id(&self) -> NodeId {
        self.config.node_id
    }

    /// Check if this node is the leader
    pub async fn is_leader(&self) -> bool {
        self.state.read().await.role == NodeRole::Leader
    }

    /// Get cluster configuration
    pub fn config(&self) -> &ClusterConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_cluster_node_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClusterConfig::new(1, "127.0.0.1:6379", temp_dir.path());

        let node = ClusterNode::new(config).await;
        assert!(node.is_ok());

        let node = node.unwrap();
        assert_eq!(node.node_id(), 1);
    }

    #[test]
    fn test_key_slot() {
        let router = SlotRouter::new();
        // Known Redis slot values
        assert_eq!(router.key_slot(b"foo"), 12182);
        assert_eq!(router.key_slot(b"bar"), 5061);
    }
}
