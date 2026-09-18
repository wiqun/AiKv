#!/usr/bin/env bash
# aikv 集群容器: build | up | down
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
AIKV_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
AIKV_PARENT="$(dirname -- "$AIKV_ROOT")"
if [[ "$(basename -- "$AIKV_PARENT")" == ".worktrees" ]]; then
    WORKSPACE_ROOT="$(cd -- "$AIKV_ROOT/../.." && pwd)"
else
    WORKSPACE_ROOT="$(cd -- "$AIKV_ROOT/.." && pwd)"
fi
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.cluster.yaml"
TEMPLATE="$SCRIPT_DIR/aikv.example.toml"
RUNTIME_ROOT="$SCRIPT_DIR/.runtime/cluster"
PROJECT_NAME="aikv-cluster"
export DOCKER_BUILDKIT=1

CLIENT_PORTS=(6379 6380 6381 7379 7380 7381)
RPC_PORTS=(16379 16380 16381 17379 17380 17381)
METRICS_PORTS=(9191 9192 9193 9194 9195 9196)
NODE_NAMES=(aikv-1 aikv-2 aikv-3 aikv-4 aikv-5 aikv-6)
STARTUP_TIMEOUT_SECONDS=""
ANNOUNCE_IP=""
PROBE_IP="127.0.0.1"
OTLP_ENDPOINT=""

usage() {
    printf '用法: %s build|up|down\n' "$(basename "$0")" >&2
    printf '  build [--local]\n' >&2
    printf '  up [-b|--bind IP] [-a|--announce IP]\n' >&2
    printf '  down [--purge]\n' >&2
    exit 2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

load_dotenv() {
    local line _k _v
    [[ -f "$SCRIPT_DIR/.env" ]] || return 0
    while IFS= read -r line || [[ -n "$line" ]]; do
        [[ "$line" =~ ^[[:space:]]*# ]] && continue
        [[ "$line" =~ ^[[:space:]]*$ ]] && continue
        if [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]]; then
            _k="${BASH_REMATCH[1]}"
            _v="${BASH_REMATCH[2]}"
            _v="${_v%\"}"
            _v="${_v#\"}"
            _v="${_v%\'}"
            _v="${_v#\'}"
            if [[ -z "${!_k+x}" ]]; then
                export "$_k=$_v"
            fi
        fi
    done < "$SCRIPT_DIR/.env"
}

need() {
    local cmd
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || die "required command not found: $cmd"
    done
}

compose() {
    docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" "$@"
}

ensure_network() {
    docker network inspect aikv-net >/dev/null 2>&1 || docker network create aikv-net >/dev/null
}

copy_context_tree() {
    tar -C "$1" \
        --exclude='.git' \
        --exclude='target' \
        --exclude='.runtime' \
        --exclude='.env' \
        --exclude='.env.*' \
        --exclude='.venv*' \
        --exclude='*.log' \
        --exclude='*.pid' \
        -cf - . | tar -C "$2" -xf -
}

cmd_build() {
    local use_local=0
    while (( $# > 0 )); do
        case "$1" in
            --local) use_local=1; shift ;;
            -h|--help) usage ;;
            *) usage ;;
        esac
    done
    need docker
    local image="${AIKV_IMAGE:-aikv:dev}"
    if (( use_local )); then
        [[ -d "$WORKSPACE_ROOT/aidb" ]] || die "未找到本地 AiDb 源码目录: $WORKSPACE_ROOT/aidb"
        printf '正在基于本地 AiDb 源码 (%s) 构建镜像 %s...\n' "$WORKSPACE_ROOT/aidb" "$image"
        local ctx
        ctx="$(mktemp -d "${TMPDIR:-/tmp}/aikv-local-context.XXXXXX")"
        cleanup_local_context() { rm -rf -- "$ctx"; }
        trap cleanup_local_context EXIT
        mkdir -p "$ctx/aikv" "$ctx/aidb"
        copy_context_tree "$AIKV_ROOT" "$ctx/aikv"
        copy_context_tree "$WORKSPACE_ROOT/aidb" "$ctx/aidb"
        docker build -f "$ctx/aikv/deploy/Dockerfile.local" -t "$image" "$ctx"
        trap - EXIT
        cleanup_local_context
    else
        printf '正在基于 GitHub 主分支构建镜像 %s...\n' "$image"
        docker build -f "$AIKV_ROOT/deploy/Dockerfile" -t "$image" "$AIKV_ROOT"
    fi
    printf '镜像构建成功: %s\n' "$image"
}

cmd_down() {
    local purge=0
    while (( $# > 0 )); do
        case "$1" in
            --purge) purge=1; shift ;;
            -h|--help) usage ;;
            *) usage ;;
        esac
    done
    need docker
    docker compose version >/dev/null
    if [[ -z "$(compose ps -aq 2>/dev/null || true)" ]]; then
        if (( purge )) && [[ -d "$RUNTIME_ROOT" ]]; then
            rm -rf -- "$RUNTIME_ROOT"
            printf 'aikv-cluster 未运行, 已清理残留的节点数据与配置\n'
        else
            printf 'aikv-cluster 未运行\n'
        fi
        exit 0
    fi
    if (( purge )); then
        compose down --volumes
        rm -rf -- "$RUNTIME_ROOT"
        printf 'aikv-cluster 已停止并清理全部节点数据与配置\n'
    else
        compose down
    fi
}

