//! Redis Cluster commands implementation using AiDb Multi-Raft API.
//!
//! This module provides a thin glue layer that maps Redis Cluster protocol
//! commands to AiDb's Multi-Raft API as documented in MULTI_RAFT_API_REFERENCE.md.
//!
//! Key principle: Minimal code - only Redis protocol format conversion.
//! All cluster logic is delegated to AiDb's MetaRaftNode, MultiRaftNode, Router, etc.
//!
//! ## MOVED Redirection
//!
//! This module implements Redis Cluster's MOVED redirection protocol. When a client
//! sends a command to the wrong node (based on the key's slot), the server returns:
//!
//! ```text
//! -MOVED <slot> <ip>:<port>
//! ```
//!
//! This tells the client which node owns the slot and where to retry the command.
//! The client should update its slot-to-node mapping and redirect future requests
//! for that slot to the correct node.

use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use bytes::Bytes;
use std::sync::Arc;
use tracing::{debug, info};

#[cfg(feature = "cluster")]
use aidb::cluster::{
    ClusterMeta, GroupId, MetaNodeInfo, MetaRaftNode, MigrationManager,
    MultiRaftNode, NodeId, NodeStatus, Router,
};

#[cfg(feature = "cluster")]
use openraft::BasicNode;

/// Redis Cluster has 16384 slots
const TOTAL_SLOTS: u16 = 16384;

/// Failover mode for CLUSTER FAILOVER command
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverMode {
    /// Default failover - wait for master agreement
    Default,
    /// Force failover without master agreement
    Force,
    /// Takeover - force failover even if master is unreachable
    Takeover,
}

/// Redirection type for cluster routing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectType {
    /// -MOVED redirect: key belongs to another node
    Moved,
    /// -ASK redirect: key is being migrated to another node
    Ask,
}

/// Node information for CLUSTER NODES response.
/// Maps from AiDb's MetaNodeInfo to Redis format.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: NodeId,
    pub addr: String,
    pub cluster_port: u16,
    pub is_master: bool,
    pub is_connected: bool,
    pub master_id: Option<NodeId>,
    pub replica_ids: Vec<NodeId>,
}

impl NodeInfo {
    /// Create from AiDb's MetaNodeInfo
    pub fn from_meta_node_info(id: NodeId, info: &MetaNodeInfo) -> Self {
        let cluster_port = Self::extract_cluster_port(&info.addr);
        Self {
            id,
            addr: info.addr.clone(),
            cluster_port,
            is_master: true, // Will be updated based on group info
            is_connected: matches!(info.status, NodeStatus::Online),
            master_id: None,
            replica_ids: Vec::new(),
        }
    }

    fn extract_cluster_port(addr: &str) -> u16 {
        if let Some(port_str) = addr.split(':').nth_back(0) {
            port_str.parse::<u16>().unwrap_or(6379) + 10000
        } else {
            16379
        }
    }
}

/// Redis Cluster commands handler.
///
/// This is a thin wrapper around AiDb's Multi-Raft components:
/// - MetaRaftNode: For cluster metadata management
/// - MultiRaftNode: For data operations with automatic routing
/// - Router: For key-to-slot-to-group routing
/// - MigrationManager: For slot migration (optional)
#[cfg(feature = "cluster")]
pub struct ClusterCommands {
    /// This node's ID
    node_id: NodeId,

    /// Reference to MetaRaftNode for cluster metadata
    meta_raft: Arc<MetaRaftNode>,

    /// Reference to MultiRaftNode for data operations
    #[allow(dead_code)]
    multi_raft: Arc<MultiRaftNode>,

    /// Router for key-to-slot-to-group mapping
    #[allow(dead_code)]
    router: Arc<Router>,

    /// Optional migration manager for slot migration
    migration_manager: Option<Arc<MigrationManager>>,
}

#[cfg(feature = "cluster")]
impl ClusterCommands {
    /// Create a new ClusterCommands handler.
    ///
    /// # Arguments
    ///
    /// * `node_id` - This node's unique identifier
    /// * `meta_raft` - MetaRaftNode for cluster metadata
    /// * `multi_raft` - MultiRaftNode for data operations
    /// * `router` - Router for key routing
    pub fn new(
        node_id: NodeId,
        meta_raft: Arc<MetaRaftNode>,
        multi_raft: Arc<MultiRaftNode>,
        router: Arc<Router>,
    ) -> Self {
        Self {
            node_id,
            meta_raft,
            multi_raft,
            router,
            migration_manager: None,
        }
    }

    /// Set the migration manager (optional)
    pub fn set_migration_manager(&mut self, manager: Arc<MigrationManager>) {
        self.migration_manager = Some(manager);
    }

    /// Handle CLUSTER INFO command.
    ///
    /// Maps to: `meta_raft.get_cluster_meta()`
    pub fn cluster_info(&self) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();

        // Count assigned slots
        let assigned_slots = meta.slots.iter().filter(|&&g| g > 0).count();

        // Count online nodes
        let known_nodes = meta.nodes.len();
        #[allow(unused)]
        let online_nodes = meta
            .nodes
            .values()
            .filter(|n| matches!(n.status, NodeStatus::Online))
            .count();

        // Determine cluster state
        // Cluster is OK if all slots are assigned and all groups with slots have leaders
        let all_groups_have_leaders = meta.groups.iter().all(|(gid, g)| {
            // Check if this group owns any slots
            let owns_slots = meta.slots.iter().any(|&s| s == *gid);
            // If it owns slots, it must have a leader
            !owns_slots || g.leader.is_some()
        });
        
        let cluster_state = if assigned_slots == TOTAL_SLOTS as usize && all_groups_have_leaders {
            "ok"
        } else {
            "fail"
        };

        let info = format!(
            "cluster_state:{}\r\n\
             cluster_slots_assigned:{}\r\n\
             cluster_slots_ok:{}\r\n\
             cluster_slots_pfail:0\r\n\
             cluster_slots_fail:0\r\n\
             cluster_known_nodes:{}\r\n\
             cluster_size:{}\r\n\
             cluster_current_epoch:{}\r\n\
             cluster_my_epoch:{}\r\n\
             cluster_stats_messages_sent:0\r\n\
             cluster_stats_messages_received:0",
            cluster_state,
            assigned_slots,
            assigned_slots,
            known_nodes,
            meta.groups.len(),
            meta.config_version,
            meta.config_version,
        );

