#!/usr/bin/env bash
# loadgen 进程: build | up | down | status
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$SCRIPT_DIR/loadgen"
BIN="$CRATE_DIR/target/release/loadgen"
WEB_DIR="$CRATE_DIR/web"
DEFAULT_CONFIG="$CRATE_DIR/loadgen.toml"
RUNTIME_DIR="$SCRIPT_DIR/.runtime/loadgen"
PID_FILE="$RUNTIME_DIR/loadgen.pid"
LOG_FILE="$RUNTIME_DIR/loadgen.log"
BIND_FILE="$RUNTIME_DIR/bind"
BIND="${LOADGEN_BIND:-127.0.0.1:8787}"

usage() {
    printf '用法: %s build|up|down|status\n' "$(basename "$0")" >&2
    printf '  up [-b|--bind ADDR:PORT] [-f|--config PATH]\n' >&2
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

probe_url_from_bind() {
    local probe_host="${BIND%:*}"
    local probe_port="${BIND##*:}"
    if [[ "$probe_host" == "0.0.0.0" || "$probe_host" == "::" ]]; then
        probe_host="127.0.0.1"
    fi
    printf 'http://%s:%s' "$probe_host" "$probe_port"
}

curl_local() {
    curl -fsS --noproxy '*' --max-time "$1" "$2"
}

is_running() {
    [[ -f "$PID_FILE" ]] || return 1
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

load_saved_bind() {
    if [[ -f "$BIND_FILE" ]]; then
        BIND="$(cat "$BIND_FILE")"
    fi
}

cmd_build() {
    if (( $# > 0 )); then
        usage
    fi
    need cargo
    (cd "$CRATE_DIR" && cargo build --release)
    printf '构建完成: %s\n' "$BIN"
}

cmd_down() {
    if (( $# > 0 )); then
        usage
    fi
    if [[ ! -f "$PID_FILE" ]]; then
        printf 'loadgen 未运行 (无 pid 文件)\n'
        exit 0
    fi
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    if [[ -z "$pid" ]] || ! kill -0 "$pid" 2>/dev/null; then
        printf 'loadgen 未运行, 清理过期 pid 文件\n'
        rm -f "$PID_FILE"
        exit 0
    fi
    kill "$pid" 2>/dev/null || true
    local _
    for _ in $(seq 1 50); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.2
    done
    if kill -0 "$pid" 2>/dev/null; then
        printf '进程未及时退出, 发送 SIGKILL\n'
        kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$PID_FILE"
    printf 'loadgen 已停止\n'
}

cmd_status() {
    if (( $# > 0 )); then
        usage
    fi
    need curl
    load_saved_bind
    local probe_url pid
    probe_url="$(probe_url_from_bind)"
    if is_running; then
        pid="$(cat "$PID_FILE")"
        printf '进程: 运行中 (PID %s)\n' "$pid"
    else
        printf '进程: 未运行\n'
        exit 1
    fi
    if ! curl_local 2 "${probe_url}/health" >/dev/null 2>&1; then
        printf '探活: 失败 (%s/health 无响应)\n' "$probe_url"
        exit 1
    fi
    printf '探活: OK (%s)\n' "$probe_url"
    printf '当前配置:\n'
    curl_local 5 "${probe_url}/api/config" | python3 -m json.tool 2>/dev/null ||
        curl_local 5 "${probe_url}/api/config"
}

cmd_up() {
    local config=""
    while (( $# > 0 )); do
        case "$1" in
            -b|--bind)
                if (( $# < 2 )); then
                    printf 'error: %s 需要参数\n' "$1" >&2
                    usage
                fi
                BIND="$2"
                shift 2
                ;;
            -f|--config)
                if (( $# < 2 )); then
                    printf 'error: %s 需要参数\n' "$1" >&2
                    usage
                fi
                config="$2"
                shift 2
                ;;
            -h|--help) usage ;;
            *) usage ;;
        esac
    done

    need curl
    local probe_url
    probe_url="$(probe_url_from_bind)"

    if is_running; then
        printf 'loadgen 已在运行 (PID %s), 控制台 %s\n' "$(cat "$PID_FILE")" "$probe_url"
        exit 0
    fi

    if [[ ! -x "$BIN" ]]; then
        cmd_build
    fi

    mkdir -p "$RUNTIME_DIR"
    local args=(--bind "$BIND" --web-dir "$WEB_DIR")
    if [[ -n "$config" ]]; then
        args+=(--config "$config")
    elif [[ -f "$DEFAULT_CONFIG" ]]; then
        args+=(--config "$DEFAULT_CONFIG")
    fi

    nohup "$BIN" "${args[@]}" </dev/null >>"$LOG_FILE" 2>&1 &
    local pid=$!

    sleep 0.1
    if ! kill -0 "$pid" 2>/dev/null; then
        printf 'error: 未能启动 loadgen, 请检查日志: %s\n' "$LOG_FILE" >&2
        tail -n 20 "$LOG_FILE" >&2 || true
        exit 1
    fi

    printf '%s' "$pid" > "$PID_FILE"
    printf '%s' "$BIND" > "$BIND_FILE"

    local _
    for _ in $(seq 1 50); do
        if curl_local 1 "${probe_url}/health" >/dev/null 2>&1; then
            printf 'loadgen 已启动 (PID %s)\n控制台: %s\n日志: %s\n' "$pid" "$probe_url" "$LOG_FILE"
            exit 0
        fi
        sleep 0.2
    done

    printf 'error: loadgen 未通过探活, 请检查日志: %s\n' "$LOG_FILE" >&2
    tail -n 20 "$LOG_FILE" >&2 || true
    exit 1
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
    status) cmd_status "$@" ;;
    build) cmd_build "$@" ;;
    -h|--help) usage ;;
    *) usage ;;
esac
