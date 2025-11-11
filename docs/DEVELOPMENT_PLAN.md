# AiKv Redis 协议兼容层开发计划

## 项目概述

AiKv 是基于 [AiDb v0.1.0](https://github.com/Genuineh/AiDb) 单机版的 Redis 协议兼容层实现。本项目旨在提供一个高性能、轻量级的键值存储服务，同时兼容 Redis 协议，使得现有的 Redis 客户端可以无缝连接。

## 项目目标

- 实现 Redis RESP (REdis Serialization Protocol) 协议解析器
- 支持 String 类型的基本操作命令
- 支持 JSON 类型的基本操作命令
- 通过 Git 仓库地址直接引用 AiDb 包
- 保持代码简洁、高性能、易维护

## 技术栈

- **语言**: Rust (使用 Rust 2024 edition)
- **依赖存储引擎**: AiDb v0.1.0
- **协议**: Redis RESP 协议
- **网络**: Tokio 异步运行时
- **序列化**: serde_json (用于 JSON 支持)

## 架构设计

```
┌─────────────────┐
│  Redis Client   │
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
│  │   AiDb    │  │  存储引擎
│  │  Engine   │  │
│  └───────────┘  │
└─────────────────┘
```

## 开发阶段

### 阶段 1: 基础架构搭建 (第 1-2 天)

#### 任务清单
- [x] 创建 Rust 项目结构
- [ ] 配置 Cargo.toml 依赖
  - tokio (异步运行时)
  - bytes (字节操作)
  - serde, serde_json (JSON 序列化)
  - anyhow (错误处理)
  - tracing (日志)
- [ ] 引入 AiDb v0.1.0 依赖
- [ ] 创建项目目录结构

#### 目录结构
```
aikv/
├── Cargo.toml
├── src/
│   ├── main.rs              # 主入口
│   ├── lib.rs               # 库入口
│   ├── server/              # 服务器模块
│   │   ├── mod.rs
│   │   └── connection.rs    # 连接处理
│   ├── protocol/            # 协议解析模块
│   │   ├── mod.rs
│   │   ├── parser.rs        # RESP 解析器
│   │   └── types.rs         # RESP 数据类型
│   ├── command/             # 命令处理模块
│   │   ├── mod.rs
│   │   ├── string.rs        # String 命令
│   │   └── json.rs          # JSON 命令
│   ├── storage/             # 存储模块
│   │   ├── mod.rs
│   │   └── aidb_adapter.rs  # AiDb 适配器
│   └── error.rs             # 错误定义
├── tests/                   # 集成测试
│   ├── integration_test.rs
│   ├── string_commands.rs
│   └── json_commands.rs
├── examples/                # 示例代码
│   └── client_example.rs
└── docs/                    # 文档目录
    ├── DEVELOPMENT_PLAN.md  # 本文件
    ├── API.md               # API 文档
    └── DEPLOYMENT.md        # 部署指南
```

### 阶段 2: RESP 协议实现 (第 3-4 天)

#### RESP 协议简介
Redis 使用 RESP (REdis Serialization Protocol) 协议进行客户端-服务器通信。RESP 支持以下数据类型：

- **Simple Strings**: `+OK\r\n`
- **Errors**: `-Error message\r\n`
- **Integers**: `:1000\r\n`
- **Bulk Strings**: `$6\r\nfoobar\r\n`
- **Arrays**: `*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n`

#### 任务清单
- [ ] 实现 RESP 数据类型定义
- [ ] 实现 RESP 解析器
  - 解析 Simple Strings
  - 解析 Errors
  - 解析 Integers
  - 解析 Bulk Strings
  - 解析 Arrays
- [ ] 实现 RESP 序列化器
- [ ] 编写协议解析单元测试

#### 示例代码结构
```rust
pub enum RespValue {
    SimpleString(String),
    Error(String),
    Integer(i64),
    BulkString(Option<Vec<u8>>),
    Array(Option<Vec<RespValue>>),
}
```

### 阶段 3: 存储引擎集成 (第 5 天)

#### 任务清单
- [ ] 创建 AiDb 适配器接口
- [ ] 实现基本的存储操作
  - `get(key) -> Option<Value>`
  - `set(key, value) -> Result<()>`
  - `delete(key) -> Result<bool>`
  - `exists(key) -> bool`
- [ ] 实现 JSON 存储支持
- [ ] 编写存储层单元测试

#### AiDb 集成方式
```toml
[dependencies]
aidb = { git = "https://github.com/Genuineh/AiDb", tag = "v0.1.0" }
```

### 阶段 4: String 命令实现 (第 6-7 天)

#### 支持的命令列表
1. **GET** - 获取键的值
   - 语法: `GET key`
   - 返回: Bulk String

2. **SET** - 设置键的值
   - 语法: `SET key value [EX seconds] [NX|XX]`
   - 返回: Simple String 或 Null

3. **DEL** - 删除键
   - 语法: `DEL key [key ...]`
   - 返回: Integer (删除的键数量)

4. **EXISTS** - 检查键是否存在
   - 语法: `EXISTS key [key ...]`
   - 返回: Integer (存在的键数量)

5. **MGET** - 批量获取多个键
   - 语法: `MGET key [key ...]`
   - 返回: Array

6. **MSET** - 批量设置多个键值对
   - 语法: `MSET key value [key value ...]`
   - 返回: Simple String

7. **STRLEN** - 获取字符串长度
   - 语法: `STRLEN key`
   - 返回: Integer

8. **APPEND** - 追加字符串
   - 语法: `APPEND key value`
   - 返回: Integer (追加后的长度)

#### 任务清单
- [ ] 实现 GET 命令
- [ ] 实现 SET 命令 (包括 EX, NX, XX 选项)
- [ ] 实现 DEL 命令
- [ ] 实现 EXISTS 命令
- [ ] 实现 MGET 命令
- [ ] 实现 MSET 命令
- [ ] 实现 STRLEN 命令
- [ ] 实现 APPEND 命令
- [ ] 编写 String 命令集成测试

### 阶段 5: JSON 命令实现 (第 8-9 天)

#### 支持的命令列表
1. **JSON.GET** - 获取 JSON 值
   - 语法: `JSON.GET key [path]`
   - 返回: Bulk String (JSON 格式)

2. **JSON.SET** - 设置 JSON 值
   - 语法: `JSON.SET key path value`
   - 返回: Simple String

3. **JSON.DEL** - 删除 JSON 路径
   - 语法: `JSON.DEL key [path]`
   - 返回: Integer (删除的路径数量)

4. **JSON.TYPE** - 获取 JSON 类型
   - 语法: `JSON.TYPE key [path]`
   - 返回: Simple String

5. **JSON.STRLEN** - 获取 JSON 字符串长度
   - 语法: `JSON.STRLEN key [path]`
   - 返回: Integer

6. **JSON.ARRLEN** - 获取 JSON 数组长度
   - 语法: `JSON.ARRLEN key [path]`
   - 返回: Integer

7. **JSON.OBJLEN** - 获取 JSON 对象键数量
   - 语法: `JSON.OBJLEN key [path]`
   - 返回: Integer

#### 任务清单
- [ ] 实现 JSON 路径解析器 (支持 JSONPath)
- [ ] 实现 JSON.GET 命令
- [ ] 实现 JSON.SET 命令
- [ ] 实现 JSON.DEL 命令
- [ ] 实现 JSON.TYPE 命令
- [ ] 实现 JSON.STRLEN 命令
- [ ] 实现 JSON.ARRLEN 命令
- [ ] 实现 JSON.OBJLEN 命令
- [ ] 编写 JSON 命令集成测试

### 阶段 6: 服务器实现 (第 10-11 天)

#### 任务清单
- [ ] 实现 TCP 服务器 (使用 Tokio)
- [ ] 实现连接处理逻辑
- [ ] 实现命令路由分发
- [ ] 实现连接池管理
- [ ] 实现优雅关闭
- [ ] 添加配置文件支持
- [ ] 添加日志系统

#### 配置示例
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

### 阶段 7: 测试与优化 (第 12-13 天)

#### 任务清单
- [ ] 编写全面的单元测试
- [ ] 编写集成测试
- [ ] 使用 redis-benchmark 进行性能测试
- [ ] 使用 redis-cli 进行功能测试
- [ ] 内存泄漏检测
- [ ] 并发压力测试
- [ ] 性能优化
- [ ] 代码审查与重构

#### 测试工具
```bash
# 使用 redis-cli 连接
redis-cli -h 127.0.0.1 -p 6379

# 使用 redis-benchmark 测试性能
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -q
```

### 阶段 8: 文档与示例 (第 14 天)

#### 任务清单
- [ ] 完善 API 文档
- [ ] 编写部署指南
- [ ] 编写使用示例
- [ ] 编写故障排查指南
- [ ] 更新 README.md
- [ ] 生成 API 参考文档

## 性能目标

- **延迟**: 
  - P50 < 1ms
  - P99 < 5ms
- **吞吐量**: 
  - 单线程 > 50k ops/s
  - 多线程 > 200k ops/s
- **内存使用**: 
  - 基础内存 < 50MB
  - 每个连接 < 100KB

## 兼容性

- **Redis 版本**: 兼容 Redis 6.0+ 客户端
- **操作系统**: Linux, macOS, Windows
- **架构**: x86_64, ARM64

## 风险与挑战

1. **协议兼容性**: 确保完整支持 RESP 协议的边界情况
2. **性能优化**: 平衡功能完整性和性能
3. **AiDb 集成**: 需要深入理解 AiDb 的 API 和限制
4. **并发安全**: 确保多连接场景下的数据一致性
5. **内存管理**: 避免内存泄漏和过度分配

## 后续规划 (v0.2.0+)

- 支持更多 Redis 数据类型 (List, Set, Hash, ZSet)
- 支持持久化 (AOF, RDB)
- 支持主从复制
- 支持集群模式
- 支持 Pub/Sub
- 支持事务 (MULTI/EXEC)
- 支持 Lua 脚本
- 监控和管理工具

## 参考资料

- [Redis Protocol Specification](https://redis.io/docs/reference/protocol-spec/)
- [Redis Commands](https://redis.io/commands/)
- [RedisJSON](https://redis.io/docs/stack/json/)
- [AiDb Repository](https://github.com/Genuineh/AiDb)
- [Tokio Documentation](https://tokio.rs/)
- [Rust Async Book](https://rust-lang.github.io/async-book/)

## 贡献指南

1. Fork 本项目
2. 创建特性分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 开启 Pull Request

## 许可证

本项目采用 MIT 许可证 - 详见 [LICENSE](../LICENSE) 文件
