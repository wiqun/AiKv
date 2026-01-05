#!/bin/bash
# Initialize AiKv cluster with dynamic MetaRaft membership
# Uses the new learner → voter promotion workflow

set -e

echo "================================"
echo "AiKv Cluster Initialization"
echo "================================"
echo ""

# Wait for all nodes to be ready
echo "Step 1: Waiting for all nodes to be ready..."
for i in 1 2 3 4 5 6; do
    port=$((6378 + i))
    echo "  Checking node $i (port $port)..."
    for retry in {1..30}; do
        if redis-cli -h 127.0.0.1 -p $port PING >/dev/null 2>&1; then
            echo "  ✓ Node $i is ready"
            break
        fi
        if [ $retry -eq 30 ]; then
            echo "  ✗ Node $i failed to start"
            exit 1
        fi
        sleep 1
    done
done

echo ""
echo "Step 2: Getting node IDs from each node..."
NODE1_ID=$(redis-cli -h 127.0.0.1 -p 6379 CLUSTER MYID)
NODE2_ID=$(redis-cli -h 127.0.0.1 -p 6380 CLUSTER MYID)
NODE3_ID=$(redis-cli -h 127.0.0.1 -p 6381 CLUSTER MYID)
echo "  Node 1 ID: $NODE1_ID"
echo "  Node 2 ID: $NODE2_ID"
echo "  Node 3 ID: $NODE3_ID"

echo ""
echo "Step 3: Adding nodes 2 and 3 as MetaRaft learners..."
echo "  Adding node 2 (ID: $NODE2_ID)..."
# Use numeric node id for MetaRaft commands (cluster-local NodeId)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER 2 aikv2:50052

echo "  Adding node 3 (ID: $NODE3_ID)..."
# Use numeric node id for MetaRaft commands
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER 3 aikv3:50053

echo "  Waiting for learners to sync logs..."
# Wait until learners' Raft RPC ports are accepting connections (test from leader container to reflect in-cluster connectivity)
check_port_from_leader() {
    local container=$1
    local port=$2
    local port_hex=$(printf "%x" $port | tr '[:lower:]' '[:upper:]')
    local retries=30
    local i=0
    while [ $i -lt $retries ]; do
        # Check if target container has a listening socket on the port (procfs)
        if docker exec "$container" sh -c "cat /proc/net/tcp | grep -i \"$port_hex\" || cat /proc/net/tcp6 | grep -i \"$port_hex\"" >/dev/null 2>&1; then
            echo "  ✓ ${container}:${port} has a listening socket"
            return 0
        fi
        i=$((i+1))
        echo "  waiting for ${container}:${port} to have listening socket... ($i/$retries)"
        sleep 1
    done
    return 1
}

echo "  Waiting for aikv2 raft port (50052) to be reachable from leader..."
if ! check_port_from_leader "aikv2" 50052; then
    echo "  ✗ Timeout waiting for aikv2:50052 to accept connections from aikv1"
    exit 1
fi

echo "  Waiting for aikv3 raft port (50053) to be reachable from leader..."
if ! check_port_from_leader "aikv3" 50053; then
    echo "  ✗ Timeout waiting for aikv3:50053 to accept connections from aikv1"
    exit 1
fi

echo ""
echo "Step 4: Promoting learners to voters..."
echo "  Promoting learners (2, 3) to voters..."
PROMOTE_RETRIES=12
PROMOTE_ATTEMPT=0
while [ $PROMOTE_ATTEMPT -lt $PROMOTE_RETRIES ]; do
    # Note: Node 1 is already a voter (bootstrap node), so only promote 2 and 3
    PROMOTE_OUTPUT=$(redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT PROMOTE 2 3 2>&1) || true
    if echo "$PROMOTE_OUTPUT" | grep -qi "ok"; then
        echo "  ✓ Promoted learners to voters"
        break
    fi
    if echo "$PROMOTE_OUTPUT" | grep -qi "InProgress\|Unreachable"; then
        echo "  Promote attempt $((PROMOTE_ATTEMPT+1)) failed (in progress or unreachable). Retrying..."
        PROMOTE_ATTEMPT=$((PROMOTE_ATTEMPT+1))
        sleep 5
        continue
    fi
    if [ -z "$PROMOTE_OUTPUT" ]; then
        # If no immediate output, wait briefly and retry
        echo "  Promote attempt $((PROMOTE_ATTEMPT+1)) produced no immediate response. Retrying..."
        PROMOTE_ATTEMPT=$((PROMOTE_ATTEMPT+1))
        sleep 5
        continue
    fi
    # Unexpected error
    echo "  ✗ Promote failed: $PROMOTE_OUTPUT"
    exit 1
done

if [ $PROMOTE_ATTEMPT -ge $PROMOTE_RETRIES ]; then
    echo "  ✗ Promote failed after retries"
    exit 1
fi

echo "  Waiting for membership change to complete..."
sleep 2

echo ""
echo "Step 5: Verifying MetaRaft membership..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS

echo ""
echo "Step 6: Adding nodes to cluster metadata..."
echo "  Meeting node 2..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6380 $NODE2_ID

echo "  Meeting node 3..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6381 $NODE3_ID

echo "  Meeting node 4..."
NODE4_ID=$(redis-cli -h 127.0.0.1 -p 6382 CLUSTER MYID)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6382 $NODE4_ID

echo "  Meeting node 5..."
NODE5_ID=$(redis-cli -h 127.0.0.1 -p 6383 CLUSTER MYID)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6383 $NODE5_ID

echo "  Meeting node 6..."
NODE6_ID=$(redis-cli -h 127.0.0.1 -p 6384 CLUSTER MYID)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6384 $NODE6_ID

echo "  Waiting for cluster metadata to sync..."
sleep 2

echo ""
echo "Step 7: Assigning slots to master nodes (via leader)..."
# All slot assignments must go through the MetaRaft leader (node 1)
# Using ADDSLOTSRANGE for efficiency

echo "  Assigning slots 0-5460 to node 1 (ID: $NODE1_ID)..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 0 5460

echo "  Assigning slots 5461-10922 to node 2 (ID: $NODE2_ID)..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 5461 10922 $NODE2_ID

echo "  Assigning slots 10923-16383 to node 3 (ID: $NODE3_ID)..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 10923 16383 $NODE3_ID

echo ""
echo "Step 8: Setting up replication (nodes 4-6 as replicas)..."
echo "  Node 4 replicating node 1..."
redis-cli -h 127.0.0.1 -p 6382 CLUSTER REPLICATE $NODE1_ID

echo "  Node 5 replicating node 2..."
redis-cli -h 127.0.0.1 -p 6383 CLUSTER REPLICATE $NODE2_ID

echo "  Node 6 replicating node 3..."
redis-cli -h 127.0.0.1 -p 6384 CLUSTER REPLICATE $NODE3_ID

echo ""
echo "================================"
echo "✅ Cluster initialization complete!"
echo "================================"
echo ""
echo "Cluster Status:"
redis-cli -h 127.0.0.1 -p 6379 CLUSTER INFO
echo ""
echo "Cluster Nodes:"
redis-cli -h 127.0.0.1 -p 6379 CLUSTER NODES
echo ""
echo "MetaRaft Members:"
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS
echo ""
echo "You can now connect with: redis-cli -c -h 127.0.0.1 -p 6379"
