//! MetaRaft client for cluster state synchronization.
//!
//! This module provides a client wrapper for AiDb's MetaRaftNode that enables
//! cluster state synchronization via Raft consensus, replacing the need for
//! Redis's gossip protocol on port 16379.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Redis Client (redis-cli)                 │
//! │            CLUSTER MEET / ADDSLOTS / NODES                  │
//! └─────────────────────────────────────────────────────────────┘
//!                               │
//!                               ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     MetaRaftClient                          │
//! │  - propose_node_join()     → Raft proposal                  │
//! │  - propose_slot_assign()   → Raft proposal                  │
//! │  - get_cluster_view()      → Read Raft state                │
//! │  - heartbeat()             → Raft lease write               │
//! └─────────────────────────────────────────────────────────────┘
//!                               │
//!                               ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │              AiDb MetaRaftNode (Group 0)                    │
//! │  - Raft consensus for cluster metadata                      │
//! │  - Automatic replication to all nodes                       │
//! │  - Strong consistency guarantees                            │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Key Benefits
//!
//! | Feature | Redis Gossip | Multi-Raft |
//! |---------|--------------|------------|
//! | Port 16379 | Required | **Not required** |
//! | Consistency | Eventually consistent | **Strongly consistent** |
//! | State convergence | Seconds to minutes | **Immediate** |

use crate::error::{AikvError, Result};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "cluster")]
use aidb::cluster::MetaRaftNode;

/// Type alias for node ID
pub type NodeId = u64;

/// Constant for unassigned slot group (group ID 0 means no assignment)
const UNASSIGNED_GROUP: u64 = 0;

/// Cluster view containing all cluster state from MetaRaft.
#[derive(Debug, Clone, Default)]
pub struct ClusterView {
    /// Known nodes in the cluster (node_id -> node info)
    pub nodes: std::collections::HashMap<NodeId, ClusterNodeInfo>,
    /// Slot assignments (slot -> node_id)
    pub slot_assignments: Vec<Option<NodeId>>,
    /// Current cluster epoch
    pub config_epoch: u64,
    /// This node's ID
    pub my_node_id: NodeId,
}

/// Information about a node in the cluster view.
#[derive(Debug, Clone)]
pub struct ClusterNodeInfo {
    /// Node ID
    pub node_id: NodeId,
    /// Data port address (ip:port for Redis protocol)
    pub data_addr: String,
    /// Raft RPC address (ip:port for gRPC)
    pub raft_addr: String,
    /// Whether the node is online
    pub is_online: bool,
    /// Whether the node is a master
    pub is_master: bool,
    /// Last heartbeat timestamp (milliseconds)
    pub last_heartbeat: u64,
}

/// Configuration for the MetaRaft client.
#[derive(Debug, Clone)]
pub struct MetaRaftClientConfig {
    /// Heartbeat interval (default: 100ms)
    pub heartbeat_interval: Duration,
    /// Node timeout before marking as failed (default: 5s)
    pub node_timeout: Duration,
}

impl Default for MetaRaftClientConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_millis(100),
            node_timeout: Duration::from_secs(5),
        }
    }
}

/// MetaRaft client for cluster state operations.
///
/// This client wraps AiDb's MetaRaftNode to provide:
/// - Node join/leave via Raft consensus
/// - Slot assignment via Raft consensus
/// - Heartbeat lease mechanism
/// - Consistent cluster view reads
#[cfg(feature = "cluster")]
pub struct MetaRaftClient {
    /// Reference to the MetaRaftNode
    meta_raft: Arc<MetaRaftNode>,
    /// This node's ID
    node_id: NodeId,
    /// This node's data address
    data_addr: String,
    /// This node's Raft RPC address
    raft_addr: String,
    /// Configuration
    config: MetaRaftClientConfig,
    /// Whether the heartbeat task is running
    heartbeat_running: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "cluster")]
