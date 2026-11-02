#!/usr/bin/env bash
# 启动/停止 2 个独立的 1 主 2 从 AiKv 集群:
# 集群 1: Master 6379 (槽位 0-16383, CPU 0,1), 从节点 6380 (CPU 2,3), 6381 (CPU 4,5)
# 集群 2: Master 7379 (槽位 0-16383, CPU 6,7), 从节点 7380 (CPU 8,9), 7381 (CPU 10,11)
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.cluster.yaml"
TEMPLATE="$SCRIPT_DIR/aikv.example.toml"
RUNTIME_ROOT="$SCRIPT_DIR/.runtime/cluster"
PROJECT_NAME="aikv-cluster"
export DOCKER_BUILDKIT=1

CLIENT_PORTS=(6379 6380 6381 7379 7380 7381)
RPC_PORTS=(16379 16380 16381 17379 17380 17381)
METRICS_PORTS=(9191 9192 9193 9194 9195 9196)
NODE_NAMES=(aikv-1 aikv-2 aikv-3 aikv-4 aikv-5 aikv-6)
STARTUP_TIMEOUT_SECONDS=60
ANNOUNCE_IP="${AIKV_ANNOUNCE_IP:-127.0.0.1}"
export AIKV_BIND_IP="${AIKV_BIND_IP:-0.0.0.0}"
export AIKV_NODE1_CPUSET="${AIKV_NODE1_CPUSET:-0,1}"
export AIKV_NODE2_CPUSET="${AIKV_NODE2_CPUSET:-2,3}"
export AIKV_NODE3_CPUSET="${AIKV_NODE3_CPUSET:-4,5}"
export AIKV_NODE4_CPUSET="${AIKV_NODE4_CPUSET:-6,7}"
export AIKV_NODE5_CPUSET="${AIKV_NODE5_CPUSET:-8,9}"
export AIKV_NODE6_CPUSET="${AIKV_NODE6_CPUSET:-10,11}"
export AIKV_BATCH_WAIT_US="${AIKV_BATCH_WAIT_US:-250}"
PROBE_IP="127.0.0.1"

redis_command() {
    local port="$1"
    shift
    redis-cli -h "$PROBE_IP" -p "$port" -t 2 --raw "$@"
}

ensure_network() {
    docker network inspect aikv-net >/dev/null 2>&1 || docker network create aikv-net >/dev/null
}

compose() {
    docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" "$@"
}

generate_configs() {
    local node index client_port rpc_port metrics_port node_name node_dir sed_exprs
    mkdir -p "$RUNTIME_ROOT"

    for index in "${!NODE_NAMES[@]}"; do
        node=$((index + 1))
        client_port="${CLIENT_PORTS[$index]}"
        rpc_port="${RPC_PORTS[$index]}"
        metrics_port="${METRICS_PORTS[$index]}"
        node_name="${NODE_NAMES[$index]}"
        node_dir="$RUNTIME_ROOT/node$node"

        mkdir -p "$node_dir/data"
        sed_exprs=(
            -e "s|^bind = .*|bind = \"0.0.0.0:$client_port\"|"
            -e 's|^metrics_addr = .*|metrics_addr = "0.0.0.0"|'
            -e "s|^metrics_port = .*|metrics_port = $metrics_port|"
        )
        sed "${sed_exprs[@]}" "$TEMPLATE" > "$node_dir/aikv.toml"

        {
            printf '\n[cluster]\n'
            printf 'node_id = %d\n' "$node"
            printf 'rpc_addr = "%s:%s"\n' "$node_name" "$rpc_port"
            if (( node == 1 )); then
                printf 'peers = []\n'
            elif (( node <= 3 )); then
                printf 'peers = ["aikv-1:16379"]\n'
            elif (( node == 4 )); then
                printf 'peers = []\n'
            else
                printf 'peers = ["aikv-4:17379"]\n'
            fi
            printf 'cluster_data_port_offset = 10000\n'
            printf 'client_addr = "%s:%s"\n' "$ANNOUNCE_IP" "$client_port"
            printf 'announce_mode = "fixed"\n'
        } >> "$node_dir/aikv.toml"
    done
}

wait_for_pong() {
    local port="$1"
    local node="$2"
    local deadline=$((SECONDS + STARTUP_TIMEOUT_SECONDS))

    while (( SECONDS < deadline )); do
        if [[ "$(redis_command "$port" ping 2>/dev/null || true)" == "PONG" ]]; then
            return 0
        fi
        sleep 1
    done

    echo "error: $node 未能在 ${STARTUP_TIMEOUT_SECONDS}s 内响应 PONG ($PROBE_IP:$port)" >&2
    exit 1
}

