# AiKv 快速开始指南

本指南帮助你在 5 分钟内开始使用 AiKv。

## 📋 前置要求

- **Rust 1.70+** (用于编译)
- **Docker & Docker Compose** (用于容器化部署)
- **redis-cli** (用于连接测试)

---

## 🚀 方式一：使用 aikv-tool（推荐）

### 1. 安装 aikv-tool

```bash
# 克隆项目
git clone https://github.com/Genuineh/AiKv.git
cd AiKv

# 安装 aikv-tool
cd aikv-toolchain && cargo install --path . && cd ..
```

### 2. 一键部署集群

```bash
# 一键完成：生成配置 → 构建镜像 → 启动容器 → 初始化集群
aikv-tool cluster setup
```

这个命令会自动完成所有工作！

### 3. 连接使用

```bash
redis-cli -c -h 127.0.0.1 -p 6379

127.0.0.1:6379> PING
PONG

127.0.0.1:6379> SET hello world
OK

127.0.0.1:6379> GET hello
"world"
```

### 4. 查看集群状态

```bash
aikv-tool cluster status
```

### 5. 停止集群

```bash
aikv-tool cluster stop
```

---

## 🐳 方式二：单节点 Docker 部署

适合快速测试或开发环境。

```bash
# 构建镜像
docker build -t aikv:latest .

# 运行容器
docker run -d -p 6379:6379 --name aikv aikv:latest

# 连接测试
redis-cli -h 127.0.0.1 -p 6379 PING
```

---

## 🖥️ 方式三：直接运行

适合开发调试。

```bash
# 编译
cargo build --release

# 运行
./target/release/aikv

# 或带配置文件运行
./target/release/aikv --config config/aikv.toml
```

---

## 📝 基本操作示例

### String 操作

```bash
SET mykey "Hello World"
GET mykey
DEL mykey
```

### Hash 操作

```bash
HSET user:1 name "John" age 30
HGET user:1 name
HGETALL user:1
```

### List 操作

```bash
LPUSH mylist "item1" "item2" "item3"
LRANGE mylist 0 -1
RPOP mylist
```

### JSON 操作

```bash
JSON.SET user $ '{"name":"John","age":30}'
JSON.GET user
JSON.GET user $.name
```

### 集群操作

```bash
CLUSTER INFO
CLUSTER NODES
CLUSTER KEYSLOT mykey
```

---

## 🔧 aikv-tool 常用命令

| 命令 | 说明 |
|------|------|
| `aikv-tool cluster setup` | 一键部署集群 |
| `aikv-tool cluster start` | 启动集群 |
| `aikv-tool cluster stop` | 停止集群 |
| `aikv-tool cluster status` | 查看集群状态 |
| `aikv-tool build --release` | 编译 release 版本 |
| `aikv-tool docker` | 构建 Docker 镜像 |
| `aikv-tool deploy -t cluster` | 生成集群部署文件 |
| `aikv-tool deploy -t single` | 生成单节点部署文件 |
| `aikv-tool status` | 查看项目状态 |
| `aikv-tool tui` | 进入交互式 TUI |

---

## 📊 集群架构

一键部署的集群包含 6 个节点：

```
┌─────────────────────────────────────────────────┐
│           AiKv Cluster (6 nodes)                │
├─────────────────────────────────────────────────┤
│                                                 │
│   ┌─────────┐  ┌─────────┐  ┌─────────┐        │
│   │ Node 1  │  │ Node 2  │  │ Node 3  │        │
│   │ Master  │  │ Master  │  │ Master  │        │
│   │ :6379   │  │ :6380   │  │ :6381   │        │
│   │ 0-5460  │  │5461-10922│ │10923-16383│      │
│   └────┬────┘  └────┬────┘  └────┬────┘        │
│        │            │            │              │
│   ┌────┴────┐  ┌────┴────┐  ┌────┴────┐        │
│   │ Node 4  │  │ Node 5  │  │ Node 6  │        │
│   │ Replica │  │ Replica │  │ Replica │        │
│   │ :6382   │  │ :6383   │  │ :6384   │        │
│   └─────────┘  └─────────┘  └─────────┘        │
│                                                 │
└─────────────────────────────────────────────────┘
```

- **Node 1-3**: 主节点，参与 MetaRaft 共识
- **Node 4-6**: 副本节点，分别复制 Node 1-3
- **16384 槽**: 均匀分布在 3 个主节点

---

## 🔗 下一步

- [API 参考](API.md) - 完整命令文档
- [部署指南](DEPLOYMENT.md) - 生产部署详情
- [Cluster API](AIDB_CLUSTER_API_REFERENCE.md) - 集群命令
- [性能调优](PERFORMANCE_TUNING.md) - 优化指南

---

## ❓ 常见问题

### Q: aikv-tool 安装失败？

确保 Rust 版本 >= 1.70：
```bash
rustup update
```

### Q: Docker 镜像构建失败？

确保 Docker 服务正在运行：
```bash
docker info
```

### Q: 集群初始化失败？

检查所有节点是否正常运行：
```bash
docker-compose ps
```

如果有节点未启动，尝试：
```bash
aikv-tool cluster stop -v  # 停止并清理
aikv-tool cluster setup    # 重新部署
```

### Q: 连接超时？

检查端口是否被占用：
```bash
lsof -i :6379
```

---

*需要更多帮助？查看 [故障排除](TROUBLESHOOTING.md) 或 [集群故障排除](CLUSTER_TROUBLESHOOTING.md)*
