//! Cluster type definitions for AiKv
//!
//! This module defines the core types used in the AiKv cluster implementation.

use serde::{Deserialize, Serialize};

/// Node role in the cluster
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NodeRole {
    /// Leader node - handles all writes
    Leader,
    /// Follower node - replicates data from leader
    #[default]
    Follower,
    /// Learner node - catching up before becoming follower
    Learner,
    /// Candidate node - participating in leader election
    Candidate,
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Leader => write!(f, "leader"),
            Self::Follower => write!(f, "follower"),
            Self::Learner => write!(f, "learner"),
            Self::Candidate => write!(f, "candidate"),
        }
    }
}

/// A range of slots assigned to a group
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRange {
    /// Start slot (inclusive)
    pub start: u16,
    /// End slot (exclusive)
    pub end: u16,
}

impl SlotRange {
    /// Create a new slot range
    pub fn new(start: u16, end: u16) -> Self {
        assert!(start < end, "start must be less than end");
        assert!(end <= 16384, "end must be <= 16384");
        Self {
            start,
            end,
        }
    }

    /// Check if a slot is within this range
    pub fn contains(&self, slot: u16) -> bool {
        slot >= self.start && slot < self.end
    }

    /// Get the number of slots in this range
    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    /// Check if the range is empty
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

impl std::fmt::Display for SlotRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}-{})", self.start, self.end)
    }
}

/// Current state of the cluster from this node's perspective
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClusterState {
    /// This node's current role
    pub role: NodeRole,
    /// Current cluster epoch (configuration version)
    pub epoch: u64,
    /// Number of known nodes in the cluster
    pub known_nodes: usize,
    /// Number of healthy nodes
    pub healthy_nodes: usize,
    /// Number of slots this node is responsible for
    pub my_slots: usize,
    /// Whether the cluster is in a healthy state
    pub cluster_ok: bool,
    /// Current MetaRaft leader (if known)
    pub meta_leader: Option<u64>,
}

impl ClusterState {
    /// Create a new cluster state
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this node is a leader
    pub fn is_leader(&self) -> bool {
        self.role == NodeRole::Leader
    }

    /// Get cluster status string (for INFO command)
    pub fn status_string(&self) -> String {
        if self.cluster_ok {
            "ok".to_string()
        } else {
            "fail".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_role_display() {
        assert_eq!(format!("{}", NodeRole::Leader), "leader");
        assert_eq!(format!("{}", NodeRole::Follower), "follower");
    }

    #[test]
    fn test_slot_range() {
        let range = SlotRange::new(0, 5461);
        assert!(range.contains(0));
        assert!(range.contains(5460));
        assert!(!range.contains(5461));
        assert_eq!(range.len(), 5461);
    }

    #[test]
    fn test_cluster_state() {
        let state = ClusterState {
            role: NodeRole::Leader,
            cluster_ok: true,
            ..Default::default()
        };
        assert!(state.is_leader());
        assert_eq!(state.status_string(), "ok");
    }
}