redis_command() {
    local port="$1"
    shift
    redis-cli -h "$PROBE_IP" -p "$port" -t 1 --raw "$@"
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
        if [[ -n "$OTLP_ENDPOINT" && "$OTLP_ENDPOINT" != "none" ]]; then
            sed_exprs+=(-e "s|^#\? *otlp_endpoint = .*|otlp_endpoint = \"$OTLP_ENDPOINT\"|")
        fi
        sed "${sed_exprs[@]}" "$TEMPLATE" > "$node_dir/aikv.toml"

        {
            printf '\n[cluster]\n'
            printf 'node_id = %d\n' "$node"
            printf 'rpc_addr = "%s:%s"\n' "$node_name" "$rpc_port"
            if (( node == 1 )); then
                printf 'peers = []\n'
            else
                printf 'peers = ["aikv-1:16379"]\n'
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

    die "$node 未能在 ${STARTUP_TIMEOUT_SECONDS}s 内响应 PONG ($PROBE_IP:$port)"
}

wait_for_all_nodes() {
    local index
    for index in "${!CLIENT_PORTS[@]}"; do
        wait_for_pong "${CLIENT_PORTS[$index]}" "${NODE_NAMES[$index]}"
    done
}

cluster_nodes() {
    redis_command 6379 CLUSTER NODES
}

node_line_for_port() {
    local port="$1"
    local pattern=":$(printf '%s' "$port")@"
    awk -v pattern="$pattern" '$2 ~ pattern { print; exit }'
}

node_line() {
    local nodes="$1"
    local port="$2"
    node_line_for_port "$port" <<< "$nodes"
}

node_id_from_line() {
    awk '{ print $1 }' <<< "$1"
}

node_flags_from_line() {
    awk '{ print $3 }' <<< "$1"
}

node_primary_from_line() {
    awk '{ print $4 }' <<< "$1"
}

node_slots_from_line() {
    awk '{
        for (i = 9; i <= NF; i++) {
            printf "%s%s", (i == 9 ? "" : " "), $i
        }
        print ""
    }' <<< "$1"
}

validate_known_nodes() {
    local nodes="$1"
    local known_count
    local port

    known_count="$(awk 'NF { count++ } END { print count + 0 }' <<< "$nodes")"
    [[ "$known_count" == "6" ]] ||
        die "集群拓扑冲突: 期望 6 个已知节点, 实际发现 $known_count"

    for port in "${CLIENT_PORTS[@]}"; do
        [[ -n "$(node_line "$nodes" "$port")" ]] ||
            die "集群拓扑冲突: 节点列表未包含端口 $port"
    done
}

