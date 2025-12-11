//! Cluster module for AiKv Redis Cluster protocol support.
//!
//! # Refactoring Status (v0.5.1 Upgrade)
//!
//! ✅ AiDb upgraded to v0.5.1  
//! 🔄 Using legacy implementation for compatibility  
//! ⏳ New minimalist implementation in progress (see *_new_wip files)

// Legacy modules (currently active)
mod cluster_bus_legacy;
mod commands_legacy;
mod metaraft_legacy;
mod node_legacy;
mod router_legacy;

// Export legacy implementations
pub use cluster_bus_legacy::{ClusterBus, ClusterBusConfig, NodeHealthInfo, NodeHealthStatus};
pub use commands_legacy::{
    ClusterCommands, ClusterState, FailoverMode, KeyCounter, KeyScanner, 
    MigrationProgress, NodeInfo, RedirectType, SlotState,
};
pub use metaraft_legacy::{ClusterNodeInfo, ClusterView, MetaRaftClient, MetaRaftClientConfig};
pub use node_legacy::{ClusterNode, NodeId};
pub use router_legacy::SlotRouter;

#[cfg(feature = "cluster")]
pub use node_legacy::{ClusterConfig, GroupId};

// Re-export AiDb v0.5.1 cluster types
#[cfg(feature = "cluster")]
pub use aidb::cluster::{
    MetaNodeInfo as AiDbNodeInfo, MetaRaftNode, MultiRaftNode, 
    NodeStatus as AiDbNodeStatus, Router as AiDbRouter, SLOT_COUNT,
    ClusterMeta, GroupMeta, MigrationManager, MigrationConfig,
    MembershipCoordinator, ReplicaAllocator, SlotMigration, SlotMigrationState,
    ShardedStateMachine, ShardedRaftStorage, MultiRaftNetworkFactory,
    ThinWriteBatch, ThinWriteOp,
};

#[cfg(not(feature = "cluster"))]
pub const SLOT_COUNT: u16 = 16384;
