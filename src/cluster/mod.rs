//! Cluster module for AiKv Redis Cluster protocol support.
//!
//! This module provides a thin glue layer between Redis Cluster protocol
//! and AiDb's Multi-Raft implementation (v0.5.1).
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │         Redis Cluster Commands              │
//! │  (CLUSTER INFO, NODES, MEET, ADDSLOTS...)   │
//! └─────────────────────────────────────────────┘
//!                      │
//!                      ▼
//! ┌─────────────────────────────────────────────┐
//! │      AiKv Cluster Glue Layer (~700 lines)   │
//! │   ClusterCommands: Redis protocol adapter   │
//! │   ClusterNode: Thin wrapper                 │
//! └─────────────────────────────────────────────┘
//!                      │
//!                      ▼
//! ┌─────────────────────────────────────────────┐
//! │       AiDb Multi-Raft API (v0.5.1)          │
//! │  MetaRaftNode, MultiRaftNode, Router        │
//! │  MigrationManager, MembershipCoordinator    │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Design Principles
//!
//! 1. **Minimal Code**: Only Redis protocol format conversion
//! 2. **Direct API Usage**: No custom wrappers around AiDb components
//! 3. **Raft Consensus**: All cluster metadata changes sync via MetaRaft
//! 4. **Zero Duplication**: All cluster logic delegated to AiDb

mod commands;
mod node;

// Multi-group Raft gRPC server adapter
#[cfg(feature = "cluster")]
pub mod raft_service;

// Export our implementations
pub use commands::{key_to_slot_with_hash_tag, ClusterCommands, FailoverMode, NodeInfo, RedirectType};
pub use node::{ClusterConfig, ClusterNode, GroupId, NodeId};

// Re-export AiDb v0.5.1 cluster types
#[cfg(feature = "cluster")]
pub use aidb::cluster::{
    // Data structures
    ClusterMeta,
    GroupMeta,
    // Membership
    MembershipCoordinator,
    MetaNodeInfo,
    // Core components
    MetaRaftNode,
    MigrationConfig,

    // Migration
    MigrationManager,
    MultiRaftNetworkFactory,

    MultiRaftNode,
    NodeStatus,
    ReplicaAllocator,

    Router,
    // Storage and network
    ShardedRaftStorage,
    ShardedStateMachine,

    SlotMigration,
    SlotMigrationState,

    // Thin replication
    ThinWriteBatch,
    ThinWriteOp,

    // Constants
    SLOT_COUNT,
};

/// Default slot count when cluster feature is disabled
#[cfg(not(feature = "cluster"))]
pub const SLOT_COUNT: u16 = 16384;
