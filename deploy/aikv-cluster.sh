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
OTLP_ENDPOINT=""

usage() {
    printf '用法: %s build|up|down\n' "$(basename "$0")" >&2
    printf '  build [--local]\n' >&2
    printf '  up [-b|--bind IP] [-a|--announce IP]\n' >&2
    printf '  down [--purge]\n' >&2
    exit 2
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

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 ||
        die "required command not found: $1"
}

compose() {
    docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" "$@"
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
    require_command docker
    local image="${AIKV_IMAGE:-aikv:dev}"
    if (( use_local )); then
        if [[ ! -d "$WORKSPACE_ROOT/aidb" ]]; then
            printf 'error: local AiDb checkout not found: %s\n' "$WORKSPACE_ROOT/aidb" >&2
            exit 1
        fi
        printf 'Building %s with local AiDb source at %s...\n' "$image" "$WORKSPACE_ROOT/aidb"
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
        printf 'Building %s with GitHub main aidb dependency...\n' "$image"
        docker build -f "$AIKV_ROOT/deploy/Dockerfile" -t "$image" "$AIKV_ROOT"
    fi
    printf 'Successfully built %s\n' "$image"
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
    require_command docker
    docker compose version >/dev/null
    if [[ -z "$(compose ps -aq 2>/dev/null || true)" ]]; then
        printf 'aikv-cluster 未运行\n'
        exit 0
    fi
    if (( purge )); then
        compose down --volumes
    else
        compose down
    fi
}

redis_command() {
    local port="$1"
    shift
    redis-cli -h 127.0.0.1 -p "$port" -t 1 --raw "$@"
}

generate_configs() {
    local node index client_port rpc_port metrics_port node_name node_dir
    mkdir -p "$RUNTIME_ROOT"

    for index in "${!NODE_NAMES[@]}"; do
        node=$((index + 1))
        client_port="${CLIENT_PORTS[$index]}"
        rpc_port="${RPC_PORTS[$index]}"
        metrics_port="${METRICS_PORTS[$index]}"
        node_name="${NODE_NAMES[$index]}"
        node_dir="$RUNTIME_ROOT/node$node"

        mkdir -p "$node_dir"
        cp "$TEMPLATE" "$node_dir/aikv.toml"
        sed_exprs=(
            -e "s|^bind = .*|bind = \"0.0.0.0:$client_port\"|"
            -e 's|^metrics_addr = .*|metrics_addr = "0.0.0.0"|'
            -e "s|^metrics_port = .*|metrics_port = $metrics_port|"
        )
        if [[ -n "$OTLP_ENDPOINT" && "$OTLP_ENDPOINT" != "none" ]]; then
            sed_exprs+=(-e "s|^#\? *otlp_endpoint = .*|otlp_endpoint = \"$OTLP_ENDPOINT\"|")
        fi
        sed -i "${sed_exprs[@]}" "$node_dir/aikv.toml"
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

    die "$node did not answer PONG on 127.0.0.1:$port within ${STARTUP_TIMEOUT_SECONDS}s"
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
        die "conflicting cluster topology: expected 6 known nodes, found $known_count"

    for port in "${CLIENT_PORTS[@]}"; do
        [[ -n "$(node_line "$nodes" "$port")" ]] ||
            die "conflicting cluster topology: no node advertises $ANNOUNCE_IP:$port"
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

    die "cluster did not converge to 6 known nodes within ${STARTUP_TIMEOUT_SECONDS}s"
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
            die "CLUSTER MEET for $node_name returned: $response"
    done
}

replicate_if_needed() {
    local nodes="$1"
    local replica_port="$2"
    local primary_id="$3"
    local line flags slots response

    line="$(node_line "$nodes" "$replica_port")"
    [[ -n "$line" ]] || die "missing node for client port $replica_port"
    flags="$(node_flags_from_line "$line")"
    slots="$(node_slots_from_line "$line")"

    case ",$flags," in
        *,slave,*)
            [[ "$(node_primary_from_line "$line")" == "$primary_id" ]] ||
                die "conflicting replica relationship on port $replica_port"
            [[ -z "$slots" ]] ||
                die "conflicting slot ownership on replica port $replica_port"
            ;;
        *,master,*)
            [[ -z "$slots" ]] ||
                die "conflicting master slot ownership on port $replica_port"
            response="$(redis_command "$replica_port" CLUSTER REPLICATE "$primary_id")"
            [[ "$response" == "OK" ]] ||
                die "CLUSTER REPLICATE on port $replica_port returned: $response"
            ;;
        *)
            die "unexpected role flags on port $replica_port: $flags"
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
    [[ -n "$line" ]] || die "missing node for slot owner port $port"
    slots="$(node_slots_from_line "$line")"

    if [[ "$slots" == "$expected_slots" ]]; then
        return 0
    fi
    [[ -z "$slots" ]] ||
        die "conflicting slot ownership on port $port: $slots"

    for (( slot = start; slot <= end; slot++ )); do
        slot_args+=("$slot")
    done
    response="$(redis_command "$port" CLUSTER ADDSLOTS "${slot_args[@]}")"
    [[ "$response" == "OK" ]] ||
        die "CLUSTER ADDSLOTS on port $port returned: $response"
}

