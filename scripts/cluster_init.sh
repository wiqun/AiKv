#!/bin/bash
# AiKv Cluster Initialization Script
#
# This script initializes an AiKv cluster by:
# 1. Connecting all nodes using CLUSTER MEET
# 2. Assigning hash slots to master nodes
# 3. Setting up replication relationships
#
# This is an alternative to redis-cli --cluster create that properly
# handles AiKv's AiDb-based consensus layer.

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default configuration
MASTERS=()
REPLICAS=()
REDIS_CLI="${REDIS_CLI:-redis-cli}"

# Print functions
print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

# Usage information
usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Initialize an AiKv cluster with specified nodes.

Options:
    -m, --masters HOSTS     Comma-separated list of master nodes (host:port)
    -r, --replicas HOSTS    Comma-separated list of replica nodes (host:port)
    -h, --help              Show this help message

Example:
    $0 -m 127.0.0.1:6379,127.0.0.1:6380,127.0.0.1:6381 \\
       -r 127.0.0.1:6382,127.0.0.1:6383,127.0.0.1:6384

    This creates a 6-node cluster with 3 masters and 3 replicas:
    - Master 1: 127.0.0.1:6379 -> Replica: 127.0.0.1:6382
    - Master 2: 127.0.0.1:6380 -> Replica: 127.0.0.1:6383
    - Master 3: 127.0.0.1:6381 -> Replica: 127.0.0.1:6384

Default (when no options provided):
    Masters: 127.0.0.1:6379, 127.0.0.1:6380, 127.0.0.1:6381
    Replicas: 127.0.0.1:6382, 127.0.0.1:6383, 127.0.0.1:6384

EOF
    exit 0
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -m|--masters)
            IFS=',' read -ra MASTERS <<< "$2"
            shift 2
            ;;
        -r|--replicas)
            IFS=',' read -ra REPLICAS <<< "$2"
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            print_error "Unknown option: $1"
            usage
            ;;
    esac
done

