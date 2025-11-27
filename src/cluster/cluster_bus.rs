//! Cluster Bus implementation for AiKv.
//!
//! This module provides the cluster bus functionality that integrates with AiDb's
//! MultiRaft API for heartbeat detection and failure detection.
//!
//! # Architecture
//!
//! The Cluster Bus acts as a glue layer between AiKv's Redis Cluster protocol
//! implementation and AiDb's Raft-based consensus:
//!
//! - **Heartbeat Detection**: Uses `MetaRaftNode` leader heartbeat mechanism
//!   provided by OpenRaft's internal heartbeat handling
//! - **Failure Detection**: Integrates with `NodeStatus::Online/Offline` and
//!   uses election timeout for automatic failure detection
//!
//! # Usage
//!
//! ```ignore
//! use aikv::cluster::{ClusterBus, ClusterBusConfig};
//!
//! let config = ClusterBusConfig::default();
//! let bus = ClusterBus::new(1, config);
//! bus.start().await?;
//! ```

use crate::error::{AikvError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "cluster")]
use aidb::cluster::NodeStatus;

#[cfg(feature = "cluster")]
use std::sync::RwLock;

/// Node ID type alias
pub type NodeId = u64;

/// Configuration for the cluster bus.
#[derive(Debug, Clone)]
pub struct ClusterBusConfig {
    /// Interval between health checks (default: 10 seconds)
    pub health_check_interval: Duration,

    /// Timeout for health check requests (default: 5 seconds)
    pub health_check_timeout: Duration,

    /// Number of consecutive failures before marking node as offline (default: 3)
    pub failure_threshold: u32,

    /// Number of consecutive successes before marking node as online (default: 2)
    pub success_threshold: u32,

    /// Election timeout - how long to wait before considering a node failed (default: 5 seconds)
    /// This aligns with Raft's election timeout concept.
    pub election_timeout: Duration,
}

impl Default for ClusterBusConfig {
    fn default() -> Self {
        Self {
            health_check_interval: Duration::from_secs(10),
            health_check_timeout: Duration::from_secs(5),
            failure_threshold: 3,
            success_threshold: 2,
            election_timeout: Duration::from_secs(5),
        }
    }
}

/// Node health status tracked by the cluster bus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NodeHealthStatus {
    /// Node is healthy and responding to heartbeats
    Online,
    /// Node is not responding, considered offline
    Offline,
    /// Node status is unknown (initial state or transitioning)
    #[default]
    Unknown,
}

#[cfg(feature = "cluster")]
impl From<NodeStatus> for NodeHealthStatus {
    fn from(status: NodeStatus) -> Self {
        match status {
            NodeStatus::Online => NodeHealthStatus::Online,
            NodeStatus::Offline => NodeHealthStatus::Offline,
            NodeStatus::Joining => NodeHealthStatus::Unknown,
            NodeStatus::Leaving => NodeHealthStatus::Offline,
        }
    }
}

/// Information about a node's health.
#[derive(Debug, Clone)]
pub struct NodeHealthInfo {
    /// Node ID
    pub node_id: NodeId,

    /// Current health status
    pub status: NodeHealthStatus,

    /// Number of consecutive failures
    pub failure_count: u32,

    /// Number of consecutive successes
    pub success_count: u32,

    /// Last successful heartbeat timestamp (milliseconds since epoch)
    pub last_heartbeat: Option<u64>,

    /// Whether this node is the current leader
    pub is_leader: bool,
}

impl NodeHealthInfo {
    /// Create a new NodeHealthInfo with initial values.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            status: NodeHealthStatus::Unknown,
            failure_count: 0,
            success_count: 0,
            last_heartbeat: None,
            is_leader: false,
        }
    }

    /// Check if the node is online.
    pub fn is_online(&self) -> bool {
        matches!(self.status, NodeHealthStatus::Online)
    }

    /// Check if the node is offline.
    pub fn is_offline(&self) -> bool {
        matches!(self.status, NodeHealthStatus::Offline)
    }
}

/// Cluster Bus for managing inter-node communication and health monitoring.
///
/// The ClusterBus integrates with AiDb's MetaRaft to provide:
/// - Leader heartbeat detection via `MetaRaftNode::is_leader()` and `get_leader()`
/// - Failure detection via node status tracking and election timeout
///
/// # Feature-Dependent API
///
/// When the `cluster` feature is enabled, the ClusterBus uses `Arc<RwLock>` for
/// thread-safe access, allowing methods like `register_node` to take `&self`.
/// Without the feature, a plain `HashMap` is used, requiring `&mut self` for
/// mutation methods. This is by design to minimize overhead in non-cluster mode.
pub struct ClusterBus {
    /// This node's ID
    node_id: NodeId,

