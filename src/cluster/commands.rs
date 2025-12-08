//! Cluster commands implementation.
//!
//! This module implements Redis Cluster protocol commands,
//! mapping them to AiDb's MultiRaft API.
//!
//! # Stage C: Slot Migration
//!
//! This module includes slot migration features:
//! - `CLUSTER GETKEYSINSLOT` - Get keys belonging to a specific slot
//! - Migration state query (`CLUSTER SETSLOT ... IMPORTING/MIGRATING`)
//! - `-ASK` redirection logic
//! - Migration manager integration
//! - `MIGRATE` command

use crate::cluster::router::SlotRouter;
use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[cfg(feature = "cluster")]
use aidb::cluster::MultiRaftNode;

#[cfg(feature = "cluster")]
use crate::cluster::metaraft::MetaRaftClient;

/// Total number of slots in Redis Cluster (16384)
const TOTAL_SLOTS: u16 = 16384;
/// Total slots as usize for vector indexing
const TOTAL_SLOTS_USIZE: usize = 16384;

/// Slot state enumeration for CLUSTER SETSLOT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// Slot is in normal state, assigned to a node
    Normal,
    /// Slot is being migrated out from this node
    Migrating,
    /// Slot is being imported to this node
    Importing,
}

/// Redirection type for cluster routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectType {
    /// -MOVED redirect: key belongs to another node
    Moved,
    /// -ASK redirect: key is being migrated to another node
    Ask,
}

/// Failover mode for CLUSTER FAILOVER command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverMode {
    /// Default failover - wait for master agreement
    Default,
    /// Force failover without master agreement
    Force,
    /// Takeover - force failover even if master is unreachable
    Takeover,
}

/// Node information for cluster management.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Node ID (40-character hex string internally stored as u64)
    pub id: u64,
    /// Node address (ip:port)
    pub addr: String,
    /// Cluster bus port (typically data port + 10000)
    pub cluster_port: u16,
    /// Whether this node is marked as a master
    pub is_master: bool,
    /// Whether this node is connected
    pub is_connected: bool,
    /// Master node ID (if this node is a replica)
    pub master_id: Option<u64>,
    /// Replica node IDs (if this node is a master)
    pub replica_ids: Vec<u64>,
}

impl NodeInfo {
    /// Create a new NodeInfo with the given id and address.
    pub fn new(id: u64, addr: String) -> Self {
        // Parse address to extract port and calculate cluster port
        let cluster_port = if let Some(port_str) = addr.split(':').next_back() {
            port_str.parse::<u16>().unwrap_or(6379) + 10000
        } else {
            16379
        };
        Self {
            id,
            addr,
            cluster_port,
            is_master: true,
            is_connected: true,
            master_id: None,
            replica_ids: Vec::new(),
        }
    }

    /// Create a new NodeInfo for a replica node.
    pub fn new_replica(id: u64, addr: String, master_id: u64) -> Self {
        let cluster_port = if let Some(port_str) = addr.split(':').next_back() {
            port_str.parse::<u16>().unwrap_or(6379) + 10000
        } else {
            16379
        };
        Self {
            id,
            addr,
            cluster_port,
            is_master: false,
            is_connected: true,
            master_id: Some(master_id),
            replica_ids: Vec::new(),
        }
    }
}

/// Migration progress information.
#[derive(Debug, Clone)]
pub struct MigrationProgress {
    /// Source node ID
    pub source_node: u64,
    /// Target node ID
    pub target_node: u64,
    /// Number of keys migrated
    pub keys_migrated: u64,
    /// Total keys to migrate (0 if unknown)
    pub total_keys: u64,
    /// Migration start time (milliseconds since epoch)
    pub start_time: u64,
}

/// Cluster state management.
#[derive(Debug, Default)]
pub struct ClusterState {
    /// Known nodes in the cluster (node_id -> NodeInfo)
    pub nodes: HashMap<u64, NodeInfo>,
    /// Slot assignments (slot -> node_id)
    pub slot_assignments: Vec<Option<u64>>,
    /// Slot states for migration (slot -> state)
    pub slot_states: HashMap<u16, SlotState>,
    /// Migration targets (slot -> target_node_id) for MIGRATING/IMPORTING
    pub migration_targets: HashMap<u16, u64>,
    /// Migration progress tracking (slot -> progress)
    pub migration_progress: HashMap<u16, MigrationProgress>,
    /// Current cluster epoch
    pub config_epoch: u64,
    /// Master-replica relationships (master_id -> Vec<replica_id>)
    pub replica_map: HashMap<u64, Vec<u64>>,
    /// Whether this node is in readonly mode (for replicas)
    pub readonly_mode: bool,
}

