#!/bin/bash
# Initialize AiKv cluster with dynamic MetaRaft membership
# Uses the new learner → voter promotion workflow

set -e

# CLI options
DEBUG=0
CLI_TIMEOUT=10

# Parse args
while [ "$1" != "" ]; do
    case "$1" in
        --debug)
            DEBUG=1
            ;;
        --timeout)
            shift
            CLI_TIMEOUT="$1"
            ;;
        *)
            ;;
    esac
    shift
done

# Detect docker-compose command
if command -v docker-compose >/dev/null 2>&1; then
    DC_CMD="docker-compose"
elif command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    DC_CMD="docker compose"
else
    DC_CMD=""
fi

if [ "$DEBUG" -eq 1 ]; then
    echo "Debug mode enabled (redis-cli timeout=${CLI_TIMEOUT}s)"
fi

# Wrapper for redis-cli with optional timeout
run_redis() {
    if command -v timeout >/dev/null 2>&1; then
        timeout "${CLI_TIMEOUT}s" redis-cli "$@"
        return $?
    else
        redis-cli "$@"
        return $?
    fi
}

collect_diagnostics() {
    echo "=== DIAGNOSTICS START ==="
    echo "MetaRaft members:"
    run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS || true
    echo "Cluster nodes:"
    run_redis -h 127.0.0.1 -p 6379 CLUSTER NODES || true
    echo "Cluster info:"
    run_redis -h 127.0.0.1 -p 6379 CLUSTER INFO || true
    if [ -n "$DC_CMD" ]; then
        $DC_CMD logs --tail 200 aikv1 || true
        $DC_CMD logs --tail 200 aikv2 || true
        $DC_CMD logs --tail 200 aikv3 || true
    fi
    echo "=== DIAGNOSTICS END ==="
}

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
        if run_redis -h 127.0.0.1 -p $port PING >/dev/null 2>&1; then
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
NODE1_ID=$(run_redis -h 127.0.0.1 -p 6379 CLUSTER MYID)
NODE2_ID=$(run_redis -h 127.0.0.1 -p 6380 CLUSTER MYID)
NODE3_ID=$(run_redis -h 127.0.0.1 -p 6381 CLUSTER MYID)
echo "  Node 1 ID: $NODE1_ID"
echo "  Node 2 ID: $NODE2_ID"
echo "  Node 3 ID: $NODE3_ID"

echo ""
echo "Step 3: Ensuring nodes 2 and 3 are MetaRaft learners..."
for entry in "2 aikv2 50052 $NODE2_ID" "3 aikv3 50053 $NODE3_ID"; do
    set -- $entry
    node_num=$1; host=$2; port=$3; node_hex=$4

    echo "  Ensuring node $node_num (ID: $node_hex) is learner..."

    # Check if node already present as learner
    if run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS 2>/dev/null | awk 'BEGIN{found=0} {idline=$0; if(getline){roleline=$0} if(match(idline, /"([^"]+)"/, m)){id=m[1]; if(roleline ~ /[Ll]earner/ && id=="'"$node_num"'") found=1}} END{ if(found) exit 0; exit 1 }'; then
        echo "  Node $node_num already a learner"
        continue
    fi

    echo "  Adding node $node_num (ID: $node_hex)..."
    ADD_OUT=$(run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER $node_num $host:$port 2>&1) || true

    if echo "$ADD_OUT" | grep -qi "OK"; then
        echo "  Added node $node_num as learner"
    elif echo "$ADD_OUT" | grep -qi "InProgress"; then
        echo "  Membership change in progress; waiting for node $node_num to become learner"
        ATTEMPTS=12; ATTEMPT=0
        while [ $ATTEMPT -lt $ATTEMPTS ]; do
        if run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS 2>/dev/null | awk 'BEGIN{found=0} {idline=$0; if(getline){roleline=$0} if(match(idline, /"([^"]+)"/, m)){id=m[1]; if(roleline ~ /[Ll]earner/ && id=="'"$node_num"'") found=1}} END{ if(found) exit 0; exit 1 }'; then
                echo "  Node $node_num is now learner"
                break
            fi
            ATTEMPT=$((ATTEMPT+1))
            echo "  Waiting for membership change (attempt $ATTEMPT/$ATTEMPTS)..."
            sleep 2
        done
        if [ $ATTEMPT -eq $ATTEMPTS ]; then
            echo "  Warning: node $node_num did not appear as learner after waiting"
        fi
    else
        echo "  Add learner output: $ADD_OUT"
    fi

done

echo "  Waiting for learners to sync logs..."
sleep 3

