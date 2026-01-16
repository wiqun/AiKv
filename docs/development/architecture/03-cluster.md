# AiKv Cluster Initialization Troubleshooting Guide

## Overview

AiKv uses AiDb's Multi-Raft consensus algorithm instead of Redis's traditional gossip protocol. This architectural difference requires special considerations during cluster initialization.

## Common Issues and Solutions

### Issue 1: redis-cli --cluster create Hangs

**Symptom:**
```
>>> Sending CLUSTER MEET messages to join the cluster
Waiting for the cluster to join
.........^C
```

**Root Cause:**
- redis-cli expects Redis's gossip protocol for immediate state propagation
- AiKv uses AiDb's Raft consensus, which requires explicit state synchronization
- CLUSTER MEET returns OK before Raft consensus completes
- redis-cli polls CLUSTER INFO waiting for `cluster_state:ok`, but nodes haven't synchronized yet

**Solution:**

**Recommended: Use aikv-tool (simplest)**
```bash
# Install aikv-tool
cd aikv-toolchain && cargo install --path . && cd ..

# One-click cluster deployment (6 nodes: 3 masters + 3 replicas)
aikv-tool cluster setup

# Verify cluster status
aikv-tool cluster status

# Connect and test
redis-cli -c -h 127.0.0.1 -p 6379
```

**Alternative: Use initialization script**
```bash
./scripts/cluster_init.sh
```

**Manual: Use redis-cli (requires patience)**
1. Be patient - state sync may take 10-30 seconds
2. Check individual node status: `redis-cli -p PORT CLUSTER NODES`
3. Manually verify all nodes see each other before slot assignment

### Issue 2: Duplicate Slot Assignments

**Symptom:**
```
M: 000000000000000000000000877ddd25a6868387 127.0.0.1:6380
   slots:[0],[5461-10922] (5462 slots) master
M: 0000000000000000000000005d07681a592ab292 127.0.0.1:6381
   slots:[0],[10923-16383] (5461 slots) master
```

Slot 0 appears on multiple masters.

**Root Cause:**
- Incomplete state synchronization between nodes
- Race condition during parallel slot assignment
- Nodes have inconsistent views of the cluster

**Solution:**
1. Clear the cluster state on all nodes:
   ```bash
   for port in 6379 6380 6381 6382 6383 6384; do
       redis-cli -p $port CLUSTER RESET SOFT
   done
   ```

2. Re-initialize using the dedicated script:
   ```bash
   ./scripts/cluster_init.sh
   ```

3. The script ensures:
   - Bidirectional CLUSTER MEET between all nodes
   - Sequential slot assignment to avoid conflicts
   - Explicit replication setup

### Issue 3: Nodes Show as Masters When They Should Be Replicas

**Symptom:**
Both port 6379 and 6383 show as masters, but one should be a replica.

**Root Cause:**
- Master/replica roles are managed at the AiKv glue layer
- CLUSTER REPLICATE command not executed or failed
- Node joined cluster but role not assigned

**Solution:**
Manually set replica relationship:

```bash
# Get master node ID
MASTER_ID=$(redis-cli -p 6379 CLUSTER MYID)

# Set replica on port 6383
redis-cli -p 6383 CLUSTER REPLICATE $MASTER_ID
```

Or use the initialization script which handles this automatically.

### Issue 4: Masters Not Communicating

**Symptom:**
Each master only knows about itself, doesn't see other masters.

**Root Cause:**
- Missing Cluster Bus implementation (gossip/heartbeat)
- CLUSTER MEET not executed bidirectionally
- MetaRaft state not synchronized

**Solution:**
1. Ensure CLUSTER MEET is executed from each node:
   ```bash
   # From node 1, meet node 2
   redis-cli -p 6379 CLUSTER MEET 127.0.0.1 6380
   
   # From node 2, meet node 1 (bidirectional)
   redis-cli -p 6380 CLUSTER MEET 127.0.0.1 6379
   ```

2. Or use the initialization script which handles full mesh connectivity:
   ```bash
   ./scripts/cluster_init.sh
   ```

### Issue 5: Cluster State Shows "fail"

**Symptom:**
```
127.0.0.1:6379> CLUSTER INFO
cluster_state:fail
cluster_slots_assigned:5461
```

**Root Cause:**
Not all 16384 slots are assigned. `cluster_state` becomes "ok" only when:
- All 16384 slots are assigned
- At least one node is known

**Solution:**
1. Check which slots are missing:
   ```bash
   redis-cli -p 6379 CLUSTER SLOTS
   ```

