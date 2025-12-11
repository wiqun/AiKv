//! Cluster module for AiKv Redis Cluster protocol support.
//!
//! This module provides a thin glue layer between Redis Cluster protocol
//! and AiDb's Multi-Raft implementation (v0.5.1).
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
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │         Redis Cluster Commands              │
//! │  (CLUSTER INFO, NODES, MEET, ADDSLOTS...)   │
//! └─────────────────────────────────────────────┘
//!                      │
//!                      ▼
//! ┌─────────────────────────────────────────────┐
//! │      AiKv Cluster Glue Layer (~500 lines)   │
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
//! # Design Principle
//!
//! - **Minimal Code**: Only Redis protocol format conversion (~500-1000 lines)
//! - **Zero Duplication**: All cluster logic delegated to AiDb
//! - **Direct API Usage**: No custom wrappers around AiDb components
//!
//! # Key Components
//!
//! - `ClusterCommands`: Maps Redis CLUSTER commands to AiDb API calls
//! - `ClusterNode`: Minimal wrapper around MultiRaftNode for initialization
//! - Re-exports from AiDb: MetaRaftNode, MultiRaftNode, Router, ClusterMeta, etc.

mod commands;
mod node;

// Use the new implementations
pub use commands::{ClusterCommands, FailoverMode, NodeInfo, RedirectType};
pub use node::{ClusterConfig, ClusterNode, GroupId, NodeId};

// Re-export AiDb cluster types directly (no custom wrappers)
#[cfg(feature = "cluster")]
pub use aidb::cluster::{
    // Core components
    MetaRaftNode,
    MultiRaftNode,
    Router,
    ShardedStateMachine,
    
    // Migration
    MigrationManager,
    MigrationConfig,
    
    // Membership
    MembershipCoordinator,
    ReplicaAllocator,
    
    // Data structures
    ClusterMeta,
    GroupMeta,
    MetaNodeInfo,
    NodeStatus,
    SlotMigration,
    SlotMigrationState,
    
    // Storage and network
    ShardedRaftStorage,
    MultiRaftNetworkFactory,
    
    // Thin replication
    ThinWriteBatch,
    ThinWriteOp,
    
    // Constants
    SLOT_COUNT,
};

/// Default slot count when cluster feature is disabled
#[cfg(not(feature = "cluster"))]
pub const SLOT_COUNT: u16 = 16384;
