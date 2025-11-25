//! Cluster module for AiKv Multi-Raft support
//!
//! This module provides the integration layer between AiKv and AiDb's Multi-Raft
//! cluster capabilities. It enables running AiKv as a distributed, highly-available
//! key-value store with automatic failover and data replication.
//!
//! # Architecture
//!
//! The cluster module follows the architecture defined in AiDb v0.4.0:
//!
//! ```text
//! AiKv Cluster Node
//! ├── MetaRaft (Group 0) - Manages cluster metadata, slot mappings
//! ├── Data Groups - Each manages a subset of Redis slots (16384 total)
//! │   ├── Group 1: slots [0, 1000)
//! │   ├── Group 2: slots [1000, 2000)
//! │   └── ...
//! └── Router - Routes commands to correct group based on key hash
//! ```
//!
//! # Features
//!
//! - **Multi-Raft**: Multiple independent Raft groups for horizontal scaling
//! - **MetaRaft**: Centralized metadata management with Raft consensus
//! - **Thin Replication**: Only WAL entries are replicated (90%+ bandwidth savings)
//! - **Slot-based Sharding**: Redis Cluster compatible 16384 slot mapping
//!
//! # Example
//!
//! ```ignore
//! use aikv::cluster::{ClusterConfig, ClusterNode};
//!
//! // Create a cluster node
//! let config = ClusterConfig::new(1, "127.0.0.1:6379", "./data/node1");
//! let node = ClusterNode::new(config).await?;
//!
//! // Bootstrap cluster (on first node only)
//! node.bootstrap(vec![
//!     (1, "127.0.0.1:6379".to_string()),
//!     (2, "127.0.0.1:6380".to_string()),
//!     (3, "127.0.0.1:6381".to_string()),
//! ]).await?;
//!
//! // Start serving requests
//! node.run().await?;
//! ```

mod config;
mod node;
mod router;
mod types;

pub use config::ClusterConfig;
pub use node::ClusterNode;
pub use router::SlotRouter;
pub use types::{ClusterState, NodeRole, SlotRange};

// Re-export AiDb cluster types for convenience
pub use aidb::cluster::{MultiRaftNode, NodeId, Router, ThinWriteBatch, ThinWriteOp, SLOT_COUNT};
