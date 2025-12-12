# AiDb v0.5.2 Multi-Raft API Migration Guide

## Overview

This document describes the migration from a custom cluster state management to AiDb v0.5.2's official Multi-Raft API.

## What Changed

### Architecture Refactoring

The old architecture maintained a separate `ClusterState` struct that tracked cluster metadata independently. The new architecture delegates all cluster state management to AiDb's Multi-Raft implementation, reducing code from 6215 lines to 810 lines.

**Old Architecture (Deprecated):**
```
Server → ClusterState → NodeInfo (custom tracking)
         ↓
         ClusterCommands (stateful)
```

**New Architecture (v0.5.2):**
```
Server → ClusterNode → MultiRaftNode (AiDb)
                    → MetaRaftNode (AiDb)
                    → Router (AiDb)
         ↓
         ClusterCommands (thin adapter)
```

## API Changes Fixed

### 1. `src/cluster/commands.rs`

**Issues Fixed:**
- ✅ `RespValue::Array` return type now wraps in `Some()` for consistency
- ✅ Added `execute()` dispatcher method for CLUSTER subcommands
- ✅ Added `readonly()` and `readwrite()` connection mode methods
- ✅ Removed unused imports (`ShardedStateMachine`, `SLOT_COUNT`)
- ✅ Fixed unused variable warning in `cluster_getkeysinslot()`

**API Changes:**
```rust
// Old (didn't exist)
// cluster_commands.execute(args) // Didn't exist

// New
pub fn execute(&self, args: &[Bytes]) -> Result<RespValue> {
    // Dispatches to cluster_info(), cluster_nodes(), etc.
}

pub fn readonly(&self) -> Result<RespValue> { ... }
pub fn readwrite(&self) -> Result<RespValue> { ... }
```

### 2. `src/cluster/node.rs`

**Issues Fixed:**
- ✅ Fixed `Router::new()` to accept `ClusterMeta` instead of `Arc<MetaRaftNode>`
- ✅ Fixed initialization order: init MetaRaft before wrapping MultiRaftNode in Arc
- ✅ Fixed Arc move/borrow issue by cloning before assignment

**API Changes:**
```rust
// Old (incorrect)
let multi_raft = Arc::new(multi_raft);
multi_raft.init_meta_raft(config).await?; // Error: can't mutate Arc

// New (correct)
let mut multi_raft = MultiRaftNode::new(...).await?;
multi_raft.init_meta_raft(config).await?;
let multi_raft = Arc::new(multi_raft); // Wrap after init
```

```rust
// Old (incorrect)
let router = Arc::new(Router::new(meta_raft.clone())); // Error: expects ClusterMeta

// New (correct)
let cluster_meta = meta_raft.get_cluster_meta();
let router = Arc::new(Router::new(cluster_meta));
```

### 3. `src/command/mod.rs`

**Issues Fixed:**
- ✅ Made `ClusterCommands` optional (`Option<ClusterCommands>`)
- ✅ Removed deprecated `with_shared_cluster_state()` method
- ✅ Added `set_cluster_commands()` for deferred initialization
- ✅ Updated CLUSTER command handlers to check if cluster is initialized

**API Changes:**
```rust
// Old (deprecated)
impl CommandExecutor {
    pub fn with_shared_cluster_state(
        storage: StorageEngine,
        port: u16,
        node_id: u64,
        cluster_state: Arc<RwLock<ClusterState>>, // Custom state
    ) -> Self { ... }
}

// New (recommended)
impl CommandExecutor {
    pub fn with_port(storage: StorageEngine, port: u16) -> Self {
        // cluster_commands: None by default
    }
    
    pub fn set_cluster_commands(&mut self, cluster_commands: ClusterCommands) {
        self.cluster_commands = Some(cluster_commands);
    }
}
```

### 4. `src/server/mod.rs`

**Issues Fixed:**
- ✅ Removed `ClusterState` and `NodeInfo` imports (internal to cluster module)
- ✅ Removed `cluster_state` field from Server struct
- ✅ Simplified server initialization to generate node_id only

**API Changes:**
```rust
// Old (deprecated)
pub struct Server {
    cluster_state: Arc<RwLock<ClusterState>>, // Removed
    ...
}

// New (simplified)
pub struct Server {
    #[cfg(feature = "cluster")]
    node_id: u64, // Just track ID, delegate state to AiDb
    ...
}
```

## How to Initialize Cluster (Post-Migration)

The new initialization flow requires setting up AiDb's Multi-Raft components:

