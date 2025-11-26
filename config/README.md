# AiKv 配置模板 / Configuration Templates

此目录包含 AiKv 的配置文件模板。
This directory contains configuration templates for AiKv.

## 配置文件 / Configuration Files

| 文件 / File | 说明 / Description |
|------------|-------------------|
| `aikv.toml` | 单机模式配置模板 / Single node configuration template |
| `aikv-cluster.toml` | 集群模式配置模板 / Cluster mode configuration template |

## 使用方法 / Usage

### 单机模式 / Single Node Mode

```bash
# 复制配置模板
cp config/aikv.toml config.toml

# 编辑配置
vim config.toml

# 启动服务
./target/release/aikv --config config.toml
```

### 集群模式 / Cluster Mode

```bash
# 使用集群特性编译
cargo build --release --features cluster

# 为每个节点复制并修改配置
cp config/aikv-cluster.toml node1-config.toml
cp config/aikv-cluster.toml node2-config.toml
cp config/aikv-cluster.toml node3-config.toml

# 编辑每个节点的配置，修改以下参数：
# - server.port
# - server.cluster_port
# - cluster.node_id
# - cluster.node_name
# - cluster.peers

# 启动各节点
./target/release/aikv --config node1-config.toml
./target/release/aikv --config node2-config.toml
./target/release/aikv --config node3-config.toml
```

## 配置项说明 / Configuration Options

### 存储引擎 / Storage Engine

| 选项 / Option | 说明 / Description |
|--------------|-------------------|
| `memory` | 内存存储，性能最佳，无持久化 / In-memory, best performance, no persistence |
| `aidb` | AiDb LSM-Tree 存储，支持持久化 / AiDb LSM-Tree, supports persistence |

推荐：
- 开发/测试：使用 `memory`
- 生产环境/集群：使用 `aidb`

Recommendations:
- Development/Testing: Use `memory`
- Production/Cluster: Use `aidb`

### 集群端口规划 / Cluster Port Planning

建议使用以下端口规划：

| 节点 / Node | 数据端口 / Data Port | 集群端口 / Cluster Port |
|-------------|---------------------|------------------------|
| Node 1 | 6379 | 16379 |
| Node 2 | 6380 | 16380 |
| Node 3 | 6381 | 16381 |
| Node 4 | 6382 | 16382 |
| Node 5 | 6383 | 16383 |
| Node 6 | 6384 | 16384 |

### 最小集群配置 / Minimum Cluster Configuration

生产环境推荐至少 6 节点（3 主 3 从）：
Production recommends at least 6 nodes (3 masters + 3 replicas):

```
Node 1 (Master) ─── Node 4 (Replica)
Node 2 (Master) ─── Node 5 (Replica)
Node 3 (Master) ─── Node 6 (Replica)
```

## 环境变量覆盖 / Environment Variable Override

配置项可以通过环境变量覆盖（优先级高于配置文件）：
Configuration can be overridden by environment variables (higher priority than config file):

```bash
export AIKV_HOST=0.0.0.0
export AIKV_PORT=6379
export AIKV_MAX_MEMORY=2GB
export AIKV_LOG_LEVEL=debug
```

## 相关文档 / Related Documentation

- [部署指南 / Deployment Guide](../docs/DEPLOYMENT.md)
- [API 文档 / API Documentation](../docs/API.md)
- [开发计划 / Development Plan](../docs/DEVELOPMENT_PLAN.md)
