#!/usr/bin/env bash
# 可观测性容器栈: up | down
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
OBS_DIR="$SCRIPT_DIR/observability"
COMPOSE_FILE="$OBS_DIR/docker-compose.yaml"
NODE_COMPOSE_FILE="$OBS_DIR/docker-compose.node.yaml"
CADVISOR_COMPOSE_FILE="$OBS_DIR/docker-compose.cadvisor.yaml"
ENV_FILE="$SCRIPT_DIR/.env"
ENV_EXAMPLE="$SCRIPT_DIR/.env.example"

usage() {
    printf '用法: %s up|down\n' "$(basename "$0")" >&2
    printf '  up [--all|--server|--agent]\n' >&2
    printf '  down [--purge] [--all|--server|--agent]\n' >&2
    exit 2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

load_dotenv() {
    local line _k _v
    [[ -f "$ENV_FILE" ]] || return 0
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
    done < "$ENV_FILE"
}

need() {
    local cmd
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || die "required command not found: $cmd"
    done
}

probe_native_node_exporter() {
    curl -fsS --noproxy '*' -m 2 "http://127.0.0.1:9100/metrics" 2>/dev/null | grep -E '^node_' >/dev/null
}

has_cadvisor_support() {
    [[ -f "$CADVISOR_COMPOSE_FILE" ]] || return 1
    # macOS 或缺少 /dev/kmsg 设备的环境自动跳过 cAdvisor, 避免容器挂载崩溃
    if [[ "$OSTYPE" != "linux"* ]] || [[ ! -e "/dev/kmsg" ]]; then
        return 1
    fi
    return 0
}

write_target_if_changed() {
    local target_file="$1"
    local content="$2"
    if [[ -f "$target_file" ]] && [[ "$(<"$target_file")" == "$content" ]]; then
        return 0
    fi
    printf '%s\n' "$content" > "$target_file"
}

ensure_env() {
    if [[ ! -f "$ENV_FILE" ]]; then
        printf '未检测到 .env, 从 .env.example 复制默认配置\n'
        cp "$ENV_EXAMPLE" "$ENV_FILE"
    fi
}

ensure_targets() {
    local targets_dir="$OBS_DIR/config/prometheus/targets"
    mkdir -p "$targets_dir"
    if [[ ! -f "$targets_dir/node.yaml" && -f "$targets_dir/node.example.yaml" ]]; then
        cp "$targets_dir/node.example.yaml" "$targets_dir/node.yaml"
    fi
    if [[ ! -f "$targets_dir/cadvisor.yaml" && -f "$targets_dir/cadvisor.example.yaml" ]]; then
        cp "$targets_dir/cadvisor.example.yaml" "$targets_dir/cadvisor.yaml"
    fi
}

COMPOSE_FILES=()

ensure_network() {
    docker network inspect aikv-net >/dev/null 2>&1 || docker network create aikv-net >/dev/null
}

prepare_compose_files() {
    local role="$1"
    local targets_dir="$OBS_DIR/config/prometheus/targets"
    COMPOSE_FILES=()

    case "$role" in
        server)
            COMPOSE_FILES+=("-f" "$COMPOSE_FILE")
            ensure_targets
            ;;
        agent)
            if has_cadvisor_support; then
                COMPOSE_FILES+=("-f" "$CADVISOR_COMPOSE_FILE")
            fi
            COMPOSE_FILES+=("-f" "$NODE_COMPOSE_FILE")
            ;;
        all)
            COMPOSE_FILES+=("-f" "$COMPOSE_FILE")
            ensure_targets

            if has_cadvisor_support; then
                COMPOSE_FILES+=("-f" "$CADVISOR_COMPOSE_FILE")
                write_target_if_changed "$targets_dir/cadvisor.yaml" "- targets:
    - cadvisor:8080"
            else
                write_target_if_changed "$targets_dir/cadvisor.yaml" "- targets: []"
            fi

            if probe_native_node_exporter; then
                write_target_if_changed "$targets_dir/node.yaml" "- targets:
    - host.docker.internal:9100"
            else
                COMPOSE_FILES+=("-f" "$NODE_COMPOSE_FILE")
                write_target_if_changed "$targets_dir/node.yaml" "- targets:
    - node-exporter:9100"
            fi
            ;;
    esac
}

compose() {
    docker compose --project-directory "$OBS_DIR" "${COMPOSE_FILES[@]}" --env-file "$ENV_FILE" "$@"
}

