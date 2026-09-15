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
    printf '  down [--purge]\n' >&2
    exit 2
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

probe_native_node_exporter() {
    curl -fsS -m 2 "http://127.0.0.1:9100/metrics" 2>/dev/null | grep -qE '^node_'
}

ensure_env() {
    if [[ ! -f "$ENV_FILE" ]]; then
        printf '未检测到 .env, 从 .env.example 复制默认配置\n'
        cp "$ENV_EXAMPLE" "$ENV_FILE"
    fi
}

COMPOSE_FILES=()

prepare_compose_files() {
    local targets_dir="$OBS_DIR/config/prometheus/targets"
    COMPOSE_FILES=("-f" "$COMPOSE_FILE")
    mkdir -p "$targets_dir"
    if [[ ! -f "$targets_dir/cadvisor.yaml" ]]; then
        cat <<'EOF' >"$targets_dir/cadvisor.yaml"
- targets:
    - cadvisor:8080
EOF
    fi
    if probe_native_node_exporter; then
        cat <<'EOF' >"$targets_dir/node.yaml"
- targets:
    - host.docker.internal:9100
EOF
    else
        COMPOSE_FILES+=("-f" "$NODE_COMPOSE_FILE")
        cat <<'EOF' >"$targets_dir/node.yaml"
- targets:
    - node-exporter:9100
EOF
    fi
    if [[ -f "$CADVISOR_COMPOSE_FILE" ]]; then
        COMPOSE_FILES+=("-f" "$CADVISOR_COMPOSE_FILE")
    fi
}

compose() {
    docker compose --project-directory "$OBS_DIR" "${COMPOSE_FILES[@]}" --env-file "$ENV_FILE" "$@"
}

cmd_up() {
    if (( $# > 0 )); then
        usage
    fi
    need docker curl
    docker compose version >/dev/null
    ensure_env
    prepare_compose_files
    printf '正在启动可观测性容器服务...\n'
    compose up -d

    local i prom_ready=0 grafana_ready=0 health_resp
    printf '等待 Prometheus / Grafana 就绪...\n'
    for ((i = 1; i <= 30; i++)); do
        if (( !prom_ready )) && curl -fsS "http://127.0.0.1:9090/-/ready" >/dev/null 2>&1; then
            prom_ready=1
            printf '  Prometheus 就绪 (http://127.0.0.1:9090)\n'
        fi
        if (( !grafana_ready )); then
            health_resp="$(curl -s "http://127.0.0.1:3000/api/health" 2>/dev/null || true)"
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
        printf 'error: 服务在 60s 内未完全就绪\n' >&2
        compose ps >&2 || true
        compose logs --tail 20 >&2 || true
        exit 1
    fi
    printf '可观测性监控栈已就绪\n'
    printf '  Grafana:     http://127.0.0.1:3000 (admin/admin)\n'
    printf '  Prometheus:  http://127.0.0.1:9090\n'
    printf '  OTel gRPC:   127.0.0.1:4317\n'
    printf '  OTel HTTP:   127.0.0.1:4318\n'
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
    ensure_env
    prepare_compose_files
    if [[ -z "$(compose ps -aq 2>/dev/null || true)" ]]; then
        printf 'observability 未运行\n'
        exit 0
    fi
    if (( purge )); then
        compose down --volumes
    else
        compose down
    fi
}

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
