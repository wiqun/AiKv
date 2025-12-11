//! Cluster module for AiKv Redis Cluster protocol support.
//!
//! This module provides the Redis Cluster protocol adaptation layer,
//! wrapping AiDb's MultiRaft API to provide Redis Cluster compatibility.
//!
//! # Feature Flag
//!
//! This module is only available when the `cluster` feature is enabled:
//!
//! ```toml
//! [features]
//! cluster = ["aidb/raft-cluster"]
//! ```
//!
//! # Architecture
//!
//! The cluster module acts as a thin glue layer between Redis Cluster protocol
//! and AiDb's MultiRaft implementation:
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │         Redis Cluster Commands              │
//! │  (CLUSTER KEYSLOT, INFO, NODES, etc.)       │
//! └─────────────────────────────────────────────┘
//!                      │
//!                      ▼
//! ┌─────────────────────────────────────────────┐
//! │         AiKv Cluster Glue Layer             │
//! │  (ClusterNode, SlotRouter, Commands)        │
//! └─────────────────────────────────────────────┘
//!                      │
//!                      ▼
//! ┌─────────────────────────────────────────────┐
//! │         AiDb MultiRaft API                  │
//! │  (Router, MultiRaftNode, MetaRaftNode)      │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Cluster Bus (v0.3.0)
//!
//! This module includes the Cluster Bus for inter-node communication:
//! - Heartbeat detection via `MetaRaftNode` leader heartbeat
//! - Failure detection via `NodeStatus::Online/Offline` + election timeout
//! - Health monitoring with configurable thresholds
//!
//! # Stage C: Slot Migration
//!
//! This module includes support for online slot migration:
//! - `CLUSTER GETKEYSINSLOT` - Get keys belonging to a specific slot
//! - `CLUSTER COUNTKEYSINSLOT` - Count keys in a slot
//! - Migration state management (`MIGRATING`/`IMPORTING`)
//! - `-ASK` redirection logic
//! - Migration progress tracking
//!
//! # Stage D: High Availability
//!
//! This module includes support for high availability:
//! - `CLUSTER REPLICATE` - Configure a node as a replica
//! - `CLUSTER FAILOVER` - Manual failover trigger
//! - `CLUSTER REPLICAS` - List replicas of a master
//! - `READONLY` / `READWRITE` - Enable/disable readonly mode for replicas
//! - Membership coordinator integration
//!
//! # Multi-Raft Integration (v0.2.0)
//!
//! The cluster module now integrates with AiDb's Multi-Raft implementation:
//! - `MultiRaftNode` for managing multiple Raft groups
//! - `MetaRaftNode` for cluster metadata consensus
//! - Automatic key-to-slot-to-group routing
//! - Support for 3-node cluster startup

mod cluster_bus;
mod commands;
mod metaraft;
mod node;
mod router;

pub use cluster_bus::{ClusterBus, ClusterBusConfig, NodeHealthInfo, NodeHealthStatus};
pub use commands::{
    ClusterCommands, ClusterState, FailoverMode, KeyCounter, KeyScanner, MigrationProgress,
    NodeInfo, RedirectType, SlotState,
};
pub use metaraft::{ClusterNodeInfo, ClusterView, MetaRaftClient, MetaRaftClientConfig};
pub use node::{ClusterNode, NodeId};
pub use router::SlotRouter;

// Re-export ClusterConfig when cluster feature is enabled
#[cfg(feature = "cluster")]
pub use node::{ClusterConfig, GroupId};

// Re-export AiDb cluster types when cluster feature is enabled
#[cfg(feature = "cluster")]
pub use aidb::cluster::{
    MetaNodeInfo as AiDbNodeInfo, MetaRaftNode, MultiRaftNode, NodeStatus as AiDbNodeStatus,
    Router as AiDbRouter, SLOT_COUNT,
};

/// Default slot count for Redis Cluster (16384 slots)
#[cfg(not(feature = "cluster"))]
pub const SLOT_COUNT: u16 = 16384;
