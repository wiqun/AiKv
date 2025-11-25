//! Cluster configuration types for AiKv
//!
//! This module provides configuration options for running AiKv in cluster mode.

use std::path::PathBuf;
use std::time::Duration;

/// Node ID type (re-exported from AiDb)
pub type NodeId = u64;

/// Configuration for a cluster node
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Unique node ID within the cluster
    pub node_id: NodeId,

    /// Address for client connections (e.g., "127.0.0.1:6379")
    pub bind_addr: String,

    /// Cluster bus port for inter-node communication (default: bind_port + 10000)
    pub cluster_port: u16,

    /// Data directory for this node
    pub data_dir: PathBuf,

    /// Initial cluster members (node_id, address)
    pub initial_members: Vec<(NodeId, String)>,

    /// Raft election timeout
    pub election_timeout: Duration,

    /// Raft heartbeat interval
    pub heartbeat_interval: Duration,

    /// Number of Raft groups for data sharding
    pub num_groups: usize,

    /// Replication factor (number of replicas per group)
    pub replication_factor: usize,
}

impl ClusterConfig {
    /// Create a new cluster configuration
    ///
    /// # Arguments
    ///
    /// * `node_id` - Unique identifier for this node (1-based)
    /// * `bind_addr` - Address for client connections
    /// * `data_dir` - Directory for storing node data
    ///
    /// # Example
    ///
    /// ```
    /// use aikv::cluster::ClusterConfig;
    ///
    /// let config = ClusterConfig::new(1, "127.0.0.1:6379", "./data/node1");
    /// assert_eq!(config.node_id, 1);
    /// assert_eq!(config.cluster_port, 16379);
    /// ```
    pub fn new<S: Into<String>, P: Into<PathBuf>>(
        node_id: NodeId,
        bind_addr: S,
        data_dir: P,
    ) -> Self {
        let bind_addr = bind_addr.into();

        // Extract port from bind_addr for calculating cluster_port
        let port: u16 = bind_addr
            .split(':')
            .next_back()
            .and_then(|p| p.parse().ok())
            .unwrap_or(6379);

        Self {
            node_id,
            bind_addr,
            cluster_port: port + 10000, // Redis cluster convention
            data_dir: data_dir.into(),
            initial_members: Vec::new(),
            election_timeout: Duration::from_millis(500),
            heartbeat_interval: Duration::from_millis(100),
            num_groups: 3, // Default to 3 groups for a 3-node cluster
            replication_factor: 3,
        }
    }

    /// Set the initial cluster members
    ///
    /// # Arguments
    ///
    /// * `members` - List of (node_id, address) pairs for initial cluster members
    pub fn with_members(mut self, members: Vec<(NodeId, String)>) -> Self {
        self.initial_members = members;
        self
    }

    /// Set the election timeout
    pub fn with_election_timeout(mut self, timeout: Duration) -> Self {
        self.election_timeout = timeout;
        self
    }

    /// Set the heartbeat interval
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Set the number of Raft groups
    pub fn with_num_groups(mut self, num_groups: usize) -> Self {
        self.num_groups = num_groups;
        self
    }

    /// Set the replication factor
    pub fn with_replication_factor(mut self, factor: usize) -> Self {
        self.replication_factor = factor;
        self
    }

    /// Build OpenRaft configuration from this config
    pub fn to_raft_config(&self) -> openraft::Config {
        openraft::Config {
            election_timeout_min: self.election_timeout.as_millis() as u64,
            election_timeout_max: (self.election_timeout.as_millis() * 2) as u64,
            heartbeat_interval: self.heartbeat_interval.as_millis() as u64,
            ..Default::default()
        }
    }
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self::new(1, "127.0.0.1:6379", "./data/cluster")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_new() {
        let config = ClusterConfig::new(1, "127.0.0.1:6379", "./data/node1");
        assert_eq!(config.node_id, 1);
        assert_eq!(config.bind_addr, "127.0.0.1:6379");
        assert_eq!(config.cluster_port, 16379);
    }

    #[test]
    fn test_config_with_members() {
        let config = ClusterConfig::new(1, "127.0.0.1:6379", "./data/node1").with_members(vec![
            (1, "127.0.0.1:16379".to_string()),
            (2, "127.0.0.1:16380".to_string()),
        ]);
        assert_eq!(config.initial_members.len(), 2);
    }

    #[test]
    fn test_config_default() {
        let config = ClusterConfig::default();
        assert_eq!(config.node_id, 1);
        assert_eq!(config.replication_factor, 3);
    }
}