add_all_slots() {
    local master_port="$1"
    local start end slot
    local slot_args=()
    for (( start = 0; start < 16384; start += 4096 )); do
        end=$((start + 4095))
        slot_args=()
        for (( slot = start; slot <= end; slot++ )); do
            slot_args+=("$slot")
        done
        redis_command "$master_port" CLUSTER ADDSLOTS "${slot_args[@]}" >/dev/null
    done
}

cmd_down() {
    compose down --volumes --remove-orphans || true
    rm -rf -- "$RUNTIME_ROOT"
    echo "已清理所有集群容器与数据"
}

cmd_up() {
    local bind_ip="${AIKV_BIND_IP:-0.0.0.0}"
    ANNOUNCE_IP="${AIKV_ANNOUNCE_IP:-127.0.0.1}"
    PROBE_IP="$bind_ip"
    if [[ "$PROBE_IP" == "0.0.0.0" ]]; then
        PROBE_IP="127.0.0.1"
    fi

    echo "==> [1/5] 清理旧环境并重新生成配置..."
    cmd_down
    generate_configs
    ensure_network

    echo "==> [2/5] 启动 6 个容器..."
    compose up -d --remove-orphans

    echo "==> [3/5] 等待所有 6 个节点 PONG 就绪..."
    for index in "${!CLIENT_PORTS[@]}"; do
        wait_for_pong "${CLIENT_PORTS[$index]}" "${NODE_NAMES[$index]}"
    done

    echo "==> [4/5] 配置集群 1 (Master 6379, 1主2从, 0-16383 全量槽位)..."
    redis_command 6379 CLUSTER MEET aikv-2 6380 16380 "$ANNOUNCE_IP" >/dev/null
    redis_command 6379 CLUSTER MEET aikv-3 6381 16381 "$ANNOUNCE_IP" >/dev/null
    sleep 2
    add_all_slots 6379

    local m1_nodes m1_id n2_id n3_id
    m1_nodes="$(redis_command 6379 CLUSTER NODES)"
    m1_id="$(awk '$2 ~ /:6379@/ { print $1 }' <<< "$m1_nodes")"
    n2_id="$(awk '$2 ~ /:6380@/ { print $1 }' <<< "$m1_nodes")"
    n3_id="$(awk '$2 ~ /:6381@/ { print $1 }' <<< "$m1_nodes")"

    redis_command 6380 CLUSTER REPLICATE "$m1_id" >/dev/null
    redis_command 6381 CLUSTER REPLICATE "$m1_id" >/dev/null
    redis_command 6379 CLUSTER ADD_REPLICA "$m1_id" "$n2_id" >/dev/null
    redis_command 6379 CLUSTER ADD_REPLICA "$m1_id" "$n3_id" >/dev/null

    echo "==> [5/5] 配置集群 2 (Master 7379, 1主2从, 0-16383 全量槽位)..."
    redis_command 7379 CLUSTER MEET aikv-5 7380 17380 "$ANNOUNCE_IP" >/dev/null
    redis_command 7379 CLUSTER MEET aikv-6 7381 17381 "$ANNOUNCE_IP" >/dev/null
    sleep 2
    add_all_slots 7379

    local m2_nodes m2_id n5_id n6_id
    m2_nodes="$(redis_command 7379 CLUSTER NODES)"
    m2_id="$(awk '$2 ~ /:7379@/ { print $1 }' <<< "$m2_nodes")"
    n5_id="$(awk '$2 ~ /:7380@/ { print $1 }' <<< "$m2_nodes")"
    n6_id="$(awk '$2 ~ /:7381@/ { print $1 }' <<< "$m2_nodes")"

    redis_command 7380 CLUSTER REPLICATE "$m2_id" >/dev/null
    redis_command 7381 CLUSTER REPLICATE "$m2_id" >/dev/null
    redis_command 7379 CLUSTER ADD_REPLICA "$m2_id" "$n5_id" >/dev/null
    redis_command 7379 CLUSTER ADD_REPLICA "$m2_id" "$n6_id" >/dev/null

    sleep 1

    echo "==> 验证集群 1 (6379):"
    redis_command 6379 CLUSTER INFO | grep -E 'cluster_state|cluster_known_nodes|cluster_size|cluster_slots_assigned'
    echo "==> 验证集群 2 (7379):"
    redis_command 7379 CLUSTER INFO | grep -E 'cluster_state|cluster_known_nodes|cluster_size|cluster_slots_assigned'

    echo ""
    echo "两套独立 1 主 2 从 AiKv 集群部署成功!"
}

case "${1:-up}" in
    up) cmd_up ;;
    down) cmd_down ;;
    *) echo "用法: $0 up|down" >&2; exit 1 ;;
esac
