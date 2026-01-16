# AiKv Cluster Deployment

This directory contains deployment files for a 6-node AiKv cluster (3 masters, 3 replicas).

## Architecture

- **Node 1-3**: Master nodes with MetaRaft voters
- **Node 4-6**: Replica nodes
- **MetaRaft**: Distributed consensus for cluster metadata
- **Slot Distribution**: 16384 slots evenly distributed across 3 masters

## Prerequisites

- Docker and Docker Compose installed
- AiKv cluster Docker image built

## Files

| File | Description |
|------|-------------|
| docker-compose.yml | Cluster Docker Compose configuration |
| aikv-node[1-6].toml | Per-node configuration files |
| start.sh | Start cluster script |
| stop.sh | Stop cluster script |
| init-cluster.sh | Initialize cluster with dynamic MetaRaft membership |

## Quick Start

1. Start all nodes:
   ./start.sh

2. Wait for nodes to be ready (~10 seconds)

3. Initialize cluster with dynamic MetaRaft membership:
   ./init-cluster.sh

## Initialization Process

The init-cluster.sh script uses the new dynamic MetaRaft membership approach:

1. **Wait for all nodes**: Ensures all 6 nodes are running and healthy
2. **Bootstrap verification**: Confirms node 1 is initialized as single-node MetaRaft cluster
3. **Add learners**: Adds nodes 2 and 3 as MetaRaft learners
4. **Promote to voters**: Promotes learners to voting members using Joint Consensus
5. **Cluster metadata**: Uses CLUSTER MEET to add all nodes to cluster metadata
6. **Slot assignment**: Distributes 16384 slots evenly across 3 masters
7. **Replication setup**: Configures nodes 4-6 as replicas

### Why Dynamic Membership?

This cluster uses **dynamic MetaRaft membership** instead of pre-configured peer lists:

- **No simultaneous startup required**: Nodes can join incrementally
- **Zero-downtime changes**: Uses OpenRaft Joint Consensus
- **Flexible scaling**: Easy to add/remove MetaRaft voters at runtime

See docs/METARAFT_DYNAMIC_MEMBERSHIP.md for details.

## Cluster Operations

### Check cluster status
redis-cli -c -h 127.0.0.1 -p 6379 CLUSTER INFO
redis-cli -c -h 127.0.0.1 -p 6379 CLUSTER NODES

### Check MetaRaft membership
redis-cli -c -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS

### Connect to cluster
redis-cli -c -h 127.0.0.1 -p 6379

## Port Mapping

| Node | Redis Port | Raft Port |
|------|------------|-----------|
| aikv1 | 6379 | 50051 |
| aikv2 | 6380 | 50052 |
| aikv3 | 6381 | 50053 |
| aikv4 | 6382 | 50054 |
| aikv5 | 6383 | 50055 |
| aikv6 | 6384 | 50056 |

## Troubleshooting

If init-cluster.sh fails:

1. Check all nodes are running: docker-compose ps
2. Check node logs: docker-compose logs aikv1
3. Reset and try again:
   ./stop.sh
   docker-compose down -v
   ./start.sh
   sleep 10
   ./init-cluster.sh

## Additional Resources

- AiKv Documentation: ../../README.md
- Cluster Architecture: ../../docs/ARCHITECTURE.md
- MetaRaft Dynamic Membership: ../../docs/METARAFT_DYNAMIC_MEMBERSHIP.md
- Cluster API Reference: ../../docs/AIDB_CLUSTER_API_REFERENCE.md