wait_for_known_nodes() {
    local deadline=$((SECONDS + STARTUP_TIMEOUT_SECONDS))
    local nodes=""

    while (( SECONDS < deadline )); do
        nodes="$(cluster_nodes 2>/dev/null || true)"
        if [[ "$(awk 'NF { count++ } END { print count + 0 }' <<< "$nodes")" == "6" ]]; then
            validate_known_nodes "$nodes"
            printf '%s' "$nodes"
            return 0
        fi
        sleep 1
    done

    die "集群未能在 ${STARTUP_TIMEOUT_SECONDS}s 内收敛至 6 个已知节点"
}

meet_missing_nodes() {
    local nodes="$1"
    local index port rpc_port node_name response

    for index in "${!CLIENT_PORTS[@]}"; do
        port="${CLIENT_PORTS[$index]}"
        rpc_port="${RPC_PORTS[$index]}"
        node_name="${NODE_NAMES[$index]}"
        (( index == 0 )) && continue

        if [[ -n "$(node_line "$nodes" "$port")" ]]; then
            continue
        fi

        response="$(redis_command 6379 CLUSTER MEET "$node_name" "$port" "$rpc_port" "$ANNOUNCE_IP")"
        [[ "$response" == "OK" ]] ||
            die "对 $node_name 执行 CLUSTER MEET 失败, 返回: $response"
    done
}

replicate_if_needed() {
    local nodes="$1"
    local replica_port="$2"
    local primary_id="$3"
    local line flags slots response

    line="$(node_line "$nodes" "$replica_port")"
    [[ -n "$line" ]] || die "未找到客户端端口为 $replica_port 的节点"
    flags="$(node_flags_from_line "$line")"
    slots="$(node_slots_from_line "$line")"

    case ",$flags," in
        *,slave,*)
            [[ "$(node_primary_from_line "$line")" == "$primary_id" ]] ||
                die "端口 $replica_port 上的副本关系冲突"
            [[ -z "$slots" ]] ||
                die "端口 $replica_port 上的副本异常持有 slot"
            ;;
        *,master,*)
            [[ -z "$slots" ]] ||
                die "端口 $replica_port 上的节点已有 slot 分配冲突"
            response="$(redis_command "$replica_port" CLUSTER REPLICATE "$primary_id")"
            [[ "$response" == "OK" ]] ||
                die "在端口 $replica_port 执行 CLUSTER REPLICATE 失败, 返回: $response"
            ;;
        *)
            die "端口 $replica_port 上的节点角色标签异常: $flags"
            ;;
    esac
}

add_slots_if_needed() {
    local port="$1"
    local expected_slots="$2"
    local start="$3"
    local end="$4"
    local nodes line slots response
    local slot_args=()
    local slot

    nodes="$(cluster_nodes)"
    line="$(node_line "$nodes" "$port")"
    [[ -n "$line" ]] || die "未找到 slot 目标端口为 $port 的节点"
    slots="$(node_slots_from_line "$line")"

    if [[ "$slots" == "$expected_slots" ]]; then
        return 0
    fi
    [[ -z "$slots" ]] ||
        die "端口 $port 上的 slot 分配冲突: $slots"

    for (( slot = start; slot <= end; slot++ )); do
        slot_args+=("$slot")
    done
    response="$(redis_command "$port" CLUSTER ADDSLOTS "${slot_args[@]}")"
    [[ "$response" == "OK" ]] ||
        die "在端口 $port 执行 CLUSTER ADDSLOTS 失败, 返回: $response"
}