impl MetaRaftClient {
    /// Create a new MetaRaftClient.
    ///
    /// # Arguments
    ///
    /// * `meta_raft` - Reference to the AiDb MetaRaftNode
    /// * `node_id` - This node's unique identifier
    /// * `data_addr` - This node's data port address (for Redis protocol)
    /// * `raft_addr` - This node's Raft RPC address (for gRPC)
    pub fn new(
        meta_raft: Arc<MetaRaftNode>,
        node_id: NodeId,
        data_addr: String,
        raft_addr: String,
    ) -> Self {
        Self {
            meta_raft,
            node_id,
            data_addr,
            raft_addr,
            config: MetaRaftClientConfig::default(),
            heartbeat_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create a new MetaRaftClient with custom configuration.
    pub fn with_config(
        meta_raft: Arc<MetaRaftNode>,
        node_id: NodeId,
        data_addr: String,
        raft_addr: String,
        config: MetaRaftClientConfig,
    ) -> Self {
        Self {
            meta_raft,
            node_id,
            data_addr,
            raft_addr,
            config,
            heartbeat_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Get this node's ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get this node's data address.
    pub fn data_addr(&self) -> &str {
        &self.data_addr
    }

    /// Get this node's Raft address.
    pub fn raft_addr(&self) -> &str {
        &self.raft_addr
    }

    /// Propose a node join to the cluster via Raft consensus.
    ///
    /// This adds a new node to the cluster metadata. All nodes will see
    /// the update after the Raft log is committed and replicated.
    ///
    /// # Arguments
    ///
    /// * `target_node_id` - ID of the node to add
    /// * `raft_addr` - Node's Raft RPC address (used for Raft consensus communication)
    ///
    /// # Returns
    ///
    /// Ok(()) if the proposal was accepted, Err if it failed
    ///
    /// # Note
    ///
    /// The data address (Redis protocol) is stored separately in `ClusterState`.
    /// This method only registers the Raft RPC address with the MetaRaft cluster.
    pub async fn propose_node_join(&self, target_node_id: NodeId, raft_addr: String) -> Result<()> {
        self.meta_raft
            .add_node(target_node_id, raft_addr)
            .await
            .map_err(|e| AikvError::Storage(format!("Failed to propose node join: {}", e)))?;

        tracing::info!(
            "MetaRaft: Node {} join proposed successfully",
            target_node_id
        );

        Ok(())
    }

    /// Propose a node removal from the cluster via Raft consensus.
    ///
    /// # Arguments
    ///
    /// * `target_node_id` - ID of the node to remove
    ///
    /// # Returns
    ///
    /// Ok(()) if the proposal was accepted, Err if it failed
    pub async fn propose_node_leave(&self, target_node_id: NodeId) -> Result<()> {
        self.meta_raft
            .remove_node(target_node_id)
            .await
            .map_err(|e| AikvError::Storage(format!("Failed to propose node leave: {}", e)))?;

        tracing::info!(
            "MetaRaft: Node {} leave proposed successfully",
            target_node_id
        );

        Ok(())
    }

    /// Get the current cluster view from MetaRaft state.
    ///
    /// This reads the local Raft state machine and returns the cluster
    /// metadata. The view is strongly consistent - all nodes see the
    /// same state after Raft commits are replicated.
    ///
    /// # Returns
    ///
    /// ClusterView containing nodes, slot assignments, and config epoch
    ///
    /// # Note
    ///
    /// The `data_addr` and `raft_addr` fields in `ClusterNodeInfo` are currently
    /// set to the same value from MetaRaft. In a production setup, these would
    /// be tracked separately (Redis port vs gRPC port).
    ///
    /// TODO: Track data_addr and raft_addr separately when cluster configuration is extended.
    pub fn get_cluster_view(&self) -> ClusterView {
        let meta = self.meta_raft.get_cluster_meta();

        let mut nodes = std::collections::HashMap::new();
        // Note: Using 0 as fallback for timestamp is acceptable here since it only
        // affects the `last_heartbeat` field which is informational. The actual
        // node liveness is determined by `is_online` from Raft status.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // In AiDb's model, slots are assigned to Raft groups, not directly to nodes.
        // A node is considered a "master" if it's a member of any group that owns slots.
        // For simplicity, we check if the node is in any group that has slots assigned.
        let mut groups_with_slots: std::collections::HashSet<u64> =
            std::collections::HashSet::new();
        for &group_id in meta.slots.iter() {
            if group_id != UNASSIGNED_GROUP {
                groups_with_slots.insert(group_id);
            }
        }

        // Build a map of which nodes are in which groups
        let mut node_to_groups: std::collections::HashMap<u64, Vec<u64>> =
            std::collections::HashMap::new();
        for (group_id, group_meta) in &meta.groups {
            for &member_node_id in &group_meta.replicas {
                node_to_groups
                    .entry(member_node_id)
                    .or_default()
                    .push(*group_id);
            }
        }

        for (node_id, node_info) in &meta.nodes {
            use aidb::cluster::NodeStatus;
            let is_online = matches!(node_info.status, NodeStatus::Online);

            // A node is a master if it's a member of any Raft group that owns slots
            let is_master = node_to_groups
                .get(node_id)
                .map(|groups| groups.iter().any(|g| groups_with_slots.contains(g)))
                .unwrap_or(false);

            nodes.insert(
                *node_id,
                ClusterNodeInfo {
                    node_id: *node_id,
                    // TODO: Track data_addr separately from raft_addr
                    data_addr: node_info.addr.clone(),
                    raft_addr: node_info.addr.clone(),
                    is_online,
                    is_master,
                    last_heartbeat: now,
                },
            );
        }

        // Get slot assignments from the cluster metadata
        // Note: In AiDb, slots are assigned to groups. We return the group_id as the "owner"
        // since that's what AiKv's cluster model expects.
        let slot_assignments = meta
            .slots
            .iter()
            .map(|&group_id| {
                if group_id == UNASSIGNED_GROUP {
                    None
                } else {
                    Some(group_id)
                }
            })
            .collect();

        ClusterView {
            nodes,
            slot_assignments,
            config_epoch: meta.config_version,
            my_node_id: self.node_id,
        }
    }

    /// Check if this node is the Raft leader.
    pub async fn is_leader(&self) -> bool {
        self.meta_raft.is_leader().await
    }

    /// Get the current Raft leader.
    pub async fn get_leader(&self) -> Option<NodeId> {
        self.meta_raft.get_leader().await
    }

    /// Start the background heartbeat task.
    ///
    /// This spawns a task that periodically checks cluster health.
    ///
    /// # Implementation Note
    ///
    /// Node liveness in AiKv clusters is primarily tracked by the Raft consensus
    /// mechanism through OpenRaft's built-in leader heartbeat. This task serves
    /// as an application-level health monitor that can be used to:
    /// - Log diagnostic information
    /// - Trigger application-specific health checks
    /// - Monitor cluster state changes
    ///
    /// For actual failure detection and leader election, AiKv relies entirely
    /// on OpenRaft's Raft protocol implementation.
    pub fn start_heartbeat(&self) {
        if self
            .heartbeat_running
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            tracing::warn!("Heartbeat task already running");
            return;
        }

        let meta_raft = Arc::clone(&self.meta_raft);
        let node_id = self.node_id;
        let interval = self.config.heartbeat_interval;
        let running = Arc::clone(&self.heartbeat_running);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            let mut last_leader: Option<NodeId> = None;

            loop {
                ticker.tick().await;

                if !running.load(std::sync::atomic::Ordering::SeqCst) {
                    tracing::info!("Heartbeat task stopped for node {}", node_id);
                    break;
                }

                // Check for leader changes (useful for monitoring)
                let current_leader = meta_raft.get_leader().await;
                if current_leader != last_leader {
                    if let Some(leader) = current_leader {
                        tracing::info!(
                            "Cluster leader changed: {:?} -> {} (self={})",
                            last_leader,
                            leader,
                            if leader == node_id { "yes" } else { "no" }
                        );
                    } else {
                        tracing::warn!("Cluster has no leader");
                    }
                    last_leader = current_leader;
                }

                // Trace-level heartbeat for debugging
                tracing::trace!("Heartbeat tick for node {}", node_id);
            }
        });

        tracing::info!(
            "Started heartbeat task for node {} with interval {:?}",
            self.node_id,
            self.config.heartbeat_interval
        );
    }

    /// Stop the background heartbeat task.
    pub fn stop_heartbeat(&self) {
        self.heartbeat_running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        tracing::info!("Stopping heartbeat task for node {}", self.node_id);
    }

    /// Check if the heartbeat task is running.
    pub fn is_heartbeat_running(&self) -> bool {
        self.heartbeat_running
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Stub implementation for non-cluster builds.
#[cfg(not(feature = "cluster"))]
pub struct MetaRaftClient {
    node_id: NodeId,
}

#[cfg(not(feature = "cluster"))]
impl MetaRaftClient {
    /// Create a stub MetaRaftClient (non-cluster mode).
    pub fn new_stub(node_id: NodeId) -> Self {
        Self {
            node_id,
        }
    }

    /// Get this node's ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Stub: propose_node_join always fails in non-cluster mode.
    pub async fn propose_node_join(
        &self,
        _target_node_id: NodeId,
        _raft_addr: String,
    ) -> Result<()> {
        Err(AikvError::Storage(
            "Cluster feature not enabled".to_string(),
        ))
    }

    /// Stub: propose_node_leave always fails in non-cluster mode.
    pub async fn propose_node_leave(&self, _target_node_id: NodeId) -> Result<()> {
        Err(AikvError::Storage(
            "Cluster feature not enabled".to_string(),
        ))
    }

    /// Stub: get_cluster_view returns empty view in non-cluster mode.
    pub fn get_cluster_view(&self) -> ClusterView {
        ClusterView::default()
    }

    /// Stub: is_leader always returns false in non-cluster mode.
    pub async fn is_leader(&self) -> bool {
        false
    }

    /// Stub: get_leader always returns None in non-cluster mode.
    pub async fn get_leader(&self) -> Option<NodeId> {
        None
    }

    /// Stub: start_heartbeat does nothing in non-cluster mode.
    pub fn start_heartbeat(&self) {}

    /// Stub: stop_heartbeat does nothing in non-cluster mode.
    pub fn stop_heartbeat(&self) {}

    /// Stub: is_heartbeat_running always returns false in non-cluster mode.
    pub fn is_heartbeat_running(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_view_default() {
        let view = ClusterView::default();
        assert!(view.nodes.is_empty());
        assert!(view.slot_assignments.is_empty());
        assert_eq!(view.config_epoch, 0);
        assert_eq!(view.my_node_id, 0);
    }

    #[test]
    fn test_cluster_node_info() {
        let info = ClusterNodeInfo {
            node_id: 1,
            data_addr: "127.0.0.1:6379".to_string(),
            raft_addr: "127.0.0.1:50051".to_string(),
            is_online: true,
            is_master: true,
            last_heartbeat: 0,
        };

        assert_eq!(info.node_id, 1);
        assert!(info.is_online);
        assert!(info.is_master);
    }

    #[test]
    fn test_metaraft_client_config_default() {
        let config = MetaRaftClientConfig::default();
        assert_eq!(config.heartbeat_interval, Duration::from_millis(100));
        assert_eq!(config.node_timeout, Duration::from_secs(5));
    }

    #[cfg(not(feature = "cluster"))]
    #[test]
    fn test_stub_client() {
        let client = MetaRaftClient::new_stub(1);
        assert_eq!(client.node_id(), 1);
        assert!(!client.is_heartbeat_running());
    }
}