    /// Configuration
    config: ClusterBusConfig,

    /// Node health information (protected by RwLock for concurrent access)
    #[cfg(feature = "cluster")]
    node_health: Arc<RwLock<HashMap<NodeId, NodeHealthInfo>>>,

    #[cfg(not(feature = "cluster"))]
    node_health: std::collections::HashMap<NodeId, NodeHealthInfo>,

    /// Reference to the MetaRaftNode (when cluster feature is enabled)
    #[cfg(feature = "cluster")]
    meta_raft: Option<Arc<aidb::cluster::MetaRaftNode>>,

    /// Running state
    #[cfg(feature = "cluster")]
    running: Arc<RwLock<bool>>,
}

impl ClusterBus {
    /// Create a new ClusterBus instance.
    ///
    /// # Arguments
    ///
    /// * `node_id` - This node's unique identifier
    /// * `config` - Configuration for the cluster bus
    pub fn new(node_id: NodeId, config: ClusterBusConfig) -> Self {
        Self {
            node_id,
            config,
            #[cfg(feature = "cluster")]
            node_health: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(feature = "cluster"))]
            node_health: std::collections::HashMap::new(),
            #[cfg(feature = "cluster")]
            meta_raft: None,
            #[cfg(feature = "cluster")]
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Create a new ClusterBus with default configuration.
    pub fn with_defaults(node_id: NodeId) -> Self {
        Self::new(node_id, ClusterBusConfig::default())
    }

    /// Get this node's ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the configuration.
    pub fn config(&self) -> &ClusterBusConfig {
        &self.config
    }

    /// Set the MetaRaftNode reference for heartbeat detection.
    ///
    /// This should be called after the ClusterNode is initialized to enable
    /// leader heartbeat detection.
    #[cfg(feature = "cluster")]
    pub fn set_meta_raft(&mut self, meta_raft: Arc<aidb::cluster::MetaRaftNode>) {
        self.meta_raft = Some(meta_raft);
    }