2. Assign remaining slots:
   ```bash
   # Get unassigned slot ranges and assign them
   redis-cli -p 6379 CLUSTER ADDSLOTS {missing-slots}
   ```

3. Or reinitialize:
   ```bash
   ./scripts/cluster_init.sh
   ```

## Architecture Notes

### Why AiKv Differs from Redis

| Aspect | Redis Cluster | AiKv Cluster |
|--------|--------------|--------------|
| Consensus | Gossip Protocol | Multi-Raft (AiDb) |
| State Sync | Eventual (seconds) | Strong (immediate after commit) |
| Port 16379 | Cluster bus required | (非必需) 仅用于 CLUSTER NODES 显示 |
|| Port 50051 | - | AiKv Raft gRPC 端口 |
| Node Discovery | Automatic via gossip | Explicit via CLUSTER MEET |
| Replication | Async gossip + replication stream | Raft log replication |

### State Synchronization

AiKv synchronizes cluster state through MetaRaft:

1. **CLUSTER MEET** → Proposes node join to MetaRaft → Raft consensus → State replicated
2. **CLUSTER INFO/NODES** → Reads from local MetaRaft state → Returns consistent view
3. **CLUSTER ADDSLOTS** → Updates local state → Synced via MetaRaft

> **说明**: CLUSTER INFO/NODES 命令直接从 `MetaRaftNode.get_cluster_meta()` 获取最新状态，确保返回一致视图。

### CLUSTER METARAFT Commands

AiKv provides MetaRaft management commands for advanced cluster operations:

```bash
# View all MetaRaft members and their roles (voter/learner)
CLUSTER METARAFT MEMBERS

# Add a node as learner to MetaRaft (first step in adding a voting member)
CLUSTER METARAFT ADDLEARNER <node_id> <addr>

# Promote one or more learners to voters
CLUSTER METARAFT PROMOTE <node_id> [<node_id>...]

# Get detailed MetaRaft status for diagnostics
CLUSTER METARAFT STATUS
```

**Example workflow for adding a new voting member:**
```bash
# 1. Add node as learner
redis-cli -p 6379 CLUSTER METARAFT ADDLEARNER 1234567890 127.0.0.1:50052

# 2. Wait for learner to sync (check logs)

# 3. Promote to voter
redis-cli -p 6379 CLUSTER METARAFT PROMOTE 1234567890

# 4. Verify membership
redis-cli -p 6379 CLUSTER METARAFT MEMBERS
```

## Verification Checklist

After cluster initialization, verify:

- [ ] All nodes see each other:
  ```bash
  redis-cli -p 6379 CLUSTER NODES | wc -l  # Should be 6 for 6-node cluster
  ```

- [ ] All slots assigned:
  ```bash
  redis-cli -p 6379 CLUSTER INFO | grep cluster_slots_assigned
  # Should show 16384
  ```

- [ ] Cluster state is OK:
  ```bash
  redis-cli -p 6379 CLUSTER INFO | grep cluster_state
  # Should show cluster_state:ok
  ```

- [ ] Replicas are properly assigned:
  ```bash
  redis-cli -p 6379 CLUSTER NODES | grep slave
  # Should show 3 slaves (for 3 masters + 3 replicas setup)
  ```

- [ ] Can write and read keys:
  ```bash
  redis-cli -c -p 6379 SET testkey testvalue
  redis-cli -c -p 6380 GET testkey
  ```

## Getting Help

If issues persist:

1. Check logs for errors:
   ```bash
   # For Docker
   docker-compose -f docker-compose.cluster.yml logs -f
   
   # For binary
   tail -f logs/aikv.log
   ```

2. Enable debug logging:
   ```toml
   # config/aikv-cluster.toml
   [logging]
   level = "debug"
   ```

3. Check MetaRaft status:
   ```bash
   # Look for MetaRaft-related log messages
   grep "MetaRaft" logs/aikv.log
   ```

4. Report issue with:
   - AiKv version (`aikv --version`)
   - Full output of `redis-cli CLUSTER INFO` from all nodes
   - Full output of `redis-cli CLUSTER NODES` from all nodes
   - Relevant log excerpts

## Additional Resources

- [scripts/README.md](../scripts/README.md) - Initialization script documentation
- [config/README.md](../config/README.md) - Configuration guide
- [Cluster API Reference](../api/02-cluster-api.md) - AiDb cluster API and commands
- [Cluster Troubleshooting](../guide/03-troubleshooting.md) - Additional troubleshooting guide
- [Best Practices](../guide/04-best-practices.md) - Cluster best practices

---

**Last Updated**: 2026-01-16  
**Version**: v0.1.0  
**AiDb Dependency**: v0.6.3
**Maintained by**: @Genuineh
