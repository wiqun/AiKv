#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBS_DIR="${SCRIPT_DIR}/observability"
COMPOSE_FILE="${OBS_DIR}/docker-compose.yaml"
ENV_FILE="${SCRIPT_DIR}/.env"
ENV_EXAMPLE="${SCRIPT_DIR}/.env.example"

compose() {
    docker compose --project-directory "${OBS_DIR}" -f "${COMPOSE_FILE}" --env-file "${ENV_FILE}" "$@"
}

echo "=== [AiKv Observability] 启动可观测性监控栈 ==="

# 1. 检查 Docker 与 Compose 环境
if ! command -v docker &> /dev/null; then
    echo "❌ 错误: 未检测到 docker 命令，请先安装 Docker。" >&2
    exit 1
fi

if ! docker compose version &> /dev/null; then
    echo "❌ 错误: 未检测到 docker compose 插件支持。" >&2
    exit 1
fi

# 2. 检查 .env 配置文件，缺失时从模板复制
if [ ! -f "${ENV_FILE}" ]; then
    echo "ℹ️ 未检测到 .env 文件，自动从 .env.example 创建默认配置..."
    cp "${ENV_EXAMPLE}" "${ENV_FILE}"
fi

# 3. 拉起容器栈 (幂等执行)
echo "🚀 正在启动容器服务 (otel-collector, prometheus, grafana)..."
compose up -d

# 4. 轮询探活与就绪检查 (超时保护 60s)
echo "⏳ 正在等待服务就绪与探活..."
MAX_RETRIES=30
RETRY_INTERVAL=2

prom_ready=false
grafana_ready=false

for ((i=1; i<=MAX_RETRIES; i++)); do
    # 检查 Prometheus 就绪接口
    if [ "${prom_ready}" = false ]; then
        if curl -s -f "http://127.0.0.1:9090/-/ready" > /dev/null 2>&1; then
            prom_ready=true
            echo "  ✓ Prometheus 就绪 (http://127.0.0.1:9090)"
        fi
    fi

    # 检查 Grafana 健康接口
    if [ "${grafana_ready}" = false ]; then
        health_resp=$(curl -s "http://127.0.0.1:3000/api/health" 2>/dev/null || true)
        if echo "${health_resp}" | grep -qE '"database":\s*"ok"'; then
            grafana_ready=true
            echo "  ✓ Grafana 就绪 (http://127.0.0.1:3000)"
        fi
    fi

    if [ "${prom_ready}" = true ] && [ "${grafana_ready}" = true ]; then
        break
    fi

    sleep "${RETRY_INTERVAL}"
done

# 5. 超时处理与诊断输出
if [ "${prom_ready}" = false ] || [ "${grafana_ready}" = false ]; then
    echo "❌ 错误: 服务在 60s 内未完全就绪，输出诊断信息:" >&2
    compose ps >&2
    echo "--- 最近容器日志 ---" >&2
    compose logs --tail 20 >&2
    exit 1
fi

echo "=========================================================="
echo "🎉 可观测性监控栈已成功启动并就绪！"
echo "  - Grafana 仪表盘:  http://127.0.0.1:3000 (admin/admin, 默认免密查看)"
echo "  - Prometheus API:  http://127.0.0.1:9090"
echo "  - OTel gRPC 接收:  127.0.0.1:4317"
echo "  - OTel HTTP 接收:  127.0.0.1:4318"
echo "=========================================================="