    /// Set the MetaRaftNode reference (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn set_meta_raft(&mut self, _meta_raft: ()) {
        // No-op when cluster feature is disabled
    }

    /// Start the cluster bus health monitoring.
    ///
    /// This spawns a background task that periodically checks node health
    /// using the MetaRaft leader heartbeat mechanism.
    #[cfg(feature = "cluster")]
    pub async fn start(&self) -> Result<()> {
        {
            let mut running = self
                .running
                .write()
                .map_err(|e| AikvError::Storage(format!("Failed to acquire write lock: {}", e)))?;
            if *running {
                return Err(AikvError::Storage(
                    "Cluster bus already running".to_string(),
                ));
            }
            *running = true;
        }

        let node_id = self.node_id;
        let config = self.config.clone();
        let node_health = Arc::clone(&self.node_health);
        let running = Arc::clone(&self.running);
        let meta_raft = self.meta_raft.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.health_check_interval);

            loop {
                interval.tick().await;

                let should_stop = running.read().map(|r| !*r).unwrap_or(true);
                if should_stop {
                    tracing::info!("Cluster bus stopped");
                    break;
                }

                // Perform health check
                if let Some(ref meta) = meta_raft {
                    Self::check_cluster_health(node_id, meta, &config, &node_health).await;
                }
            }
        });

        tracing::info!(
            "Cluster bus started with health check interval: {:?}",
            self.config.health_check_interval
        );

        Ok(())
    }

    /// Start the cluster bus (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub async fn start(&self) -> Result<()> {
        Err(AikvError::Storage(
            "Cluster feature not enabled. Build with --features cluster".to_string(),
        ))
    }

    /// Stop the cluster bus.
    #[cfg(feature = "cluster")]
    pub fn stop(&self) {
        if let Ok(mut running) = self.running.write() {
            *running = false;
        }
        tracing::info!("Cluster bus stop requested");
    }

    /// Stop the cluster bus (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn stop(&self) {
        // No-op when cluster feature is disabled
    }

    /// Check if the cluster bus is running.
    #[cfg(feature = "cluster")]
    pub fn is_running(&self) -> bool {
        self.running.read().map(|r| *r).unwrap_or(false)
    }

    /// Check if the cluster bus is running (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn is_running(&self) -> bool {
        false
    }

    /// Perform cluster health check by querying MetaRaft.
    ///
    /// This method reads cluster metadata from the MetaRaft state machine and updates
    /// the local node health tracking. `get_cluster_meta()` is infallible as it reads
    /// from the local state machine without going through Raft consensus.
    #[cfg(feature = "cluster")]
    async fn check_cluster_health(
        node_id: NodeId,
        meta_raft: &Arc<aidb::cluster::MetaRaftNode>,
        config: &ClusterBusConfig,
        node_health: &Arc<RwLock<HashMap<NodeId, NodeHealthInfo>>>,
    ) {
        // Get cluster metadata (infallible - reads from local state machine)
        let cluster_meta = meta_raft.get_cluster_meta();

        // Check leader status
        let current_leader = meta_raft.get_leader().await;
        let is_leader = meta_raft.is_leader().await;

        // Update node health based on cluster metadata
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut health_map = match node_health.write() {
            Ok(guard) => guard,
            Err(e) => {
                tracing::error!("Failed to acquire write lock: {}", e);
                return;
            }
        };

        // Update self node status
        {
            let self_health = health_map
                .entry(node_id)
                .or_insert_with(|| NodeHealthInfo::new(node_id));
            self_health.is_leader = is_leader;
            self_health.last_heartbeat = Some(now);
            self_health.status = NodeHealthStatus::Online;
            self_health.success_count = self_health.success_count.saturating_add(1);
            self_health.failure_count = 0;
        }

        // Update other nodes from cluster metadata
        for (other_node_id, node_info) in &cluster_meta.nodes {
            if *other_node_id == node_id {
                continue; // Skip self
            }

            let health = health_map
                .entry(*other_node_id)
                .or_insert_with(|| NodeHealthInfo::new(*other_node_id));

            // Convert AiDb NodeStatus to our NodeHealthStatus
            let new_status: NodeHealthStatus = node_info.status.into();

            match new_status {
                NodeHealthStatus::Online => {
                    health.failure_count = 0;
                    health.success_count = health.success_count.saturating_add(1);

                    if health.success_count >= config.success_threshold {
                        health.status = NodeHealthStatus::Online;
                        health.last_heartbeat = Some(now);
                    }
                }
                NodeHealthStatus::Offline => {
                    health.success_count = 0;
                    health.failure_count = health.failure_count.saturating_add(1);

                    if health.failure_count >= config.failure_threshold {
                        health.status = NodeHealthStatus::Offline;
                    }
                }
                NodeHealthStatus::Unknown => {
                    // Keep existing status for unknown state
                }
            }

            // Update leader status
            health.is_leader = current_leader == Some(*other_node_id);
        }

        // Check for election timeout on nodes that haven't been seen
        let timeout_threshold = now.saturating_sub(config.election_timeout.as_millis() as u64);

        for (_, health) in health_map.iter_mut() {
            if let Some(last_hb) = health.last_heartbeat {
                if last_hb < timeout_threshold && health.status == NodeHealthStatus::Online {
                    health.failure_count = health.failure_count.saturating_add(1);

                    if health.failure_count >= config.failure_threshold {
                        health.status = NodeHealthStatus::Offline;
                        tracing::warn!(
                            "Node {} marked offline due to election timeout",
                            health.node_id
                        );
                    }
                }
            }
        }
    }

    /// Get health info for a specific node.
    #[cfg(feature = "cluster")]
    pub fn get_node_health(&self, node_id: NodeId) -> Option<NodeHealthInfo> {
        self.node_health
            .read()
            .ok()
            .and_then(|guard| guard.get(&node_id).cloned())
    }

    /// Get health info for a specific node (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn get_node_health(&self, node_id: NodeId) -> Option<NodeHealthInfo> {
        self.node_health.get(&node_id).cloned()
    }

    /// Get health info for all known nodes.
    #[cfg(feature = "cluster")]
    pub fn get_all_node_health(&self) -> HashMap<NodeId, NodeHealthInfo> {
        self.node_health
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Get health info for all known nodes (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn get_all_node_health(&self) -> HashMap<NodeId, NodeHealthInfo> {
        self.node_health.clone()
    }

    /// Check if a node is online.
    #[cfg(feature = "cluster")]
    pub fn is_node_online(&self, node_id: NodeId) -> bool {
        self.node_health
            .read()
            .ok()
            .and_then(|guard| guard.get(&node_id).map(|h| h.is_online()))
            .unwrap_or(false)
    }

    /// Check if a node is online (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn is_node_online(&self, node_id: NodeId) -> bool {
        self.node_health
            .get(&node_id)
            .map(|h| h.is_online())
            .unwrap_or(false)
    }

    /// Check if a node is offline.
    #[cfg(feature = "cluster")]
    pub fn is_node_offline(&self, node_id: NodeId) -> bool {
        self.node_health
            .read()
            .ok()
            .and_then(|guard| guard.get(&node_id).map(|h| h.is_offline()))
            .unwrap_or(true) // Unknown nodes are considered offline
    }

    /// Check if a node is offline (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn is_node_offline(&self, node_id: NodeId) -> bool {
        self.node_health
            .get(&node_id)
            .map(|h| h.is_offline())
            .unwrap_or(true)
    }

    /// Get the current leader node ID (if known).
    #[cfg(feature = "cluster")]
    pub async fn get_leader(&self) -> Option<NodeId> {
        if let Some(ref meta_raft) = self.meta_raft {
            meta_raft.get_leader().await
        } else {
            None
        }
    }

    /// Get the current leader node ID (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub async fn get_leader(&self) -> Option<NodeId> {
        None
    }

    /// Check if this node is the leader.
    #[cfg(feature = "cluster")]
    pub async fn is_leader(&self) -> bool {
        if let Some(ref meta_raft) = self.meta_raft {
            meta_raft.is_leader().await
        } else {
            false
        }
    }

    /// Check if this node is the leader (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub async fn is_leader(&self) -> bool {
        false
    }

    /// Register a node with the cluster bus for health tracking.
    #[cfg(feature = "cluster")]
    pub fn register_node(&self, node_id: NodeId) {
        if let Ok(mut guard) = self.node_health.write() {
            guard
                .entry(node_id)
                .or_insert_with(|| NodeHealthInfo::new(node_id));
        }
    }

    /// Register a node with the cluster bus (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn register_node(&mut self, node_id: NodeId) {
        self.node_health
            .entry(node_id)
            .or_insert_with(|| NodeHealthInfo::new(node_id));
    }

    /// Unregister a node from the cluster bus.
    #[cfg(feature = "cluster")]
    pub fn unregister_node(&self, node_id: NodeId) {
        if let Ok(mut guard) = self.node_health.write() {
            guard.remove(&node_id);
        }
    }

    /// Unregister a node from the cluster bus (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn unregister_node(&mut self, node_id: NodeId) {
        self.node_health.remove(&node_id);
    }

    /// Get the number of online nodes.
    #[cfg(feature = "cluster")]
    pub fn online_node_count(&self) -> usize {
        self.node_health
            .read()
            .map(|guard| guard.values().filter(|h| h.is_online()).count())
            .unwrap_or(0)
    }

    /// Get the number of online nodes (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn online_node_count(&self) -> usize {
        self.node_health.values().filter(|h| h.is_online()).count()
    }

    /// Get the total number of known nodes.
    #[cfg(feature = "cluster")]
    pub fn total_node_count(&self) -> usize {
        self.node_health
            .read()
            .map(|guard| guard.len())
            .unwrap_or(0)
    }

    /// Get the total number of known nodes (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn total_node_count(&self) -> usize {
        self.node_health.len()
    }

    /// Manually update a node's status (for testing or manual intervention).
    #[cfg(feature = "cluster")]
    pub fn update_node_status(&self, node_id: NodeId, status: NodeHealthStatus) {
        if let Ok(mut guard) = self.node_health.write() {
            if let Some(health) = guard.get_mut(&node_id) {
                health.status = status;
            } else {
                let mut info = NodeHealthInfo::new(node_id);
                info.status = status;
                guard.insert(node_id, info);
            }
        }
    }

    /// Manually update a node's status (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn update_node_status(&mut self, node_id: NodeId, status: NodeHealthStatus) {
        if let Some(health) = self.node_health.get_mut(&node_id) {
            health.status = status;
        } else {
            let mut info = NodeHealthInfo::new(node_id);
            info.status = status;
            self.node_health.insert(node_id, info);
        }
    }
}

