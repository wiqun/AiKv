//! Redis Cluster commands implementation using AiDb Multi-Raft API.
//!
//! This module provides a thin glue layer that maps Redis Cluster protocol
//! commands to AiDb's Multi-Raft API as documented in MULTI_RAFT_API_REFERENCE.md.
//!
//! Key principle: Minimal code - only Redis protocol format conversion.
//! All cluster logic is delegated to AiDb's MetaRaftNode, MultiRaftNode, Router, etc.

use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use bytes::Bytes;
use std::sync::Arc;

#[cfg(feature = "cluster")]
use aidb::cluster::{
    ClusterMeta, GroupId, MetaNodeInfo, MetaRaftNode, MigrationManager, MultiRaftNode, NodeId,
    NodeStatus, Router, ShardedStateMachine, SLOT_COUNT,
};

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
    multi_raft: Arc<MultiRaftNode>,
    
    /// Router for key-to-slot-to-group mapping
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
        let online_nodes = meta.nodes.values()
            .filter(|n| matches!(n.status, NodeStatus::Online))
            .count();
        
        // Determine cluster state
        let cluster_state = if assigned_slots == TOTAL_SLOTS as usize && online_nodes > 0 {
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
            let status = match node_info.status {
                NodeStatus::Online => "connected",
                NodeStatus::Offline => "disconnected",
                _ => "handshake",
            };
            
            // Find slots for this node by checking which groups it belongs to
            let mut slot_ranges = Vec::new();
            for (group_id, group_meta) in &meta.groups {
                if group_meta.replicas.contains(node_id) {
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
            
            // Check if this node is a master or replica
            let role = if meta.groups.values().any(|g| g.leader == Some(*node_id)) {
                "master"
            } else {
                "slave"
            };
            
            // Format: <id> <ip:port@cport> <flags> <master> <ping-sent> <pong-recv> <config-epoch> <link-state> <slot> <slot> ...
            let myself_flag = if *node_id == self.node_id { "myself," } else { "" };
            let node_line = format!(
                "{:040x} {}@{} {}{} - 0 0 {} {} {}",
                node_id,
                node_info.addr,
                Self::extract_cluster_port(&node_info.addr),
                myself_flag,
                role,
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
                        slots_info.push(self.format_slot_range(&meta, range_start, (slot - 1) as u16, group));
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
                    slots_info.push(self.format_slot_range(&meta, range_start, (slot - 1) as u16, cg));
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
    fn format_slot_range(&self, meta: &ClusterMeta, start: u16, end: u16, group_id: GroupId) -> RespValue {
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
        
        RespValue::Array(elements)
    }

    /// Format node info for CLUSTER SLOTS response
    fn format_node_info(&self, node_id: NodeId, node_info: &MetaNodeInfo) -> RespValue {
        let (ip, port) = self.parse_addr(&node_info.addr);
        RespValue::Array(Some(vec![
            RespValue::BulkString(Some(Bytes::from(ip))),
            RespValue::Integer(port),
            RespValue::BulkString(Some(Bytes::from(format!("{:040x}", node_id)))),
        ]))
    }

    /// Parse address into (ip, port)
    fn parse_addr(&self, addr: &str) -> (String, i64) {
        if let Some((ip, port_str)) = addr.rsplit_once(':') {
            let port = port_str.parse::<i64>().unwrap_or(6379);
            (ip.to_string(), port)
        } else {
            (addr.to_string(), 6379)
        }
    }

    /// Extract cluster port from address string
    fn extract_cluster_port(addr: &str) -> u16 {
        if let Some(port_str) = addr.split(':').nth_back(0) {
            port_str.parse::<u16>().unwrap_or(6379) + 10000
        } else {
            16379
        }
    }

    /// Handle CLUSTER MYID command.
    ///
    /// Maps to: node_id
    pub fn cluster_myid(&self) -> Result<RespValue> {
        Ok(RespValue::BulkString(Some(Bytes::from(format!("{:040x}", self.node_id)))))
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
    pub async fn cluster_meet(&self, ip: String, port: u16, node_id_opt: Option<NodeId>) -> Result<RespValue> {
        let addr = format!("{}:{}", ip, port);
        
        // Generate node ID if not provided
        let node_id = node_id_opt.unwrap_or_else(|| {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            addr.hash(&mut hasher);
            hasher.finish()
        });
        
        // Add node via MetaRaft - this will sync to all nodes via Raft consensus
        self.meta_raft.add_node(node_id, addr).await
            .map_err(|e| AikvError::Internal(format!("Failed to add node: {}", e)))?;
        
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER FORGET command.
    ///
    /// Maps to: `meta_raft.remove_node(node_id)`
    pub async fn cluster_forget(&self, node_id: NodeId) -> Result<RespValue> {
        // Remove node via MetaRaft - this will sync to all nodes via Raft consensus
        self.meta_raft.remove_node(node_id).await
            .map_err(|e| AikvError::Internal(format!("Failed to remove node: {}", e)))?;
        
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER ADDSLOTS command.
    ///
    /// Maps to: `meta_raft.update_slots(start, end, group_id)`
    ///
    /// Note: For Redis compatibility, we need to assign slots to a group.
    /// The group_id is determined by finding which group this node belongs to.
    pub async fn cluster_addslots(&self, slots: Vec<u16>) -> Result<RespValue> {
        let meta = self.meta_raft.get_cluster_meta();
        
        // Find the group that this node belongs to
        let group_id = meta.groups.iter()
            .find(|(_, g)| g.replicas.contains(&self.node_id))
            .map(|(gid, _)| *gid)
            .ok_or_else(|| AikvError::Internal("Node does not belong to any group".to_string()))?;
        
        // Assign each slot to this node's group - sync via Raft consensus
        for slot in slots {
            if slot >= TOTAL_SLOTS {
                return Err(AikvError::Invalid(format!("Invalid slot: {}", slot)));
            }
            
            self.meta_raft.update_slots(slot, slot + 1, group_id).await
                .map_err(|e| AikvError::Internal(format!("Failed to assign slot {}: {}", slot, e)))?;
        }
        
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
            
            self.meta_raft.update_slots(slot, slot + 1, 0).await
                .map_err(|e| AikvError::Internal(format!("Failed to delete slot {}: {}", slot, e)))?;
        }
        
        Ok(RespValue::SimpleString("OK".to_string()))
    }

    /// Handle CLUSTER GETKEYSINSLOT command.
    ///
    /// Maps to: `state_machine.scan_slot_keys_sync(group, slot)`
    ///
    /// Note: This requires access to ShardedStateMachine which we'll need to add
    pub fn cluster_getkeysinslot(&self, slot: u16, count: usize) -> Result<RespValue> {
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
