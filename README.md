# AiKv - Redis 协议兼容的键值存储服务

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)]()

AiKv 是一个基于 [AiDb v0.1.0](https://github.com/Genuineh/AiDb) 的高性能 Redis 协议兼容层实现，使用 Rust 编写。它提供了一个轻量级、高性能的键值存储服务，支持 Redis RESP 协议，使得现有的 Redis 客户端可以无缝连接。

## ✨ 特性

- 🚀 **高性能**: 基于 Tokio 异步运行时，支持高并发
- 🔌 **Redis 协议兼容**: 完全兼容 RESP 协议，支持各种 Redis 客户端
- 📦 **轻量级**: 小内存占用，快速启动
- 🔧 **易于部署**: 单一可执行文件，无需复杂配置
- 🔒 **类型安全**: 使用 Rust 编写，保证内存安全和并发安全
- 📊 **JSON 支持**: 原生支持 JSON 数据类型操作

## 🎯 支持的命令

### String 命令

- `GET` - 获取键的值
- `SET` - 设置键的值（支持 EX, NX, XX 选项）
- `DEL` - 删除一个或多个键
- `EXISTS` - 检查键是否存在
- `MGET` - 批量获取多个键
- `MSET` - 批量设置多个键值对
- `STRLEN` - 获取字符串长度
- `APPEND` - 追加字符串

### JSON 命令

- `JSON.GET` - 获取 JSON 值
- `JSON.SET` - 设置 JSON 值
- `JSON.DEL` - 删除 JSON 路径
- `JSON.TYPE` - 获取 JSON 类型
- `JSON.STRLEN` - 获取 JSON 字符串长度
- `JSON.ARRLEN` - 获取 JSON 数组长度
- `JSON.OBJLEN` - 获取 JSON 对象键数量

## 🚀 快速开始

### 前置要求

- Rust 1.70.0 或更高版本
- Cargo（随 Rust 安装）

### 安装

```bash
# 克隆仓库
git clone https://github.com/Genuineh/AiKv.git
cd AiKv

# 编译项目（生产版本）
cargo build --release

# 运行服务
./target/release/aikv
```

### 使用 Docker

```bash
# 构建镜像
docker build -t aikv:latest .

# 运行容器
docker run -d -p 6379:6379 --name aikv aikv:latest
```

### 连接到 AiKv

使用任何 Redis 客户端连接：

```bash
# 使用 redis-cli
redis-cli -h 127.0.0.1 -p 6379

# 测试连接
127.0.0.1:6379> PING
PONG

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

- [开发计划](docs/DEVELOPMENT_PLAN.md) - 详细的开发计划和架构设计
- [API 文档](docs/API.md) - 完整的命令参考和使用示例
- [部署指南](docs/DEPLOYMENT.md) - 生产环境部署和配置说明

## 🏗️ 架构

```
┌─────────────────┐
│  Redis Client   │  (任何支持 RESP 协议的客户端)
└────────┬────────┘
         │ RESP Protocol
         ▼
┌─────────────────┐
│  AiKv Server    │
│  ┌───────────┐  │
│  │ Protocol  │  │  RESP 协议解析
│  │  Parser   │  │
│  └─────┬─────┘  │
│        │        │
│  ┌─────┴─────┐  │
│  │  Command  │  │  命令处理器
│  │  Handlers │  │
│  └─────┬─────┘  │
│        │        │
│  ┌─────┴─────┐  │
│  │   AiDb    │  │  存储引擎 (v0.1.0)
│  │  Engine   │  │
│  └───────────┘  │
└─────────────────┘
```

## 🔧 配置

创建 `config.toml` 文件：

```toml
[server]
host = "127.0.0.1"
port = 6379
max_connections = 1000

[storage]
data_dir = "./data"
max_memory = "1GB"

[logging]
level = "info"
file = "./logs/aikv.log"
```

启动时指定配置文件：

```bash
./target/release/aikv --config config.toml
```

## 📊 性能

在标准硬件上的性能基准（使用 redis-benchmark）：

```
SET: ~80,000 ops/s
GET: ~100,000 ops/s
```

性能目标：
- 延迟: P50 < 1ms, P99 < 5ms
- 吞吐量: 单线程 > 50k ops/s, 多线程 > 200k ops/s

## 🧪 测试

```bash
# 运行所有测试
cargo test

# 运行特定测试
cargo test string_commands
cargo test json_commands

# 使用 redis-benchmark 性能测试
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -q
```

## 🛣️ 路线图

### v0.1.0 (当前版本)
- ✅ RESP 协议解析器
- ✅ String 命令支持
- ✅ JSON 命令支持
- ✅ 基于 AiDb 的存储引擎

### v0.2.0 (计划中)
- ⬜ List 数据类型支持
- ⬜ Set 数据类型支持
- ⬜ Hash 数据类型支持
- ⬜ 持久化支持 (AOF/RDB)
- ⬜ 主从复制

### v0.3.0 (计划中)
- ⬜ 集群模式
- ⬜ Pub/Sub 支持
- ⬜ 事务支持 (MULTI/EXEC)
- ⬜ Lua 脚本支持

## 🤝 贡献

欢迎贡献！请查看我们的贡献指南。

1. Fork 本项目
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启 Pull Request

## 📝 开发

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
```

## 📄 许可证

本项目采用 MIT 许可证 - 详见 [LICENSE](LICENSE) 文件

## 🙏 致谢

- [AiDb](https://github.com/Genuineh/AiDb) - 提供核心存储引擎
- [Tokio](https://tokio.rs/) - 异步运行时
- [Redis](https://redis.io/) - 协议规范和设计灵感

## 📧 联系方式

- GitHub Issues: [https://github.com/Genuineh/AiKv/issues](https://github.com/Genuineh/AiKv/issues)
- 邮件: support@aikv.example.com

## ⭐ Star History

如果这个项目对你有帮助，请给它一个 Star！

---

使用 ❤️ 和 Rust 构建
