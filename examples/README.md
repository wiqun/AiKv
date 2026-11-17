# AiKv 部署与配置示例

本目录提供开箱即用的单机与 3 节点集群 Docker Compose 演示配置。

## 文件结构

```shell
examples/
├── configs/
│   ├── single.toml                 # 单机完整配置示例
│   └── cluster.toml                # 集群节点基础配置示例
├── docker-compose.single.yaml      # 单机容器编排
├── docker-compose.cluster.yaml     # 3 节点集群容器编排 (通过 -o 覆盖节点差异字段)
└── README.md
```

## 快速启动

```bash
# 单机实例 (端口 6379, 指标 9191)
docker compose -f docker-compose.single.yaml up -d

# 3 节点集群 (端口 6379~6381, 指标 9191~9193)
docker compose -f docker-compose.cluster.yaml up -d
```
