# AiKv Scripts

This directory contains utility scripts for AiKv cluster management and testing.

## Available Scripts

### cluster_init.sh

A dedicated cluster initialization script for AiKv that properly handles the AiDb-based consensus layer.

#### Why This Script?

`redis-cli --cluster create` relies on Redis's gossip protocol and assumes certain behaviors that don't align with AiKv's AiDb consensus implementation. This script provides an alternative initialization method that:

1. **Properly handles bidirectional CLUSTER MEET**: Ensures nodes establish connections in both directions
2. **Respects AiDb's peer-to-peer consensus**: Works with AiDb's consensus algorithm instead of assuming Redis Sentinel behavior
3. **Explicit slot assignment**: Directly assigns hash slots without relying on gossip propagation
4. **Clear replication setup**: Establishes master-replica relationships at the glue layer level

#### Usage

**Basic usage (default 6-node cluster: 3 masters + 3 replicas):**

```bash
./scripts/cluster_init.sh
```

**Custom configuration:**

```bash
./scripts/cluster_init.sh \
  -m 127.0.0.1:6379,127.0.0.1:6380,127.0.0.1:6381 \
  -r 127.0.0.1:6382,127.0.0.1:6383,127.0.0.1:6384
```

**With Docker Compose:**

```bash
# Start the cluster nodes
docker-compose -f docker-compose.cluster.yml up -d

# Wait for nodes to be ready
sleep 10

# Initialize the cluster
./scripts/cluster_init.sh
```

#### What It Does

1. **Connectivity Check**: Verifies all nodes are reachable
2. **Node ID Discovery**: Retrieves unique node IDs from each instance
3. **Cluster Formation**: Uses CLUSTER MEET to create a full mesh network
4. **Slot Distribution**: Assigns the 16384 hash slots evenly across masters
5. **Replication Setup**: Configures replicas using CLUSTER REPLICATE
6. **Verification**: Displays final cluster status

#### Example Output

```
[INFO] Cluster Configuration:
  Masters: 3
    Master 1: 127.0.0.1:6379
    Master 2: 127.0.0.1:6380
    Master 3: 127.0.0.1:6381
  Replicas: 3
    Replica 1: 127.0.0.1:6382
    Replica 2: 127.0.0.1:6383
    Replica 3: 127.0.0.1:6384

[SUCCESS] Node 127.0.0.1:6379 is reachable
[SUCCESS] Node 127.0.0.1:6380 is reachable
...
[SUCCESS] Cluster initialization completed!
```

#### Options

- `-m, --masters HOSTS`: Comma-separated list of master nodes (host:port)
- `-r, --replicas HOSTS`: Comma-separated list of replica nodes (host:port)
- `-h, --help`: Show help message

#### Requirements

- `redis-cli` must be installed and available in PATH
- All specified nodes must be running and accessible
- Nodes must have cluster mode enabled (compiled with `--features cluster`)

#### Troubleshooting

**"redis-cli not found"**
- Install Redis tools: `apt-get install redis-tools` or `brew install redis`
- Or set custom path: `REDIS_CLI=/path/to/redis-cli ./scripts/cluster_init.sh`

**"Node X is not reachable"**
- Ensure the node is running: `redis-cli -h HOST -p PORT PING`
- Check firewall rules and network connectivity
- Verify the node was started with `--features cluster`

**"Failed to assign slots"**
- Nodes may already have slots assigned
- Clear existing cluster state: `redis-cli -p PORT CLUSTER RESET SOFT`
- Then re-run the initialization script

### e2e_test.sh

End-to-end testing script for basic AiKv functionality. See the script for details.

### benchmark.sh

Performance benchmarking script. See the script for details.

## Contributing

When adding new scripts:
1. Include a header comment explaining the script's purpose
2. Add usage documentation to this README
3. Make scripts executable: `chmod +x scripts/your_script.sh`
4. Test thoroughly before committing
