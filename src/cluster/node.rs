//! Cluster node implementation wrapping AiDb's MultiRaftNode.
//!
//! This module provides the `ClusterNode` type which wraps AiDb's
//! MultiRaft implementation for Redis Cluster compatibility.

#[cfg(not(feature = "cluster"))]
use crate::error::AikvError;
use crate::error::Result;

/// Node ID type alias
pub type NodeId = u64;

/// Group ID type alias (for future use with Raft Groups)
#[cfg(feature = "cluster")]
#[allow(dead_code)]
pub type GroupId = u64;

/// Cluster node that wraps AiDb's MultiRaftNode.
///
/// `ClusterNode` provides a high-level interface for cluster operations,
/// abstracting away the details of AiDb's Raft implementation.
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

    /// Initialize the cluster node.
    ///
    /// This sets up the necessary Raft groups and metadata.
    ///
    /// # Arguments
    ///
    /// * `data_dir` - Directory for persistent storage
    /// * `is_bootstrap` - Whether this is the first node in the cluster
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    #[cfg(feature = "cluster")]
    pub async fn initialize(&mut self, _data_dir: &str, _is_bootstrap: bool) -> Result<()> {
        // TODO: Implement full initialization with AiDb MultiRaftNode
        // For now, mark as initialized for basic testing
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

    /// Get the inner MultiRaftNode.
    #[cfg(feature = "cluster")]
    pub fn inner(&self) -> Option<&std::sync::Arc<aidb::cluster::MultiRaftNode>> {
        self.inner.as_ref()
    }

    /// Set the inner MultiRaftNode.
    #[cfg(feature = "cluster")]
    pub fn set_inner(&mut self, node: std::sync::Arc<aidb::cluster::MultiRaftNode>) {
        self.inner = Some(node);
        self.initialized = true;
    }

    /// Shutdown the cluster node.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.initialized = false;
        #[cfg(feature = "cluster")]
        {
            self.inner = None;
        }
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
}