echo ""
echo "Step 4: Promoting learners to voters..."
echo "  Waiting for learners 2 and 3 to be visible in MetaRaft membership..."
WAIT_RETRIES=12
WAIT_ATTEMPT=0
PROMOTE_LIST=""
while [ $WAIT_ATTEMPT -lt $WAIT_RETRIES ]; do
    MEMBERS=$(run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS 2>&1) || true
    echo "  Raw MetaRaft members (debug):"
    echo "$MEMBERS" | sed -n '1,200p' | nl -ba -w2 -s ': '

    # Extract learner node ids robustly: read pairs of lines and print the id when the second line contains 'learner'
    LEARNERS=$(echo "$MEMBERS" | awk 'BEGIN{ORS=" "} {prev=$0; if(getline){line=$0; if(line ~ /[Ll]earner/){ if(match(prev, /"([^"]+)"/, m)) print m[1]; else if(prev ~ /^[0-9]+$/) print prev } } }')
    echo "  Detected learners: $LEARNERS"

    PROMOTE_LIST=""
    for id in $LEARNERS; do
        if [ "$id" = "2" ] || [ "$id" = "3" ]; then
            if [ -z "$PROMOTE_LIST" ]; then
                PROMOTE_LIST="$id"
            else
                PROMOTE_LIST="$PROMOTE_LIST $id"
            fi
        fi
    done

    if [ -n "$PROMOTE_LIST" ]; then
        echo "  Learners ready to promote: $PROMOTE_LIST"
        break
    fi

    WAIT_ATTEMPT=$((WAIT_ATTEMPT + 1))
    echo "  Waiting for learners to sync (attempt $WAIT_ATTEMPT/$WAIT_RETRIES)..."
    sleep 2
done

if [ -z "$PROMOTE_LIST" ]; then
    echo "  No learners 2 or 3 found after waiting, skipping promote step"
else
    echo "  Promoting learners $PROMOTE_LIST to voters..."

    # Ensure raft gRPC port is reachable for each learner before attempting promotion
    for id in $PROMOTE_LIST; do
        case "$id" in
            2)
                host=aikv2; port=50052;;
            3)
                host=aikv3; port=50053;;
            *)
                echo "  Unknown learner id $id, skipping port check"; continue;;
        esac

        PORT_RETRIES=12
        PORT_ATTEMPT=0

        # Prefer testing reachability from inside the leader container (aikv1) so Docker hostnames resolve
        if command -v docker-compose >/dev/null 2>&1; then
            DC_CMD="docker-compose"
        elif command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
            DC_CMD="docker compose"
        else
            DC_CMD=""
        fi

        while [ $PORT_ATTEMPT -lt $PORT_RETRIES ]; do
            if [ -n "$DC_CMD" ]; then
                # Test from within aikv1 container where service discovery works
                if $DC_CMD exec -T aikv1 bash -lc "bash -c '</dev/tcp/$host/$port'" >/dev/null 2>&1; then
                    echo "  Raft port $host:$port is open (checked from aikv1)"
                    break
                fi
                echo "  Raft port $host:$port appears closed from aikv1 (attempt $((PORT_ATTEMPT+1))/$PORT_RETRIES)"
            else
                # Fallback to testing from local host
                if bash -c "</dev/tcp/$host/$port" >/dev/null 2>&1; then
                    echo "  Raft port $host:$port is open (checked locally)"
                    break
                fi
                echo "  Raft port $host:$port appears closed locally (attempt $((PORT_ATTEMPT+1))/$PORT_RETRIES)"
            fi

            PORT_ATTEMPT=$((PORT_ATTEMPT + 1))
            echo "  Waiting for raft port $host:$port to be open (attempt $PORT_ATTEMPT/$PORT_RETRIES)..."
            sleep 2
        done

        if [ $PORT_ATTEMPT -eq $PORT_RETRIES ]; then
            echo "  Warning: raft port $host:$port not open after waiting; promotion may fail"
        fi
    done

    # Promote learners sequentially and wait for each to appear as voter (handles InProgress cases)
    for node in $PROMOTE_LIST; do
        echo "  Promoting node $node..."

        # Try to promote; if a change is already in progress, wait for it to finish
        ATTEMPTS=5; ATTEMPT=0
        while [ $ATTEMPT -lt $ATTEMPTS ]; do
            PROMOTE_OUTPUT=$(run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT PROMOTE $node 2>&1) || true
            echo "  Promote output: $PROMOTE_OUTPUT"

            if echo "$PROMOTE_OUTPUT" | grep -qi "OK"; then
                echo "  Node $node promotion acknowledged"
                break
            fi

            if echo "$PROMOTE_OUTPUT" | grep -qi "InProgress"; then
                echo "  Membership change in progress; waiting for node $node to become voter"

                # Wait for the node to appear as voter
                WAIT_MAX=60; WAIT=0
                while [ $WAIT -lt $WAIT_MAX ]; do
                    MEMBERS=$(run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS 2>&1) || true
                    if echo "$MEMBERS" | awk 'BEGIN{getline;prev=$0} {idline=prev; roleline=$0; if(match(idline, /"?([0-9]+)"?/, m)){id=m[1]; if(roleline ~ /[Vv]oter/ && id=="'"$node"'") print id; } prev=$0}' | grep -q "^$node$"; then
                        echo "  Node $node is now voter"
                        break
                    fi
                    WAIT=$((WAIT+1))
                    sleep 1
                done

                if [ $WAIT -ge $WAIT_MAX ]; then
                    echo "  Timeout waiting for node $node to become voter"
                    if [ "$DEBUG" -eq 1 ]; then
                        collect_diagnostics
                    fi
                    # Continue to next attempt to re-run promote
                else
                    # promoted successfully by the in-progress change
                    break
                fi
            else
                # Some other error; retry a few times with backoff
                ATTEMPT=$((ATTEMPT+1))
                echo "  Promote failed (attempt $ATTEMPT/$ATTEMPTS), retrying in $((ATTEMPT*2))s..."
                sleep $((ATTEMPT*2))
            fi
        done

        # Final membership check for node
        FINAL_MEMBERS=$(run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS 2>&1) || true
        if echo "$FINAL_MEMBERS" | awk 'BEGIN{getline;prev=$0} {idline=prev; roleline=$0; if(match(idline, /"?([0-9]+)"?/, m)){id=m[1]; if(roleline ~ /[Vv]oter/ && id=="'"$node"'") print id; } prev=$0}' | grep -q "^$node$"; then
            echo "  Confirmed: node $node is voter"
        else
            echo "  Warning: node $node is not voter after attempts"
            if [ "$DEBUG" -eq 1 ]; then
                collect_diagnostics
            fi
        fi
    done
