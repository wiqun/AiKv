#!/usr/bin/env bash
# 启动 loadgen 控制台 (后台), 按需构建并探活.
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$SCRIPT_DIR/../.runtime/loadgen"
BIN="$SCRIPT_DIR/target/release/loadgen"
PID_FILE="$RUNTIME_DIR/loadgen.pid"
LOG_FILE="$RUNTIME_DIR/loadgen.log"
BIND="${LOADGEN_BIND:-127.0.0.1:8787}"

usage() {
    printf '用法: %s [--bind ADDR:PORT] [--mode single|cluster] [--endpoints LIST] [--foreground] [--build]\n' \
        "$(basename "$0")" >&2
    printf '环境变量: LOADGEN_BIND (默认 127.0.0.1:8787), LOADGEN_LOG (默认 info)\n' >&2
    exit 2
}

for cmd in curl cargo; do
    command -v "$cmd" >/dev/null 2>&1 || {
        printf 'error: required command not found: %s\n' "$cmd" >&2
        exit 1
    }
done

mode="cluster"
endpoints="127.0.0.1:6379"
foreground=0
force_build=0
while (( $# > 0 )); do
    case "$1" in
        --bind) BIND="$2"; shift 2 ;;
        --mode) mode="$2"; shift 2 ;;
        --endpoints) endpoints="$2"; shift 2 ;;
        --foreground) foreground=1; shift ;;
        --build) force_build=1; shift ;;
        -h|--help) usage ;;
        *) usage ;;
    esac
done

is_running() {
    [[ -f "$PID_FILE" ]] || return 1
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

# 0.0.0.0 不能作为浏览器/curl 目标; 探活始终走 127.0.0.1, 并绕过 http_proxy.
probe_host="${BIND%:*}"
probe_port="${BIND##*:}"
if [[ "$probe_host" == "0.0.0.0" || "$probe_host" == "::" ]]; then
    probe_host="127.0.0.1"
fi
probe_url="http://${probe_host}:${probe_port}"

curl_local() {
    curl -fsS --noproxy '*' --max-time "$1" "$2"
}

if is_running; then
    printf 'loadgen 已在运行 (PID %s), 控制台 %s\n' "$(cat "$PID_FILE")" "$probe_url"
    exit 0
fi

if (( force_build )) || [[ ! -x "$BIN" ]]; then
    bash "$SCRIPT_DIR/build.sh"
fi

mkdir -p "$RUNTIME_DIR"
args=(--bind "$BIND" --mode "$mode" --endpoints "$endpoints" --log "${LOADGEN_LOG:-info}")

if (( foreground )); then
    exec "$BIN" "${args[@]}"
fi

# 新会话启动, 避免 Cursor/agent shell 退出时把进程组一起杀掉.
setsid -f -- "$BIN" "${args[@]}" </dev/null >>"$LOG_FILE" 2>&1
pid=""
for _ in $(seq 1 20); do
    pid="$(pgrep -n -f -- "$BIN" || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        break
    fi
    sleep 0.1
done
if [[ -z "$pid" ]]; then
    printf 'error: 未能启动 loadgen, 请检查日志: %s\n' "$LOG_FILE" >&2
    tail -n 20 "$LOG_FILE" >&2 || true
    exit 1
fi
printf '%s' "$pid" > "$PID_FILE"

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