add_replica_if_needed() {
    local primary_port="$1"
    local replica_port="$2"
    local primary_id="$3"
    local nodes line flags primary slots response

    nodes="$(cluster_nodes)"
    line="$(node_line "$nodes" "$replica_port")"
    [[ -n "$line" ]] || die "未找到端口为 $replica_port 的副本节点"
    flags="$(node_flags_from_line "$line")"
    primary="$(node_primary_from_line "$line")"
    slots="$(node_slots_from_line "$line")"

    case ",$flags," in
        *,slave,*)
            [[ "$primary" == "$primary_id" ]] ||
                die "端口 $replica_port 上的副本主节点从属关系冲突"
            [[ -z "$slots" ]] ||
                die "端口 $replica_port 上的副本节点异常持有 slot"
            ;;
        *,master,*)
            [[ -z "$slots" ]] ||
                die "端口 $replica_port 上的主节点已有 slot 分配冲突"
            response="$(redis_command "$primary_port" CLUSTER ADD_REPLICA \
                "$primary_id" "$(node_id_from_line "$line")")"
            [[ "$response" == "OK" ]] ||
                die "对端口 $replica_port 执行 CLUSTER ADD_REPLICA 失败, 返回: $response"
            ;;
        *)
            die "端口 $replica_port 上的节点角色标签异常: $flags"
            ;;
    esac
}

validate_final_topology() {
    local nodes info line flags primary slots
    local master_id shard2_id
    local master_count replica_count assigned known size state

    nodes="$(cluster_nodes)"
    validate_known_nodes "$nodes"
    line="$(node_line "$nodes" 6379)"
    master_id="$(node_id_from_line "$line")"
    line="$(node_line "$nodes" 7379)"
    shard2_id="$(node_id_from_line "$line")"

    for port in 6379 7379; do
        line="$(node_line "$nodes" "$port")"
        flags="$(node_flags_from_line "$line")"
        slots="$(node_slots_from_line "$line")"
        [[ ",$flags," == *,master,* ]] ||
            die "端口 $port 期望为主节点, 实际角色标签: $flags"
        [[ -n "$slots" ]] ||
            die "端口 $port 上的主节点未持有 slot"
    done

    for port in 6380 6381; do
        line="$(node_line "$nodes" "$port")"
        flags="$(node_flags_from_line "$line")"
        primary="$(node_primary_from_line "$line")"
        [[ ",$flags," == *,slave,* && "$primary" == "$master_id" ]] ||
            die "端口 $port 期望复制主节点 $master_id"
        [[ -z "$(node_slots_from_line "$line")" ]] ||
            die "端口 $port 上的副本节点异常持有 slot"
    done

    for port in 7380 7381; do
        line="$(node_line "$nodes" "$port")"
        flags="$(node_flags_from_line "$line")"
        primary="$(node_primary_from_line "$line")"
        [[ ",$flags," == *,slave,* && "$primary" == "$shard2_id" ]] ||
            die "端口 $port 期望复制分片主节点 $shard2_id"
        [[ -z "$(node_slots_from_line "$line")" ]] ||
            die "端口 $port 上的副本节点异常持有 slot"
    done

    master_count="$(awk -F' ' '$3 ~ /(^|,)master(,|$)/ { count++ } END { print count + 0 }' <<< "$nodes")"
    replica_count="$(awk -F' ' '$3 ~ /(^|,)slave(,|$)/ { count++ } END { print count + 0 }' <<< "$nodes")"
    [[ "$master_count" == "2" && "$replica_count" == "4" ]] ||
        die "期望 2 个主节点与 4 个副本节点, 实际发现 $master_count 个主节点, $replica_count 个副本节点"

    info="$(redis_command 6379 CLUSTER INFO)"
    state="$(awk -F: '$1 == "cluster_state" { print $2 }' <<< "$info")"
    known="$(awk -F: '$1 == "cluster_known_nodes" { print $2 }' <<< "$info")"
    size="$(awk -F: '$1 == "cluster_size" { print $2 }' <<< "$info")"
    assigned="$(awk -F: '$1 == "cluster_slots_assigned" { print $2 }' <<< "$info")"
    [[ "$state" == "ok" ]] || die "cluster_state 为 $state, 期望为 ok"
    [[ "$known" == "6" ]] || die "cluster_known_nodes 为 $known, 期望为 6"
    [[ "$size" == "2" ]] || die "cluster_size 为 $size, 期望为 2"
    [[ "$assigned" == "16384" ]] ||
        die "cluster_slots_assigned 为 $assigned, 期望为 16384"
}

