//! Cluster node implementation wrapping AiDb's MultiRaftNode.
//!
//! This module provides the `ClusterNode` type which wraps AiDb's
//! MultiRaft implementation for Redis Cluster compatibility.
//!
//! # Architecture
//!
//! The `ClusterNode` wraps AiDb's `MultiRaftNode` and `MetaRaftNode` to provide:
//!
//! - Multi-Raft group management for data sharding
//! - Metadata consensus through MetaRaft (Group 0)
//! - Automatic key-to-slot-to-group routing
//! - Cluster membership management
//!
//! # Usage
//!
//! ```ignore
//! use aikv::cluster::ClusterNode;
//!
//! // Create a node
//! let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
//!
//! // Initialize as bootstrap node (first node in cluster)
//! node.initialize("./data", true).await?;
//!
//! // Start the cluster (creates default Raft groups)
//! node.start_cluster(3).await?;
//! ```

#[cfg(not(feature = "cluster"))]
use crate::error::AikvError;
use crate::error::Result;

/// Node ID type alias
pub type NodeId = u64;

/// Group ID type alias (for Raft Groups)
#[cfg(feature = "cluster")]
pub type GroupId = u64;

/// Configuration for cluster node initialization.
#[cfg(feature = "cluster")]
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Number of Raft groups to create for data sharding
    pub num_groups: u64,
    /// Initial cluster members as (node_id, address) pairs
    pub initial_members: Vec<(NodeId, String)>,
}

#[cfg(feature = "cluster")]
impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            num_groups: 4,
            initial_members: vec![],
        }
    }
}

/// Cluster node that wraps AiDb's MultiRaftNode.
///
/// `ClusterNode` provides a high-level interface for cluster operations,
/// abstracting away the details of AiDb's Raft implementation.
///
/// # Features
///
/// When the `cluster` feature is enabled, `ClusterNode` provides:
///
/// - `MultiRaftNode` for managing multiple Raft groups
/// - `MetaRaftNode` for cluster metadata consensus
/// - Automatic routing of keys to appropriate Raft groups
/// - Dynamic cluster membership management
pub struct ClusterNode {
    /// The node's unique identifier
    node_id: NodeId,

    /// The node's bind address
    bind_address: String,

    /// The cluster bus port (typically +10000 from data port)
    cluster_port: u16,

    /// Whether the node is initialized
    initialized: bool,

    /// Inner MultiRaftNode (only available with cluster feature)
    #[cfg(feature = "cluster")]
    inner: Option<std::sync::Arc<aidb::cluster::MultiRaftNode>>,

    /// Data directory path
    #[cfg(feature = "cluster")]
    data_dir: Option<std::path::PathBuf>,

    /// Whether this node is the bootstrap node
    #[cfg(feature = "cluster")]
    is_bootstrap: bool,
}

impl ClusterNode {
    /// Create a new ClusterNode.
    ///
    /// # Arguments
    ///
    /// * `node_id` - Unique identifier for this node
    /// * `bind_address` - Address to bind for client connections
    /// * `cluster_port` - Port for cluster bus (inter-node communication)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
    /// ```
    pub fn new(node_id: NodeId, bind_address: String, cluster_port: u16) -> Self {
        Self {
            node_id,
            bind_address,
            cluster_port,
            initialized: false,
            #[cfg(feature = "cluster")]
            inner: None,
            #[cfg(feature = "cluster")]
            data_dir: None,
            #[cfg(feature = "cluster")]
            is_bootstrap: false,
        }
    }

    /// Get the node's ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the node's bind address.
    pub fn bind_address(&self) -> &str {
        &self.bind_address
    }

    /// Get the cluster bus port.
    pub fn cluster_port(&self) -> u16 {
        self.cluster_port
    }

    /// Check if the node is initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Check if this is the bootstrap node.
    #[cfg(feature = "cluster")]
    pub fn is_bootstrap(&self) -> bool {
        self.is_bootstrap
    }

