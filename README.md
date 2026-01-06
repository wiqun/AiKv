# AiKv - Redis 协议兼容的高性能分布式键值存储

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)]()
[![Tests](https://img.shields.io/badge/tests-96%20passed-brightgreen.svg)]()

AiKv 是一个基于 [AiDb v0.5.1](https://github.com/Genuineh/AiDb) 的高性能 Redis 协议兼容层实现，使用 Rust 编写。它提供了一个轻量级、高性能的键值存储服务，支持 Redis RESP 协议，使得现有的 Redis 客户端可以无缝连接。

**🎯 目标**: 发布全球第一个 100% Redis Cluster 协议兼容 + 完全 Rust 原生 + 基于 Multi-Raft 的生产级分布式 KV 引擎 (**v1.0.0 - 2026.03.31**)

> **📢 当前状态**: v0.1.0 已发布，集群方案已完成约 90%，包括完整的 CLUSTER 命令实现、槽路由、在线迁移和高可用支持。

## ✨ 特性

### 核心功能
- 🚀 **高性能**: 基于 Tokio 异步运行时，支持高并发，单节点 > 200k ops/s
- 🔌 **Redis 协议兼容**: 完全兼容 RESP2 和 RESP3 协议，支持所有主流 Redis 客户端
- 💾 **双存储引擎**: 支持内存存储（高速缓存）和 AiDb LSM-Tree 持久化存储
- 📊 **丰富的数据类型**: String, List, Hash, Set, Sorted Set, JSON
- 📜 **Lua 脚本**: 完整的 EVAL/EVALSHA 支持，带事务性回滚

### 集群特性 (90% 完成)
- 🌐 **Redis Cluster 协议**: 兼容 Redis Cluster 协议，支持标准客户端连接
- 🗺️ **16384 槽映射**: CRC16 槽计算，与 Redis 完全兼容
- ↔️ **-MOVED/-ASK 重定向**: 完整的请求重定向逻辑
- 🔄 **在线槽迁移**: 支持 CLUSTER SETSLOT MIGRATING/IMPORTING
- 🔁 **高可用**: 副本管理、手动故障转移 (CLUSTER REPLICATE/FAILOVER)
- 📖 **读写分离**: READONLY/READWRITE 命令支持

### 其他特性
- 📦 **轻量级**: 小内存占用（< 50MB 基础），快速启动
- 🔧 **易于部署**: 单一可执行文件，Docker 支持
- 🔒 **类型安全**: Rust 编写，保证内存安全和并发安全
- 🗄️ **数据持久化**: WAL + SSTable，Bloom Filter 加速，Snappy 压缩

## 🎯 支持的命令 (100+ 命令)

### 协议命令 (3个)
- `HELLO` - 协议版本协商 (RESP2/RESP3 切换)
- `PING` - 测试连接
- `ECHO` - 回显消息

### String 命令 (8个)
- `GET`, `SET` (支持 EX, PX, NX, XX 选项)
- `DEL`, `EXISTS`
- `MGET`, `MSET`
- `STRLEN`, `APPEND`

### JSON 命令 (7个)
- `JSON.GET`, `JSON.SET`, `JSON.DEL`
- `JSON.TYPE`, `JSON.STRLEN`
- `JSON.ARRLEN`, `JSON.OBJLEN`

### List 命令 (10个)
- `LPUSH`, `RPUSH`, `LPOP`, `RPOP`
- `LLEN`, `LRANGE`, `LINDEX`
- `LSET`, `LREM`, `LTRIM`

### Hash 命令 (12个)
- `HSET`, `HSETNX`, `HGET`, `HMGET`
- `HDEL`, `HEXISTS`, `HLEN`
- `HKEYS`, `HVALS`, `HGETALL`
- `HINCRBY`, `HINCRBYFLOAT`

### Set 命令 (13个)
- `SADD`, `SREM`, `SISMEMBER`, `SMEMBERS`
- `SCARD`, `SPOP`, `SRANDMEMBER`
- `SUNION`, `SINTER`, `SDIFF`
- `SUNIONSTORE`, `SINTERSTORE`, `SDIFFSTORE`

### Sorted Set 命令 (12个)
- `ZADD`, `ZREM`, `ZSCORE`
- `ZRANK`, `ZREVRANK`
- `ZRANGE`, `ZREVRANGE`
- `ZRANGEBYSCORE`, `ZREVRANGEBYSCORE`
- `ZCARD`, `ZCOUNT`, `ZINCRBY`

### Database 命令 (6个)
- `SELECT` - 切换数据库 (16 个数据库)
- `DBSIZE`, `FLUSHDB`, `FLUSHALL`
- `SWAPDB`, `MOVE`

### Key 管理命令 (17个)
- `KEYS`, `SCAN`, `RANDOMKEY`
- `RENAME`, `RENAMENX`, `TYPE`, `COPY`
- `EXPIRE`, `EXPIREAT`, `PEXPIRE`, `PEXPIREAT`
- `TTL`, `PTTL`, `PERSIST`
- `EXPIRETIME`, `PEXPIRETIME` (Redis 7.0+)

### Server 命令 (10个)
- `INFO`, `TIME`
- `CONFIG GET/SET`
- `CLIENT LIST/SETNAME/GETNAME`
- `MONITOR` - 实时命令监控 (支持 Redis 桌面客户端 Profiler)

### Lua 脚本命令 (6个)
- `EVAL`, `EVALSHA`
- `SCRIPT LOAD/EXISTS/FLUSH/KILL`
- ✅ 支持事务性回滚

### Cluster 命令 (17个) ⭐ 新增
- **信息查询**: `CLUSTER INFO`, `CLUSTER NODES`, `CLUSTER SLOTS`, `CLUSTER MYID`, `CLUSTER KEYSLOT`
- **节点管理**: `CLUSTER MEET`, `CLUSTER FORGET`
- **槽管理**: `CLUSTER ADDSLOTS`, `CLUSTER DELSLOTS`, `CLUSTER SETSLOT`
- **迁移支持**: `CLUSTER GETKEYSINSLOT`, `CLUSTER COUNTKEYSINSLOT`
- **高可用**: `CLUSTER REPLICATE`, `CLUSTER FAILOVER`, `CLUSTER REPLICAS`
- **读写分离**: `READONLY`, `READWRITE`

## 🚀 快速开始

### 前置要求

- Rust 1.70.0 或更高版本
- Cargo（随 Rust 安装）

### 编译安装

```bash
# 克隆仓库
git clone https://github.com/Genuineh/AiKv.git
cd AiKv

# 编译项目（生产版本）
cargo build --release

# 编译带集群支持的版本
cargo build --release --features cluster

# 运行服务
./target/release/aikv
```

### 使用 Docker

```bash
# 构建镜像（单机版）
docker build -t aikv:latest .

# 构建镜像（集群版）
docker build -t aikv:cluster --build-arg FEATURES=cluster .

# 运行单节点容器
docker run -d -p 6379:6379 --name aikv aikv:latest

# 运行带数据持久化的容器
docker run -d -p 6379:6379 \
  -v $(pwd)/data:/app/data \
  --name aikv aikv:latest
```

### 连接到 AiKv

使用任何 Redis 客户端连接：

```bash
# 使用 redis-cli
redis-cli -h 127.0.0.1 -p 6379

# 测试连接
127.0.0.1:6379> PING
PONG

# 切换到 RESP3 协议
127.0.0.1:6379> HELLO 3
 1) "server"
 2) "aikv"
 3) "version"
 4) "0.1.0"
 5) "proto"
 6) (integer) 3

# String 操作
127.0.0.1:6379> SET mykey "Hello World"
OK
127.0.0.1:6379> GET mykey
"Hello World"

# JSON 操作
127.0.0.1:6379> JSON.SET user $ '{"name":"John","age":30}'
OK
127.0.0.1:6379> JSON.GET user
"{\"name\":\"John\",\"age\":30}"
```

## 📖 文档

### 核心文档
- [开发计划](docs/DEVELOPMENT_PLAN.md) - 项目概述和技术栈
- [API 文档](docs/API.md) - 完整的命令参考和使用示例
- [部署指南](docs/DEPLOYMENT.md) - 生产环境部署和配置说明

### 专题文档
- [架构重构](docs/ARCHITECTURE_REFACTORING.md) - 存储层架构设计
- [AiDb 集成](docs/AIDB_INTEGRATION.md) - AiDb 存储引擎集成
- [AiDb Cluster API](docs/AIDB_CLUSTER_API_REFERENCE.md) - 集群 API 参考
- [Lua 脚本](docs/LUA_SCRIPTING.md) - Lua 脚本支持详解
- [性能优化](docs/PERFORMANCE.md) - 性能基准和调优

### 开发文档
- [TODO 列表](TODO.md) - 完整的开发计划和进度
- [变更日志](CHANGELOG.md) - 版本变更记录
- [贡献指南](CONTRIBUTING.md) - 如何参与贡献

## 🏗️ 架构

### 单节点架构

```
┌─────────────────┐
│  Redis Client   │  (任何支持 RESP 协议的客户端)
└────────┬────────┘
         │ RESP Protocol
         ▼
┌─────────────────┐
│  AiKv Server    │
│  ┌───────────┐  │
│  │ Protocol  │  │  RESP2/RESP3 协议解析
│  │  Parser   │  │
│  └─────┬─────┘  │
│        │        │
│  ┌─────┴─────┐  │
│  │  Command  │  │  命令处理器 (100+ 命令)
│  │  Handlers │  │
│  └─────┬─────┘  │
│        │        │
│  ┌─────┴─────┐  │
│  │  Storage  │  │  双存储引擎
│  │  Adapter  │  │  (Memory / AiDb)
│  └───────────┘  │
└─────────────────┘
```

### 集群架构 (Cluster Feature)

```
┌─────────────────────────────────────────────────────────────────────┐
│                     AiKv RESP Listener (6379)                       │
│                             ↓                                       │
│                   Command Parser (RESP2/RESP3)                      │
│                             ↓                                       │
│    ┌────────────────────────────────────────────────────────┐       │
│    │           Redis Cluster 协议层 (AiKv ~1000 行)          │       │
│    │  • ClusterCommands: INFO/NODES/SLOTS/KEYSLOT/MEET      │       │
│    │  • SlotRouter: CRC16 槽计算 (与 Redis 兼容)             │       │
│    │  • SlotRedirector: -MOVED/-ASK 重定向                  │       │
│    │  • MigrationManager: 在线槽迁移                        │       │
│    │  • ClusterState: 副本管理和故障转移                    │       │
│    └────────────────────────────────────────────────────────┘       │
│                             ↓                                       │
│    ┌────────────────────────────────────────────────────────┐       │
│    │           AiDb MultiRaft API (v0.5.0)                   │       │
│    │  • MultiRaftNode: 自动路由、数据读写                   │       │
│    │  • MetaRaftNode: 元数据 Raft                           │       │
│    │  • 16384 Slots → Raft Groups 映射                      │       │
│    └────────────────────────────────────────────────────────┘       │
│                             ↓                                       │
│               Cluster Bus 端口 16379（gossip + 心跳）               │
└─────────────────────────────────────────────────────────────────────┘
```

## 🔧 配置

项目提供了完整的配置模板，位于 `config/` 目录：

| 配置文件 | 说明 |
|---------|-----|
| [`config/aikv.toml`](config/aikv.toml) | 单机模式配置模板 |
| [`config/aikv-cluster.toml`](config/aikv-cluster.toml) | 集群模式配置模板 |

### 单节点配置

```bash
# 复制配置模板
cp config/aikv.toml config.toml

# 编辑配置
vim config.toml

# 启动服务
./target/release/aikv --config config.toml
```

示例配置：

```toml
[server]
host = "127.0.0.1"
port = 6379
max_connections = 1000

[storage]
# 存储引擎选择：memory（内存）或 aidb（持久化）
engine = "memory"  # 或 "aidb"
data_dir = "./data"
max_memory = "1GB"

[logging]
level = "info"
file = "./logs/aikv.log"
```

### 集群配置 (Feature: cluster)

```bash
# 复制集群配置模板
cp config/aikv-cluster.toml config.toml

# 编辑配置（修改 node_id, peers 等）
vim config.toml

# 使用集群特性编译并启动
cargo build --release --features cluster
./target/release/aikv --config config.toml
```

示例配置：

```toml
[server]
host = "0.0.0.0"
port = 6379
cluster_port = 16379  # 集群总线端口

[cluster]
enabled = true
node_id = "node1"
data_dir = "./cluster-data"

# 初始节点列表
peers = [
    "192.168.1.101:16379",
    "192.168.1.102:16379",
    "192.168.1.103:16379"
]

[storage]
engine = "aidb"  # 集群模式推荐使用持久化存储
data_dir = "./data"
```

> 完整的配置选项请参考 [config/README.md](config/README.md)

### 存储引擎说明

AiKv 支持两种存储引擎：

| 特性 | 内存存储 (Memory) | AiDb 存储 (LSM-Tree) |
|-----|------------------|---------------------|
| 性能 | ⭐⭐⭐⭐⭐ 最高 | ⭐⭐⭐⭐ 优秀 |
| 持久化 | ❌ 不支持 | ✅ WAL + SSTable |
| 压缩 | ❌ | ✅ Snappy |
| 适用场景 | 缓存、开发测试 | 生产环境、集群部署 |

启动时指定配置文件：

```bash
./target/release/aikv --config config.toml
```

## 🌐 集群部署

### 🚀 推荐方式：使用 aikv-tool 一键部署

aikv-tool 是 AiKv 官方提供的一站式部署工具，可以一键完成集群部署：

```bash
# 1. 安装 aikv-tool
cd aikv-toolchain && cargo install --path . && cd ..

# 2. 一键部署集群 (6 节点: 3 主 3 从)
aikv-tool cluster setup

# 3. 查看集群状态
aikv-tool cluster status

# 4. 连接使用
redis-cli -c -h 127.0.0.1 -p 6379
```

**就这么简单！** `cluster setup` 命令会自动完成：
- ✅ 生成 Docker Compose 和节点配置文件
- ✅ 构建带集群功能的 Docker 镜像
- ✅ 启动 6 个节点容器
- ✅ 初始化 MetaRaft 成员和槽分配
- ✅ 配置主从复制

### aikv-tool 集群管理命令

```bash
aikv-tool cluster setup      # 一键部署集群
aikv-tool cluster start      # 启动集群
aikv-tool cluster stop       # 停止集群
aikv-tool cluster stop -v    # 停止并清理数据
aikv-tool cluster restart    # 重启集群
aikv-tool cluster status     # 查看集群状态
aikv-tool cluster logs       # 查看日志
aikv-tool cluster logs -f    # 实时查看日志

# 快捷方式
aikv-tool up                 # 等同于 cluster setup
aikv-tool down               # 等同于 cluster stop
```

### 集群架构

```
┌───────────────────────────────────────────────────────┐
│              AiKv Cluster (6 nodes)                   │
├───────────────────────────────────────────────────────┤
│                                                       │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐           │
│   │  Node 1  │  │  Node 2  │  │  Node 3  │           │
│   │  Master  │  │  Master  │  │  Master  │           │
│   │  :6379   │  │  :6380   │  │  :6381   │           │
│   │  Slots:  │  │  Slots:  │  │  Slots:  │           │
│   │  0-5460  │  │5461-10922│  │10923-16383│          │
│   └────┬─────┘  └────┬─────┘  └────┬─────┘           │
│        │             │             │                  │
│   ┌────┴─────┐  ┌────┴─────┐  ┌────┴─────┐           │
│   │  Node 4  │  │  Node 5  │  │  Node 6  │           │
│   │  Replica │  │  Replica │  │  Replica │           │
│   │  :6382   │  │  :6383   │  │  :6384   │           │
│   └──────────┘  └──────────┘  └──────────┘           │
│                                                       │
│   MetaRaft: Node 1, 2, 3 (3 voters)                  │
│   Slots: 16384 total, evenly distributed             │
│                                                       │
└───────────────────────────────────────────────────────┘
```

### 集群操作示例

```bash
# 连接到集群
redis-cli -c -p 6379

# 查看集群信息
127.0.0.1:6379> CLUSTER INFO
cluster_state:ok
cluster_slots_assigned:16384
cluster_known_nodes:6
cluster_size:3

# 查看节点列表
127.0.0.1:6379> CLUSTER NODES

# 计算 key 的槽位
127.0.0.1:6379> CLUSTER KEYSLOT mykey
(integer) 14687

# 使用哈希标签确保相关 key 在同一槽
127.0.0.1:6379> SET {user:1000}:name "John"
OK
127.0.0.1:6379> SET {user:1000}:age "30"
OK
```

### 在线扩容 (槽迁移)

```bash
# 查看槽内的 key
redis-cli CLUSTER GETKEYSINSLOT 5000 10

# 开始迁移槽 5000 到新节点
redis-cli CLUSTER SETSLOT 5000 MIGRATING <target-node-id>
redis-cli CLUSTER SETSLOT 5000 IMPORTING <source-node-id>

# 迁移完成后确认
redis-cli CLUSTER SETSLOT 5000 NODE <target-node-id>
```

### 故障转移

```bash
# 在副本节点上执行手动故障转移
redis-cli -p 6382 CLUSTER FAILOVER

# 强制故障转移 (即使主节点不可用)
redis-cli -p 6382 CLUSTER FAILOVER FORCE
```

## 📊 性能

### 单节点性能

在标准硬件上的性能基准（使用 redis-benchmark）：

```
SET: ~80,000 ops/s
GET: ~100,000 ops/s
LPUSH: ~75,000 ops/s
HSET: ~70,000 ops/s
```

### 集群性能目标 (v1.0.0)

| 指标 | 目标值 | 测试方法 |
|------|--------|----------|
| 3 节点吞吐 (50%读50%写) | ≥ 420k ops/sec | redis-benchmark -t set,get -c 500 |
| 单节点吞吐 | ≥ 220k ops/sec | 同上 |
| 槽迁移速度 (1000 槽) | < 25 秒 | redis-cli --cluster reshard |
| 自动故障转移时间 | < 10 秒 | 杀进程 + 监控切换 |
| 副本延迟 (99.9%) | < 50 ms | 自研同步延迟监控 |

### 延迟目标

- **P50**: < 1ms
- **P99**: < 5ms
- **P99.9**: < 10ms

## 🧪 测试

```bash
# 运行所有测试 (96 个测试)
cargo test

# 运行特定模块测试
cargo test string_commands
cargo test json_commands
cargo test cluster

# 运行带集群特性的测试
cargo test --features cluster

# 使用 redis-benchmark 性能测试
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -q

# 集群模式性能测试
redis-benchmark -h 127.0.0.1 -p 6379 -c -t set,get -n 100000 -q
```

## 🛣️ 路线图

### v0.1.0 ✅ (当前版本 - 已发布)
- ✅ RESP2/RESP3 协议完整支持
- ✅ String/List/Hash/Set/ZSet 命令 (50+ 命令)
- ✅ JSON 命令 (7 个)
- ✅ 多数据库支持（16 个数据库）
- ✅ 键过期机制（TTL 支持）
- ✅ 双存储引擎：内存和 AiDb LSM-Tree
- ✅ Lua 脚本支持 (事务性)
- ✅ 96 个测试用例通过

### v0.2.0 ⏳ (Stage 0-1: 集群基础)
- ✅ Cluster 命令框架 (17 个命令)
- ✅ 16384 槽映射和 CRC16 路由
- ✅ -MOVED/-ASK 重定向
- ✅ 槽迁移状态管理
- ✅ 副本管理和故障转移
- ⬜ Multi-Raft 完整集成 (AiDb v0.5.0)
- ⬜ Cluster Bus (gossip 心跳)

### v0.5.0 (Stage 2-4: 集群完善)
- ⬜ 槽在线迁移 (reshard)
- ⬜ 副本自动同步
- ⬜ 自动 failover
- ⬜ 集群总线 (gossip)

### v1.0.0 🎯 (目标: 2026.03.31)
- ⬜ 100% Redis Cluster 协议兼容
- ⬜ 官方测试套件通过
- ⬜ Docker/Helm/Prometheus
- ⬜ 完整文档和运维工具
- ⬜ YCSB 性能报告

### 集群实现进度

```
总体进度: ████████████████████░░░ 90%

阶段 A: 基础集成     ████████████████████ 100% ✅
阶段 B: 集群命令     ████████████████████ 100% ✅
阶段 C: 槽迁移       ████████████████████ 100% ✅
阶段 D: 高可用       ████████████████████ 100% ✅
阶段 E: Cluster Bus  ░░░░░░░░░░░░░░░░░░░░   0% (未开始)
       Multi-Raft    ░░░░░░░░░░░░░░░░░░░░   0% (待集成)
```

> **注**: 阶段 A-D 的协议层和命令层已完成，剩余 10% 主要是 Multi-Raft 底层集成和 Cluster Bus 节点间通信实现。

详细的 18 周开发计划请参考 [TODO.md](TODO.md)。

## 🤝 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md) 了解详细的贡献指南。

1. Fork 本项目
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启 Pull Request

### 开发环境

```bash
# 克隆仓库
git clone https://github.com/Genuineh/AiKv.git
cd AiKv

# 开发构建
cargo build

# 运行开发版本
cargo run

# 运行测试
cargo test

# 代码格式化
cargo fmt

# 代码检查
cargo clippy

# 安全审计
cargo audit
```

### 代码质量工具

- **rustfmt**: 代码格式化
- **clippy**: 代码 lint
- **cargo-audit**: 安全漏洞检查
- **cargo-deny**: 许可证和依赖检查

## 📄 许可证

本项目采用 MIT 许可证 - 详见 [LICENSE](LICENSE) 文件

## 🙏 致谢

- [AiDb](https://github.com/Genuineh/AiDb) - 核心存储引擎和 Multi-Raft 支持
- [Tokio](https://tokio.rs/) - 异步运行时
- [Redis](https://redis.io/) - 协议规范和设计灵感
- [openraft](https://github.com/datafuselabs/openraft) - Raft 共识算法

## 📧 联系方式

- GitHub Issues: [https://github.com/Genuineh/AiKv/issues](https://github.com/Genuineh/AiKv/issues)

## 📈 项目统计

| 指标 | 数值 |
|------|------|
| 支持的命令 | 100+ |
| 单元测试 | 96 个 |
| 代码行数 | 8000+ |
| 文档字数 | 40,000+ |

## ⭐ Star History

如果这个项目对你有帮助，请给它一个 Star！

---

使用 ❤️ 和 Rust 构建 | **v0.1.0** | 集群支持 90% 完成