cmd_up() {
    local bind_ip="${AIKV_BIND_IP:-127.0.0.1}"
    ANNOUNCE_IP="${AIKV_ANNOUNCE_IP:-127.0.0.1}"
    while (( $# > 0 )); do
        case "$1" in
            -b|--bind)
                if (( $# < 2 )); then
                    printf 'error: %s 需要参数\n' "$1" >&2
                    usage
                fi
                bind_ip="$2"
                shift 2
                ;;
            -a|--announce)
                if (( $# < 2 )); then
                    printf 'error: %s 需要参数\n' "$1" >&2
                    usage
                fi
                ANNOUNCE_IP="$2"
                shift 2
                ;;
            -h|--help) usage ;;
            *) usage ;;
        esac
    done
    export AIKV_BIND_IP="$bind_ip"

    PROBE_IP="$bind_ip"
    if [[ "$PROBE_IP" == "0.0.0.0" ]]; then
        PROBE_IP="127.0.0.1"
    fi

    STARTUP_TIMEOUT_SECONDS="${AIKV_CLUSTER_TIMEOUT_SECONDS:-120}"
    if ! [[ "$STARTUP_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
        die "AIKV_CLUSTER_TIMEOUT_SECONDS 必须为正整数"
    fi
    OTLP_ENDPOINT="${AIKV_OTLP_ENDPOINT:-}"
    if [[ -z "$OTLP_ENDPOINT" ]]; then
        if docker ps --format '{{.Names}}' 2>/dev/null | grep -qE '^(aikv-)?otel-collector$'; then
            OTLP_ENDPOINT="http://aikv-otel-collector:4317"
        else
            OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-}"
        fi
    fi

    need docker redis-cli curl
    docker compose version >/dev/null
    ensure_network
    local image="${AIKV_IMAGE:-aikv:dev}"
    if ! docker image inspect "$image" >/dev/null 2>&1; then
        die "镜像不存在: $image (请先执行: $(basename "$0") build)"
    fi

    generate_configs
    compose up -d --remove-orphans
    wait_for_all_nodes

    local nodes known_count node1_id node4_id
    nodes="$(cluster_nodes)"
    known_count="$(awk 'NF { count++ } END { print count + 0 }' <<< "$nodes")"
    if (( known_count > 6 )); then
        die "集群拓扑冲突: 发现 $known_count 个已知节点"
    fi
    meet_missing_nodes "$nodes"
    nodes="$(wait_for_known_nodes)"

    node1_id="$(node_id_from_line "$(node_line "$nodes" 6379)")"
    node4_id="$(node_id_from_line "$(node_line "$nodes" 7379)")"
    [[ "$node1_id" != "$node4_id" && -n "$node1_id" && -n "$node4_id" ]] ||
        die "未能获取到互不相同的主节点 ID"

    replicate_if_needed "$nodes" 6380 "$node1_id"
    replicate_if_needed "$nodes" 6381 "$node1_id"
    replicate_if_needed "$nodes" 7380 "$node4_id"
    replicate_if_needed "$nodes" 7381 "$node4_id"

    add_slots_if_needed 6379 "0-8191" 0 8191
    add_slots_if_needed 7379 "8192-16383" 8192 16383

    add_replica_if_needed 6379 6380 "$node1_id"
    add_replica_if_needed 6379 6381 "$node1_id"
    add_replica_if_needed 7379 7380 "$node4_id"
    add_replica_if_needed 7379 7381 "$node4_id"

    validate_final_topology
    if [[ -n "$OTLP_ENDPOINT" && "$OTLP_ENDPOINT" != "none" ]]; then
        printf 'aikv 集群已就绪: 2 主节点, 4 副本节点, 16384 槽位 (OTel: %s)\n' "$OTLP_ENDPOINT"
    else
        printf 'aikv 集群已就绪: 2 主节点, 4 副本节点, 16384 槽位\n'
    fi
}

load_dotenv
cmd="${1:-}"
if [[ -z "$cmd" ]]; then
    usage
fi
shift
case "$cmd" in
    build) cmd_build "$@" ;;
    up) cmd_up "$@" ;;
    down) cmd_down "$@" ;;
    -h|--help) usage ;;
    *) usage ;;
esac
