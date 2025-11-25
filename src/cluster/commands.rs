//! Cluster commands implementation.
//!
//! This module implements Redis Cluster protocol commands,
//! mapping them to AiDb's MultiRaft API.

use crate::cluster::router::SlotRouter;
use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use bytes::Bytes;

/// Cluster commands handler.
///
/// Implements Redis Cluster protocol commands:
/// - `CLUSTER KEYSLOT` - Calculate slot for a key
/// - `CLUSTER INFO` - Get cluster information (future)
/// - `CLUSTER NODES` - Get cluster nodes (future)
/// - `CLUSTER SLOTS` - Get slot-to-node mapping (future)
/// - `CLUSTER MYID` - Get current node ID (future)
pub struct ClusterCommands {
    router: SlotRouter,
    #[allow(dead_code)]
    node_id: Option<u64>,
}

impl ClusterCommands {
    /// Create a new ClusterCommands handler.
    pub fn new() -> Self {
        Self {
            router: SlotRouter::new(),
            node_id: None,
        }
    }

    /// Create a new ClusterCommands handler with a node ID.
    ///
    /// This is used to set the node ID for commands like CLUSTER MYID.
    pub fn with_node_id(node_id: u64) -> Self {
        Self {
            router: SlotRouter::new(),
            node_id: Some(node_id),
        }
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
    /// Currently returns a placeholder response.
    fn info(&self, _args: &[Bytes]) -> Result<RespValue> {
        // For now, return basic cluster info
        // TODO: Implement full cluster info when MetaRaftNode is available
        let info = "\
cluster_state:ok\r\n\
cluster_slots_assigned:0\r\n\
cluster_slots_ok:0\r\n\
cluster_slots_pfail:0\r\n\
cluster_slots_fail:0\r\n\
cluster_known_nodes:1\r\n\
cluster_size:0\r\n\
cluster_current_epoch:0\r\n\
cluster_my_epoch:0\r\n\
cluster_stats_messages_sent:0\r\n\
cluster_stats_messages_received:0\r\n";

        Ok(RespValue::bulk_string(Bytes::from(info.to_string())))
    }

    /// CLUSTER NODES
    ///
    /// Returns the cluster nodes information.
    /// Currently returns a placeholder response.
    fn nodes(&self, _args: &[Bytes]) -> Result<RespValue> {
        // TODO: Implement full nodes listing when MetaRaftNode is available
        let node_id = self.node_id.unwrap_or(0);
        let nodes = format!(
            "{:040x} 127.0.0.1:6379@16379 myself,master - 0 0 0 connected\r\n",
            node_id
        );

        Ok(RespValue::bulk_string(Bytes::from(nodes)))
    }

    /// CLUSTER SLOTS
    ///
    /// Returns the slot-to-node mapping.
    /// Currently returns an empty array.
    fn slots(&self, _args: &[Bytes]) -> Result<RespValue> {
        // TODO: Implement slot mapping when MetaRaftNode is available
        Ok(RespValue::Array(Some(vec![])))
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
}