fi

echo ""
echo "Step 5: Verifying MetaRaft membership..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS

echo ""
echo "Step 6: Adding nodes to cluster metadata..."
echo "  Meeting node 2..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6380 $NODE2_ID

echo "  Meeting node 3..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6381 $NODE3_ID

echo "  Meeting node 4..."
NODE4_ID=$(run_redis -h 127.0.0.1 -p 6382 CLUSTER MYID)
run_redis -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6382 $NODE4_ID

echo "  Meeting node 5..."
NODE5_ID=$(run_redis -h 127.0.0.1 -p 6383 CLUSTER MYID)
run_redis -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6383 $NODE5_ID

echo "  Meeting node 6..."
NODE6_ID=$(run_redis -h 127.0.0.1 -p 6384 CLUSTER MYID)
run_redis -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6384 $NODE6_ID

echo "  Waiting for cluster metadata to sync..."
sleep 2

echo ""
echo "Step 7: Assigning slots to master nodes (via leader)..."
# All slot assignments must go through the MetaRaft leader (node 1)
# Using ADDSLOTSRANGE for efficiency

echo "  Assigning slots 0-5460 to node 1..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 0 5460

echo "  Assigning slots 5461-10922 to node 2 (ID: $NODE2_ID)..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 5461 10922 $NODE2_ID

echo "  Assigning slots 10923-16383 to node 3 (ID: $NODE3_ID)..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 10923 16383 $NODE3_ID

echo ""
echo "Step 8: Setting up replication (nodes 4-6 as replicas via leader)..."
# Use CLUSTER ADDREPLICATION through the leader since replica nodes don't have ClusterMeta

echo "  Node 4 (ID: $NODE4_ID) replicating node 1 (ID: $NODE1_ID)..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER ADDREPLICATION $NODE4_ID $NODE1_ID

echo "  Node 5 (ID: $NODE5_ID) replicating node 2 (ID: $NODE2_ID)..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER ADDREPLICATION $NODE5_ID $NODE2_ID

echo "  Node 6 (ID: $NODE6_ID) replicating node 3 (ID: $NODE3_ID)..."
run_redis -h 127.0.0.1 -p 6379 CLUSTER ADDREPLICATION $NODE6_ID $NODE3_ID

echo ""
echo "================================"
echo "✅ Cluster initialization complete!"
echo "================================"
echo ""
echo "Cluster Status:"
run_redis -h 127.0.0.1 -p 6379 CLUSTER INFO
echo ""
echo "Cluster Nodes:"
run_redis -h 127.0.0.1 -p 6379 CLUSTER NODES
echo ""
echo "MetaRaft Members:"
run_redis -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS
echo ""
echo "You can now connect with: redis-cli -c -h 127.0.0.1 -p 6379"
