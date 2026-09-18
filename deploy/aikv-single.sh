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
TEMPLATE="$SCRIPT_DIR/aikv.example.toml"
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

    local probe_ip="$bind_ip"
    if [[ "$probe_ip" == "0.0.0.0" ]]; then
        probe_ip="127.0.0.1"
    fi

    need docker redis-cli curl
    docker compose version >/dev/null
    ensure_network
    local timeout_seconds="${AIKV_STARTUP_TIMEOUT_SECONDS:-60}"
    if ! [[ "$timeout_seconds" =~ ^[1-9][0-9]*$ ]]; then
        die "AIKV_STARTUP_TIMEOUT_SECONDS 必须为正整数"
    fi
    local image="${AIKV_IMAGE:-aikv:dev}"
    if ! docker image inspect "$image" >/dev/null 2>&1; then
        die "镜像不存在: $image (请先执行: $(basename "$0") build)"
    fi

    mkdir -p "$RUNTIME_DIR/data"
    local otlp
    otlp="$(resolve_otlp)"
    local sed_exprs=(
        -e 's|^bind = .*|bind = "0.0.0.0:6379" # 仅用于容器内监听; 宿主端口映射由 Docker 提供|'
        -e 's|^metrics_addr = .*|metrics_addr = "0.0.0.0" # 仅用于容器内监听; 宿主端口映射由 Docker 提供|'
    )
    if [[ -n "$otlp" && "$otlp" != "none" ]]; then
        sed_exprs+=(-e "s|^#\? *otlp_endpoint = .*|otlp_endpoint = \"$otlp\"|")
    fi
    sed "${sed_exprs[@]}" "$TEMPLATE" > "$CONFIG_PATH"

    compose up -d

    local deadline=$((SECONDS + timeout_seconds))
    while (( SECONDS < deadline )); do
        if [[ "$(redis-cli -h "$probe_ip" -p 6379 -t 1 ping 2>/dev/null || true)" == "PONG" ]]; then
            break
        fi
        sleep 1
    done
    if (( SECONDS >= deadline )); then
        compose ps || true
        die "aikv 单机容器未能在 ${timeout_seconds}s 内响应 PONG (${probe_ip}:6379)"
    fi
    if ! curl -fsS --noproxy '*' --max-time 5 "http://${probe_ip}:9191/health" >/dev/null; then
        compose ps || true
        die "aikv 单机健康检查接口访问失败 (http://${probe_ip}:9191/health)"
    fi
    if [[ -n "$otlp" && "$otlp" != "none" ]]; then
        printf 'aikv 单机服务已就绪: %s:6379 (OTel: %s)\n' "$probe_ip" "$otlp"
    else
        printf 'aikv 单机服务已就绪: %s:6379\n' "$probe_ip"
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
        if (( purge )) && [[ -d "$RUNTIME_DIR" ]]; then
            rm -rf -- "$RUNTIME_DIR"
            printf 'aikv-single 未运行, 已清理残留的运行时数据与配置\n'
        else
            printf 'aikv-single 未运行\n'
        fi
        exit 0
    fi
    if (( purge )); then
        compose down --volumes
        rm -rf -- "$RUNTIME_DIR"
        printf 'aikv-single 已停止并清理全部运行时数据与配置\n'
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