add_replica_if_needed() {
    local primary_port="$1"
    local replica_port="$2"
    local primary_id="$3"
    local nodes line flags primary slots response

    nodes="$(cluster_nodes)"
    line="$(node_line "$nodes" "$replica_port")"
    [[ -n "$line" ]] || die "missing node for replica port $replica_port"
    flags="$(node_flags_from_line "$line")"
    primary="$(node_primary_from_line "$line")"
    slots="$(node_slots_from_line "$line")"

    case ",$flags," in
        *,slave,*)
            [[ "$primary" == "$primary_id" ]] ||
                die "conflicting replica relationship on port $replica_port"
            [[ -z "$slots" ]] ||
                die "replica on port $replica_port owns slots"
            ;;
        *,master,*)
            [[ -z "$slots" ]] ||
                die "conflicting master slot ownership on port $replica_port"
            response="$(redis_command "$primary_port" CLUSTER ADD_REPLICA \
                "$primary_id" "$(node_id_from_line "$line")")"
            [[ "$response" == "OK" ]] ||
                die "CLUSTER ADD_REPLICA for port $replica_port returned: $response"
            ;;
        *)
            die "unexpected role flags on port $replica_port: $flags"
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
            die "expected master on port $port, got flags: $flags"
        [[ -n "$slots" ]] ||
            die "master on port $port has no slots"
    done

    for port in 6380 6381; do
        line="$(node_line "$nodes" "$port")"
        flags="$(node_flags_from_line "$line")"
        primary="$(node_primary_from_line "$line")"
        [[ ",$flags," == *,slave,* && "$primary" == "$master_id" ]] ||
            die "expected port $port to replicate $master_id"
        [[ -z "$(node_slots_from_line "$line")" ]] ||
            die "replica on port $port owns slots"
    done

    for port in 7380 7381; do
        line="$(node_line "$nodes" "$port")"
        flags="$(node_flags_from_line "$line")"
        primary="$(node_primary_from_line "$line")"
        [[ ",$flags," == *,slave,* && "$primary" == "$shard2_id" ]] ||
            die "expected port $port to replicate $shard2_id"
        [[ -z "$(node_slots_from_line "$line")" ]] ||
            die "replica on port $port owns slots"
    done

    master_count="$(awk -F' ' '$3 ~ /(^|,)master(,|$)/ { count++ } END { print count + 0 }' <<< "$nodes")"
    replica_count="$(awk -F' ' '$3 ~ /(^|,)slave(,|$)/ { count++ } END { print count + 0 }' <<< "$nodes")"
    [[ "$master_count" == "2" && "$replica_count" == "4" ]] ||
        die "expected 2 masters and 4 replicas, found $master_count masters and $replica_count replicas"

    info="$(redis_command 6379 CLUSTER INFO)"
    state="$(awk -F: '$1 == "cluster_state" { print $2 }' <<< "$info")"
    known="$(awk -F: '$1 == "cluster_known_nodes" { print $2 }' <<< "$info")"
    size="$(awk -F: '$1 == "cluster_size" { print $2 }' <<< "$info")"
    assigned="$(awk -F: '$1 == "cluster_slots_assigned" { print $2 }' <<< "$info")"
    [[ "$state" == "ok" ]] || die "cluster_state is $state, expected ok"
    [[ "$known" == "6" ]] || die "cluster_known_nodes is $known, expected 6"
    [[ "$size" == "2" ]] || die "cluster_size is $size, expected 2"
    [[ "$assigned" == "16384" ]] ||
        die "cluster_slots_assigned is $assigned, expected 16384"
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

    STARTUP_TIMEOUT_SECONDS="${AIKV_CLUSTER_TIMEOUT_SECONDS:-120}"
    if ! [[ "$STARTUP_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
        die "AIKV_CLUSTER_TIMEOUT_SECONDS must be a positive integer"
    fi
    OTLP_ENDPOINT="${AIKV_OTLP_ENDPOINT:-}"
    if [[ -z "$OTLP_ENDPOINT" ]]; then
        if docker ps --format '{{.Names}}' 2>/dev/null | grep -qE '^(aikv-)?otel-collector$'; then
            OTLP_ENDPOINT="http://aikv-otel-collector:4317"
        else
            OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-}"
        fi
    fi

    require_command docker
    require_command redis-cli
    require_command curl
    docker compose version >/dev/null
    local image="${AIKV_IMAGE:-aikv:dev}"
    if ! docker image inspect "$image" >/dev/null 2>&1; then
        printf 'error: 镜像不存在: %s\n先运行: %s build\n' "$image" "$(basename "$0")" >&2
        exit 1
    fi

    generate_configs
    # Remove containers from the previous service names before rebinding ports.
    compose up -d --remove-orphans
    wait_for_all_nodes

    local nodes known_count node1_id node4_id
    nodes="$(cluster_nodes)"
    known_count="$(awk 'NF { count++ } END { print count + 0 }' <<< "$nodes")"
    if (( known_count > 6 )); then
        die "conflicting cluster topology: found $known_count known nodes"
    fi
    meet_missing_nodes "$nodes"
    nodes="$(wait_for_known_nodes)"

    node1_id="$(node_id_from_line "$(node_line "$nodes" 6379)")"
    node4_id="$(node_id_from_line "$(node_line "$nodes" 7379)")"
    [[ "$node1_id" != "$node4_id" && -n "$node1_id" && -n "$node4_id" ]] ||
        die "could not obtain distinct master node IDs"

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
        printf 'aikv cluster is ready: 2 masters, 4 replicas, 16384 slots (OTel: %s)\n' "$OTLP_ENDPOINT"
    else
        printf 'aikv cluster is ready: 2 masters, 4 replicas, 16384 slots\n'
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