```rust
use aikv::cluster::{ClusterNode, ClusterConfig, ClusterCommands};

// 1. Create cluster configuration
let config = ClusterConfig {
    node_id: 1,
    data_dir: PathBuf::from("./data/node1"),
    bind_address: "127.0.0.1:6379".to_string(),
    raft_address: "127.0.0.1:50051".to_string(),
    num_groups: 4,
    is_bootstrap: true,
    initial_members: vec![(1, "127.0.0.1:50051".to_string())],
};

// 2. Create and initialize cluster node
let mut cluster_node = ClusterNode::new(config);
cluster_node.initialize().await?;

// 3. Get initialized components
let meta_raft = cluster_node.meta_raft().unwrap().clone();
let multi_raft = cluster_node.multi_raft().unwrap().clone();
let router = cluster_node.router().unwrap().clone();

// 4. Create cluster commands
let cluster_commands = ClusterCommands::new(
    node_id,
    meta_raft,
    multi_raft,
    router,
);

// 5. Set in command executor
let mut executor = CommandExecutor::with_port(storage, 6379);
executor.set_cluster_commands(cluster_commands);
```

## Migration Status

### ✅ Completed (This PR)

1. **Core compilation fixes** - All source files compile with `--features cluster`
2. **Unit tests** - All 118 unit tests pass with and without cluster feature
3. **API compatibility** - ClusterCommands now compatible with AiDb v0.5.1 API
4. **Import fixes** - Integration test imports updated from `crate::` to `aikv::`

### ⚠️ Remaining Work (Follow-up Tasks)

1. **Cluster integration tests** - Need updating for new API:
   - `tests/cluster_new_tests.rs` - Update test cases to use ClusterNode wrapper
   - `tests/cluster_sync_test.rs` - Verify sync behavior with new API
   
2. **Server initialization** - Full cluster mode initialization:
   - Add ClusterConfig loading from configuration file
   - Initialize ClusterNode in Server::new() when cluster feature enabled
   - Pass initialized ClusterCommands to CommandExecutor
   
3. **Command executor updates** - Integration with cluster routing:
   - Implement key routing based on Router::key_to_slot()
   - Handle -MOVED and -ASK redirections
   - Implement cross-slot command validation

4. **Documentation updates**:
   - Update README.md with new cluster initialization steps
   - Add examples showing Multi-Raft cluster setup
   - Document differences from standard Redis Cluster

## Key Benefits of Migration

1. **Reduced complexity** - 6215 lines → 810 lines (87% reduction)
2. **Strong consistency** - Raft consensus instead of eventual consistency
3. **No custom state management** - Leverage battle-tested AiDb implementation
4. **Automatic metadata sync** - All cluster changes sync via Raft
5. **Official API** - Use documented AiDb Multi-Raft API instead of custom code

## Breaking Changes

### For Library Users

If you were using cluster features before this migration:

1. **ClusterState removed** - No longer accessible as public API
2. **NodeInfo changed** - Now internal struct, use CLUSTER NODES command instead
3. **Initialization changed** - Must use ClusterNode instead of manual setup

### For Contributors

If you're working on cluster code:

1. **Test imports** - Use `aikv::` not `crate::` in integration tests
2. **API usage** - Refer to AiDb v0.5.1 Multi-Raft documentation
3. **State access** - Always go through MetaRaftNode, never cache state

## Troubleshooting

### Compilation Errors

**Error: "Cluster not initialized"**
```
Solution: Call executor.set_cluster_commands() after creating ClusterNode
```

**Error: "cannot borrow as mutable" on Arc<MultiRaftNode>**
```
Solution: Initialize before wrapping in Arc:
  let mut node = MultiRaftNode::new(...).await?;
  node.init_meta_raft(config).await?;
  let node = Arc::new(node); // Now wrap
```

**Error: Router::new() expects ClusterMeta**
```
Solution: Get metadata from MetaRaft:
  let meta = meta_raft.get_cluster_meta();
  let router = Router::new(meta);
```

### Runtime Issues

**CLUSTER commands return "Cluster not initialized"**
```
Check: CommandExecutor.cluster_commands should be Some(), not None
Solution: Call set_cluster_commands() with initialized ClusterCommands
```

**No metadata synchronization between nodes**
```
Check: MetaRaft is initialized and bootstrapped correctly
Check: All nodes have initialized_meta_cluster() or joined existing cluster
```

## References

- [AiDb Multi-Raft API Reference](./AIDB_CLUSTER_API_REFERENCE.md)
- [Cluster Bus Analysis](./CLUSTER_BUS_ANALYSIS.md)
- [AiDb v0.5.0 Upgrade Guide](./AIDB_V050_UPGRADE.md)
- [Architecture Refactoring](./ARCHITECTURE_REFACTORING.md)

## Version History

- **2025-12-12**: Upgraded to AiDb v0.5.2 - Fixed metadata synchronization issues (all 7 cluster tests now pass)
- **2025-12-11**: Initial migration to AiDb v0.5.1 Multi-Raft API
- **2025-11-27**: Cluster bus implementation using AiDb
- **2025-11-26**: Multi-Raft integration started