impl Default for ClusterBus {
    fn default() -> Self {
        Self::with_defaults(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_bus_config_default() {
        let config = ClusterBusConfig::default();
        assert_eq!(config.health_check_interval, Duration::from_secs(10));
        assert_eq!(config.health_check_timeout, Duration::from_secs(5));
        assert_eq!(config.failure_threshold, 3);
        assert_eq!(config.success_threshold, 2);
        assert_eq!(config.election_timeout, Duration::from_secs(5));
    }

    #[test]
    fn test_cluster_bus_creation() {
        let bus = ClusterBus::with_defaults(1);
        assert_eq!(bus.node_id(), 1);
        assert!(!bus.is_running());
    }

    #[test]
    fn test_node_health_info() {
        let info = NodeHealthInfo::new(1);
        assert_eq!(info.node_id, 1);
        assert!(!info.is_online());
        assert!(!info.is_offline());
        assert!(!info.is_leader);
    }

    #[test]
    fn test_node_health_status() {
        assert_eq!(NodeHealthStatus::default(), NodeHealthStatus::Unknown);

        let online = NodeHealthStatus::Online;
        let offline = NodeHealthStatus::Offline;

        assert_ne!(online, offline);
    }

    #[cfg(not(feature = "cluster"))]
    #[test]
    fn test_cluster_bus_non_cluster_mode() {
        let mut bus = ClusterBus::with_defaults(1);

        // Register node
        bus.register_node(2);
        assert_eq!(bus.total_node_count(), 1);

        // Update status
        bus.update_node_status(2, NodeHealthStatus::Online);
        assert!(bus.is_node_online(2));

        // Unregister
        bus.unregister_node(2);
        assert_eq!(bus.total_node_count(), 0);
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn test_cluster_bus_cluster_mode() {
        let bus = ClusterBus::with_defaults(1);

        // Register node
        bus.register_node(2);
        assert_eq!(bus.total_node_count(), 1);

        // Update status
        bus.update_node_status(2, NodeHealthStatus::Online);
        assert!(bus.is_node_online(2));
        assert!(!bus.is_node_offline(2));

        // Check unknown node
        assert!(!bus.is_node_online(999));
        assert!(bus.is_node_offline(999)); // Unknown nodes considered offline

        // Unregister
        bus.unregister_node(2);
        assert_eq!(bus.total_node_count(), 0);
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn test_node_status_conversion() {
        use aidb::cluster::NodeStatus;

        assert_eq!(
            NodeHealthStatus::from(NodeStatus::Online),
            NodeHealthStatus::Online
        );
        assert_eq!(
            NodeHealthStatus::from(NodeStatus::Offline),
            NodeHealthStatus::Offline
        );
        assert_eq!(
            NodeHealthStatus::from(NodeStatus::Joining),
            NodeHealthStatus::Unknown
        );
        assert_eq!(
            NodeHealthStatus::from(NodeStatus::Leaving),
            NodeHealthStatus::Offline
        );
    }

    #[test]
    fn test_cluster_bus_default() {
        let bus = ClusterBus::default();
        assert_eq!(bus.node_id(), 0);
    }

    #[test]
    fn test_node_health_info_states() {
        let mut info = NodeHealthInfo::new(1);

        // Test online state
        info.status = NodeHealthStatus::Online;
        assert!(info.is_online());
        assert!(!info.is_offline());

        // Test offline state
        info.status = NodeHealthStatus::Offline;
        assert!(!info.is_online());
        assert!(info.is_offline());

        // Test unknown state
        info.status = NodeHealthStatus::Unknown;
        assert!(!info.is_online());
        assert!(!info.is_offline());
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn test_online_node_count() {
        let bus = ClusterBus::with_defaults(1);

        bus.register_node(1);
        bus.register_node(2);
        bus.register_node(3);

        bus.update_node_status(1, NodeHealthStatus::Online);
        bus.update_node_status(2, NodeHealthStatus::Online);
        bus.update_node_status(3, NodeHealthStatus::Offline);

        assert_eq!(bus.online_node_count(), 2);
        assert_eq!(bus.total_node_count(), 3);
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn test_get_all_node_health() {
        let bus = ClusterBus::with_defaults(1);

        bus.register_node(1);
        bus.register_node(2);
        bus.update_node_status(1, NodeHealthStatus::Online);

        let all_health = bus.get_all_node_health();
        assert_eq!(all_health.len(), 2);
        assert!(all_health.contains_key(&1));
        assert!(all_health.contains_key(&2));
    }
}
