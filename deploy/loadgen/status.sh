#!/usr/bin/env bash
# 查看 loadgen 运行状态与当前生效参数 (不含任何压测统计).
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="$SCRIPT_DIR/../.runtime/loadgen/loadgen.pid"
BIND="${LOADGEN_BIND:-127.0.0.1:8787}"
probe_host="${BIND%:*}"
probe_port="${BIND##*:}"
if [[ "$probe_host" == "0.0.0.0" || "$probe_host" == "::" ]]; then
    probe_host="127.0.0.1"
fi
probe_url="http://${probe_host}:${probe_port}"

curl_local() {
    curl -fsS --noproxy '*' --max-time "$1" "$2"
}

running=0
if [[ -f "$PID_FILE" ]]; then
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        running=1
        printf '进程: 运行中 (PID %s)\n' "$pid"
    fi
fi
if (( ! running )); then
    printf '进程: 未运行\n'
fi

if ! curl_local 2 "${probe_url}/health" >/dev/null 2>&1; then
    printf '探活: 失败 (%s/health 无响应)\n' "$probe_url"
    exit 1
fi
printf '探活: OK (%s)\n' "$probe_url"
printf '当前配置:\n'
curl_local 5 "${probe_url}/api/config" | python3 -m json.tool 2>/dev/null ||
    curl_local 5 "${probe_url}/api/config"
