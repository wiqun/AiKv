#!/usr/bin/env bash
# 停止 loadgen: SIGTERM → 最多 10s → SIGKILL.
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="$SCRIPT_DIR/../.runtime/loadgen/loadgen.pid"

if [[ ! -f "$PID_FILE" ]]; then
    printf 'loadgen 未运行 (无 pid 文件)\n'
    exit 0
fi

pid="$(cat "$PID_FILE" 2>/dev/null || true)"
if [[ -z "$pid" ]] || ! kill -0 "$pid" 2>/dev/null; then
    printf 'loadgen 未运行, 清理过期 pid 文件\n'
    rm -f "$PID_FILE"
    exit 0
fi

kill "$pid" 2>/dev/null || true
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