wait_for_server() {
    local i prom_ready=0 grafana_ready=0 health_resp
    printf '等待 Prometheus / Grafana 就绪...\n'
    for ((i = 1; i <= 30; i++)); do
        if (( !prom_ready )) && curl -fsS --noproxy '*' "http://127.0.0.1:9090/-/ready" >/dev/null 2>&1; then
            prom_ready=1
            printf '  Prometheus 就绪 (http://127.0.0.1:9090)\n'
        fi
        if (( !grafana_ready )); then
            health_resp="$(curl -s --noproxy '*' "http://127.0.0.1:3000/api/health" 2>/dev/null || true)"
            if grep -qE '"database":\s*"ok"' <<<"$health_resp"; then
                grafana_ready=1
                printf '  Grafana 就绪 (http://127.0.0.1:3000)\n'
            fi
        fi
        if (( prom_ready && grafana_ready )); then
            break
        fi
        sleep 2
    done
    if (( !prom_ready || !grafana_ready )); then
        compose ps >&2 || true
        compose logs --tail 20 >&2 || true
        die "服务在 60s 内未完全就绪"
    fi
}

wait_for_agent() {
    local i node_ready=0
    printf '等待 Node Exporter 就绪...\n'
    for ((i = 1; i <= 30; i++)); do
        if curl -fsS --noproxy '*' -m 2 "http://127.0.0.1:9100/metrics" 2>/dev/null | grep -E '^node_' >/dev/null; then
            node_ready=1
            printf '  Node Exporter 就绪 (http://127.0.0.1:9100/metrics)\n'
            break
        fi
        sleep 1
    done
    if (( !node_ready )); then
        compose ps >&2 || true
        compose logs --tail 20 >&2 || true
        die "Node Exporter 在 30s 内未就绪"
    fi
}

print_server_ready() {
    printf '可观测性监控中心 (Server) 已就绪\n'
    printf '  Grafana:     http://127.0.0.1:3000 (admin/admin)\n'
    printf '  Prometheus:  http://127.0.0.1:9090\n'
    printf '  OTel gRPC:   127.0.0.1:4317\n'
    printf '  OTel HTTP:   127.0.0.1:4318\n'
}

print_agent_ready() {
    printf '可观测性节点探针 (Agent) 已就绪\n'
    printf '  Node Exp:    http://127.0.0.1:9100/metrics\n'
    if has_cadvisor_support; then
        printf '  cAdvisor:    http://127.0.0.1:8080\n'
    fi
}

cmd_up() {
    local role="all"
    while (( $# > 0 )); do
        case "$1" in
            --all) role="all"; shift ;;
            --server) role="server"; shift ;;
            --agent) role="agent"; shift ;;
            -h|--help) usage ;;
            *) usage ;;
        esac
    done

    need docker curl
    docker compose version >/dev/null
    ensure_env
    load_dotenv
    ensure_network
    prepare_compose_files "$role"

    case "$role" in
        server)
            printf '正在启动可观测性监控中心 (Server)...\n'
            compose up -d
            wait_for_server
            print_server_ready
            ;;
        agent)
            if has_cadvisor_support; then
                printf '正在启动节点探针 (Agent: Node Exporter + cAdvisor)...\n'
            else
                printf '正在启动节点探针 (Agent: Node Exporter, 已跳过 cAdvisor)...\n'
            fi
            compose up -d
            wait_for_agent
            print_agent_ready
            ;;
        all)
            if has_cadvisor_support; then
                printf '正在启动全量可观测性容器服务 (含 cAdvisor)...\n'
            else
                printf '正在启动全量可观测性容器服务 (非 Linux/无 /dev/kmsg 环境, 已自动跳过 cAdvisor)...\n'
            fi
            compose up -d
            wait_for_server
            print_server_ready
            print_agent_ready
            ;;
    esac
}

cmd_down() {
    local purge=0
    local role="all"
    while (( $# > 0 )); do
        case "$1" in
            --purge) purge=1; shift ;;
            --all) role="all"; shift ;;
            --server) role="server"; shift ;;
            --agent) role="agent"; shift ;;
            -h|--help) usage ;;
            *) usage ;;
        esac
    done

    need docker
    docker compose version >/dev/null
    ensure_env
    load_dotenv
    prepare_compose_files "$role"

    if [[ -z "$(compose ps -aq 2>/dev/null || true)" ]]; then
        printf 'observability (%s) 未运行\n' "$role"
        exit 0
    fi

    if (( purge )); then
        compose down --volumes
        printf 'observability (%s) 已停止并清理关联资源\n' "$role"
    else
        compose down
        printf 'observability (%s) 已停止\n' "$role"
    fi
}

load_dotenv
cmd="${1:-}"
if [[ -z "$cmd" ]]; then
    usage
fi
shift
case "$cmd" in
    up) cmd_up "$@" ;;
    down) cmd_down "$@" ;;
    -h|--help) usage ;;
    *) usage ;;
esac