# Set defaults if not provided
if [ ${#MASTERS[@]} -eq 0 ]; then
    MASTERS=("127.0.0.1:6379" "127.0.0.1:6380" "127.0.0.1:6381")
fi

if [ ${#REPLICAS[@]} -eq 0 ]; then
    REPLICAS=("127.0.0.1:6382" "127.0.0.1:6383" "127.0.0.1:6384")
fi

# Validate configuration
MASTER_COUNT=${#MASTERS[@]}
REPLICA_COUNT=${#REPLICAS[@]}

print_info "Cluster Configuration:"
echo "  Masters: ${MASTER_COUNT}"
for i in "${!MASTERS[@]}"; do
    echo "    Master $((i+1)): ${MASTERS[$i]}"
done
echo "  Replicas: ${REPLICA_COUNT}"
for i in "${!REPLICAS[@]}"; do
    echo "    Replica $((i+1)): ${REPLICAS[$i]}"
done
echo

# Validate that we have at least 3 masters
if [ ${MASTER_COUNT} -lt 3 ]; then
    print_error "At least 3 master nodes are required for a cluster"
    exit 1
fi

# Check redis-cli availability
if ! command -v ${REDIS_CLI} &> /dev/null; then
    print_error "redis-cli not found. Please install redis-cli or set REDIS_CLI environment variable."
    exit 1
fi

# Function to execute redis command
redis_exec() {
    local host=$1
    local port=$2
    shift 2
    ${REDIS_CLI} -h ${host} -p ${port} "$@" 2>/dev/null
}

# Function to get node ID
get_node_id() {
    local host=$1
    local port=$2
    local node_id=$(redis_exec ${host} ${port} CLUSTER MYID 2>/dev/null)
    echo "${node_id}"
}

# Function to check if node is reachable
check_node() {
    local host=$1
    local port=$2
    if redis_exec ${host} ${port} PING | grep -q "PONG"; then
        return 0
    else
        return 1
    fi
}

# Step 1: Check all nodes are reachable
print_info "Step 1: Checking node connectivity..."
ALL_NODES=("${MASTERS[@]}" "${REPLICAS[@]}")
for node in "${ALL_NODES[@]}"; do
    IFS=':' read -r host port <<< "${node}"
    if check_node ${host} ${port}; then
        print_success "Node ${node} is reachable"
    else
        print_error "Node ${node} is not reachable"
        exit 1
    fi
done
echo

# Step 2: Get node IDs
print_info "Step 2: Retrieving node IDs..."
declare -A NODE_IDS
for node in "${ALL_NODES[@]}"; do
    IFS=':' read -r host port <<< "${node}"
    node_id=$(get_node_id ${host} ${port})
    if [ -z "${node_id}" ] || [ "${node_id}" == "(nil)" ]; then
        print_error "Failed to get node ID for ${node}"
        exit 1
    fi
    NODE_IDS["${node}"]="${node_id}"
    print_success "Node ${node} -> ID ${node_id}"
done
echo

# Step 3: Form cluster using CLUSTER MEET
print_info "Step 3: Forming cluster (CLUSTER MEET)..."
# Use first master as the meet point
first_master="${MASTERS[0]}"
IFS=':' read -r first_host first_port <<< "${first_master}"

for node in "${ALL_NODES[@]}"; do
    if [ "${node}" == "${first_master}" ]; then
        continue
    fi
    
    IFS=':' read -r host port <<< "${node}"
    
    # Execute MEET command on the first master to add this node
    print_info "Meeting ${node} from ${first_master}..."
    if redis_exec ${first_host} ${first_port} CLUSTER MEET ${host} ${port} | grep -q "OK"; then
        print_success "Successfully met ${node}"
    else
        print_warn "CLUSTER MEET may have failed for ${node}, continuing..."
    fi
    
    # Also execute MEET in reverse direction to ensure bidirectional connection
    print_info "Meeting ${first_master} from ${node}..."
    if redis_exec ${host} ${port} CLUSTER MEET ${first_host} ${first_port} | grep -q "OK"; then
        print_success "Reverse MEET successful"
    else
        print_warn "Reverse CLUSTER MEET may have failed, continuing..."
    fi
    
    # Small delay to allow nodes to sync
    sleep 0.5
done

# Additional cross-meets between all nodes to ensure full connectivity
print_info "Ensuring full mesh connectivity..."
for i in "${!ALL_NODES[@]}"; do
    for j in "${!ALL_NODES[@]}"; do
        if [ $i -ge $j ]; then
            continue
        fi
        
        node1="${ALL_NODES[$i]}"
        node2="${ALL_NODES[$j]}"
        
        IFS=':' read -r host1 port1 <<< "${node1}"
        IFS=':' read -r host2 port2 <<< "${node2}"
        
        redis_exec ${host1} ${port1} CLUSTER MEET ${host2} ${port2} > /dev/null 2>&1 || true
    done
done

print_success "Cluster mesh formed"
echo

# Wait for cluster to stabilize
print_info "Waiting for cluster to stabilize..."
sleep 2

# Step 4: Assign hash slots to masters
print_info "Step 4: Assigning hash slots to masters..."
TOTAL_SLOTS=16384
SLOTS_PER_MASTER=$((TOTAL_SLOTS / MASTER_COUNT))

for i in "${!MASTERS[@]}"; do
    master="${MASTERS[$i]}"
    IFS=':' read -r host port <<< "${master}"
    
    start_slot=$((i * SLOTS_PER_MASTER))
    if [ $i -eq $((MASTER_COUNT - 1)) ]; then
        # Last master gets remaining slots
        end_slot=$((TOTAL_SLOTS - 1))
    else
        end_slot=$((start_slot + SLOTS_PER_MASTER - 1))
    fi
    
    print_info "Assigning slots ${start_slot}-${end_slot} to ${master}..."
    
    # Build slot range arguments
    slot_args=()
    for ((slot=start_slot; slot<=end_slot; slot++)); do
        slot_args+=("${slot}")
    done
    
    # Execute ADDSLOTS command
    if redis_exec ${host} ${port} CLUSTER ADDSLOTS "${slot_args[@]}" | grep -q "OK"; then
        print_success "Assigned slots ${start_slot}-${end_slot} to ${master}"
    else
        print_error "Failed to assign slots to ${master}"
        exit 1
    fi
done
echo

# Step 5: Set up replication
print_info "Step 5: Setting up replication..."

# Calculate replicas per master
REPLICAS_PER_MASTER=$((REPLICA_COUNT / MASTER_COUNT))
if [ ${REPLICAS_PER_MASTER} -eq 0 ]; then
    print_warn "Not enough replicas for all masters. Some masters will have no replicas."
    REPLICAS_PER_MASTER=1
fi

replica_idx=0
for i in "${!MASTERS[@]}"; do
    if [ ${replica_idx} -ge ${REPLICA_COUNT} ]; then
        break
    fi
    
    master="${MASTERS[$i]}"
    master_id="${NODE_IDS[${master}]}"
    
    # Assign replica(s) to this master
    for ((r=0; r<REPLICAS_PER_MASTER && replica_idx<REPLICA_COUNT; r++)); do
        replica="${REPLICAS[$replica_idx]}"
        IFS=':' read -r host port <<< "${replica}"
        
        print_info "Setting ${replica} as replica of ${master} (ID: ${master_id})..."
        
        if redis_exec ${host} ${port} CLUSTER REPLICATE ${master_id} | grep -q "OK"; then
            print_success "${replica} is now a replica of ${master}"
        else
            print_error "Failed to set up replication for ${replica}"
            exit 1
        fi
        
        replica_idx=$((replica_idx + 1))
    done
done
echo

# Step 6: Verify cluster status
print_info "Step 6: Verifying cluster status..."
sleep 1

first_master="${MASTERS[0]}"
IFS=':' read -r host port <<< "${first_master}"

print_info "Cluster info from ${first_master}:"
redis_exec ${host} ${port} CLUSTER INFO
echo

print_info "Cluster nodes:"
redis_exec ${host} ${port} CLUSTER NODES
echo

print_success "Cluster initialization completed!"
echo
print_info "You can now connect to the cluster using:"
echo "  redis-cli -c -h ${host} -p ${port}"