    /// Initialize the cluster node.
    ///
    /// This sets up the `MultiRaftNode` and `MetaRaftNode` for cluster operations.
    /// The MultiRaftNode manages multiple Raft groups for data sharding,
    /// while MetaRaftNode (Group 0) handles cluster metadata consensus.
    ///
    /// # Arguments
    ///
    /// * `data_dir` - Directory for persistent storage
    /// * `is_bootstrap` - Whether this is the first node in the cluster
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
    /// node.initialize("./data/node1", true).await?;
    /// ```
    #[cfg(feature = "cluster")]
    pub async fn initialize(&mut self, data_dir: &str, is_bootstrap: bool) -> Result<()> {
        use crate::error::AikvError;
        use std::path::PathBuf;
        use std::sync::Arc;

        let data_path = PathBuf::from(data_dir);
        std::fs::create_dir_all(&data_path)
            .map_err(|e| AikvError::Storage(format!("Failed to create data directory: {}", e)))?;

        // Create default Raft configuration
        let raft_config = openraft::Config::default();

        // Initialize MultiRaftNode
        let mut multi_raft =
            aidb::cluster::MultiRaftNode::new(self.node_id, &data_path, raft_config.clone())
                .await
                .map_err(|e| {
                    AikvError::Storage(format!("Failed to create MultiRaftNode: {}", e))
                })?;

        // Initialize MetaRaft for cluster metadata consensus
        multi_raft
            .init_meta_raft(raft_config)
            .await
            .map_err(|e| AikvError::Storage(format!("Failed to initialize MetaRaft: {}", e)))?;

        self.inner = Some(Arc::new(multi_raft));
        self.data_dir = Some(data_path);
        self.is_bootstrap = is_bootstrap;
        self.initialized = true;

        Ok(())
    }

