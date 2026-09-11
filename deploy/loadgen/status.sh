#!/usr/bin/env bash
# 查看 loadgen 运行状态与当前生效参数 (不含任何压测统计).
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="$SCRIPT_DIR/../.runtime/loadgen/loadgen.pid"
BIND="${LOADGEN_BIND:-127.0.0.1:8787}"

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

if ! curl -fsS --max-time 2 "http://${BIND}/health" >/dev/null 2>&1; then
    printf '探活: 失败 (%s/health 无响应)\n' "$BIND"
    exit 1
fi
printf '探活: OK (%s)\n' "$BIND"
printf '当前配置:\n'
curl -fsS "http://${BIND}/api/config" | python3 -m json.tool 2>/dev/null ||
    curl -fsS "http://${BIND}/api/config"
