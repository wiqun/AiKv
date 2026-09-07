#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

show_help() {
    cat << 'EOF'
用法: ./down-observability.sh [选项]

选项:
  -c, --clean    停止容器并清理 Named Volume 数据卷 (prom-data, grafana-data)
  -h, --help     显示本帮助信息

示例:
  ./down-observability.sh          # 停止并移除容器与网络 (保留历史数据)
  ./down-observability.sh --clean  # 停止并彻底清除所有数据卷
EOF
}

CLEAN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        -c|--clean)
            CLEAN=true
            shift
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            echo "❌ 未知参数: $1" >&2
            show_help >&2
            exit 1
            ;;
    esac
done

echo "=== [AiKv Observability] 停止可观测性监控栈 ==="

if [ "${CLEAN}" = true ]; then
    echo "⚠️ 警告: 检测到 --clean 参数！将永久删除 prom-data 与 grafana-data 数据卷，所有历史监控数据与仪表盘缓存将被清空。"
    docker compose down -v
    echo "✓ 容器已停止，数据卷已清理完成。"
else
    docker compose down
    echo "✓ 容器已优雅停止 (数据卷已保留)。"
fi