    /// Initialize the cluster node (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub async fn initialize(&mut self, _data_dir: &str, _is_bootstrap: bool) -> Result<()> {
        Err(AikvError::Storage(
            "Cluster feature not enabled. Build with --features cluster".to_string(),
        ))
    }

    /// Bootstrap the MetaRaft cluster.
    ///
    /// This should be called on the first node (bootstrap node) to initialize
    /// the MetaRaft cluster with the initial members.
    ///
    /// # Arguments
    ///
    /// * `members` - Initial cluster members as (node_id, address) pairs
    ///
    /// # Errors
    ///
    /// Returns an error if bootstrap fails or if this is not the bootstrap node.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let members = vec![
    ///     (1, "127.0.0.1:50051".to_string()),
    ///     (2, "127.0.0.1:50052".to_string()),
    ///     (3, "127.0.0.1:50053".to_string()),
    /// ];
    /// node.bootstrap_meta_cluster(members).await?;
    /// ```
    #[cfg(feature = "cluster")]
    pub async fn bootstrap_meta_cluster(&self, members: Vec<(NodeId, String)>) -> Result<()> {
        use crate::error::AikvError;

        if !self.is_bootstrap {
            return Err(AikvError::InvalidCommand(
                "Only bootstrap node can initialize the MetaRaft cluster".to_string(),
            ));
        }

        // Get the MultiRaftNode - it's stored in an Arc but we need mutable access
        // Since MultiRaftNode's methods take &self, we can work with Arc
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| AikvError::Storage("ClusterNode not initialized".to_string()))?;

        // Initialize MetaRaft cluster with members
        inner.initialize_meta_cluster(members).await.map_err(|e| {
            AikvError::Storage(format!("Failed to bootstrap MetaRaft cluster: {}", e))
        })?;

        Ok(())
    }

    /// Bootstrap the MetaRaft cluster (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub async fn bootstrap_meta_cluster(&self, _members: Vec<(NodeId, String)>) -> Result<()> {
        Err(AikvError::Storage(
            "Cluster feature not enabled. Build with --features cluster".to_string(),
        ))
    }

    /// Start the cluster with the specified number of Raft groups.
    ///
    /// This creates Raft groups for data sharding and initializes the router
    /// for automatic key-to-slot-to-group routing.
    ///
    /// # Arguments
    ///
    /// * `num_groups` - Number of Raft groups to create for data sharding
    ///
    /// # Errors
    ///
    /// Returns an error if starting fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Create 4 Raft groups for data sharding
    /// node.start_cluster(4).await?;
    /// ```
    #[cfg(feature = "cluster")]
    pub async fn start_cluster(&self, num_groups: u64) -> Result<()> {
        use crate::error::AikvError;

        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| AikvError::Storage("ClusterNode not initialized".to_string()))?;

        // Create Raft groups for data sharding
        for group_id in 0..num_groups {
            inner
                .create_raft_group(group_id, vec![self.node_id])
                .await
                .map_err(|e| {
                    AikvError::Storage(format!("Failed to create Raft group {}: {}", group_id, e))
                })?;
        }

        Ok(())
    }

    /// Start the cluster (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub async fn start_cluster(&self, _num_groups: u64) -> Result<()> {
        Err(AikvError::Storage(
            "Cluster feature not enabled. Build with --features cluster".to_string(),
        ))
    }

    /// Add a node address for cluster communication.
    ///
    /// This registers a node's address for Raft RPC communication.
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to add
    /// * `addr` - Address of the node (host:port)
    #[cfg(feature = "cluster")]
    pub fn add_node_address(&self, node_id: NodeId, addr: String) {
        if let Some(inner) = &self.inner {
            inner.add_node_address(node_id, addr);
        }
    }

    /// Add a node address (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn add_node_address(&self, _node_id: NodeId, _addr: String) {
        // No-op when cluster feature is disabled
    }

    /// Get the inner MultiRaftNode.
    #[cfg(feature = "cluster")]
    pub fn inner(&self) -> Option<&std::sync::Arc<aidb::cluster::MultiRaftNode>> {
        self.inner.as_ref()
    }

    /// Check if MetaRaft is available.
    ///
    /// To access the MetaRaft instance, use `inner().meta_raft()`.
    #[cfg(feature = "cluster")]
    pub fn has_meta_raft(&self) -> bool {
        self.inner
            .as_ref()
            .and_then(|inner| inner.meta_raft())
            .is_some()
    }

    /// Check if MetaRaft is available (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn has_meta_raft(&self) -> bool {
        false
    }

    /// Set the inner MultiRaftNode.
    #[cfg(feature = "cluster")]
    pub fn set_inner(&mut self, node: std::sync::Arc<aidb::cluster::MultiRaftNode>) {
        self.inner = Some(node);
        self.initialized = true;
    }

    /// Get the list of active Raft groups.
    #[cfg(feature = "cluster")]
    pub fn list_groups(&self) -> Vec<GroupId> {
        self.inner
            .as_ref()
            .map(|inner| inner.list_groups())
            .unwrap_or_default()
    }

    /// Get the list of active Raft groups (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn list_groups(&self) -> Vec<u64> {
        vec![]
    }

    /// Get the number of active Raft groups.
    #[cfg(feature = "cluster")]
    pub fn group_count(&self) -> usize {
        self.inner
            .as_ref()
            .map(|inner| inner.group_count())
            .unwrap_or(0)
    }

    /// Get the number of active Raft groups (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn group_count(&self) -> usize {
        0
    }

    /// Shutdown the cluster node.
    ///
    /// This gracefully shuts down all Raft groups and the MetaRaft node.
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown fails.
    pub async fn shutdown(&mut self) -> Result<()> {
        #[cfg(feature = "cluster")]
        {
            use crate::error::AikvError;

            if let Some(inner) = &self.inner {
                // Shutdown the MultiRaftNode (which includes all Raft groups and MetaRaft)
                inner.shutdown().await.map_err(|e| {
                    AikvError::Storage(format!("Failed to shutdown cluster node: {}", e))
                })?;
            }
            self.inner = None;
        }
        self.initialized = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_node_creation() {
        let node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
        assert_eq!(node.node_id(), 1);
        assert_eq!(node.bind_address(), "127.0.0.1:6379");
        assert_eq!(node.cluster_port(), 16379);
        assert!(!node.is_initialized());
    }

    #[test]
    fn test_cluster_node_default_values() {
        let node = ClusterNode::new(42, "192.168.1.100:7000".to_string(), 17000);
        assert_eq!(node.node_id(), 42);
        assert_eq!(node.bind_address(), "192.168.1.100:7000");
        assert_eq!(node.cluster_port(), 17000);
        assert!(!node.is_initialized());
        assert!(!node.has_meta_raft());
        assert_eq!(node.group_count(), 0);
        assert!(node.list_groups().is_empty());
    }

    #[cfg(feature = "cluster")]
    mod cluster_tests {
        use super::*;
        use tempfile::TempDir;

        #[tokio::test]
        async fn test_cluster_node_initialize() {
            let temp_dir = TempDir::new().unwrap();
            let data_path = temp_dir.path().to_str().unwrap();

            let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
            assert!(!node.is_initialized());

            // Initialize as bootstrap node
            let result = node.initialize(data_path, true).await;
            assert!(result.is_ok(), "Initialize failed: {:?}", result.err());
            assert!(node.is_initialized());
            assert!(node.is_bootstrap());
        }

        #[tokio::test]
        async fn test_cluster_node_bootstrap_meta_cluster() {
            let temp_dir = TempDir::new().unwrap();
            let data_path = temp_dir.path().to_str().unwrap();

            let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
            node.initialize(data_path, true).await.unwrap();

            // Bootstrap MetaRaft cluster with single member
            let members = vec![(1, "127.0.0.1:50051".to_string())];
            let result = node.bootstrap_meta_cluster(members).await;
            assert!(result.is_ok(), "Bootstrap failed: {:?}", result.err());
        }

        #[tokio::test]
        async fn test_cluster_node_start_cluster() {
            let temp_dir = TempDir::new().unwrap();
            let data_path = temp_dir.path().to_str().unwrap();

            let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
            node.initialize(data_path, true).await.unwrap();

            // Start cluster with 4 Raft groups
            let result = node.start_cluster(4).await;
            assert!(result.is_ok(), "Start cluster failed: {:?}", result.err());
            assert_eq!(node.group_count(), 4);

            // Verify groups were created
            let groups = node.list_groups();
            assert_eq!(groups.len(), 4);
        }

        #[tokio::test]
        async fn test_cluster_node_add_node_address() {
            let temp_dir = TempDir::new().unwrap();
            let data_path = temp_dir.path().to_str().unwrap();

            let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
            node.initialize(data_path, true).await.unwrap();

            // Add addresses for other nodes
            node.add_node_address(2, "127.0.0.1:50052".to_string());
            node.add_node_address(3, "127.0.0.1:50053".to_string());
        }

        #[tokio::test]
        async fn test_cluster_node_shutdown() {
            let temp_dir = TempDir::new().unwrap();
            let data_path = temp_dir.path().to_str().unwrap();

            let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
            node.initialize(data_path, true).await.unwrap();
            assert!(node.is_initialized());

            // Shutdown
            let result = node.shutdown().await;
            assert!(result.is_ok());
            assert!(!node.is_initialized());
        }

        #[tokio::test]
        async fn test_cluster_node_full_workflow() {
            let temp_dir = TempDir::new().unwrap();
            let data_path = temp_dir.path().to_str().unwrap();

            // Create and initialize node
            let mut node = ClusterNode::new(1, "127.0.0.1:6379".to_string(), 16379);
            node.initialize(data_path, true).await.unwrap();

            // Bootstrap MetaRaft cluster
            let members = vec![(1, "127.0.0.1:50051".to_string())];
            node.bootstrap_meta_cluster(members).await.unwrap();

            // Start cluster with Raft groups
            node.start_cluster(2).await.unwrap();
            assert_eq!(node.group_count(), 2);

            // Verify state
            assert!(node.is_initialized());
            assert!(node.is_bootstrap());
            assert!(node.inner().is_some());

            // Shutdown
            node.shutdown().await.unwrap();
            assert!(!node.is_initialized());
        }

        #[test]
        fn test_cluster_config_default() {
            let config = ClusterConfig::default();
            assert_eq!(config.num_groups, 4);
            assert!(config.initial_members.is_empty());
        }
    }
}
