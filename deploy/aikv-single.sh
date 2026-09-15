#!/usr/bin/env bash
# aikv 单机容器: build | up | down
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
AIKV_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
AIKV_PARENT="$(dirname -- "$AIKV_ROOT")"
if [[ "$(basename -- "$AIKV_PARENT")" == ".worktrees" ]]; then
    WORKSPACE_ROOT="$(cd -- "$AIKV_ROOT/../.." && pwd)"
else
    WORKSPACE_ROOT="$(cd -- "$AIKV_ROOT/.." && pwd)"
fi
RUNTIME_DIR="$SCRIPT_DIR/.runtime/single"
CONFIG_PATH="$RUNTIME_DIR/aikv.toml"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.single.yaml"
PROJECT_NAME="aikv-single"
export DOCKER_BUILDKIT=1

usage() {
    printf '用法: %s build|up|down\n' "$(basename "$0")" >&2
    printf '  build [--local]\n' >&2
    printf '  up [-b|--bind IP]\n' >&2
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

need() {
    local cmd
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || {
            printf 'error: required command not found: %s\n' "$cmd" >&2
            exit 1
        }
    done
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
    need docker
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

resolve_otlp() {
    local endpoint="${AIKV_OTLP_ENDPOINT:-}"
    if [[ -z "$endpoint" ]]; then
        if docker ps --format '{{.Names}}' 2>/dev/null | grep -qE '^(aikv-)?otel-collector$'; then
            endpoint="http://aikv-otel-collector:4317"
        else
            endpoint="${OTEL_EXPORTER_OTLP_ENDPOINT:-}"
        fi
    fi
    printf '%s' "$endpoint"
}

cmd_up() {
    local bind_ip="${AIKV_BIND_IP:-127.0.0.1}"
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
            -h|--help) usage ;;
            *) usage ;;
        esac
    done
    export AIKV_BIND_IP="$bind_ip"

    need docker redis-cli curl
    docker compose version >/dev/null
    local timeout_seconds="${AIKV_STARTUP_TIMEOUT_SECONDS:-60}"
    if ! [[ "$timeout_seconds" =~ ^[1-9][0-9]*$ ]]; then
        printf 'error: AIKV_STARTUP_TIMEOUT_SECONDS must be a positive integer\n' >&2
        exit 2
    fi
    local image="${AIKV_IMAGE:-aikv:dev}"
    if ! docker image inspect "$image" >/dev/null 2>&1; then
        printf 'error: 镜像不存在: %s\n先运行: %s build\n' "$image" "$(basename "$0")" >&2
        exit 1
    fi

    mkdir -p "$RUNTIME_DIR"
    cp "$SCRIPT_DIR/aikv.example.toml" "$CONFIG_PATH"
    local otlp
    otlp="$(resolve_otlp)"
    local sed_exprs=(
        -e 's|^bind = .*|bind = "0.0.0.0:6379" # 仅用于容器内监听; 宿主端口映射由 Docker 提供|'
        -e 's|^metrics_addr = .*|metrics_addr = "0.0.0.0" # 仅用于容器内监听; 宿主端口映射由 Docker 提供|'
    )
    if [[ -n "$otlp" && "$otlp" != "none" ]]; then
        sed_exprs+=(-e "s|^#\? *otlp_endpoint = .*|otlp_endpoint = \"$otlp\"|")
    fi
    sed -i "${sed_exprs[@]}" "$CONFIG_PATH"

    compose up -d

    local deadline=$((SECONDS + timeout_seconds))
    while (( SECONDS < deadline )); do
        if [[ "$(redis-cli -h 127.0.0.1 -p 6379 -t 1 ping 2>/dev/null || true)" == "PONG" ]]; then
            break
        fi
        sleep 1
    done
    if (( SECONDS >= deadline )); then
        printf 'error: aikv single did not answer PONG within %ss\n' \
            "$timeout_seconds" >&2
        compose ps || true
        exit 1
    fi
    if ! curl -fsS --max-time 5 http://127.0.0.1:9191/health >/dev/null; then
        printf 'error: aikv single health endpoint failed\n' >&2
        compose ps || true
        exit 1
    fi
    if [[ -n "$otlp" && "$otlp" != "none" ]]; then
        printf 'aikv single is ready on 127.0.0.1:6379 (OTel: %s)\n' "$otlp"
    else
        printf 'aikv single is ready on 127.0.0.1:6379\n'
    fi
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
        printf 'aikv-single 未运行\n'
        exit 0
    fi
    if (( purge )); then
        compose down --volumes
    else
        compose down
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
