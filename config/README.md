# AiKv 配置模板 / Configuration Templates

此目录包含 AiKv 的配置文件模板。
This directory contains configuration templates for AiKv.

## 配置文件 / Configuration Files

| 文件 / File | 说明 / Description |
|------------|-------------------|
| `aikv.toml` | 单机模式配置模板 / Single node configuration template |
| `aikv-cluster.toml` | 集群模式配置模板 / Cluster mode configuration template |

## 配置项实现状态 / Configuration Implementation Status

配置文件中的选项有两种状态：
Configuration options have two states:

- ✅ **已实现 / Implemented** - 该配置项已在代码中生效
- 🚧 **计划中 / Planned** - 该配置项已定义但尚未实现

### 已实现的配置项 / Implemented Options

| 配置节 / Section | 配置项 / Option | 说明 / Description |
|-----------------|----------------|-------------------|
| `[server]` | `host` | 监听地址 / Bind address |
| `[server]` | `port` | 监听端口 / Bind port |
| `[storage]` | `engine` | 存储引擎类型 (`memory` 或 `aidb`) / Storage engine type |
| `[storage]` | `data_dir` | 数据目录 (aidb 模式) / Data directory for aidb mode |
| `[storage]` | `databases` | 数据库数量 / Number of databases |
| `[logging]` | `level` | 日志级别 / Log level (trace, debug, info, warn, error) |

### 计划中的配置项 / Planned Options

以下配置项在配置文件中已定义但尚未实现，将在后续版本中添加支持：
The following options are defined but not yet implemented, support will be added in future versions:

- `[server]`: `max_connections`, `connection_timeout`, `tcp_buffer_size`, `cluster_port`
- `[storage]`: `max_memory`
- `[logging]`: `file`, `console`, `max_size`, `max_backups`
- `[persistence]`: 整个配置节 / Entire section
- `[performance]`: 整个配置节 / Entire section
- `[security]`: 整个配置节 / Entire section
- `[expiration]`: 整个配置节 / Entire section
- `[cluster]`: 整个配置节 / Entire section
- `[raft]`: 整个配置节 / Entire section
- `[migration]`: 整个配置节 / Entire section
- `[failover]`: 整个配置节 / Entire section
- `[monitoring]`: 整个配置节 / Entire section

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

### 命令行参数 / Command Line Arguments

命令行参数优先于配置文件：
Command line arguments override config file:

```bash
# 使用配置文件
./target/release/aikv --config config.toml

# 覆盖主机和端口
./target/release/aikv --config config.toml --host 0.0.0.0 --port 6380

# 直接指定地址（不使用配置文件）
./target/release/aikv --host 127.0.0.1 --port 6379

# 旧版兼容模式
./target/release/aikv 127.0.0.1:6379
```

### 集群模式 / Cluster Mode

> **重要**: 集群模式需要使用 `--features cluster` 编译。服务器必须报告 `cluster_enabled:1` 才能使用 `redis-cli --cluster create` 初始化集群。
> **Important**: Cluster mode requires building with `--features cluster`. The server must report `cluster_enabled:1` for `redis-cli --cluster create` to work.

#### 使用 Docker Compose 部署集群 / Deploy Cluster with Docker Compose

推荐使用预配置的 Docker Compose 文件快速部署集群：
For quick cluster deployment, use the pre-configured Docker Compose file:

```bash
# 启动 6 节点集群 (3 主 3 从)
# Start 6-node cluster (3 master + 3 replica)
docker-compose -f docker-compose.cluster.yml up -d

# 等待所有节点启动 / Wait for all nodes to start
docker-compose -f docker-compose.cluster.yml ps

# 验证节点已启用集群模式 / Verify cluster mode is enabled
redis-cli -p 6379 INFO cluster
# 应显示 cluster_enabled:1 / Should show cluster_enabled:1

# 初始化集群 / Initialize cluster
redis-cli --cluster create \
  127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 \
  127.0.0.1:6382 127.0.0.1:6383 127.0.0.1:6384 \
  --cluster-replicas 1

# 验证集群状态 / Verify cluster status
redis-cli -c -p 6379 CLUSTER INFO
```

#### 常见问题 / Common Issues

如果看到错误 "Node is not configured as a cluster node"：
If you see error "Node is not configured as a cluster node":

1. 确保使用 `--features cluster` 编译 / Ensure built with `--features cluster`
2. 运行 `redis-cli INFO cluster` 检查 `cluster_enabled` 值 / Run `redis-cli INFO cluster` to check `cluster_enabled` value
3. 如果显示 `cluster_enabled:0`，需要重新编译 / If it shows `cluster_enabled:0`, rebuild is needed

#### 手动部署 / Manual Deployment

```bash
# 使用集群特性编译 (必需!)
# Build with cluster feature (required!)
cargo build --release --features cluster

# 验证编译正确
./target/release/aikv &
redis-cli INFO cluster | grep cluster_enabled
# 应输出: cluster_enabled:1

# 复制并修改配置
cp config/aikv-cluster.toml config.toml

# 启动服务
./target/release/aikv --config config.toml
```

## 配置项详细说明 / Configuration Options

### 存储引擎 / Storage Engine

| 选项 / Option | 说明 / Description |
|--------------|-------------------|
| `memory` | 内存存储，性能最佳，无持久化 / In-memory, best performance, no persistence |
| `aidb` | AiDb LSM-Tree 存储，支持持久化 / AiDb LSM-Tree, supports persistence |

推荐 / Recommendations:
- 开发/测试：使用 `memory` / Development/Testing: Use `memory`
- 生产环境：使用 `aidb` / Production: Use `aidb`

### 日志级别 / Log Level

| 级别 / Level | 说明 / Description |
|-------------|-------------------|
| `trace` | 最详细的日志，包括所有调试信息 / Most detailed, includes all debug info |
| `debug` | 调试信息 / Debug information |
| `info` | 一般信息（推荐）/ General information (recommended) |
| `warn` | 警告信息 / Warning messages |
| `error` | 仅错误信息 / Error messages only |

### 最小配置示例 / Minimal Configuration Example

```toml
[server]
host = "127.0.0.1"
port = 6379

[storage]
engine = "memory"

[logging]
level = "info"
```

### 使用 AiDb 持久化存储 / Using AiDb Persistent Storage

```toml
[server]
host = "0.0.0.0"
port = 6379

[storage]
engine = "aidb"
data_dir = "./data"
databases = 16

[logging]
level = "info"
```

## 相关文档 / Related Documentation

- [部署指南 / Deployment Guide](../docs/DEPLOYMENT.md)
- [API 文档 / API Documentation](../docs/API.md)
- [开发计划 / Development Plan](../docs/DEVELOPMENT_PLAN.md)