        Ok(RespValue::BulkString(Some(Bytes::from(info))))
    }

    /// Handle CLUSTER NODES command.
    ///
    /// Maps to: `meta_raft.get_cluster_meta().nodes` and `.groups`
    pub fn cluster_nodes(&self) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();
        let mut lines = Vec::new();

        for (node_id, node_info) in &meta.nodes {
            // For Redis cluster compatibility, report all nodes as "connected"
            // since they are registered in the cluster metadata and reachable.
            // TODO: Implement proper health checking to determine actual node status
            let status = match node_info.status {
                NodeStatus::Online => "connected",
                NodeStatus::Offline => "disconnected",
                // Treat Joining and other states as connected for Redis compatibility
                _ => "connected",
            };

            // Check if this node is a master (leader of a group) or replica
            let is_master = meta.groups.values().any(|g| g.leader == Some(*node_id));
            let role = if is_master { "master" } else { "slave" };

            // Find the master node ID if this is a replica
            let master_id = if is_master {
                "-".to_string()
            } else {
                // Find which group this replica belongs to and get its leader
                meta.groups
                    .values()
                    .find(|g| g.replicas.contains(node_id) && g.leader.is_some())
                    .and_then(|g| g.leader)
                    .map(|lid| format!("{:040x}", lid))
                    .unwrap_or_else(|| "-".to_string())
            };

            // Only masters have slot ranges in CLUSTER NODES output
            let mut slot_ranges = Vec::new();
            if is_master {
                for (group_id, group_meta) in &meta.groups {
                    if group_meta.leader == Some(*node_id) {
                        // Find slot range for this group
                        let mut start = None;
                        let mut end = None;
                        for (slot_idx, &assigned_group) in meta.slots.iter().enumerate() {
                            if assigned_group == *group_id {
                                if start.is_none() {
                                    start = Some(slot_idx);
                                }
                                end = Some(slot_idx);
                            } else if start.is_some() {
                                slot_ranges.push(format!("{}-{}", start.unwrap(), end.unwrap()));
                                start = None;
                                end = None;
                            }
                        }
                        if let Some(s) = start {
                            slot_ranges.push(format!("{}-{}", s, end.unwrap()));
                        }
                    }
                }
            }

            // Format address properly: ip:data_port@cluster_bus_port
            // node_info.addr is like "aikv1:50051" (raft address), we need to convert to data port
            let data_addr = Self::extract_data_address(&node_info.addr);
            let cluster_port = Self::extract_cluster_port_from_data_port(&data_addr);

            // Format: <id> <ip:port@cport> <flags> <master> <ping-sent> <pong-recv> <config-epoch> <link-state> <slot> <slot> ...
            let myself_flag = if *node_id == self.node_id {
                "myself,"
            } else {
                ""
            };
            let node_line = format!(
                "{:040x} {}@{} {}{} {} 0 0 {} {} {}",
                node_id,
                data_addr,
                cluster_port,
                myself_flag,
                role,
                master_id,
                meta.config_version,
                status,
                slot_ranges.join(" ")
            );

            lines.push(node_line);
        }

        let result = lines.join("\r\n");
        Ok(RespValue::BulkString(Some(Bytes::from(result))))
    }

    /// Handle CLUSTER SLOTS command.
    ///
    /// Maps to: `meta_raft.get_cluster_meta().slots` and `.groups`
    pub fn cluster_slots(&self) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();
        let mut slots_info = Vec::new();

        // Group consecutive slots by group_id
        let mut current_group: Option<GroupId> = None;
        let mut range_start: u16 = 0;

        for (slot, &group_id) in meta.slots.iter().enumerate() {
            if group_id == 0 {
                // Unassigned slot
                if current_group.is_some() {
                    if let Some(group) = current_group {
                        slots_info.push(self.format_slot_range(
                            &meta,
                            range_start,
                            (slot - 1) as u16,
                            group,
                        ));
                    }
                    current_group = None;
                }
                continue;
            }

            match current_group {
                None => {
                    // Start new range
                    current_group = Some(group_id);
                    range_start = slot as u16;
                }
                Some(cg) if cg != group_id => {
                    // Different group, output previous range and start new one
                    slots_info.push(self.format_slot_range(
                        &meta,
                        range_start,
                        (slot - 1) as u16,
                        cg,
                    ));
                    current_group = Some(group_id);
                    range_start = slot as u16;
                }
                _ => {
                    // Same group, continue range
                }
            }
        }

        // Output last range if any
        if let Some(group) = current_group {
            slots_info.push(self.format_slot_range(&meta, range_start, TOTAL_SLOTS - 1, group));
        }

        Ok(RespValue::Array(Some(slots_info)))
    }

    /// Format a slot range for CLUSTER SLOTS response
    fn format_slot_range(
        &self,
        meta: &ClusterMeta,
        start: u16,
        end: u16,
        group_id: GroupId,
    ) -> RespValue {
        let mut elements = vec![
            RespValue::Integer(start as i64),
            RespValue::Integer(end as i64),
        ];

        if let Some(group_meta) = meta.groups.get(&group_id) {
            // Add master node first
            if let Some(leader_id) = group_meta.leader {
                if let Some(node_info) = meta.nodes.get(&leader_id) {
                    elements.push(self.format_node_info(leader_id, node_info));
                }
            }

            // Add replica nodes
            for &replica_id in &group_meta.replicas {
                if Some(replica_id) != group_meta.leader {
                    if let Some(node_info) = meta.nodes.get(&replica_id) {
                        elements.push(self.format_node_info(replica_id, node_info));
                    }
                }
            }
        }

        RespValue::Array(Some(elements))
    }

    /// Format node info for CLUSTER SLOTS response
    fn format_node_info(&self, node_id: NodeId, node_info: &MetaNodeInfo) -> RespValue {
        // Convert Raft address to data address
        let data_addr = Self::extract_data_address(&node_info.addr);
        let (ip, port) = Self::parse_addr(&data_addr);
        RespValue::Array(Some(vec![
            RespValue::BulkString(Some(Bytes::from(ip))),
            RespValue::Integer(port),
            RespValue::BulkString(Some(Bytes::from(format!("{:040x}", node_id)))),
        ]))
    }

    /// Parse address into (ip, port)
    fn parse_addr(addr: &str) -> (String, i64) {
        if let Some((ip, port_str)) = addr.rsplit_once(':') {
            let port = port_str.parse::<i64>().unwrap_or(6379);
            (ip.to_string(), port)
        } else {
            (addr.to_string(), 6379)
        }
    }

    /// Extract cluster port from address string
    #[allow(dead_code)]
    fn extract_cluster_port(addr: &str) -> u16 {
        if let Some(port_str) = addr.split(':').nth_back(0) {
            port_str.parse::<u16>().unwrap_or(6379) + 10000
        } else {
            16379
        }
    }

    /// Extract data address from node address
    /// Handles two formats:
    /// - Data format: "127.0.0.1:6380" -> returns as is
    /// - Raft format: "aikv1:50051" -> converts to "127.0.0.1:6379"
    fn extract_data_address(addr: &str) -> String {
        if let Some(port_str) = addr.split(':').nth_back(0) {
            if let Ok(port) = port_str.parse::<u16>() {
                // If port is in Raft range (50051-50056), convert to data port
                if (50051..=50056).contains(&port) {
                    let data_port = 6379 + (port - 50051);
                    return format!("127.0.0.1:{}", data_port);
                }
                // If port is in data range (6379-6384), keep as is but use 127.0.0.1
                if (6379..=6384).contains(&port) {
                    return format!("127.0.0.1:{}", port);
                }
            }
        }
        // Fallback
        addr.to_string()
    }

    /// Extract cluster bus port from data port
    fn extract_cluster_port_from_data_port(data_addr: &str) -> u16 {
        if let Some(port_str) = data_addr.split(':').nth_back(0) {
            port_str.parse::<u16>().unwrap_or(6379) + 10000
        } else {
            16379
        }
    }

    /// Handle CLUSTER MYID command.
    ///
    /// Maps to: node_id
    pub fn cluster_myid(&self) -> Result<RespValue> {
        Ok(RespValue::BulkString(Some(Bytes::from(format!(
            "{:040x}",
            self.node_id
        )))))
    }

    /// Handle CLUSTER KEYSLOT command.
    ///
    /// Maps to: `Router::key_to_slot(key)`
    pub fn cluster_keyslot(&self, key: &[u8]) -> Result<RespValue> {
        let slot = Router::key_to_slot(key);
        Ok(RespValue::Integer(slot as i64))
    }

    /// Handle CLUSTER MEET command.
    ///
    /// Maps to: `meta_raft.add_node(node_id, addr)`
    ///
    /// # Arguments
    ///
    /// * `ip` - IP address of the node to add
    /// * `port` - Port of the node to add
    /// * `node_id_opt` - Optional pre-assigned node ID
    pub async fn cluster_meet(
        &self,
        ip: String,
        port: u16,
        node_id_opt: Option<NodeId>,
    ) -> Result<RespValue> {
        let addr = format!("{}:{}", ip, port);

        // Generate node ID if not provided
        let node_id = node_id_opt.unwrap_or_else(|| {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            addr.hash(&mut hasher);
            hasher.finish()
        });

        // Add node to cluster metadata via MetaRaft
        // This adds the node to the cluster's node list
        self.meta_raft
            .add_node(node_id, addr.clone())
            .await
            .map_err(|e| AikvError::Internal(format!("Failed to add node to cluster: {}", e)))?;

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER FORGET command.
    ///
    /// Maps to: `meta_raft.remove_node(node_id)`
    pub async fn cluster_forget(&self, node_id: NodeId) -> Result<RespValue> {
        // Remove node via MetaRaft - this will sync to all nodes via Raft consensus
        self.meta_raft
            .remove_node(node_id)
            .await
            .map_err(|e| AikvError::Internal(format!("Failed to remove node: {}", e)))?;

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER ADDSLOTS command.
    ///
    /// Maps to: `meta_raft.update_slots(start, end, group_id)`
    ///
    /// Note: For Redis compatibility, we need to assign slots to a group.
    /// The group_id is determined by finding which group this node belongs to.
    /// If the node doesn't belong to any group yet, we create one automatically.
    pub async fn cluster_addslots(&self, slots: Vec<u16>) -> Result<RespValue> {
        let meta = self.meta_raft.get_cluster_meta();

        // Find the group that this node belongs to, or create one if it doesn't exist
        let group_id = if let Some((gid, _)) = meta
            .groups
            .iter()
            .find(|(_, g)| g.replicas.contains(&self.node_id))
        {
            *gid
        } else {
            // Auto-create a group for this node using its node_id as the group_id
            // This matches Redis behavior where each master initially forms its own group
            let group_id = self.node_id;
            self.meta_raft
                .create_group(group_id, vec![self.node_id])
                .await
                .map_err(|e| {
                    AikvError::Internal(format!("Failed to create group for node: {}", e))
                })?;
            group_id
        };

        // Validate all slots first
        for &slot in &slots {
            if slot >= TOTAL_SLOTS {
                return Err(AikvError::Invalid(format!("Invalid slot: {}", slot)));
            }
        }

        // Optimize: merge consecutive slots into ranges for batch updates
        if slots.is_empty() {
            return Ok(RespValue::SimpleString("OK".to_string()));
        }

        let mut sorted_slots = slots.clone();
        sorted_slots.sort_unstable();

        // Group consecutive slots into ranges
        let mut ranges: Vec<(u16, u16)> = Vec::new();
        let mut range_start = sorted_slots[0];
        let mut range_end = sorted_slots[0];

        for &slot in &sorted_slots[1..] {
            if slot == range_end + 1 {
                range_end = slot;
            } else {
                ranges.push((range_start, range_end + 1)); // end is exclusive
                range_start = slot;
                range_end = slot;
            }
        }
        ranges.push((range_start, range_end + 1));

        // Apply each range in a single update
        for (start, end) in ranges {
            self.meta_raft
                .update_slots(start, end, group_id)
                .await
                .map_err(|e| {
                    AikvError::Internal(format!("Failed to assign slots {}-{}: {}", start, end - 1, e))
                })?;
        }

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER ADDSLOTSRANGE command.
    ///
    /// Maps to: `meta_raft.update_slots(start, end, group_id)` and `meta_raft.create_group`
    /// This is more efficient than ADDSLOTS for large ranges as it uses a single Raft proposal.
    ///
    /// # Arguments
    /// * `start` - Start slot (inclusive)
    /// * `end` - End slot (inclusive)
    /// * `target_node_id` - Node to assign slots to (0 means current node)
    pub async fn cluster_addslotsrange(
        &self,
        start: u16,
        end: u16,
        target_node_id: NodeId,
    ) -> Result<RespValue> {
        let meta = self.meta_raft.get_cluster_meta();
        
        // Determine the actual node_id to use
        let node_id = if target_node_id == 0 {
            self.node_id
        } else {
            // Verify the target node exists
            if !meta.nodes.contains_key(&target_node_id) {
                return Err(AikvError::Invalid(format!(
                    "Target node {:040x} not found in cluster",
                    target_node_id
                )));
            }
            target_node_id
        };

        // Find or create group for the target node
        let group_id = if let Some((gid, _)) = meta
            .groups
            .iter()
            .find(|(_, g)| g.replicas.contains(&node_id))
        {
            *gid
        } else {
            // Create a group for this node using its node_id as the group_id
            let group_id = node_id;
            self.meta_raft
                .create_group(group_id, vec![node_id])
                .await
                .map_err(|e| {
                    AikvError::Internal(format!("Failed to create group for node: {}", e))
                })?;
            // Set the node as the leader of this group
            self.meta_raft
                .update_group_leader(group_id, node_id)
                .await
                .map_err(|e| {
                    AikvError::Internal(format!("Failed to set group leader: {}", e))
                })?;
            group_id
        };

        // Validate range
        if start > end || end >= TOTAL_SLOTS {
            return Err(AikvError::Invalid(format!(
                "Invalid slot range: {}-{}",
                start, end
            )));
        }

        // Assign the entire range in a single update (end is exclusive in update_slots)
        self.meta_raft
            .update_slots(start, end + 1, group_id)
            .await
            .map_err(|e| {
                AikvError::Internal(format!("Failed to assign slots {}-{}: {}", start, end, e))
            })?;

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER DELSLOTS command.
    ///
    /// Maps to: `meta_raft.update_slots(start, end, 0)` where 0 means unassigned
    pub async fn cluster_delslots(&self, slots: Vec<u16>) -> Result<RespValue> {
        // Delete slots via MetaRaft - sync to all nodes via Raft consensus
        for slot in slots {
            if slot >= TOTAL_SLOTS {
                return Err(AikvError::Invalid(format!("Invalid slot: {}", slot)));
            }

            self.meta_raft
                .update_slots(slot, slot + 1, 0)
                .await
                .map_err(|e| {
                    AikvError::Internal(format!("Failed to delete slot {}: {}", slot, e))
                })?;
        }

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER REPLICATE command.
    ///
    /// Sets this node as a replica of the specified master node.
    /// Maps to: `meta_raft.update_group_members(group_id, new_replicas)`
    pub async fn cluster_replicate(&self, master_id: NodeId) -> Result<RespValue> {
        let meta = self.meta_raft.get_cluster_meta();

        // Find the group that the master belongs to
        // Groups are created with group_id == node_id, so we check:
        // 1. group_id matches master_id directly
        // 2. leader field matches master_id  
        // 3. replicas list contains master_id
        let group_id = meta
            .groups
            .iter()
            .find(|(gid, g)| **gid == master_id || g.leader == Some(master_id) || g.replicas.contains(&master_id))
            .map(|(gid, _)| *gid)
            .ok_or_else(|| {
                AikvError::Internal(format!(
                    "Master node {:040x} does not belong to any group",
                    master_id
                ))
            })?;

        // Get current group members
        let group = meta.groups.get(&group_id).ok_or_else(|| {
            AikvError::Internal(format!("Group {} not found", group_id))
        })?;

        // Add this node to the group's replicas if not already present
        let mut new_replicas = group.replicas.clone();
        if !new_replicas.contains(&self.node_id) {
            new_replicas.push(self.node_id);
            
            // Update group membership via MetaRaft
            self.meta_raft
                .update_group_members(group_id, new_replicas)
                .await
                .map_err(|e| {
                    AikvError::Internal(format!(
                        "Failed to add replica to group {}: {}",
                        group_id, e
                    ))
                })?;
        }

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER ADDREPLICATION command.
    ///
    /// Adds a replica to a master's group. This command is sent to the MetaRaft leader
    /// and specifies both the replica and master node IDs, allowing it to work even when
    /// the replica node doesn't have the latest ClusterMeta.
    ///
    /// # Arguments
    /// * `replica_id` - Node ID of the replica to add
    /// * `master_id` - Node ID of the master to replicate
    pub async fn cluster_add_replication(
        &self,
        replica_id: NodeId,
        master_id: NodeId,
    ) -> Result<RespValue> {
        let meta = self.meta_raft.get_cluster_meta();

        // Find the group that the master belongs to
        // Groups are created with group_id == node_id, so we check:
        // 1. group_id matches master_id directly
        // 2. leader field matches master_id  
        // 3. replicas list contains master_id
        let group_id = meta
            .groups
            .iter()
            .find(|(gid, g)| **gid == master_id || g.leader == Some(master_id) || g.replicas.contains(&master_id))
            .map(|(gid, _)| *gid)
            .ok_or_else(|| {
                AikvError::Internal(format!(
                    "Master node {:040x} does not belong to any group",
                    master_id
                ))
            })?;

        // Get current group members
        let group = meta.groups.get(&group_id).ok_or_else(|| {
            AikvError::Internal(format!("Group {} not found", group_id))
        })?;

        // Add the replica to the group's replicas if not already present
        let mut new_replicas = group.replicas.clone();
        if !new_replicas.contains(&replica_id) {
            new_replicas.push(replica_id);
            
            // Update group membership via MetaRaft
            self.meta_raft
                .update_group_members(group_id, new_replicas)
                .await
                .map_err(|e| {
                    AikvError::Internal(format!(
                        "Failed to add replica {:040x} to group {}: {}",
                        replica_id, group_id, e
                    ))
                })?;
        }

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER GETKEYSINSLOT command.
    ///
    /// Maps to: `state_machine.scan_slot_keys_sync(group, slot)`
    ///
    /// Note: This requires access to ShardedStateMachine which we'll need to add
    pub fn cluster_getkeysinslot(&self, slot: u16, _count: usize) -> Result<RespValue> {
        if slot >= TOTAL_SLOTS {
            return Err(AikvError::Invalid(format!("Invalid slot: {}", slot)));
        }

        // TODO: Implement using ShardedStateMachine.scan_slot_keys_sync()
        // For now, return empty array
        Ok(RespValue::Array(Some(vec![])))
    }

    /// Handle CLUSTER COUNTKEYSINSLOT command.
    pub fn cluster_countkeysinslot(&self, slot: u16) -> Result<RespValue> {
        if slot >= TOTAL_SLOTS {
            return Err(AikvError::Invalid(format!("Invalid slot: {}", slot)));
        }

        // TODO: Implement using ShardedStateMachine
        // For now, return 0
        Ok(RespValue::Integer(0))
    }

    /// Handle CLUSTER SHARDS command (Redis 7.0+).
    ///
    /// Returns the mapping of cluster slots to shards in Redis 7.0+ format.
    /// This command is used by modern Redis clients (like RedisInsight) to detect cluster mode.
    ///
    /// Maps to: `meta_raft.get_cluster_meta()`
    pub fn cluster_shards(&self) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();
        let mut shards = Vec::new();

        // Build shard info for each group that has slots
        for (group_id, group_meta) in &meta.groups {
            // Find slots assigned to this group
            let mut slot_ranges: Vec<(u16, u16)> = Vec::new();
            let mut start: Option<u16> = None;
            let mut end: Option<u16> = None;

            for (slot_idx, &assigned_group) in meta.slots.iter().enumerate() {
                if assigned_group == *group_id {
                    if start.is_none() {
                        start = Some(slot_idx as u16);
                    }
                    end = Some(slot_idx as u16);
                } else if start.is_some() {
                    slot_ranges.push((start.unwrap(), end.unwrap()));
                    start = None;
                    end = None;
                }
            }
            if let Some(s) = start {
                slot_ranges.push((s, end.unwrap()));
            }

            // Skip groups without slots
            if slot_ranges.is_empty() {
                continue;
            }

            // Build slots array for this shard
            let mut slots_array = Vec::new();
            for (range_start, range_end) in &slot_ranges {
                slots_array.push(RespValue::Array(Some(vec![
                    RespValue::Integer(*range_start as i64),
                    RespValue::Integer(*range_end as i64),
                ])));
            }

            // Build nodes array for this shard
            let mut nodes_array = Vec::new();

            // Add master node first (leader)
            if let Some(leader_id) = group_meta.leader {
                if let Some(node_info) = meta.nodes.get(&leader_id) {
                    let data_addr = Self::extract_data_address(&node_info.addr);
                    let (ip, port) = Self::parse_addr(&data_addr);
                    let health = match node_info.status {
                        NodeStatus::Online => "online",
                        NodeStatus::Offline => "offline",
                        _ => "loading",
                    };

                    nodes_array.push(RespValue::Array(Some(vec![
                        RespValue::BulkString(Some(Bytes::from("id"))),
                        RespValue::BulkString(Some(Bytes::from(format!("{:040x}", leader_id)))),
                        RespValue::BulkString(Some(Bytes::from("port"))),
                        RespValue::Integer(port),
                        RespValue::BulkString(Some(Bytes::from("ip"))),
                        RespValue::BulkString(Some(Bytes::from(ip.clone()))),
                        RespValue::BulkString(Some(Bytes::from("endpoint"))),
                        RespValue::BulkString(Some(Bytes::from(ip.clone()))),
                        RespValue::BulkString(Some(Bytes::from("role"))),
                        RespValue::BulkString(Some(Bytes::from("master"))),
                        RespValue::BulkString(Some(Bytes::from("replication-offset"))),
                        RespValue::Integer(0),
                        RespValue::BulkString(Some(Bytes::from("health"))),
                        RespValue::BulkString(Some(Bytes::from(health))),
                    ])));
                }
            }

            // Add replica nodes
            for &replica_id in &group_meta.replicas {
                // Skip leader (already added as master)
                if Some(replica_id) == group_meta.leader {
                    continue;
                }
                if let Some(node_info) = meta.nodes.get(&replica_id) {
                    let data_addr = Self::extract_data_address(&node_info.addr);
                    let (ip, port) = Self::parse_addr(&data_addr);
                    let health = match node_info.status {
                        NodeStatus::Online => "online",
                        NodeStatus::Offline => "offline",
                        _ => "loading",
                    };

                    nodes_array.push(RespValue::Array(Some(vec![
                        RespValue::BulkString(Some(Bytes::from("id"))),
                        RespValue::BulkString(Some(Bytes::from(format!("{:040x}", replica_id)))),
                        RespValue::BulkString(Some(Bytes::from("port"))),
                        RespValue::Integer(port),
                        RespValue::BulkString(Some(Bytes::from("ip"))),
                        RespValue::BulkString(Some(Bytes::from(ip.clone()))),
                        RespValue::BulkString(Some(Bytes::from("endpoint"))),
                        RespValue::BulkString(Some(Bytes::from(ip.clone()))),
                        RespValue::BulkString(Some(Bytes::from("role"))),
                        RespValue::BulkString(Some(Bytes::from("replica"))),
                        RespValue::BulkString(Some(Bytes::from("replication-offset"))),
                        RespValue::Integer(0),
                        RespValue::BulkString(Some(Bytes::from("health"))),
                        RespValue::BulkString(Some(Bytes::from(health))),
                    ])));
                }
            }

            // Build shard entry
            shards.push(RespValue::Array(Some(vec![
                RespValue::BulkString(Some(Bytes::from("slots"))),
                RespValue::Array(Some(slots_array)),
                RespValue::BulkString(Some(Bytes::from("nodes"))),
                RespValue::Array(Some(nodes_array)),
            ])));
        }

        Ok(RespValue::Array(Some(shards)))
    }

    /// Handle CLUSTER MYSHARDID command.
    ///
    /// Returns the shard ID that this node belongs to.
    pub fn cluster_myshardid(&self) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();

        // Find which group this node belongs to
        for (group_id, group_meta) in &meta.groups {
            if group_meta.leader == Some(self.node_id) || group_meta.replicas.contains(&self.node_id) {
                return Ok(RespValue::BulkString(Some(Bytes::from(format!("{:040x}", group_id)))));
            }
        }

        // Node not assigned to any shard yet - return node_id as shard id
        Ok(RespValue::BulkString(Some(Bytes::from(format!("{:040x}", self.node_id)))))
    }

    /// Handle CLUSTER SET-CONFIG-EPOCH command.
    ///
    /// Sets the configuration epoch for this node.
    pub fn cluster_set_config_epoch(&self, _epoch: u64) -> Result<RespValue> {
        // In our implementation, config epoch is managed by MetaRaft
        // This command is used during cluster creation to set initial epochs
        // For now, just return OK as the epoch is managed internally
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER REPLICAS command.
    ///
    /// Returns a list of replica nodes for the given master node.
    pub fn cluster_replicas(&self, master_id: NodeId) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();
        let mut replicas = Vec::new();

        // Find the group where this node is leader
        for (_group_id, group_meta) in &meta.groups {
            if group_meta.leader == Some(master_id) {
                // Found the group, list all replicas (excluding the leader)
                for &replica_id in &group_meta.replicas {
                    if replica_id == master_id {
                        continue;
                    }
                    if let Some(node_info) = meta.nodes.get(&replica_id) {
                        // For Redis cluster compatibility, report nodes as "connected"
                        // TODO: Implement proper health checking
                        let status = match node_info.status {
                            NodeStatus::Online => "connected",
                            NodeStatus::Offline => "disconnected",
                            _ => "connected",
                        };
                        let data_addr = Self::extract_data_address(&node_info.addr);
                        let cluster_port = Self::extract_cluster_port_from_data_port(&data_addr);
                        
                        // Format: <id> <ip:port@cport> slave <master-id> <ping-sent> <pong-recv> <config-epoch> <link-state>
                        let line = format!(
                            "{:040x} {}@{} slave {:040x} 0 0 {} {}",
                            replica_id,
                            data_addr,
                            cluster_port,
                            master_id,
                            meta.config_version,
                            status
                        );
                        replicas.push(RespValue::BulkString(Some(Bytes::from(line))));
                    }
                }
                break;
            }
        }

        // Also check if node_id might be a hex string
        if replicas.is_empty() {
            // The master might not be a leader of any group (could be a replica itself)
            return Err(AikvError::Invalid(format!(
                "Node {:040x} is not a master or does not exist",
                master_id
            )));
        }

        Ok(RespValue::Array(Some(replicas)))
    }

    /// Handle CLUSTER SAVECONFIG command.
    ///
    /// Forces the node to save cluster configuration to disk.
    pub fn cluster_saveconfig(&self) -> Result<RespValue> {
        // In our implementation, cluster config is persisted via Raft log
        // This is essentially a no-op since Raft handles persistence
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER FAILOVER command.
    ///
    /// Triggers a manual failover (replica becomes master).
    pub async fn cluster_failover(&self, mode: FailoverMode) -> Result<RespValue> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();

        // Find which group this node is a replica of
        let group_id = meta
            .groups
            .iter()
            .find(|(_, g)| g.replicas.contains(&self.node_id) && g.leader != Some(self.node_id))
            .map(|(gid, _)| *gid);

        let group_id = match group_id {
            Some(id) => id,
            None => {
                return Err(AikvError::Invalid(
                    "This node is not a replica or already a master".to_string(),
                ));
            }
        };

        // Perform failover based on mode
        match mode {
            FailoverMode::Default | FailoverMode::Force => {
                // Update group leader to this node
                self.meta_raft
                    .update_group_leader(group_id, self.node_id)
                    .await
                    .map_err(|e| {
                        AikvError::Internal(format!("Failed to perform failover: {}", e))
                    })?;
            }
            FailoverMode::Takeover => {
                // Force takeover without coordination
                self.meta_raft
                    .update_group_leader(group_id, self.node_id)
                    .await
                    .map_err(|e| {
                        AikvError::Internal(format!("Failed to perform takeover: {}", e))
                    })?;
            }
        }

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER RESET command.
    ///
    /// Resets the cluster node (SOFT or HARD).
    pub async fn cluster_reset(&self, hard: bool) -> Result<RespValue> {
        if hard {
            // HARD reset: clear all data and cluster state
            // This would require clearing the storage and MetaRaft state
            // For now, just clear slot assignments for this node
            let meta = self.meta_raft.get_cluster_meta();
            
            for (group_id, group_meta) in &meta.groups {
                if group_meta.leader == Some(self.node_id) {
                    // Clear slots for groups where this node is leader
                    for (slot_idx, &assigned_group) in meta.slots.iter().enumerate() {
                        if assigned_group == *group_id {
                            self.meta_raft
                                .update_slots(slot_idx as u16, (slot_idx + 1) as u16, 0)
                                .await
                                .map_err(|e| {
                                    AikvError::Internal(format!("Failed to clear slot: {}", e))
                                })?;
                        }
                    }
                }
            }
        }
        // SOFT reset: just return OK (minimal reset)
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER COUNT-FAILURE-REPORTS command.
    ///
    /// Returns the number of failure reports for a given node.
    pub fn cluster_count_failure_reports(&self, _node_id: NodeId) -> Result<RespValue> {
        // In our implementation, failure detection is handled by Raft
        // Return 0 as we don't track failure reports separately
        Ok(RespValue::Integer(0))
    }

    /// Handle CLUSTER BUMPEPOCH command.
    ///
    /// Advances the cluster config epoch.
    pub fn cluster_bumpepoch(&self) -> Result<RespValue> {
        // In our implementation, epochs are managed by MetaRaft
        // Just return the current epoch
        let meta = self.meta_raft.get_cluster_meta();
        Ok(RespValue::BulkString(Some(Bytes::from(format!("BUMPED {}", meta.config_version)))))
    }

    /// Handle CLUSTER FLUSHSLOTS command.
    ///
    /// Deletes all slots from this node.
    pub async fn cluster_flushslots(&self) -> Result<RespValue> {
        let meta = self.meta_raft.get_cluster_meta();
        
        // Find groups where this node is leader and clear their slots
        for (group_id, group_meta) in &meta.groups {
            if group_meta.leader == Some(self.node_id) {
                for (slot_idx, &assigned_group) in meta.slots.iter().enumerate() {
                    if assigned_group == *group_id {
                        self.meta_raft
                            .update_slots(slot_idx as u16, (slot_idx + 1) as u16, 0)
                            .await
                            .map_err(|e| {
                                AikvError::Internal(format!("Failed to flush slot: {}", e))
                            })?;
                    }
                }
            }
        }
        
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER DELSLOTSRANGE command.
    ///
    /// Deletes a range of slots from this node.
    pub async fn cluster_delslotsrange(&self, start: u16, end: u16) -> Result<RespValue> {
        if start > end || end >= TOTAL_SLOTS {
            return Err(AikvError::Invalid(format!(
                "Invalid slot range: {}-{}",
                start, end
            )));
        }

        // Clear the entire range (end is exclusive)
        self.meta_raft
            .update_slots(start, end + 1, 0)
            .await
            .map_err(|e| {
                AikvError::Internal(format!("Failed to delete slots {}-{}: {}", start, end, e))
            })?;

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle ASKING command.
    ///
    /// Signals that the next command is for a key being migrated.
    /// This is called on the target node after receiving -ASK redirect.
    pub fn asking(&self) -> Result<RespValue> {
        // In a full implementation, this would set a flag on the connection
        // to allow the next command to operate on an importing slot
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Generate a unique node ID.
    /// This is a utility function for server initialization.
    pub fn generate_node_id() -> NodeId {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Use a combination of timestamp and random number
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // Mix with random bits
        let random: u64 = rand::random();
        timestamp ^ random
    }

    /// Generate a consistent node ID from a peer address.
    /// This ensures all nodes agree on each other's IDs in multi-master setup.
    pub fn generate_node_id_from_addr(addr: &str) -> NodeId {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        addr.hash(&mut hasher);
        hasher.finish()
    }

    /// Execute a CLUSTER subcommand.
    ///
    /// This is the main dispatcher for CLUSTER commands.
    pub fn execute(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("CLUSTER".to_string()));
        }

        let subcommand = String::from_utf8_lossy(&args[0]).to_uppercase();
        match subcommand.as_str() {
            "INFO" => self.cluster_info(),
            "NODES" => self.cluster_nodes(),
            "SLOTS" => self.cluster_slots(),
            "MYID" => self.cluster_myid(),
            "KEYSLOT" => {
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount("CLUSTER KEYSLOT".to_string()));
                }
                self.cluster_keyslot(&args[1])
            }
            "GETKEYSINSLOT" => {
                if args.len() != 3 {
                    return Err(AikvError::WrongArgCount(
                        "CLUSTER GETKEYSINSLOT".to_string(),
                    ));
                }
                let slot = String::from_utf8_lossy(&args[1])
                    .parse::<u16>()
                    .map_err(|_| AikvError::Invalid("Invalid slot".to_string()))?;
                let count = String::from_utf8_lossy(&args[2])
                    .parse::<usize>()
                    .map_err(|_| AikvError::Invalid("Invalid count".to_string()))?;
                self.cluster_getkeysinslot(slot, count)
            }
            "COUNTKEYSINSLOT" => {
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount(
                        "CLUSTER COUNTKEYSINSLOT".to_string(),
                    ));
                }
                let slot = String::from_utf8_lossy(&args[1])
                    .parse::<u16>()
                    .map_err(|_| AikvError::Invalid("Invalid slot".to_string()))?;
                self.cluster_countkeysinslot(slot)
            }
            "SHARDS" => self.cluster_shards(),
            "MYSHARDID" => self.cluster_myshardid(),
            "SET-CONFIG-EPOCH" => {
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount(
                        "CLUSTER SET-CONFIG-EPOCH".to_string(),
                    ));
                }
                let epoch = String::from_utf8_lossy(&args[1])
                    .parse::<u64>()
                    .map_err(|_| AikvError::Invalid("Invalid epoch".to_string()))?;
                self.cluster_set_config_epoch(epoch)
            }
            "REPLICAS" => {
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount("CLUSTER REPLICAS".to_string()));
                }
                let node_id_str = String::from_utf8_lossy(&args[1]);
                let node_id = u64::from_str_radix(&node_id_str, 16)
                    .or_else(|_| node_id_str.parse::<u64>())
                    .map_err(|_| AikvError::Invalid("Invalid node ID".to_string()))?;
                self.cluster_replicas(node_id)
            }
            "SLAVES" => {
                // Deprecated alias for REPLICAS
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount("CLUSTER SLAVES".to_string()));
                }
                let node_id_str = String::from_utf8_lossy(&args[1]);
                let node_id = u64::from_str_radix(&node_id_str, 16)
                    .or_else(|_| node_id_str.parse::<u64>())
                    .map_err(|_| AikvError::Invalid("Invalid node ID".to_string()))?;
                self.cluster_replicas(node_id)
            }
            "SAVECONFIG" => self.cluster_saveconfig(),
            "BUMPEPOCH" => self.cluster_bumpepoch(),
            "COUNT-FAILURE-REPORTS" => {
                if args.len() != 2 {
                    return Err(AikvError::WrongArgCount(
                        "CLUSTER COUNT-FAILURE-REPORTS".to_string(),
                    ));
                }
                let node_id_str = String::from_utf8_lossy(&args[1]);
                let node_id = u64::from_str_radix(&node_id_str, 16)
                    .or_else(|_| node_id_str.parse::<u64>())
                    .map_err(|_| AikvError::Invalid("Invalid node ID".to_string()))?;
                self.cluster_count_failure_reports(node_id)
            }
            _ => Err(AikvError::InvalidCommand(format!(
                "Unknown CLUSTER subcommand: {}",
                subcommand
            ))),
        }
    }

    /// Handle READONLY command.
    ///
    /// Sets connection to read-only mode for replica reads.
    pub fn readonly(&self) -> Result<RespValue> {
        // For now, just return OK
        // In a full implementation, this would set a flag on the connection
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle READWRITE command.
    ///
    /// Sets connection back to read-write mode (default).
    pub fn readwrite(&self) -> Result<RespValue> {
        // For now, just return OK
        // In a full implementation, this would clear the read-only flag
        Ok(RespValue::SimpleString("OK".to_string()))
    }
    /// Handle CLUSTER METARAFT ADDLEARNER command.
    ///
    /// Adds a node as a learner to the MetaRaft cluster. This is the first step
    /// in adding a new voting member to the MetaRaft cluster.
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to add
    /// * `addr` - Raft address of the node (ip:port for gRPC)
    ///
    /// # Returns
    ///
    /// `OK` on success
    ///
    /// # Example
    ///
    /// ```text
    /// CLUSTER METARAFT ADDLEARNER 2 127.0.0.1:50052
    /// ```
    pub async fn cluster_metaraft_addlearner(
        &self,
        node_id: NodeId,
        addr: String,
    ) -> Result<RespValue> {
        // CRITICAL: Register node address in network factory BEFORE adding learner
        // This enables the Leader to connect to the new node for log replication
        // The address must include http:// scheme for gRPC client
        let grpc_addr = if addr.starts_with("http://") || addr.starts_with("https://") {
            addr.clone()
        } else {
            format!("http://{}", addr)
        };

        // Register address in BOTH factories:
        // 1. MultiRaft factory (for data group replication)
        self.multi_raft.add_node_address(node_id, grpc_addr.clone());
        // 2. MetaRaft factory (for metadata replication) - new in AiDb v0.6.1
        self.meta_raft.add_node_address(node_id, grpc_addr.clone());

        // BasicNode.addr MUST also have http:// scheme for Raft replication
        let node = BasicNode { addr: grpc_addr };

        self.meta_raft
            .add_learner(node_id, node)
            .await
            .map_err(|e| {
                AikvError::Internal(format!("Failed to add MetaRaft learner: {}", e))
            })?;

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER METARAFT PROMOTE command.
    ///
    /// Promotes one or more learners to voting members in the MetaRaft cluster.
    /// The provided node IDs will be added to the existing voter set.
    /// Existing voters are automatically retained.
    ///
    /// # Arguments
    ///
    /// * `new_voters` - List of learner node IDs to promote to voters
    ///
    /// # Returns
    ///
    /// `OK` on success
    ///
    /// # Example
    ///
    /// ```text
    /// CLUSTER METARAFT PROMOTE 2 3
    /// ```
    pub async fn cluster_metaraft_promote(&self, new_voters: Vec<NodeId>) -> Result<RespValue> {
        use std::collections::BTreeSet;
        
        // Get current voters from metrics
        let raft = self.meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();
        let current_voters: BTreeSet<NodeId> = metrics.membership_config.membership().voter_ids().collect();
        
        // Merge current voters with new voters to promote
        let mut all_voters: BTreeSet<NodeId> = current_voters;
        for voter in new_voters {
            all_voters.insert(voter);
        }
        
        info!("Promoting to voter set: {:?}", all_voters);

        self.meta_raft
            .change_membership(all_voters, true)
            .await
            .map_err(|e| AikvError::Internal(format!("Failed to promote voters: {}", e)))?;

        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER METARAFT MEMBERS command.
    ///
    /// Returns information about MetaRaft cluster members, including voters and learners.
    ///
    /// # Returns
    ///
    /// Array of member information
    ///
    /// # Example
    ///
    /// ```text
    /// CLUSTER METARAFT MEMBERS
    /// ```
    pub async fn cluster_metaraft_members(&self) -> Result<RespValue> {
        // Get Raft metrics to determine current voters and learners
        let raft = self.meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();

        let mut members = Vec::new();

        // Add voters
        let membership = metrics.membership_config.membership();
        for node_id in membership.voter_ids() {
            members.push(RespValue::Array(Some(vec![
                RespValue::BulkString(Some(Bytes::from(format!("{}", node_id)))),
                RespValue::SimpleString("voter".to_string()),
            ])));
        }

        // Add learners
        for node_id in membership.learner_ids() {
            members.push(RespValue::Array(Some(vec![
                RespValue::BulkString(Some(Bytes::from(format!("{}", node_id)))),
                RespValue::SimpleString("learner".to_string()),
            ])));
        }

        Ok(RespValue::Array(Some(members)))
    }

    /// Return raw raft metrics and membership state for diagnostics
    pub async fn cluster_metaraft_status(&self) -> Result<RespValue> {
        let raft = self.meta_raft.raft();
        let metrics = raft.metrics().borrow().clone();

        // Also include cluster meta snapshot
        let cluster_meta = self.meta_raft.get_cluster_meta();

        let mut info = String::new();
        info.push_str(&format!("metrics: {:?}\n", metrics));
        info.push_str(&format!("cluster_meta: {:?}\n", cluster_meta));

        Ok(RespValue::BulkString(Some(Bytes::from(info))))
    }
}

#[cfg(feature = "cluster")]
impl ClusterCommands {
    /// Create error response for -MOVED redirection
    pub fn moved_error(slot: u16, addr: &str) -> AikvError {
        AikvError::Moved(slot, addr.to_string())
    }

    /// Create error response for -ASK redirection
    pub fn ask_error(slot: u16, addr: &str) -> AikvError {
        AikvError::Ask(slot, addr.to_string())
    }

    /// Check if a key should be handled by this node.
    ///
    /// Returns `Ok(())` if the key belongs to this node, or an error with
    /// MOVED/ASK redirection information if the key should be handled elsewhere.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to check
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Key belongs to this node
    /// * `Err(AikvError::Moved(slot, addr))` - Key belongs to another node
    /// * `Err(AikvError::Ask(slot, addr))` - Key is being migrated
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Before executing a command, check if the key belongs to this node
    /// cluster_commands.check_key_slot(b"user:1000")?;
    /// // If no error, proceed with the command
    /// ```
    pub fn check_key_slot(&self, key: &[u8]) -> Result<()> {
        let slot = Router::key_to_slot(key);
        self.check_slot_ownership(slot)
    }

    /// Check if a slot should be handled by this node.
    ///
    /// Returns `Ok(())` if the slot belongs to this node, or an error with
    /// MOVED/ASK redirection information if the slot should be handled elsewhere.
    ///
    /// # Arguments
    ///
    /// * `slot` - The slot number to check (0-16383)
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Slot belongs to this node
    /// * `Err(AikvError::Moved(slot, addr))` - Slot belongs to another node
    /// * `Err(AikvError::Ask(slot, addr))` - Slot is being migrated
    pub fn check_slot_ownership(&self, slot: u16) -> Result<()> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();

        // Check if slot is assigned to any group
        if slot as usize >= meta.slots.len() {
            return Err(AikvError::Invalid(format!("Invalid slot: {}", slot)));
        }

        let assigned_group = meta.slots[slot as usize];

        // Slot not assigned to any group
        if assigned_group == 0 {
            return Err(AikvError::Internal(format!(
                "CLUSTERDOWN Hash slot {} not served",
                slot
            )));
        }

        // Check if this node owns the slot (is the leader of the assigned group)
        if let Some(group_meta) = meta.groups.get(&assigned_group) {
            // Check if this node is the leader of the group
            if group_meta.leader == Some(self.node_id) {
                // This node owns the slot
                return Ok(());
            }

            // Check if this node is a replica (can handle READONLY requests)
            // For now, we always redirect to the leader for write operations
            if group_meta.replicas.contains(&self.node_id) {
                // This node is a replica, redirect to the leader
                if let Some(leader_id) = group_meta.leader {
                    if let Some(leader_info) = meta.nodes.get(&leader_id) {
                        let data_addr = Self::extract_data_address(&leader_info.addr);
                        return Err(Self::moved_error(slot, &data_addr));
                    }
                }
            }

            // Slot belongs to another node, find the leader and redirect
            if let Some(leader_id) = group_meta.leader {
                if let Some(leader_info) = meta.nodes.get(&leader_id) {
                    let data_addr = Self::extract_data_address(&leader_info.addr);
                    debug!(
                        slot = slot,
                        target = %data_addr,
                        "Redirecting key to owner node"
                    );
                    return Err(Self::moved_error(slot, &data_addr));
                }
            }
        }

        // Fallback: slot is assigned but group info is missing
        Err(AikvError::Internal(format!(
            "CLUSTERDOWN Hash slot {} not served (group {} not found)",
            slot, assigned_group
        )))
    }

    /// Check if multiple keys all belong to this node.
    ///
    /// For multi-key commands (like MGET, MSET), all keys must belong to the same
    /// slot, or the command must be rejected. This method checks if all keys
    /// belong to this node.
    ///
    /// # Arguments
    ///
    /// * `keys` - The keys to check
    ///
    /// # Returns
    ///
    /// * `Ok(())` - All keys belong to this node
    /// * `Err(AikvError::Moved(slot, addr))` - Keys belong to another node
    /// * `Err(AikvError::CrossSlot)` - Keys span multiple slots (not supported)
    pub fn check_keys_slot(&self, keys: &[&[u8]]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }

        // Calculate slot for first key
        let first_slot = Router::key_to_slot(keys[0]);

        // Verify all keys are in the same slot
        for key in &keys[1..] {
            let slot = Router::key_to_slot(key);
            if slot != first_slot {
                return Err(AikvError::CrossSlot);
            }
        }

        // Check if the slot belongs to this node
        self.check_slot_ownership(first_slot)
    }

    /// Get the slot number for a key.
    ///
    /// This is a convenience wrapper around `Router::key_to_slot()`.
    pub fn get_key_slot(key: &[u8]) -> u16 {
        Router::key_to_slot(key)
    }

    /// Check if cluster is fully operational (all slots assigned and served).
    ///
    /// Returns `Ok(())` if the cluster is operational, or an error describing
    /// what's wrong.
    pub fn check_cluster_state(&self) -> Result<()> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();

        // Check if all slots are assigned
        let assigned_slots = meta.slots.iter().filter(|&&g| g > 0).count();
        if assigned_slots != TOTAL_SLOTS as usize {
            return Err(AikvError::Internal(format!(
                "CLUSTERDOWN The cluster is down. Only {} of {} slots are assigned",
                assigned_slots, TOTAL_SLOTS
            )));
        }

        // Check if all groups have leaders
        for (group_id, group_meta) in &meta.groups {
            // Check if this group owns any slots
            let owns_slots = meta.slots.iter().any(|&s| s == *group_id);
            if owns_slots && group_meta.leader.is_none() {
                return Err(AikvError::Internal(format!(
                    "CLUSTERDOWN The cluster is down. Group {} has no leader",
                    group_id
                )));
            }
        }

        Ok(())
    }

    /// Get the node address that owns a specific slot.
    ///
    /// Returns `Some((node_id, addr))` if the slot is assigned, `None` otherwise.
    pub fn get_slot_owner(&self, slot: u16) -> Option<(NodeId, String)> {
        let meta: ClusterMeta = self.meta_raft.get_cluster_meta();

        if slot as usize >= meta.slots.len() {
            return None;
        }

        let assigned_group = meta.slots[slot as usize];
        if assigned_group == 0 {
            return None;
        }

        if let Some(group_meta) = meta.groups.get(&assigned_group) {
            if let Some(leader_id) = group_meta.leader {
                if let Some(leader_info) = meta.nodes.get(&leader_id) {
                    let data_addr = Self::extract_data_address(&leader_info.addr);
                    return Some((leader_id, data_addr));
                }
            }
        }

        None
    }

    /// Get this node's ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
}

/// Placeholder struct for when cluster feature is disabled
#[cfg(not(feature = "cluster"))]
pub struct ClusterCommands;

#[cfg(not(feature = "cluster"))]
impl ClusterCommands {
    pub fn cluster_info(&self) -> Result<RespValue> {
        Err(AikvError::ClusterDisabled)
    }
}