impl ClusterState {
    /// Create a new ClusterState with default values.
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            slot_assignments: vec![None; TOTAL_SLOTS_USIZE],
            slot_states: HashMap::new(),
            migration_targets: HashMap::new(),
            migration_progress: HashMap::new(),
            config_epoch: 0,
            replica_map: HashMap::new(),
            readonly_mode: false,
        }
    }

    /// Count the number of assigned slots.
    pub fn assigned_slots_count(&self) -> usize {
        self.slot_assignments.iter().filter(|s| s.is_some()).count()
    }

    /// Check if all slots are assigned.
    pub fn all_slots_assigned(&self) -> bool {
        self.assigned_slots_count() == TOTAL_SLOTS_USIZE
    }

    /// Get migration state for a slot.
    ///
    /// Returns information about the migration state of a slot.
    ///
    /// # Arguments
    /// * `slot` - The slot number to query
    ///
    /// # Returns
    /// Tuple of (state, source_or_target_node_id) or None if slot is not in migration
    pub fn get_migration_state(&self, slot: u16) -> Option<(SlotState, u64)> {
        let state = self.slot_states.get(&slot)?;
        let target = self.migration_targets.get(&slot)?;
        Some((*state, *target))
    }

    /// Check if a slot is in migrating state (outbound migration).
    pub fn is_slot_migrating(&self, slot: u16) -> bool {
        matches!(self.slot_states.get(&slot), Some(SlotState::Migrating))
    }

    /// Check if a slot is in importing state (inbound migration).
    pub fn is_slot_importing(&self, slot: u16) -> bool {
        matches!(self.slot_states.get(&slot), Some(SlotState::Importing))
    }

    /// Get the migration target node for a slot being migrated.
    pub fn get_migration_target(&self, slot: u16) -> Option<u64> {
        if self.is_slot_migrating(slot) {
            self.migration_targets.get(&slot).copied()
        } else {
            None
        }
    }

    /// Get the migration source node for a slot being imported.
    pub fn get_import_source(&self, slot: u16) -> Option<u64> {
        if self.is_slot_importing(slot) {
            self.migration_targets.get(&slot).copied()
        } else {
            None
        }
    }

    /// Add a replica relationship.
    ///
    /// # Arguments
    /// * `master_id` - The ID of the master node
    /// * `replica_id` - The ID of the replica node
    pub fn add_replica(&mut self, master_id: u64, replica_id: u64) {
        // Update replica_map
        self.replica_map
            .entry(master_id)
            .or_default()
            .push(replica_id);

        // Update node info for replica
        if let Some(replica) = self.nodes.get_mut(&replica_id) {
            replica.is_master = false;
            replica.master_id = Some(master_id);
        }

        // Update node info for master
        if let Some(master) = self.nodes.get_mut(&master_id) {
            if !master.replica_ids.contains(&replica_id) {
                master.replica_ids.push(replica_id);
            }
        }

        self.config_epoch += 1;
    }

    /// Remove a replica relationship.
    ///
    /// # Arguments
    /// * `replica_id` - The ID of the replica node to remove
    pub fn remove_replica(&mut self, replica_id: u64) {
        // Find and remove from master's replica list
        if let Some(replica) = self.nodes.get(&replica_id) {
            if let Some(master_id) = replica.master_id {
                // Remove from replica_map
                if let Some(replicas) = self.replica_map.get_mut(&master_id) {
                    replicas.retain(|&id| id != replica_id);
                    if replicas.is_empty() {
                        self.replica_map.remove(&master_id);
                    }
                }

                // Remove from master's replica_ids
                if let Some(master) = self.nodes.get_mut(&master_id) {
                    master.replica_ids.retain(|&id| id != replica_id);
                }
            }
        }

        // Update replica node info
        if let Some(replica) = self.nodes.get_mut(&replica_id) {
            replica.master_id = None;
            replica.is_master = true;
        }

        self.config_epoch += 1;
    }

    /// Get replicas of a master node.
    ///
    /// # Arguments
    /// * `master_id` - The ID of the master node
    ///
    /// # Returns
    /// Vector of replica node IDs
    pub fn get_replicas(&self, master_id: u64) -> Vec<u64> {
        self.replica_map
            .get(&master_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get master of a replica node.
    ///
    /// # Arguments
    /// * `replica_id` - The ID of the replica node
    ///
    /// # Returns
    /// Option containing the master node ID
    pub fn get_master(&self, replica_id: u64) -> Option<u64> {
        self.nodes.get(&replica_id).and_then(|n| n.master_id)
    }

    /// Check if a node is a master.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to check
    ///
    /// # Returns
    /// true if the node is a master, false otherwise
    pub fn is_master(&self, node_id: u64) -> bool {
        self.nodes.get(&node_id).is_some_and(|n| n.is_master)
    }

    /// Check if a node is a replica.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to check
    ///
    /// # Returns
    /// true if the node is a replica, false otherwise
    pub fn is_replica(&self, node_id: u64) -> bool {
        self.nodes.get(&node_id).is_some_and(|n| !n.is_master)
    }

    /// Perform a failover: promote a replica to master.
    ///
    /// This method transfers slot ownership from the old master to the new master.
    ///
    /// # Arguments
    /// * `replica_id` - The ID of the replica to promote
    ///
    /// # Returns
    /// Ok(old_master_id) on success, Err if the node is not a replica
    pub fn promote_replica(&mut self, replica_id: u64) -> std::result::Result<u64, String> {
        // Get the master ID of this replica
        let master_id = self
            .get_master(replica_id)
            .ok_or_else(|| format!("Node {} is not a replica", replica_id))?;

        // Transfer slot ownership from master to replica
        for slot in self.slot_assignments.iter_mut() {
            if *slot == Some(master_id) {
                *slot = Some(replica_id);
            }
        }

        // Update replica to become master
        if let Some(replica) = self.nodes.get_mut(&replica_id) {
            replica.is_master = true;
            replica.master_id = None;
        }

        // Update old master to become replica
        if let Some(master) = self.nodes.get_mut(&master_id) {
            master.is_master = false;
            master.master_id = Some(replica_id);
            master.replica_ids.clear();
        }

        // Update replica map
        if let Some(old_replicas) = self.replica_map.remove(&master_id) {
            // Other replicas of the old master now replicate the new master
            let new_replicas: Vec<u64> = old_replicas
                .into_iter()
                .filter(|&id| id != replica_id)
                .chain(std::iter::once(master_id))
                .collect();

            if !new_replicas.is_empty() {
                self.replica_map.insert(replica_id, new_replicas.clone());

                // Update new master's replica_ids
                if let Some(new_master) = self.nodes.get_mut(&replica_id) {
                    new_master.replica_ids = new_replicas.clone();
                }

                // Update master_id for all transferred replicas (excluding the old master
                // which was already updated above)
                for other_replica_id in &new_replicas {
                    if *other_replica_id != master_id {
                        if let Some(other_replica) = self.nodes.get_mut(other_replica_id) {
                            other_replica.master_id = Some(replica_id);
                        }
                    }
                }
            }
        }

        self.config_epoch += 1;
        Ok(master_id)
    }
}

/// Type alias for a key scanner function.
///
/// This function is used by CLUSTER GETKEYSINSLOT to scan keys
/// belonging to a specific slot. The function takes:
/// - db_index: The database index to scan
/// - slot: The slot number to search for
/// - count: Maximum number of keys to return
///
/// Returns: Vector of keys belonging to the slot
pub type KeyScanner = Box<dyn Fn(usize, u16, usize) -> Vec<String> + Send + Sync>;

/// Type alias for a key counter function.
///
/// This function is used by CLUSTER COUNTKEYSINSLOT to count keys
/// belonging to a specific slot efficiently without loading them into memory.
/// The function takes:
/// - db_index: The database index to scan
/// - slot: The slot number to count keys for
///
/// Returns: Number of keys belonging to the slot
pub type KeyCounter = Box<dyn Fn(usize, u16) -> usize + Send + Sync>;

/// Cluster commands handler.
///
/// Implements Redis Cluster protocol commands:
/// - `CLUSTER KEYSLOT` - Calculate slot for a key
/// - `CLUSTER INFO` - Get cluster information
/// - `CLUSTER NODES` - Get cluster nodes
/// - `CLUSTER SLOTS` - Get slot-to-node mapping
/// - `CLUSTER MYID` - Get current node ID
/// - `CLUSTER MEET` - Add a node to the cluster
/// - `CLUSTER FORGET` - Remove a node from the cluster
/// - `CLUSTER ADDSLOTS` - Assign slots to this node
/// - `CLUSTER DELSLOTS` - Remove slot assignments
/// - `CLUSTER SETSLOT` - Set slot state (NODE/MIGRATING/IMPORTING)
/// - `CLUSTER GETKEYSINSLOT` - Get keys belonging to a slot
/// - `CLUSTER COUNTKEYSINSLOT` - Count keys in a slot
/// - `CLUSTER REPLICATE` - Configure node as replica of a master
/// - `CLUSTER FAILOVER` - Trigger manual failover
/// - `CLUSTER REPLICAS` - List replicas of a master
/// - `READONLY` - Enable readonly mode (for replicas)
/// - `READWRITE` - Disable readonly mode
pub struct ClusterCommands {
    router: SlotRouter,
    node_id: Option<u64>,
    /// Shared cluster state
    state: Arc<RwLock<ClusterState>>,
    /// Optional key scanner for GETKEYSINSLOT command
    key_scanner: Option<Arc<KeyScanner>>,
    /// Optional key counter for COUNTKEYSINSLOT command
    key_counter: Option<Arc<KeyCounter>>,
    /// Optional MultiRaftNode for cluster operations (when cluster feature is enabled)
    #[cfg(feature = "cluster")]
    multi_raft: Option<Arc<MultiRaftNode>>,
    /// Optional MetaRaftClient for cluster state synchronization via Raft consensus
    #[cfg(feature = "cluster")]
    meta_raft_client: Option<Arc<MetaRaftClient>>,
}

impl ClusterCommands {
    /// Create a new ClusterCommands handler.
    pub fn new() -> Self {
        Self {
            router: SlotRouter::new(),
            node_id: None,
            state: Arc::new(RwLock::new(ClusterState::new())),
            key_scanner: None,
            key_counter: None,
            #[cfg(feature = "cluster")]
            multi_raft: None,
            #[cfg(feature = "cluster")]
            meta_raft_client: None,
        }
    }

    /// Create a new ClusterCommands handler with a node ID.
    ///
    /// This is used to set the node ID for commands like CLUSTER MYID.
    pub fn with_node_id(node_id: u64) -> Self {
        Self::with_node_id_and_addr(node_id, "127.0.0.1:6379".to_string())
    }

    /// Create a new ClusterCommands handler with a node ID and address.
    ///
    /// This is used to set the node ID and address for cluster mode.
    /// The address should be in the format "host:port".
    pub fn with_node_id_and_addr(node_id: u64, addr: String) -> Self {
        let state = Arc::new(RwLock::new(ClusterState::new()));
        // Add self as a node
        {
            let mut state_guard = state.write().unwrap();
            let node_info = NodeInfo::new(node_id, addr);
            state_guard.nodes.insert(node_id, node_info);
        }
        Self {
            router: SlotRouter::new(),
            node_id: Some(node_id),
            state,
            key_scanner: None,
            key_counter: None,
            #[cfg(feature = "cluster")]
            multi_raft: None,
            #[cfg(feature = "cluster")]
            meta_raft_client: None,
        }
    }

    /// Create a new ClusterCommands handler with shared state.
    ///
    /// This allows multiple handlers to share the same cluster state.
    pub fn with_shared_state(node_id: Option<u64>, state: Arc<RwLock<ClusterState>>) -> Self {
        Self {
            router: SlotRouter::new(),
            node_id,
            state,
            key_scanner: None,
            key_counter: None,
            #[cfg(feature = "cluster")]
            multi_raft: None,
            #[cfg(feature = "cluster")]
            meta_raft_client: None,
        }
    }

    /// Create a new ClusterCommands handler with shared state and MultiRaftNode.
    ///
    /// This allows the handler to interact with the AiDb cluster for operations
    /// like CLUSTER MEET.
    #[cfg(feature = "cluster")]
    pub fn with_multi_raft(
        node_id: Option<u64>,
        state: Arc<RwLock<ClusterState>>,
        multi_raft: Arc<MultiRaftNode>,
    ) -> Self {
        Self {
            router: SlotRouter::new(),
            node_id,
            state,
            key_scanner: None,
            key_counter: None,
            multi_raft: Some(multi_raft),
            meta_raft_client: None,
        }
    }

    /// Create a new ClusterCommands handler with MetaRaftClient.
    ///
    /// This is the preferred constructor for full Multi-Raft cluster support.
    /// The MetaRaftClient provides Raft-based cluster state synchronization,
    /// eliminating the need for Redis gossip protocol.
    #[cfg(feature = "cluster")]
    pub fn with_meta_raft_client(
        node_id: Option<u64>,
        state: Arc<RwLock<ClusterState>>,
        multi_raft: Arc<MultiRaftNode>,
        meta_raft_client: Arc<MetaRaftClient>,
    ) -> Self {
        Self {
            router: SlotRouter::new(),
            node_id,
            state,
            key_scanner: None,
            key_counter: None,
            multi_raft: Some(multi_raft),
            meta_raft_client: Some(meta_raft_client),
        }
    }

    /// Set the MultiRaftNode for cluster operations.
    #[cfg(feature = "cluster")]
    pub fn set_multi_raft(&mut self, multi_raft: Arc<MultiRaftNode>) {
        self.multi_raft = Some(multi_raft);
    }

    /// Set the MetaRaftClient for cluster state synchronization.
    #[cfg(feature = "cluster")]
    pub fn set_meta_raft_client(&mut self, client: Arc<MetaRaftClient>) {
        self.meta_raft_client = Some(client);
    }

    /// Get the MetaRaftClient if available.
    #[cfg(feature = "cluster")]
    pub fn meta_raft_client(&self) -> Option<&Arc<MetaRaftClient>> {
        self.meta_raft_client.as_ref()
    }

    /// Generate a unique node ID for this cluster node.
    ///
    /// The node ID is generated based on:
    /// - Current timestamp in nanoseconds
    /// - Process ID
    /// - Thread ID
    ///
    /// This ensures uniqueness across restarts and different nodes.
    pub fn generate_node_id() -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();

        // Include timestamp for uniqueness across time
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .hash(&mut hasher);

        // Include process ID for uniqueness across processes
        std::process::id().hash(&mut hasher);

        // Include thread ID for additional variation
        std::thread::current().id().hash(&mut hasher);

        hasher.finish()
    }

    /// Set the key scanner for GETKEYSINSLOT command.
    ///
    /// The key scanner is a function that can scan keys belonging to a specific slot.
    /// This is typically implemented using the storage adapter's key scanning capability.
    pub fn set_key_scanner(&mut self, scanner: KeyScanner) {
        self.key_scanner = Some(Arc::new(scanner));
    }

    /// Set the key counter for COUNTKEYSINSLOT command.
    ///
    /// The key counter is an efficient function that counts keys belonging to a specific slot
    /// without loading them into memory.
    pub fn set_key_counter(&mut self, counter: KeyCounter) {
        self.key_counter = Some(Arc::new(counter));
    }

    /// Create a ClusterCommands handler with a key scanner.
    pub fn with_key_scanner(mut self, scanner: KeyScanner) -> Self {
        self.key_scanner = Some(Arc::new(scanner));
        self
    }

    /// Create a ClusterCommands handler with a key counter.
    pub fn with_key_counter(mut self, counter: KeyCounter) -> Self {
        self.key_counter = Some(Arc::new(counter));
        self
    }

    /// Get the shared cluster state.
    pub fn state(&self) -> Arc<RwLock<ClusterState>> {
        Arc::clone(&self.state)
    }

    /// Synchronize cluster state from MetaRaft.
    ///
    /// This method reads the cluster view from MetaRaft and updates the local
    /// cluster state. This ensures all nodes have a consistent view of the cluster
    /// after CLUSTER MEET operations.
    ///
    /// # Returns
    ///
    /// Ok(()) if synchronization succeeded, Err if MetaRaft is not available
    #[cfg(feature = "cluster")]
    pub fn sync_from_metaraft(&self) -> Result<()> {
        let meta_client = self.meta_raft_client.as_ref().ok_or_else(|| {
            AikvError::InvalidCommand("MetaRaft client not available".to_string())
        })?;

        let cluster_view = meta_client.get_cluster_view();
        let mut state = self.state.write().map_err(|e| {
            AikvError::Storage(format!("Failed to acquire cluster state write lock: {}", e))
        })?;

        // Update nodes from cluster view
        for (node_id, node_info) in cluster_view.nodes {
            if let Some(existing_node) = state.nodes.get_mut(&node_id) {
                // Update existing node
                existing_node.is_connected = node_info.is_online;
                existing_node.is_master = node_info.is_master;
            } else {
                // Add new node
                let mut new_node = NodeInfo::new(node_id, node_info.data_addr.clone());
                new_node.is_connected = node_info.is_online;
                new_node.is_master = node_info.is_master;
                state.nodes.insert(node_id, new_node);
            }
        }

        // Update slot assignments from cluster view
        // MetaRaft guarantees this vector has exactly TOTAL_SLOTS_USIZE elements
        debug_assert_eq!(
            cluster_view.slot_assignments.len(),
            TOTAL_SLOTS_USIZE,
            "MetaRaft slot assignments should always have {} elements",
            TOTAL_SLOTS_USIZE
        );
        for (slot_idx, node_id_opt) in cluster_view.slot_assignments.iter().enumerate() {
            state.slot_assignments[slot_idx] = *node_id_opt;
        }

        // Update config epoch
        if cluster_view.config_epoch > state.config_epoch {
            state.config_epoch = cluster_view.config_epoch;
        }

        tracing::debug!(
            "Synchronized cluster state from MetaRaft: {} nodes, {} slots assigned",
            state.nodes.len(),
            state.assigned_slots_count()
        );

        Ok(())
    }

    /// Synchronize cluster state from MetaRaft (stub for non-cluster builds).
    #[cfg(not(feature = "cluster"))]
    pub fn sync_from_metaraft(&self) -> Result<()> {
        Err(AikvError::InvalidCommand(
            "Cluster feature not enabled".to_string(),
        ))
    }

    /// Get the slot router for key-to-slot calculations.
    pub fn router(&self) -> &SlotRouter {
        &self.router
    }

    /// Enable readonly mode for this node.
    ///
    /// When readonly mode is enabled, this replica node can serve read requests
    /// for keys it doesn't own (its master's keys).
    ///
    /// Note: In a full Redis Cluster implementation, readonly mode should be tracked
    /// per-client-connection rather than globally. This implementation provides the
    /// command interface; the actual per-connection tracking should be done at the
    /// server connection handler level.
    ///
    /// # Returns
    ///
    /// OK on success
    pub fn readonly(&self) -> Result<RespValue> {
        let mut state = self.state.write().unwrap();
        state.readonly_mode = true;
        Ok(RespValue::simple_string("OK"))
    }

    /// Disable readonly mode for this node.
    ///
    /// When readonly mode is disabled, the node will only serve requests
    /// for keys it owns.
    ///
    /// Note: In a full Redis Cluster implementation, readonly mode should be tracked
    /// per-client-connection rather than globally. This implementation provides the
    /// command interface; the actual per-connection tracking should be done at the
    /// server connection handler level.
    ///
    /// # Returns
    ///
    /// OK on success
    pub fn readwrite(&self) -> Result<RespValue> {
        let mut state = self.state.write().unwrap();
        state.readonly_mode = false;
        Ok(RespValue::simple_string("OK"))
    }

    /// Check if readonly mode is enabled.
    ///
    /// Note: In a full implementation, this would check the per-connection readonly
    /// flag set by the READONLY command for that specific client connection.
    pub fn is_readonly(&self) -> bool {
        self.state.read().map(|s| s.readonly_mode).unwrap_or(false)
    }

    /// Execute a CLUSTER command.
    ///
    /// # Arguments
    ///
    /// * `args` - Command arguments (subcommand and its arguments)
    ///
    /// # Returns
    ///
    /// The command result as a RespValue
    pub fn execute(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("CLUSTER".to_string()));
        }

        let subcommand = String::from_utf8_lossy(&args[0]).to_uppercase();
        match subcommand.as_str() {
            "KEYSLOT" => self.keyslot(&args[1..]),
            "INFO" => self.info(&args[1..]),
            "NODES" => self.nodes(&args[1..]),
            "SLOTS" => self.slots(&args[1..]),
            "MYID" => self.myid(&args[1..]),
            "MEET" => self.meet(&args[1..]),
            "FORGET" => self.forget(&args[1..]),
            "ADDSLOTS" => self.addslots(&args[1..]),
            "DELSLOTS" => self.delslots(&args[1..]),
            "SETSLOT" => self.setslot(&args[1..]),
            "GETKEYSINSLOT" => self.getkeysinslot(&args[1..]),
            "COUNTKEYSINSLOT" => self.countkeysinslot(&args[1..]),
            "REPLICATE" => self.replicate(&args[1..]),
            "FAILOVER" => self.failover(&args[1..]),
            "SLAVES" | "REPLICAS" => self.replicas(&args[1..]),
            "HELP" => self.help(),
            _ => Err(AikvError::InvalidCommand(format!(
                "Unknown CLUSTER subcommand: {}",
                subcommand
            ))),
        }
    }

    /// CLUSTER KEYSLOT key
    ///
    /// Returns the hash slot of the specified key.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain exactly one argument: the key
    ///
    /// # Returns
    ///
    /// An integer representing the slot number (0-16383)
    fn keyslot(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CLUSTER KEYSLOT".to_string()));
        }

        let key = &args[0];
        let slot = self.router.key_to_slot(key);

        Ok(RespValue::Integer(slot as i64))
    }

    /// CLUSTER INFO
    ///
    /// Returns information about the cluster state.
    fn info(&self, _args: &[Bytes]) -> Result<RespValue> {
        // Sync from MetaRaft if available to get latest cluster state
        #[cfg(feature = "cluster")]
        {
            let _ = self.sync_from_metaraft();
        }

        let state = self.state.read().unwrap();

        let assigned_slots = state.assigned_slots_count();
        let cluster_state = if state.all_slots_assigned() && !state.nodes.is_empty() {
            "ok"
        } else {
            "fail"
        };
        let known_nodes = state.nodes.len();
        // Count nodes with assigned slots (masters)
        let cluster_size = state
            .slot_assignments
            .iter()
            .filter_map(|s| *s)
            .collect::<std::collections::HashSet<_>>()
            .len();

        let info = format!(
            "\
cluster_state:{}\r\n\
cluster_slots_assigned:{}\r\n\
cluster_slots_ok:{}\r\n\
cluster_slots_pfail:0\r\n\
cluster_slots_fail:0\r\n\
cluster_known_nodes:{}\r\n\
cluster_size:{}\r\n\
cluster_current_epoch:{}\r\n\
cluster_my_epoch:{}\r\n\
cluster_stats_messages_sent:0\r\n\
cluster_stats_messages_received:0\r\n",
            cluster_state,
            assigned_slots,
            assigned_slots,
            known_nodes.max(1), // At least 1 (self)
            cluster_size,
            state.config_epoch,
            state.config_epoch,
        );

        Ok(RespValue::bulk_string(Bytes::from(info)))
    }

    /// CLUSTER NODES
    ///
    /// Returns the cluster nodes information in Redis format.
    fn nodes(&self, _args: &[Bytes]) -> Result<RespValue> {
        // Sync from MetaRaft if available to get latest cluster state
        #[cfg(feature = "cluster")]
        {
            let _ = self.sync_from_metaraft();
        }

        let state = self.state.read().unwrap();
        let my_node_id = self.node_id.unwrap_or(0);
        let mut output = String::new();

        // Build slot ranges for each node
        let mut node_slots: HashMap<u64, Vec<(u16, u16)>> = HashMap::new();
        let mut current_start: Option<u16> = None;
        let mut current_node: Option<u64> = None;

        for (slot, &node) in state.slot_assignments.iter().enumerate() {
            let slot = slot as u16;
            match (current_start, current_node, node) {
                (Some(_start), Some(curr), Some(n)) if curr == n => {
                    // Continue current range
                }
                (Some(start), Some(curr), _) => {
                    // End current range
                    node_slots.entry(curr).or_default().push((start, slot - 1));
                    current_start = node.map(|_| slot);
                    current_node = node;
                }
                (None, None, Some(n)) => {
                    current_start = Some(slot);
                    current_node = Some(n);
                }
                _ => {
                    current_start = node.map(|_| slot);
                    current_node = node;
                }
            }
        }
        // Handle last range
        if let (Some(start), Some(curr)) = (current_start, current_node) {
            node_slots
                .entry(curr)
                .or_default()
                .push((start, TOTAL_SLOTS - 1));
        }

        // If no nodes in state, output self
        if state.nodes.is_empty() {
            output.push_str(&format!(
                "{:040x} 127.0.0.1:6379@16379 myself,master - 0 0 0 connected\r\n",
                my_node_id
            ));
        } else {
            for (node_id, info) in &state.nodes {
                let myself = if *node_id == my_node_id {
                    "myself,"
                } else {
                    ""
                };
                let role = if info.is_master { "master" } else { "slave" };
                let status = if info.is_connected {
                    "connected"
                } else {
                    "disconnected"
                };

                // Format slots
                let slots_str = node_slots
                    .get(node_id)
                    .map(|ranges| {
                        ranges
                            .iter()
                            .map(|(start, end)| {
                                if start == end {
                                    format!("{}", start)
                                } else {
                                    format!("{}-{}", start, end)
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                // Format: <node-id> <ip:port@cluster-port> <flags> <master-id> <ping-sent> <pong-recv> <config-epoch> <link-state> <slot> ...
                output.push_str(&format!(
                    "{:040x} {}@{} {}{} - 0 0 {} {} {}\r\n",
                    node_id,
                    info.addr,
                    info.cluster_port,
                    myself,
                    role,
                    state.config_epoch,
                    status,
                    slots_str
                ));
            }
        }

        Ok(RespValue::bulk_string(Bytes::from(output)))
    }

    /// CLUSTER SLOTS
    ///
    /// Returns the slot-to-node mapping.
    fn slots(&self, _args: &[Bytes]) -> Result<RespValue> {
        let state = self.state.read().unwrap();
        let mut result = Vec::new();

        // Group consecutive slots assigned to the same node
        let mut ranges: Vec<(u16, u16, u64)> = Vec::new();
        let mut current_start: Option<u16> = None;
        let mut current_node: Option<u64> = None;

        for (slot, &node) in state.slot_assignments.iter().enumerate() {
            let slot = slot as u16;
            match (current_start, current_node, node) {
                (Some(_), Some(curr), Some(n)) if curr == n => {
                    // Continue current range
                }
                (Some(start), Some(curr), _) => {
                    // End current range and push
                    ranges.push((start, slot - 1, curr));
                    current_start = node.map(|_| slot);
                    current_node = node;
                }
                (None, None, Some(n)) => {
                    current_start = Some(slot);
                    current_node = Some(n);
                }
                _ => {
                    current_start = node.map(|_| slot);
                    current_node = node;
                }
            }
        }
        // Handle last range
        if let (Some(start), Some(curr)) = (current_start, current_node) {
            ranges.push((start, TOTAL_SLOTS - 1, curr));
        }

        // Build RESP response for each range
        for (start, end, node_id) in ranges {
            let node_info = state.nodes.get(&node_id);
            let (ip, port) = if let Some(info) = node_info {
                let parts: Vec<&str> = info.addr.split(':').collect();
                let ip = parts.first().unwrap_or(&"127.0.0.1").to_string();
                let port = parts
                    .get(1)
                    .and_then(|p| p.parse::<i64>().ok())
                    .unwrap_or(6379);
                (ip, port)
            } else {
                ("127.0.0.1".to_string(), 6379)
            };

            // Format: [start, end, [ip, port, node_id], ...]
            let node_entry = RespValue::Array(Some(vec![
                RespValue::bulk_string(Bytes::from(ip)),
                RespValue::Integer(port),
                RespValue::bulk_string(Bytes::from(format!("{:040x}", node_id))),
            ]));

            let slot_entry = RespValue::Array(Some(vec![
                RespValue::Integer(start as i64),
                RespValue::Integer(end as i64),
                node_entry,
            ]));

            result.push(slot_entry);
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// CLUSTER MYID
    ///
    /// Returns the current node's ID.
    fn myid(&self, _args: &[Bytes]) -> Result<RespValue> {
        let node_id = self.node_id.unwrap_or(0);
        Ok(RespValue::bulk_string(Bytes::from(format!(
            "{:040x}",
            node_id
        ))))
    }

    /// CLUSTER MEET ip port [cluster-port] [node-id]
    ///
    /// Add a node to the cluster by specifying its address.
    /// When MetaRaftClient is available, this uses the MetaRaft consensus
    /// to add the node to the cluster via Raft.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain at least: ip, port. Optionally: cluster-port, node-id
    ///   - If node-id is provided, it will be used instead of generating a deterministic ID
    ///   - node-id should be a 40-character hex string (e.g., from CLUSTER MYID)
    ///
    /// # Returns
    ///
    /// OK on success
    fn meet(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("CLUSTER MEET".to_string()));
        }

        let ip = String::from_utf8_lossy(&args[0]).to_string();
        let port = String::from_utf8_lossy(&args[1])
            .parse::<u16>()
            .map_err(|_| AikvError::InvalidArgument("Invalid port number".to_string()))?;

        let cluster_port = if args.len() > 2 && args[2].iter().all(|&b| b.is_ascii_digit()) {
            String::from_utf8_lossy(&args[2])
                .parse::<u16>()
                .map_err(|_| {
                    AikvError::InvalidArgument("Invalid cluster port number".to_string())
                })?
        } else {
            port + 10000
        };

        // Data port address for Redis protocol
        let data_addr = format!("{}:{}", ip, port);
        // Cluster bus address for Raft RPC (gRPC)
        let raft_addr = format!("{}:{}", ip, cluster_port);

        // Determine the node ID to use
        // If a node-id is provided as the last argument (40-char hex string), use it
        // Otherwise, generate a deterministic ID based on address hash
        //
        // Note: Node IDs are stored as u64 (8 bytes = 16 hex digits), but formatted
        // as 40-character hex strings with leading zeros for Redis compatibility.
        // Parsing a 40-char hex string into u64 works correctly - the leading zeros
        // are simply ignored by from_str_radix.
        let target_node_id = if args.len() >= 3 {
            let last_arg = String::from_utf8_lossy(&args[args.len() - 1]);
            // Check if last arg looks like a node ID (40 hex chars)
            if last_arg.len() == 40 && last_arg.chars().all(|c| c.is_ascii_hexdigit()) {
                // Parse the provided node ID (40-char hex with leading zeros -> u64)
                u64::from_str_radix(&last_arg, 16).map_err(|_| {
                    AikvError::InvalidArgument("Invalid node ID format".to_string())
                })?
            } else {
                // Not a valid node ID, generate deterministic ID
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                data_addr.hash(&mut hasher);
                hasher.finish()
            }
        } else {
            // No optional args, generate deterministic ID
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            data_addr.hash(&mut hasher);
            hasher.finish()
        };

        // Add node to local cluster state for immediate visibility
        {
            let mut state = self.state.write().unwrap();
            let mut node_info = NodeInfo::new(target_node_id, data_addr.clone());
            node_info.cluster_port = cluster_port;
            state.nodes.insert(target_node_id, node_info);
            state.config_epoch += 1;
        }

        // Prefer using MetaRaftClient for Raft-based cluster state synchronization
        #[cfg(feature = "cluster")]
        {
            if let Some(ref meta_client) = self.meta_raft_client {
                // Use MetaRaftClient for Raft consensus-based node join
                let meta_client = Arc::clone(meta_client);
                let data_addr_clone = data_addr.clone();
                let raft_addr_clone = raft_addr.clone();

                // Spawn async task for Raft proposal
                tokio::spawn(async move {
                    match meta_client
                        .propose_node_join(target_node_id, raft_addr_clone)
                        .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                "CLUSTER MEET: Node {} ({}) added via MetaRaftClient",
                                data_addr_clone,
                                target_node_id
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "CLUSTER MEET: MetaRaftClient failed to add node {}: {}",
                                data_addr_clone,
                                e
                            );
                        }
                    }
                });

                // Attempt to synchronize state from MetaRaft after MEET
                // This helps ensure consistency across nodes
                let _ = self.sync_from_metaraft();
            } else if let Some(ref multi_raft) = self.multi_raft {
                // Fallback to direct MultiRaftNode usage
                multi_raft.add_node_address(target_node_id, raft_addr.clone());

                if let Some(meta_raft) = multi_raft.meta_raft() {
                    let meta_raft = meta_raft.clone();
                    let data_addr_clone = data_addr.clone();
                    let raft_addr_clone = raft_addr.clone();

                    tokio::spawn(async move {
                        match meta_raft.add_node(target_node_id, raft_addr_clone).await {
                            Ok(_) => {
                                tracing::info!(
                                    "CLUSTER MEET: Node {} ({}) added via MetaRaft",
                                    data_addr_clone,
                                    target_node_id
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "CLUSTER MEET: MetaRaft failed to add node {}: {}",
                                    data_addr_clone,
                                    e
                                );
                            }
                        }
                    });
                }
            }
        }

        Ok(RespValue::simple_string("OK"))
    }

    /// CLUSTER FORGET node-id
    ///
    /// Remove a node from the cluster.
    /// When MetaRaftClient is available, this uses Raft consensus
    /// to remove the node from the cluster.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain exactly one argument: the node ID (40-char hex)
    ///
    /// # Returns
    ///
    /// OK on success, error if node not found or is self
    fn forget(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CLUSTER FORGET".to_string()));
        }

        let node_id_str = String::from_utf8_lossy(&args[0]).to_string();
        let node_id = u64::from_str_radix(&node_id_str, 16)
            .map_err(|_| AikvError::InvalidArgument("Invalid node ID".to_string()))?;

        // Cannot forget self
        if Some(node_id) == self.node_id {
            return Err(AikvError::InvalidArgument(
                "I tried hard but I can't forget myself".to_string(),
            ));
        }

        {
            let mut state = self.state.write().unwrap();

            // Check if node exists
            if !state.nodes.contains_key(&node_id) {
                return Err(AikvError::InvalidArgument(format!(
                    "Unknown node {}",
                    node_id_str
                )));
            }

            // Remove the node
            state.nodes.remove(&node_id);

            // Remove any slot assignments to this node
            for slot in state.slot_assignments.iter_mut() {
                if *slot == Some(node_id) {
                    *slot = None;
                }
            }

            state.config_epoch += 1;
        }

        // Use MetaRaftClient to remove node via Raft consensus
        #[cfg(feature = "cluster")]
        {
            if let Some(ref meta_client) = self.meta_raft_client {
                let meta_client = Arc::clone(meta_client);
                let node_id_str_clone = node_id_str.clone();

                tokio::spawn(async move {
                    match meta_client.propose_node_leave(node_id).await {
                        Ok(_) => {
                            tracing::info!(
                                "CLUSTER FORGET: Node {} removed via MetaRaftClient",
                                node_id_str_clone
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "CLUSTER FORGET: MetaRaftClient failed to remove node {}: {}",
                                node_id_str_clone,
                                e
                            );
                        }
                    }
                });
            } else if let Some(ref multi_raft) = self.multi_raft {
                if let Some(meta_raft) = multi_raft.meta_raft() {
                    let meta_raft = meta_raft.clone();
                    let node_id_str_clone = node_id_str.clone();

                    tokio::spawn(async move {
                        match meta_raft.remove_node(node_id).await {
                            Ok(_) => {
                                tracing::info!(
                                    "CLUSTER FORGET: Node {} removed via MetaRaft",
                                    node_id_str_clone
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "CLUSTER FORGET: MetaRaft failed to remove node {}: {}",
                                    node_id_str_clone,
                                    e
                                );
                            }
                        }
                    });
                }
            }
        }

        Ok(RespValue::simple_string("OK"))
    }

    /// CLUSTER ADDSLOTS slot [slot ...]
    ///
    /// Assign slots to the current node.
    ///
    /// # Arguments
    ///
    /// * `args` - One or more slot numbers to assign
    ///
    /// # Returns
    ///
    /// OK on success
    fn addslots(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("CLUSTER ADDSLOTS".to_string()));
        }

        let my_node_id = self.node_id.ok_or_else(|| {
            AikvError::InvalidCommand("Node ID not set for this cluster node".to_string())
        })?;

        // Parse and validate all slots first
        let mut slots_to_add = Vec::new();
        for arg in args {
            let slot = String::from_utf8_lossy(arg)
                .parse::<u16>()
                .map_err(|_| AikvError::InvalidArgument("Invalid slot number".to_string()))?;

            if slot >= TOTAL_SLOTS {
                return Err(AikvError::InvalidArgument(format!(
                    "Invalid slot {} (out of range 0-{})",
                    slot,
                    TOTAL_SLOTS - 1
                )));
            }
            slots_to_add.push(slot);
        }

        let mut state = self.state.write().unwrap();

        // Check if any slot is already assigned
        for &slot in &slots_to_add {
            if let Some(assigned_to) = state.slot_assignments[slot as usize] {
                if assigned_to != my_node_id {
                    return Err(AikvError::InvalidArgument(format!(
                        "Slot {} is already busy",
                        slot
                    )));
                }
            }
        }

        // Assign all slots
        for slot in slots_to_add {
            state.slot_assignments[slot as usize] = Some(my_node_id);
        }
        state.config_epoch += 1;

        Ok(RespValue::simple_string("OK"))
    }

    /// CLUSTER DELSLOTS slot [slot ...]
    ///
    /// Remove slot assignments from the current node.
    ///
    /// # Arguments
    ///
    /// * `args` - One or more slot numbers to remove
    ///
    /// # Returns
    ///
    /// OK on success
    fn delslots(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("CLUSTER DELSLOTS".to_string()));
        }

        let my_node_id = self.node_id;

        // Parse and validate all slots first
        let mut slots_to_del = Vec::new();
        for arg in args {
            let slot = String::from_utf8_lossy(arg)
                .parse::<u16>()
                .map_err(|_| AikvError::InvalidArgument("Invalid slot number".to_string()))?;

            if slot >= TOTAL_SLOTS {
                return Err(AikvError::InvalidArgument(format!(
                    "Invalid slot {} (out of range 0-{})",
                    slot,
                    TOTAL_SLOTS - 1
                )));
            }
            slots_to_del.push(slot);
        }

        let mut state = self.state.write().unwrap();

        // Check if slots are assigned to this node or unassigned
        // When node_id is None (standalone mode), only allow deleting unassigned slots
        // When node_id is Some, only allow deleting slots owned by this node or unassigned slots
        for &slot in &slots_to_del {
            if let Some(assigned_to) = state.slot_assignments[slot as usize] {
                match my_node_id {
                    Some(my_id) if assigned_to == my_id => {
                        // This node owns the slot, OK to delete
                    }
                    Some(_) => {
                        // Slot is owned by another node
                        return Err(AikvError::InvalidArgument(format!(
                            "Slot {} is not owned by this node",
                            slot
                        )));
                    }
                    None => {
                        // Standalone mode without node ID cannot delete assigned slots
                        return Err(AikvError::InvalidArgument(format!(
                            "Slot {} is already assigned. Set node_id to manage slots",
                            slot
                        )));
                    }
                }
            }
            // Unassigned slots (None) are OK to "delete" (no-op)
        }

        // Remove all slot assignments
        for slot in slots_to_del {
            state.slot_assignments[slot as usize] = None;
            // Also clear any migration state
            state.slot_states.remove(&slot);
            state.migration_targets.remove(&slot);
        }
        state.config_epoch += 1;

        Ok(RespValue::simple_string("OK"))
    }

    /// CLUSTER SETSLOT slot IMPORTING|MIGRATING|NODE|STABLE [node-id]
    ///
    /// Set slot state for migration or assign to a node.
    ///
    /// # Arguments
    ///
    /// * `args` - slot, subcommand (IMPORTING/MIGRATING/NODE/STABLE), and optionally node-id
    ///
    /// # Returns
    ///
    /// OK on success
    fn setslot(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("CLUSTER SETSLOT".to_string()));
        }

        let slot = String::from_utf8_lossy(&args[0])
            .parse::<u16>()
            .map_err(|_| AikvError::InvalidArgument("Invalid slot number".to_string()))?;

        if slot >= TOTAL_SLOTS {
            return Err(AikvError::InvalidArgument(format!(
                "Invalid slot {} (out of range 0-{})",
                slot,
                TOTAL_SLOTS - 1
            )));
        }

        let subcommand = String::from_utf8_lossy(&args[1]).to_uppercase();

        match subcommand.as_str() {
            "IMPORTING" => {
                // CLUSTER SETSLOT <slot> IMPORTING <node-id>
                // Set slot as importing from another node
                if args.len() < 3 {
                    return Err(AikvError::WrongArgCount(
                        "CLUSTER SETSLOT IMPORTING".to_string(),
                    ));
                }
                let source_node_id_str = String::from_utf8_lossy(&args[2]).to_string();
                let source_node_id = u64::from_str_radix(&source_node_id_str, 16)
                    .map_err(|_| AikvError::InvalidArgument("Invalid node ID".to_string()))?;

                let mut state = self.state.write().unwrap();
                state.slot_states.insert(slot, SlotState::Importing);
                state.migration_targets.insert(slot, source_node_id);
                state.config_epoch += 1;

                Ok(RespValue::simple_string("OK"))
            }
            "MIGRATING" => {
                // CLUSTER SETSLOT <slot> MIGRATING <node-id>
                // Set slot as migrating to another node
                if args.len() < 3 {
                    return Err(AikvError::WrongArgCount(
                        "CLUSTER SETSLOT MIGRATING".to_string(),
                    ));
                }
                let target_node_id_str = String::from_utf8_lossy(&args[2]).to_string();
                let target_node_id = u64::from_str_radix(&target_node_id_str, 16)
                    .map_err(|_| AikvError::InvalidArgument("Invalid node ID".to_string()))?;

                let mut state = self.state.write().unwrap();
                state.slot_states.insert(slot, SlotState::Migrating);
                state.migration_targets.insert(slot, target_node_id);
                state.config_epoch += 1;

                Ok(RespValue::simple_string("OK"))
            }
            "NODE" => {
                // CLUSTER SETSLOT <slot> NODE <node-id>
                // Assign slot to a specific node
                if args.len() < 3 {
                    return Err(AikvError::WrongArgCount("CLUSTER SETSLOT NODE".to_string()));
                }
                let target_node_id_str = String::from_utf8_lossy(&args[2]).to_string();
                let target_node_id = u64::from_str_radix(&target_node_id_str, 16)
                    .map_err(|_| AikvError::InvalidArgument("Invalid node ID".to_string()))?;

                let mut state = self.state.write().unwrap();

                // Check if target node is known
                if !state.nodes.contains_key(&target_node_id)
                    && self.node_id != Some(target_node_id)
                {
                    return Err(AikvError::InvalidArgument(format!(
                        "Unknown node {}",
                        target_node_id_str
                    )));
                }

                // Assign the slot to the node
                state.slot_assignments[slot as usize] = Some(target_node_id);
                // Clear migration state
                state.slot_states.remove(&slot);
                state.migration_targets.remove(&slot);
                state.config_epoch += 1;

                Ok(RespValue::simple_string("OK"))
            }
            "STABLE" => {
                // CLUSTER SETSLOT <slot> STABLE
                // Clear migration state, slot remains assigned to current node
                let mut state = self.state.write().unwrap();
                state.slot_states.remove(&slot);
                state.migration_targets.remove(&slot);
                state.migration_progress.remove(&slot);
                state.config_epoch += 1;

                Ok(RespValue::simple_string("OK"))
            }
            _ => Err(AikvError::InvalidArgument(format!(
                "Unknown SETSLOT subcommand: {}",
                subcommand
            ))),
        }
    }

    /// CLUSTER GETKEYSINSLOT slot count
    ///
    /// Returns up to `count` keys from the specified hash slot.
    /// This command is used during cluster resharding to migrate keys.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain: slot number, count
    ///
    /// # Returns
    ///
    /// An array of key names belonging to the slot
    fn getkeysinslot(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount(
                "CLUSTER GETKEYSINSLOT".to_string(),
            ));
        }

        let slot = String::from_utf8_lossy(&args[0])
            .parse::<u16>()
            .map_err(|_| AikvError::InvalidArgument("Invalid slot number".to_string()))?;

        if slot >= TOTAL_SLOTS {
            return Err(AikvError::InvalidArgument(format!(
                "Invalid slot {} (out of range 0-{})",
                slot,
                TOTAL_SLOTS - 1
            )));
        }

        let count = String::from_utf8_lossy(&args[1])
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("Invalid count".to_string()))?;

        // Use the key scanner if available
        if let Some(ref scanner) = self.key_scanner {
            let keys = scanner(0, slot, count);
            let resp_keys: Vec<RespValue> = keys
                .into_iter()
                .map(|k| RespValue::bulk_string(Bytes::from(k)))
                .collect();
            return Ok(RespValue::Array(Some(resp_keys)));
        }

        // No key scanner configured, return empty array
        // In production, this would use AiDb's state_machine.scan_slot_keys_sync
        Ok(RespValue::Array(Some(vec![])))
    }

    /// CLUSTER COUNTKEYSINSLOT slot
    ///
    /// Returns the number of keys in the specified hash slot.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain: slot number
    ///
    /// # Returns
    ///
    /// An integer representing the number of keys in the slot
    fn countkeysinslot(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount(
                "CLUSTER COUNTKEYSINSLOT".to_string(),
            ));
        }

        let slot = String::from_utf8_lossy(&args[0])
            .parse::<u16>()
            .map_err(|_| AikvError::InvalidArgument("Invalid slot number".to_string()))?;

        if slot >= TOTAL_SLOTS {
            return Err(AikvError::InvalidArgument(format!(
                "Invalid slot {} (out of range 0-{})",
                slot,
                TOTAL_SLOTS - 1
            )));
        }

        // Prefer the dedicated key counter for efficiency
        if let Some(ref counter) = self.key_counter {
            let count = counter(0, slot);
            return Ok(RespValue::Integer(count as i64));
        }

        // Fall back to key scanner if available (less efficient)
        if let Some(ref scanner) = self.key_scanner {
            // This is less efficient as it loads all keys into memory
            // Consider providing a dedicated KeyCounter for better performance
            let keys = scanner(0, slot, usize::MAX);
            return Ok(RespValue::Integer(keys.len() as i64));
        }

        // No key scanner or counter configured, return 0
        Ok(RespValue::Integer(0))
    }

    /// CLUSTER REPLICATE node-id
    ///
    /// Configures a node to be a replica of the specified master.
    /// This is used to set up replication relationships in the cluster.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain exactly one argument: the master node ID (40-char hex)
    ///
    /// # Returns
    ///
    /// OK on success, error if master is unknown or operation fails
    fn replicate(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CLUSTER REPLICATE".to_string()));
        }

        let master_node_id_str = String::from_utf8_lossy(&args[0]).to_string();
        let master_node_id = u64::from_str_radix(&master_node_id_str, 16)
            .map_err(|_| AikvError::InvalidArgument("Invalid node ID".to_string()))?;

        let my_node_id = self.node_id.ok_or_else(|| {
            AikvError::InvalidCommand("Node ID not set for this cluster node".to_string())
        })?;

        // Cannot replicate self
        if master_node_id == my_node_id {
            return Err(AikvError::InvalidArgument(
                "Cannot replicate myself".to_string(),
            ));
        }

        let mut state = self.state.write().unwrap();

        // Check if master node exists
        if !state.nodes.contains_key(&master_node_id) {
            return Err(AikvError::InvalidArgument(format!(
                "Unknown node {}",
                master_node_id_str
            )));
        }

        // Check if target node is actually a master
        if !state.is_master(master_node_id) {
            return Err(AikvError::InvalidArgument(format!(
                "Node {} is not a master",
                master_node_id_str
            )));
        }

        // If this node is already a replica, remove the old relationship
        if state.is_replica(my_node_id) {
            state.remove_replica(my_node_id);
        }

        // If this node is a master with replicas, we need to handle them
        // In a production implementation, the replicas would be reassigned
        // For now, we just remove them
        if let Some(replicas) = state.replica_map.remove(&my_node_id) {
            for replica_id in replicas {
                if let Some(replica) = state.nodes.get_mut(&replica_id) {
                    replica.master_id = None;
                    replica.is_master = true;
                }
            }
        }

        // Remove slot assignments from this node (replicas don't own slots)
        for slot in state.slot_assignments.iter_mut() {
            if *slot == Some(my_node_id) {
                *slot = None;
            }
        }

        // Set up the new replication relationship
        state.add_replica(master_node_id, my_node_id);

        Ok(RespValue::simple_string("OK"))
    }

    /// CLUSTER FAILOVER [FORCE|TAKEOVER]
    ///
    /// Forces a replica to start a manual failover of its master.
    ///
    /// Options:
    /// - FORCE: Force failover without master agreement
    /// - TAKEOVER: Force failover even if master is unreachable (takes over immediately)
    ///
    /// # Arguments
    ///
    /// * `args` - Optional: FORCE or TAKEOVER mode
    ///
    /// # Returns
    ///
    /// OK on success, error if this node is not a replica
    fn failover(&self, args: &[Bytes]) -> Result<RespValue> {
        let mode = if args.is_empty() {
            FailoverMode::Default
        } else {
            let mode_str = String::from_utf8_lossy(&args[0]).to_uppercase();
            match mode_str.as_str() {
                "FORCE" => FailoverMode::Force,
                "TAKEOVER" => FailoverMode::Takeover,
                _ => {
                    return Err(AikvError::InvalidArgument(format!(
                        "Invalid failover option: {}",
                        mode_str
                    )));
                }
            }
        };

        let my_node_id = self.node_id.ok_or_else(|| {
            AikvError::InvalidCommand("Node ID not set for this cluster node".to_string())
        })?;

        let mut state = self.state.write().unwrap();

        // Check if this node is a replica
        if !state.is_replica(my_node_id) {
            return Err(AikvError::InvalidArgument(
                "This node is not a replica - cannot failover".to_string(),
            ));
        }

        // Get master ID
        let master_id = state.get_master(my_node_id).ok_or_else(|| {
            AikvError::InvalidArgument("Cannot find master for this replica".to_string())
        })?;

        // Check if master is connected (for non-takeover modes)
        let master_connected = state.nodes.get(&master_id).is_some_and(|m| m.is_connected);

        match mode {
            FailoverMode::Default if !master_connected => {
                return Err(AikvError::InvalidArgument(
                    "Master is disconnected. Use FORCE or TAKEOVER option".to_string(),
                ));
            }
            FailoverMode::Force if !master_connected => {
                // FORCE mode can proceed even if master is disconnected
                // but it will wait for the master to come back
                // For our implementation, we'll proceed with failover
            }
            FailoverMode::Takeover => {
                // TAKEOVER mode proceeds immediately regardless of master state
            }
            _ => {}
        }

        // Perform the failover
        state
            .promote_replica(my_node_id)
            .map_err(AikvError::InvalidArgument)?;

        Ok(RespValue::simple_string("OK"))
    }

    /// CLUSTER REPLICAS node-id / CLUSTER SLAVES node-id
    ///
    /// Returns the list of replica nodes for the given master node.
    ///
    /// # Arguments
    ///
    /// * `args` - Should contain exactly one argument: the master node ID (40-char hex)
    ///
    /// # Returns
    ///
    /// An array of replica node information in CLUSTER NODES format
    fn replicas(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CLUSTER REPLICAS".to_string()));
        }

        let master_node_id_str = String::from_utf8_lossy(&args[0]).to_string();
        let master_node_id = u64::from_str_radix(&master_node_id_str, 16)
            .map_err(|_| AikvError::InvalidArgument("Invalid node ID".to_string()))?;

        let state = self.state.read().unwrap();

        // Check if master node exists and is a master
        if !state.nodes.contains_key(&master_node_id) {
            return Err(AikvError::InvalidArgument(format!(
                "Unknown node {}",
                master_node_id_str
            )));
        }

        if !state.is_master(master_node_id) {
            return Err(AikvError::InvalidArgument(format!(
                "Node {} is not a master",
                master_node_id_str
            )));
        }

        // Get replicas
        let replica_ids = state.get_replicas(master_node_id);

        let mut result = Vec::new();
        for replica_id in replica_ids {
            if let Some(info) = state.nodes.get(&replica_id) {
                let status = if info.is_connected {
                    "connected"
                } else {
                    "disconnected"
                };
                let node_line = format!(
                    "{:040x} {}@{} slave {:040x} 0 0 {} {}",
                    replica_id,
                    info.addr,
                    info.cluster_port,
                    master_node_id,
                    state.config_epoch,
                    status
                );
                result.push(RespValue::bulk_string(Bytes::from(node_line)));
            }
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// CLUSTER HELP
    ///
    /// Returns help text for CLUSTER commands.
    fn help(&self) -> Result<RespValue> {
        let help_lines = vec![
            RespValue::bulk_string(Bytes::from("CLUSTER KEYSLOT <key>")),
            RespValue::bulk_string(Bytes::from("    Return the hash slot for <key>.")),
            RespValue::bulk_string(Bytes::from("CLUSTER INFO")),
            RespValue::bulk_string(Bytes::from("    Return information about the cluster.")),
            RespValue::bulk_string(Bytes::from("CLUSTER NODES")),
            RespValue::bulk_string(Bytes::from(
                "    Return information about the cluster nodes.",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER SLOTS")),
            RespValue::bulk_string(Bytes::from(
                "    Return information about slot-to-node mapping.",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER MYID")),
            RespValue::bulk_string(Bytes::from("    Return the node ID.")),
            RespValue::bulk_string(Bytes::from(
                "CLUSTER MEET <ip> <port> [<bus-port>] [<node-id>]",
            )),
            RespValue::bulk_string(Bytes::from(
                "    Add a node to the cluster. Optionally specify node-id (40-char hex).",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER FORGET <node-id>")),
            RespValue::bulk_string(Bytes::from("    Remove a node from the cluster.")),
            RespValue::bulk_string(Bytes::from("CLUSTER ADDSLOTS <slot> [<slot> ...]")),
            RespValue::bulk_string(Bytes::from("    Assign slots to this node.")),
            RespValue::bulk_string(Bytes::from("CLUSTER DELSLOTS <slot> [<slot> ...]")),
            RespValue::bulk_string(Bytes::from("    Remove slot assignments.")),
            RespValue::bulk_string(Bytes::from(
                "CLUSTER SETSLOT <slot> IMPORTING|MIGRATING|NODE|STABLE [<node-id>]",
            )),
            RespValue::bulk_string(Bytes::from("    Set slot state or assign to node.")),
            RespValue::bulk_string(Bytes::from("CLUSTER GETKEYSINSLOT <slot> <count>")),
            RespValue::bulk_string(Bytes::from(
                "    Return up to <count> keys belonging to the specified slot.",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER COUNTKEYSINSLOT <slot>")),
            RespValue::bulk_string(Bytes::from(
                "    Return the number of keys in the specified slot.",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER REPLICATE <node-id>")),
            RespValue::bulk_string(Bytes::from(
                "    Configure this node as replica of the specified master.",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER FAILOVER [FORCE|TAKEOVER]")),
            RespValue::bulk_string(Bytes::from(
                "    Trigger a manual failover of this replica to become master.",
            )),
            RespValue::bulk_string(Bytes::from("CLUSTER REPLICAS <node-id>")),
            RespValue::bulk_string(Bytes::from(
                "    Return the list of replicas for the specified master.",
            )),
        ];

        Ok(RespValue::Array(Some(help_lines)))
    }

    /// Generate a -MOVED error response.
    ///
    /// This is used when a client sends a command for a key that belongs
    /// to a different node in the cluster.
    ///
    /// # Arguments
    ///
    /// * `slot` - The slot number the key belongs to
    /// * `addr` - The address of the node that owns the slot (e.g., "127.0.0.1:6379")
    ///
    /// # Returns
    ///
    /// A RESP error value with the MOVED redirect
    pub fn moved_error(slot: u16, addr: &str) -> RespValue {
        RespValue::Error(format!("MOVED {} {}", slot, addr))
    }

    /// Generate an -ASK error response.
    ///
    /// This is used during slot migration when a key is being moved
    /// from one node to another.
    ///
    /// # Arguments
    ///
    /// * `slot` - The slot number the key belongs to
    /// * `addr` - The address of the target node
    ///
    /// # Returns
    ///
    /// A RESP error value with the ASK redirect
    pub fn ask_error(slot: u16, addr: &str) -> RespValue {
        RespValue::Error(format!("ASK {} {}", slot, addr))
    }

    /// Check if a key should be redirected to another node.
    ///
    /// # Arguments
    ///
    /// * `key` - The key to check
    /// * `local_slots` - The slots owned by this node (if available)
    ///
    /// # Returns
    ///
    /// None if the key should be handled locally, or Some(slot, addr) if redirected
    #[allow(unused_variables)]
    pub fn check_redirect(&self, key: &[u8], local_slots: &[bool]) -> Option<(u16, String)> {
        let slot = self.router.key_to_slot(key);

        // TODO: Implement actual redirect logic when cluster routing is available
        #[cfg(feature = "cluster")]
        {
            if let Some(addr) = self.router.get_slot_leader_address(slot) {
                return Some((slot, addr));
            }
        }

        // For now, no redirect needed
        None
    }

    /// Check if a key should be redirected, considering migration state.
    ///
    /// This method implements the full Redis Cluster redirect logic including:
    /// - -MOVED redirect when the slot belongs to another node
    /// - -ASK redirect when the slot is being migrated
    ///
    /// # Arguments
    ///
    /// * `key` - The key to check
    /// * `key_exists_locally` - Whether the key exists in local storage
    ///
    /// # Returns
    ///
    /// None if the key should be handled locally, or Some((redirect_type, slot, addr))
    pub fn check_redirect_with_migration(
        &self,
        key: &[u8],
        key_exists_locally: bool,
    ) -> Option<(RedirectType, u16, String)> {
        let slot = self.router.key_to_slot(key);
        let state = self.state.read().ok()?;

        // Get the node that owns this slot
        let slot_owner = state.slot_assignments.get(slot as usize)?.as_ref()?;
        let my_node_id = self.node_id?;

        // Check if we own this slot
        if *slot_owner == my_node_id {
            // We own this slot, but check if it's being migrated
            if state.is_slot_migrating(slot) && !key_exists_locally {
                // Slot is migrating and key doesn't exist locally
                // Return -ASK redirect to the migration target
                if let Some(target_node) = state.get_migration_target(slot) {
                    if let Some(node_info) = state.nodes.get(&target_node) {
                        return Some((RedirectType::Ask, slot, node_info.addr.clone()));
                    }
                }
            }
            // Key should be handled locally
            return None;
        }

        // We don't own this slot
        // Check if we're importing this slot
        if state.is_slot_importing(slot) {
            // We're importing this slot - the key might be here or at the source
            // This situation typically occurs when:
            // 1. Client sent a request without ASKING first
            // 2. The key may or may not have been migrated yet
            //
            // According to Redis Cluster spec, we should return -MOVED to the
            // slot owner because the client should use ASKING if they want to
            // access an importing slot.
            //
            // The key will be handled locally only if:
            // - The client sent ASKING first (checked elsewhere via should_handle_after_asking)
            // - AND the key exists locally
        }

        // Generate -MOVED redirect to the slot owner
        if let Some(node_info) = state.nodes.get(slot_owner) {
            return Some((RedirectType::Moved, slot, node_info.addr.clone()));
        }

        None
    }

    /// Check if we should handle a request after receiving ASKING command.
    ///
    /// When a client sends ASKING followed by a command, we should handle
    /// the command locally even if the slot is being imported.
    ///
    /// # Arguments
    ///
    /// * `key` - The key being accessed
    ///
    /// # Returns
    ///
    /// true if the command should be handled locally, false otherwise
    pub fn should_handle_after_asking(&self, key: &[u8]) -> bool {
        let slot = self.router.key_to_slot(key);
        let state = match self.state.read() {
            Ok(s) => s,
            Err(_) => return false,
        };

        // If we're importing this slot, allow access after ASKING
        state.is_slot_importing(slot)
    }

    /// Start a migration for a slot.
    ///
    /// This sets up the migration state for both source and target nodes.
    /// The source node marks the slot as MIGRATING, and the target node
    /// marks it as IMPORTING.
    ///
    /// # Arguments
    ///
    /// * `slot` - The slot to migrate
    /// * `target_node_id` - The ID of the target node
    ///
    /// # Returns
    ///
    /// Ok(()) if migration was started, Err if there was an error
    pub fn start_migration(&self, slot: u16, target_node_id: u64) -> Result<()> {
        if slot >= TOTAL_SLOTS {
            return Err(AikvError::InvalidArgument(format!(
                "Invalid slot {} (out of range 0-{})",
                slot,
                TOTAL_SLOTS - 1
            )));
        }

        let my_node_id = self.node_id.ok_or_else(|| {
            AikvError::InvalidCommand("Node ID not set for this cluster node".to_string())
        })?;

        let mut state = self
            .state
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        // Verify we own this slot
        let slot_owner = state.slot_assignments.get(slot as usize).and_then(|s| *s);
        if slot_owner != Some(my_node_id) {
            return Err(AikvError::InvalidArgument(format!(
                "Cannot migrate slot {} - not owned by this node",
                slot
            )));
        }

        // Set migration state
        state.slot_states.insert(slot, SlotState::Migrating);
        state.migration_targets.insert(slot, target_node_id);

        // Initialize migration progress
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        state.migration_progress.insert(
            slot,
            MigrationProgress {
                source_node: my_node_id,
                target_node: target_node_id,
                keys_migrated: 0,
                total_keys: 0,
                start_time: now,
            },
        );

        state.config_epoch += 1;
        Ok(())
    }

    /// Complete a migration for a slot.
    ///
    /// This finalizes the migration by updating slot ownership and clearing
    /// migration state.
    ///
    /// # Arguments
    ///
    /// * `slot` - The slot that was migrated
    /// * `new_owner` - The ID of the new owner node
    ///
    /// # Returns
    ///
    /// Ok(()) if migration was completed, Err if there was an error
    pub fn complete_migration(&self, slot: u16, new_owner: u64) -> Result<()> {
        if slot >= TOTAL_SLOTS {
            return Err(AikvError::InvalidArgument(format!(
                "Invalid slot {} (out of range 0-{})",
                slot,
                TOTAL_SLOTS - 1
            )));
        }

        let mut state = self
            .state
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        // Update slot assignment
        state.slot_assignments[slot as usize] = Some(new_owner);

        // Clear migration state
        state.slot_states.remove(&slot);
        state.migration_targets.remove(&slot);
        state.migration_progress.remove(&slot);

        state.config_epoch += 1;
        Ok(())
    }

    /// Get migration progress for a slot.
    ///
    /// # Arguments
    ///
    /// * `slot` - The slot to check
    ///
    /// # Returns
    ///
    /// Some(MigrationProgress) if the slot is being migrated, None otherwise
    pub fn get_migration_progress(&self, slot: u16) -> Option<MigrationProgress> {
        let state = self.state.read().ok()?;
        state.migration_progress.get(&slot).cloned()
    }
}

impl Default for ClusterCommands {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_keyslot() {
        let cmd = ClusterCommands::new();

        // Test KEYSLOT command
        let result = cmd.execute(&[Bytes::from("KEYSLOT"), Bytes::from("foo")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Integer(slot)) = result {
            assert!((0..16384).contains(&slot));
        } else {
            panic!("Expected integer response");
        }
    }

    #[test]
    fn test_cluster_keyslot_hash_tag() {
        let cmd = ClusterCommands::new();

        // Keys with hash tags should return valid slots
        let result1 = cmd.execute(&[Bytes::from("KEYSLOT"), Bytes::from("{user}name")]);
        let result2 = cmd.execute(&[Bytes::from("KEYSLOT"), Bytes::from("{user}age")]);

        let slot1 = match result1 {
            Ok(RespValue::Integer(s)) => s,
            _ => panic!("Expected integer"),
        };
        let slot2 = match result2 {
            Ok(RespValue::Integer(s)) => s,
            _ => panic!("Expected integer"),
        };

        // Both slots should be in valid range
        assert!((0..16384).contains(&slot1));
        assert!((0..16384).contains(&slot2));

        // Note: Hash tag handling depends on AiDb implementation when cluster feature is enabled
        // When not using cluster feature, our fallback implementation handles hash tags
        #[cfg(not(feature = "cluster"))]
        {
            assert_eq!(slot1, slot2);
        }
    }

    #[test]
    fn test_cluster_info() {
        let cmd = ClusterCommands::new();
        let result = cmd.execute(&[Bytes::from("INFO")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_nodes() {
        let cmd = ClusterCommands::new();
        let result = cmd.execute(&[Bytes::from("NODES")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_myid() {
        let cmd = ClusterCommands::new();
        let result = cmd.execute(&[Bytes::from("MYID")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_help() {
        let cmd = ClusterCommands::new();
        let result = cmd.execute(&[Bytes::from("HELP")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_unknown_subcommand() {
        let cmd = ClusterCommands::new();
        let result = cmd.execute(&[Bytes::from("UNKNOWN")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_moved_error() {
        let error = ClusterCommands::moved_error(12345, "127.0.0.1:7000");
        if let RespValue::Error(msg) = error {
            assert!(msg.contains("MOVED"));
            assert!(msg.contains("12345"));
            assert!(msg.contains("127.0.0.1:7000"));
        } else {
            panic!("Expected error response");
        }
    }

    #[test]
    fn test_ask_error() {
        let error = ClusterCommands::ask_error(12345, "127.0.0.1:7001");
        if let RespValue::Error(msg) = error {
            assert!(msg.contains("ASK"));
            assert!(msg.contains("12345"));
            assert!(msg.contains("127.0.0.1:7001"));
        } else {
            panic!("Expected error response");
        }
    }

    #[test]
    fn test_cluster_meet() {
        let cmd = ClusterCommands::with_node_id(1);

        // Test MEET command
        let result = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ]);
        assert!(result.is_ok());

        // Verify node was added
        let state = cmd.state();
        let state = state.read().unwrap();
        assert!(state.nodes.len() >= 2); // Self + new node
    }

    #[test]
    fn test_cluster_meet_with_cluster_port() {
        let cmd = ClusterCommands::with_node_id(1);

        // Test MEET with explicit cluster port
        let result = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
            Bytes::from("16380"),
        ]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_meet_with_node_id() {
        let cmd = ClusterCommands::with_node_id(1);

        // Test MEET with explicit node ID
        let target_node_id = 999u64;
        let result = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
            Bytes::from(format!("{:040x}", target_node_id)),
        ]);
        assert!(result.is_ok());

        // Verify node was added with the specified ID
        let state = cmd.state();
        let state = state.read().unwrap();
        assert!(state.nodes.contains_key(&target_node_id));
        let node_info = state.nodes.get(&target_node_id).unwrap();
        assert_eq!(node_info.addr, "192.168.1.100:6380");
    }

    #[test]
    fn test_cluster_meet_with_cluster_port_and_node_id() {
        let cmd = ClusterCommands::with_node_id(1);

        // Test MEET with both cluster port and node ID
        // When the last arg is a 40-char hex, it should be treated as node ID
        let target_node_id = 888u64;
        let result = cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
            Bytes::from(format!("{:040x}", target_node_id)),
        ]);
        assert!(result.is_ok());

        // Verify node was added with the specified ID
        let state = cmd.state();
        let state = state.read().unwrap();
        assert!(state.nodes.contains_key(&target_node_id));
    }

    #[test]
    fn test_cluster_meet_wrong_args() {
        let cmd = ClusterCommands::with_node_id(1);

        // Missing port
        let result = cmd.execute(&[Bytes::from("MEET"), Bytes::from("192.168.1.100")]);
        assert!(result.is_err());

        // No args
        let result = cmd.execute(&[Bytes::from("MEET")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_forget() {
        let cmd = ClusterCommands::with_node_id(1);

        // First add a node
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ])
        .unwrap();

        // Get the node ID of the added node
        let node_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Forget the node
        let result = cmd.execute(&[
            Bytes::from("FORGET"),
            Bytes::from(format!("{:040x}", node_id)),
        ]);
        assert!(result.is_ok());

        // Verify node was removed
        let state = cmd.state();
        let state = state.read().unwrap();
        assert!(!state.nodes.contains_key(&node_id));
    }

    #[test]
    fn test_cluster_forget_self() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to forget self - should fail
        let result = cmd.execute(&[Bytes::from("FORGET"), Bytes::from(format!("{:040x}", 1u64))]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_forget_unknown() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to forget unknown node
        let result = cmd.execute(&[
            Bytes::from("FORGET"),
            Bytes::from("0000000000000000000000000000000000000999"),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_addslots() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add some slots
        let result = cmd.execute(&[
            Bytes::from("ADDSLOTS"),
            Bytes::from("0"),
            Bytes::from("1"),
            Bytes::from("2"),
        ]);
        assert!(result.is_ok());

        // Verify slots were added
        let state = cmd.state();
        let state = state.read().unwrap();
        assert_eq!(state.slot_assignments[0], Some(1));
        assert_eq!(state.slot_assignments[1], Some(1));
        assert_eq!(state.slot_assignments[2], Some(1));
    }

    #[test]
    fn test_cluster_addslots_already_assigned() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add slot 0
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("0")])
            .unwrap();

        // Create another node and try to add the same slot
        let cmd2 = ClusterCommands::with_shared_state(Some(2), cmd.state());
        let result = cmd2.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("0")]);
        assert!(result.is_err()); // Should fail - slot already busy
    }

    #[test]
    fn test_cluster_addslots_invalid_slot() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to add invalid slot
        let result = cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("99999")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_delslots() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add then delete slots
        cmd.execute(&[
            Bytes::from("ADDSLOTS"),
            Bytes::from("0"),
            Bytes::from("1"),
            Bytes::from("2"),
        ])
        .unwrap();

        let result = cmd.execute(&[Bytes::from("DELSLOTS"), Bytes::from("0"), Bytes::from("1")]);
        assert!(result.is_ok());

        // Verify slots were removed
        let state = cmd.state();
        let state = state.read().unwrap();
        assert_eq!(state.slot_assignments[0], None);
        assert_eq!(state.slot_assignments[1], None);
        assert_eq!(state.slot_assignments[2], Some(1)); // This one should still be assigned
    }

    #[test]
    fn test_cluster_setslot_node() {
        let cmd = ClusterCommands::with_node_id(1);

        // Assign slot 100 to node 1
        let result = cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("NODE"),
            Bytes::from(format!("{:040x}", 1u64)),
        ]);
        assert!(result.is_ok());

        // Verify slot was assigned
        let state = cmd.state();
        let state = state.read().unwrap();
        assert_eq!(state.slot_assignments[100], Some(1));
    }

    #[test]
    fn test_cluster_setslot_migrating() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a slot first
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("100")])
            .unwrap();

        // Set slot as migrating
        let target_node_id = 2u64;
        let result = cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("MIGRATING"),
            Bytes::from(format!("{:040x}", target_node_id)),
        ]);
        assert!(result.is_ok());

        // Verify migration state
        let state = cmd.state();
        let state = state.read().unwrap();
        assert_eq!(state.slot_states.get(&100), Some(&SlotState::Migrating));
        assert_eq!(state.migration_targets.get(&100), Some(&target_node_id));
    }

    #[test]
    fn test_cluster_setslot_importing() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set slot as importing
        let source_node_id = 2u64;
        let result = cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("IMPORTING"),
            Bytes::from(format!("{:040x}", source_node_id)),
        ]);
        assert!(result.is_ok());

        // Verify import state
        let state = cmd.state();
        let state = state.read().unwrap();
        assert_eq!(state.slot_states.get(&100), Some(&SlotState::Importing));
        assert_eq!(state.migration_targets.get(&100), Some(&source_node_id));
    }

    #[test]
    fn test_cluster_setslot_stable() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set up a migration first
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("100")])
            .unwrap();
        cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("MIGRATING"),
            Bytes::from(format!("{:040x}", 2u64)),
        ])
        .unwrap();

        // Clear migration with STABLE
        let result = cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("STABLE"),
        ]);
        assert!(result.is_ok());

        // Verify migration state was cleared
        let state = cmd.state();
        let state = state.read().unwrap();
        assert!(!state.slot_states.contains_key(&100));
        assert!(!state.migration_targets.contains_key(&100));
    }

    #[test]
    fn test_cluster_setslot_invalid_subcommand() {
        let cmd = ClusterCommands::with_node_id(1);

        let result = cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("INVALID"),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_slots_after_addslots() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add some slots
        cmd.execute(&[
            Bytes::from("ADDSLOTS"),
            Bytes::from("0"),
            Bytes::from("1"),
            Bytes::from("2"),
            Bytes::from("100"),
            Bytes::from("101"),
        ])
        .unwrap();

        // Get SLOTS response
        let result = cmd.execute(&[Bytes::from("SLOTS")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Array(Some(slots))) = result {
            // Should have 2 ranges: 0-2 and 100-101
            assert_eq!(slots.len(), 2);
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_cluster_info_with_slots() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add all slots (16384)
        {
            let state = cmd.state();
            let mut state = state.write().unwrap();
            for i in 0..16384u16 {
                state.slot_assignments[i as usize] = Some(1);
            }
        }

        // Check cluster info shows ok state
        let result = cmd.execute(&[Bytes::from("INFO")]);
        assert!(result.is_ok());

        if let Ok(RespValue::BulkString(Some(info))) = result {
            let info_str = String::from_utf8_lossy(&info);
            assert!(info_str.contains("cluster_state:ok"));
            assert!(info_str.contains("cluster_slots_assigned:16384"));
        } else {
            panic!("Expected bulk string response");
        }
    }

    #[test]
    fn test_cluster_nodes_format() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add some slots
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("0"), Bytes::from("1")])
            .unwrap();

        // Get NODES response
        let result = cmd.execute(&[Bytes::from("NODES")]);
        assert!(result.is_ok());

        if let Ok(RespValue::BulkString(Some(nodes))) = result {
            let nodes_str = String::from_utf8_lossy(&nodes);
            // Should contain myself flag
            assert!(nodes_str.contains("myself"));
            // Should contain master flag
            assert!(nodes_str.contains("master"));
            // Should contain connected
            assert!(nodes_str.contains("connected"));
            // Should contain slot range
            assert!(nodes_str.contains("0-1"));
        } else {
            panic!("Expected bulk string response");
        }
    }

    // ========== Stage C: Slot Migration Tests ==========

    #[test]
    fn test_cluster_getkeysinslot_without_scanner() {
        let cmd = ClusterCommands::with_node_id(1);

        // Without a key scanner, should return empty array
        let result = cmd.execute(&[
            Bytes::from("GETKEYSINSLOT"),
            Bytes::from("0"),
            Bytes::from("10"),
        ]);
        assert!(result.is_ok());

        if let Ok(RespValue::Array(Some(keys))) = result {
            assert!(keys.is_empty());
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_cluster_getkeysinslot_with_scanner() {
        let mut cmd = ClusterCommands::with_node_id(1);

        // Set up a mock key scanner that returns predefined keys for slot 0
        cmd.set_key_scanner(Box::new(|_db_index, slot, count| {
            if slot == 0 {
                vec!["key1", "key2", "key3"]
                    .into_iter()
                    .take(count)
                    .map(|s| s.to_string())
                    .collect()
            } else {
                vec![]
            }
        }));

        // Get keys in slot 0
        let result = cmd.execute(&[
            Bytes::from("GETKEYSINSLOT"),
            Bytes::from("0"),
            Bytes::from("10"),
        ]);
        assert!(result.is_ok());

        if let Ok(RespValue::Array(Some(keys))) = result {
            assert_eq!(keys.len(), 3);
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_cluster_getkeysinslot_with_count_limit() {
        let mut cmd = ClusterCommands::with_node_id(1);

        // Set up a mock key scanner
        cmd.set_key_scanner(Box::new(|_db_index, slot, count| {
            if slot == 0 {
                vec!["key1", "key2", "key3", "key4", "key5"]
                    .into_iter()
                    .take(count)
                    .map(|s| s.to_string())
                    .collect()
            } else {
                vec![]
            }
        }));

        // Get only 2 keys
        let result = cmd.execute(&[
            Bytes::from("GETKEYSINSLOT"),
            Bytes::from("0"),
            Bytes::from("2"),
        ]);
        assert!(result.is_ok());

        if let Ok(RespValue::Array(Some(keys))) = result {
            assert_eq!(keys.len(), 2);
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_cluster_getkeysinslot_invalid_slot() {
        let cmd = ClusterCommands::with_node_id(1);

        // Invalid slot number
        let result = cmd.execute(&[
            Bytes::from("GETKEYSINSLOT"),
            Bytes::from("99999"),
            Bytes::from("10"),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_getkeysinslot_wrong_args() {
        let cmd = ClusterCommands::with_node_id(1);

        // Missing count
        let result = cmd.execute(&[Bytes::from("GETKEYSINSLOT"), Bytes::from("0")]);
        assert!(result.is_err());

        // Missing slot and count
        let result = cmd.execute(&[Bytes::from("GETKEYSINSLOT")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_countkeysinslot_without_scanner() {
        let cmd = ClusterCommands::with_node_id(1);

        // Without a key scanner, should return 0
        let result = cmd.execute(&[Bytes::from("COUNTKEYSINSLOT"), Bytes::from("0")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Integer(count)) = result {
            assert_eq!(count, 0);
        } else {
            panic!("Expected integer response");
        }
    }

    #[test]
    fn test_cluster_countkeysinslot_with_scanner() {
        let mut cmd = ClusterCommands::with_node_id(1);

        // Set up a mock key scanner
        cmd.set_key_scanner(Box::new(|_db_index, slot, _count| {
            if slot == 100 {
                vec!["a", "b", "c", "d", "e"]
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                vec![]
            }
        }));

        // Count keys in slot 100
        let result = cmd.execute(&[Bytes::from("COUNTKEYSINSLOT"), Bytes::from("100")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Integer(count)) = result {
            assert_eq!(count, 5);
        } else {
            panic!("Expected integer response");
        }
    }

    #[test]
    fn test_cluster_countkeysinslot_invalid_slot() {
        let cmd = ClusterCommands::with_node_id(1);

        // Invalid slot number
        let result = cmd.execute(&[Bytes::from("COUNTKEYSINSLOT"), Bytes::from("99999")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_countkeysinslot_with_counter() {
        let mut cmd = ClusterCommands::with_node_id(1);

        // Set up a dedicated key counter (more efficient than scanner)
        cmd.set_key_counter(Box::new(|_db_index, slot| {
            if slot == 42 {
                100 // Return 100 keys for slot 42
            } else {
                0
            }
        }));

        // Count keys in slot 42
        let result = cmd.execute(&[Bytes::from("COUNTKEYSINSLOT"), Bytes::from("42")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Integer(count)) = result {
            assert_eq!(count, 100);
        } else {
            panic!("Expected integer response");
        }
    }

    #[test]
    fn test_cluster_countkeysinslot_counter_priority() {
        let mut cmd = ClusterCommands::with_node_id(1);

        // Set up both scanner and counter - counter should be used
        cmd.set_key_scanner(Box::new(|_db_index, slot, _count| {
            if slot == 50 {
                vec!["a", "b", "c"] // 3 keys via scanner
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                vec![]
            }
        }));

        cmd.set_key_counter(Box::new(|_db_index, slot| {
            if slot == 50 {
                10 // Counter says 10 keys
            } else {
                0
            }
        }));

        // Counter should take priority
        let result = cmd.execute(&[Bytes::from("COUNTKEYSINSLOT"), Bytes::from("50")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Integer(count)) = result {
            assert_eq!(count, 10); // Should use counter, not scanner
        } else {
            panic!("Expected integer response");
        }
    }

    #[test]
    fn test_migration_state_query() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set slot as migrating
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("100")])
            .unwrap();
        cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("100"),
            Bytes::from("MIGRATING"),
            Bytes::from(format!("{:040x}", 2u64)),
        ])
        .unwrap();

        // Query migration state
        let state = cmd.state();
        let state_guard = state.read().unwrap();

        assert!(state_guard.is_slot_migrating(100));
        assert!(!state_guard.is_slot_importing(100));
        assert_eq!(state_guard.get_migration_target(100), Some(2));
        assert_eq!(state_guard.get_import_source(100), None);

        // Check migration state query
        let migration_state = state_guard.get_migration_state(100);
        assert!(migration_state.is_some());
        let (slot_state, target) = migration_state.unwrap();
        assert_eq!(slot_state, SlotState::Migrating);
        assert_eq!(target, 2);
    }

    #[test]
    fn test_import_state_query() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set slot as importing
        cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("200"),
            Bytes::from("IMPORTING"),
            Bytes::from(format!("{:040x}", 3u64)),
        ])
        .unwrap();

        // Query import state
        let state = cmd.state();
        let state_guard = state.read().unwrap();

        assert!(!state_guard.is_slot_migrating(200));
        assert!(state_guard.is_slot_importing(200));
        assert_eq!(state_guard.get_migration_target(200), None);
        assert_eq!(state_guard.get_import_source(200), Some(3));
    }

    #[test]
    fn test_start_migration() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add slot first
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("50")])
            .unwrap();

        // Start migration
        let result = cmd.start_migration(50, 2);
        assert!(result.is_ok());

        // Verify migration state
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(state_guard.is_slot_migrating(50));
        assert_eq!(state_guard.get_migration_target(50), Some(2));

        // Check progress
        drop(state_guard);
        let progress = cmd.get_migration_progress(50);
        assert!(progress.is_some());
        let prog = progress.unwrap();
        assert_eq!(prog.source_node, 1);
        assert_eq!(prog.target_node, 2);
        assert_eq!(prog.keys_migrated, 0);
    }

    #[test]
    fn test_start_migration_not_owner() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to migrate a slot we don't own
        let result = cmd.start_migration(50, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_complete_migration() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add slot and start migration
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("60")])
            .unwrap();
        cmd.start_migration(60, 2).unwrap();

        // Complete migration
        let result = cmd.complete_migration(60, 2);
        assert!(result.is_ok());

        // Verify migration completed
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(!state_guard.is_slot_migrating(60));
        assert_eq!(state_guard.slot_assignments[60], Some(2));

        // No more progress info
        drop(state_guard);
        assert!(cmd.get_migration_progress(60).is_none());
    }

    #[test]
    fn test_ask_redirect_logic() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set up slot 500 owned by this node, migrating to node 2
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("500")])
            .unwrap();

        // Add node 2
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ])
        .unwrap();

        // Get node 2's ID
        let node2_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Start migration
        cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("500"),
            Bytes::from("MIGRATING"),
            Bytes::from(format!("{:040x}", node2_id)),
        ])
        .unwrap();

        // Test ASK redirect - when key doesn't exist locally, should redirect
        // Note: This is a simplified test since we can't test with actual key existence
        let _redirect = cmd.check_redirect_with_migration(b"test_key", false);

        // The redirect might be None if slot 500 is not where "test_key" hashes to
        // Let's verify the migration state is correct
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(state_guard.is_slot_migrating(500));
    }

    #[test]
    fn test_should_handle_after_asking() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set slot as importing
        cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("300"),
            Bytes::from("IMPORTING"),
            Bytes::from(format!("{:040x}", 2u64)),
        ])
        .unwrap();

        // For a key that hashes to slot 300, we should handle after ASKING
        // Since we don't know which key hashes to 300, we'll just test the method exists
        // In real usage, the server would check the slot of the key

        // Verify state is set correctly
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(state_guard.is_slot_importing(300));
    }

    #[test]
    fn test_migration_progress_tracking() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add slot and start migration
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("700")])
            .unwrap();
        cmd.start_migration(700, 2).unwrap();

        // Get progress
        let progress = cmd.get_migration_progress(700).unwrap();
        assert_eq!(progress.source_node, 1);
        assert_eq!(progress.target_node, 2);
        assert!(progress.start_time > 0);

        // Non-migrating slot should have no progress
        assert!(cmd.get_migration_progress(701).is_none());
    }

    #[test]
    fn test_cluster_setslot_stable_clears_progress() {
        let cmd = ClusterCommands::with_node_id(1);

        // Set up migration
        cmd.execute(&[Bytes::from("ADDSLOTS"), Bytes::from("800")])
            .unwrap();
        cmd.start_migration(800, 2).unwrap();

        // Verify progress exists
        assert!(cmd.get_migration_progress(800).is_some());

        // Clear with STABLE
        cmd.execute(&[
            Bytes::from("SETSLOT"),
            Bytes::from("800"),
            Bytes::from("STABLE"),
        ])
        .unwrap();

        // Verify progress is cleared
        assert!(cmd.get_migration_progress(800).is_none());

        // Verify migration state is cleared
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(!state_guard.is_slot_migrating(800));
    }

    #[test]
    fn test_help_includes_new_commands() {
        let cmd = ClusterCommands::new();
        let result = cmd.execute(&[Bytes::from("HELP")]);
        assert!(result.is_ok());

        if let Ok(RespValue::Array(Some(lines))) = result {
            let help_text: String = lines
                .iter()
                .filter_map(|v| {
                    if let RespValue::BulkString(Some(s)) = v {
                        Some(String::from_utf8_lossy(s).to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            // Verify new commands are in help
            assert!(help_text.contains("GETKEYSINSLOT"));
            assert!(help_text.contains("COUNTKEYSINSLOT"));
            assert!(help_text.contains("REPLICATE"));
            assert!(help_text.contains("FAILOVER"));
            assert!(help_text.contains("REPLICAS"));
        } else {
            panic!("Expected array response");
        }
    }

    // ========== Stage D: High Availability Tests ==========

    #[test]
    fn test_cluster_replicate() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add another node (master) to the cluster
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ])
        .unwrap();

        // Get the master node ID
        let master_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Replicate the master
        let result = cmd.execute(&[
            Bytes::from("REPLICATE"),
            Bytes::from(format!("{:040x}", master_id)),
        ]);
        assert!(result.is_ok());

        // Verify this node is now a replica
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(state_guard.is_replica(1));
        assert!(!state_guard.is_master(1));
        assert_eq!(state_guard.get_master(1), Some(master_id));
    }

    #[test]
    fn test_cluster_replicate_self_error() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to replicate self - should fail
        let result = cmd.execute(&[
            Bytes::from("REPLICATE"),
            Bytes::from(format!("{:040x}", 1u64)),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_replicate_unknown_node() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to replicate unknown node - should fail
        let result = cmd.execute(&[
            Bytes::from("REPLICATE"),
            Bytes::from("0000000000000000000000000000000000000999"),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_replicate_wrong_args() {
        let cmd = ClusterCommands::with_node_id(1);

        // No args
        let result = cmd.execute(&[Bytes::from("REPLICATE")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_failover() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a master node
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ])
        .unwrap();

        // Get the master node ID
        let master_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Add slots to master
        {
            let state = cmd.state();
            let mut state_guard = state.write().unwrap();
            for i in 0..1000u16 {
                state_guard.slot_assignments[i as usize] = Some(master_id);
            }
        }

        // Make node 1 a replica of the master
        cmd.execute(&[
            Bytes::from("REPLICATE"),
            Bytes::from(format!("{:040x}", master_id)),
        ])
        .unwrap();

        // Perform failover
        let result = cmd.execute(&[Bytes::from("FAILOVER")]);
        assert!(result.is_ok());

        // Verify node 1 is now master
        let state = cmd.state();
        let state_guard = state.read().unwrap();
        assert!(state_guard.is_master(1));
        assert!(state_guard.is_replica(master_id));

        // Verify slots transferred
        assert_eq!(state_guard.slot_assignments[0], Some(1));
    }

    #[test]
    fn test_cluster_failover_force() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a master node
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ])
        .unwrap();

        // Get the master node ID
        let master_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Mark master as disconnected
        {
            let state = cmd.state();
            let mut state_guard = state.write().unwrap();
            if let Some(master) = state_guard.nodes.get_mut(&master_id) {
                master.is_connected = false;
            }
        }

        // Make node 1 a replica
        cmd.execute(&[
            Bytes::from("REPLICATE"),
            Bytes::from(format!("{:040x}", master_id)),
        ])
        .unwrap();

        // Default failover should fail (master disconnected)
        let result = cmd.execute(&[Bytes::from("FAILOVER")]);
        assert!(result.is_err());

        // FORCE failover should succeed
        let result = cmd.execute(&[Bytes::from("FAILOVER"), Bytes::from("FORCE")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_failover_takeover() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a master node
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.100"),
            Bytes::from("6380"),
        ])
        .unwrap();

        // Get the master node ID
        let master_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Make node 1 a replica
        cmd.execute(&[
            Bytes::from("REPLICATE"),
            Bytes::from(format!("{:040x}", master_id)),
        ])
        .unwrap();

        // TAKEOVER failover should succeed regardless of master state
        let result = cmd.execute(&[Bytes::from("FAILOVER"), Bytes::from("TAKEOVER")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cluster_failover_not_replica() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try failover on a master node - should fail
        let result = cmd.execute(&[Bytes::from("FAILOVER")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_failover_invalid_option() {
        let cmd = ClusterCommands::with_node_id(1);

        // Invalid option
        let result = cmd.execute(&[Bytes::from("FAILOVER"), Bytes::from("INVALID")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_replicas() {
        let cmd = ClusterCommands::with_node_id(1);

        // Add a replica node
        cmd.execute(&[
            Bytes::from("MEET"),
            Bytes::from("192.168.1.101"),
            Bytes::from("6381"),
        ])
        .unwrap();

        // Get the replica node ID
        let replica_id: u64 = {
            let state = cmd.state();
            let state = state.read().unwrap();
            *state.nodes.keys().find(|&&id| id != 1).unwrap()
        };

        // Create a shared state handler for the replica
        let cmd_replica = ClusterCommands::with_shared_state(Some(replica_id), cmd.state());

        // Make replica replicate master (node 1)
        cmd_replica
            .execute(&[
                Bytes::from("REPLICATE"),
                Bytes::from(format!("{:040x}", 1u64)),
            ])
            .unwrap();

        // List replicas of node 1
        let result = cmd.execute(&[
            Bytes::from("REPLICAS"),
            Bytes::from(format!("{:040x}", 1u64)),
        ]);
        assert!(result.is_ok());

        if let Ok(RespValue::Array(Some(replicas))) = result {
            assert_eq!(replicas.len(), 1);
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_cluster_replicas_unknown_node() {
        let cmd = ClusterCommands::with_node_id(1);

        // Try to get replicas of unknown node - should fail
        let result = cmd.execute(&[
            Bytes::from("REPLICAS"),
            Bytes::from("0000000000000000000000000000000000000999"),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cluster_replicas_wrong_args() {
        let cmd = ClusterCommands::with_node_id(1);

        // No args
        let result = cmd.execute(&[Bytes::from("REPLICAS")]);
        assert!(result.is_err());
    }

    #[test]
    fn test_readonly_readwrite() {
        let cmd = ClusterCommands::with_node_id(1);

        // Initially not in readonly mode
        assert!(!cmd.is_readonly());

        // Enable readonly mode
        let result = cmd.readonly();
        assert!(result.is_ok());
        assert!(cmd.is_readonly());

        // Disable readonly mode
        let result = cmd.readwrite();
        assert!(result.is_ok());
        assert!(!cmd.is_readonly());
    }

    #[test]
    fn test_cluster_state_add_replica() {
        let mut state = ClusterState::new();

        // Add two nodes
        let master_id = 1u64;
        let replica_id = 2u64;
        state.nodes.insert(
            master_id,
            NodeInfo::new(master_id, "127.0.0.1:6379".to_string()),
        );
        state.nodes.insert(
            replica_id,
            NodeInfo::new(replica_id, "127.0.0.1:6380".to_string()),
        );

        // Add replica relationship
        state.add_replica(master_id, replica_id);

        // Verify
        assert!(!state.is_replica(master_id));
        assert!(state.is_replica(replica_id));
        assert_eq!(state.get_master(replica_id), Some(master_id));
        assert!(state.get_replicas(master_id).contains(&replica_id));
    }

    #[test]
    fn test_cluster_state_remove_replica() {
        let mut state = ClusterState::new();

        // Add two nodes
        let master_id = 1u64;
        let replica_id = 2u64;
        state.nodes.insert(
            master_id,
            NodeInfo::new(master_id, "127.0.0.1:6379".to_string()),
        );
        state.nodes.insert(
            replica_id,
            NodeInfo::new(replica_id, "127.0.0.1:6380".to_string()),
        );

        // Add and then remove replica relationship
        state.add_replica(master_id, replica_id);
        state.remove_replica(replica_id);

        // Verify
        assert!(state.is_master(replica_id));
        assert_eq!(state.get_master(replica_id), None);
        assert!(state.get_replicas(master_id).is_empty());
    }

    #[test]
    fn test_cluster_state_promote_replica() {
        let mut state = ClusterState::new();

        // Add two nodes
        let master_id = 1u64;
        let replica_id = 2u64;
        state.nodes.insert(
            master_id,
            NodeInfo::new(master_id, "127.0.0.1:6379".to_string()),
        );
        state.nodes.insert(
            replica_id,
            NodeInfo::new(replica_id, "127.0.0.1:6380".to_string()),
        );

        // Assign slots to master
        for i in 0..100u16 {
            state.slot_assignments[i as usize] = Some(master_id);
        }

        // Add replica
        state.add_replica(master_id, replica_id);

        // Promote replica
        let result = state.promote_replica(replica_id);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), master_id);

        // Verify new master
        assert!(state.is_master(replica_id));
        assert!(state.is_replica(master_id));
        assert_eq!(state.get_master(master_id), Some(replica_id));

        // Verify slots transferred
        for i in 0..100u16 {
            assert_eq!(state.slot_assignments[i as usize], Some(replica_id));
        }
    }

    #[test]
    fn test_node_info_new_replica() {
        let master_id = 1u64;
        let replica = NodeInfo::new_replica(2, "127.0.0.1:6380".to_string(), master_id);

        assert_eq!(replica.id, 2);
        assert!(!replica.is_master);
        assert_eq!(replica.master_id, Some(master_id));
    }

    #[test]
    fn test_failover_mode() {
        // Test that FailoverMode enum works correctly
        assert_eq!(FailoverMode::Default, FailoverMode::Default);
        assert_eq!(FailoverMode::Force, FailoverMode::Force);
        assert_eq!(FailoverMode::Takeover, FailoverMode::Takeover);
        assert_ne!(FailoverMode::Default, FailoverMode::Force);
    }

    #[test]
    fn test_cluster_state_promote_replica_with_multiple_replicas() {
        let mut state = ClusterState::new();

        // Add master and two replicas
        let master_id = 1u64;
        let replica1_id = 2u64;
        let replica2_id = 3u64;

        state.nodes.insert(
            master_id,
            NodeInfo::new(master_id, "127.0.0.1:6379".to_string()),
        );
        state.nodes.insert(
            replica1_id,
            NodeInfo::new(replica1_id, "127.0.0.1:6380".to_string()),
        );
        state.nodes.insert(
            replica2_id,
            NodeInfo::new(replica2_id, "127.0.0.1:6381".to_string()),
        );

        // Assign slots to master
        for i in 0..100u16 {
            state.slot_assignments[i as usize] = Some(master_id);
        }

        // Add both replicas
        state.add_replica(master_id, replica1_id);
        state.add_replica(master_id, replica2_id);

        // Verify initial state
        assert_eq!(state.get_master(replica1_id), Some(master_id));
        assert_eq!(state.get_master(replica2_id), Some(master_id));
        assert_eq!(state.get_replicas(master_id).len(), 2);

        // Promote replica1 to master
        let result = state.promote_replica(replica1_id);
        assert!(result.is_ok());

        // Verify replica1 is now master
        assert!(state.is_master(replica1_id));
        assert_eq!(state.get_master(replica1_id), None);

        // Verify old master is now replica of replica1
        assert!(state.is_replica(master_id));
        assert_eq!(state.get_master(master_id), Some(replica1_id));

        // Verify replica2's master_id is updated to point to new master (replica1)
        assert!(state.is_replica(replica2_id));
        assert_eq!(state.get_master(replica2_id), Some(replica1_id));

        // Verify new master (replica1) has both old master and replica2 as replicas
        let new_replicas = state.get_replicas(replica1_id);
        assert!(new_replicas.contains(&master_id));
        assert!(new_replicas.contains(&replica2_id));
    }
}
