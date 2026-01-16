# AiKv 架构设计文档

## 概述

AiKv 是一个基于 [AiDb v0.5.0](https://github.com/wiqun/AiDb) 的高性能 Redis 协议兼容层实现。本文档详细描述了 AiKv 的系统架构、核心组件和设计原则。

## 设计目标

1. **100% Redis 协议兼容**: 支持 RESP2 和 RESP3 协议，兼容所有主流 Redis 客户端
2. **高性能**: 单节点 200k+ ops/sec，低延迟响应
3. **可扩展性**: 支持 Redis Cluster 协议的分布式部署
4. **持久化**: 基于 AiDb LSM-Tree 的可靠数据持久化
5. **易于维护**: 清晰的分层架构，模块化设计

## 系统架构

### 整体架构图

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            客户端 (Redis Clients)                        │
│         redis-cli, Jedis, redis-py, node-redis, go-redis 等             │
└─────────────────────────────────┬───────────────────────────────────────┘
                                  │ TCP/6379
                                  ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                              网络层                                      │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                     Tokio Async Runtime                          │    │
│  │  • 异步 I/O 事件循环                                              │    │
│  │  • 连接管理和任务调度                                             │    │
│  │  • 高并发连接处理                                                 │    │
│  └─────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────┬───────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                              协议层                                      │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                    RESP Protocol Parser                          │    │
│  │  • RESP2 协议解析和编码                                          │    │
│  │  • RESP3 协议解析和编码                                          │    │
│  │  • 协议版本自动协商 (HELLO 命令)                                  │    │
│  │  • 流式响应支持                                                  │    │
│  └─────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────┬───────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                              命令层                                      │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                     Command Handlers                             │    │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐           │    │
│  │  │  String  │ │   List   │ │   Hash   │ │   Set    │           │    │
│  │  │ Commands │ │ Commands │ │ Commands │ │ Commands │           │    │
│  │  └──────────┘ └──────────┘ └──────────┘ └──────────┘           │    │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐           │    │
│  │  │  ZSet    │ │   JSON   │ │  Server  │ │   Key    │           │    │
│  │  │ Commands │ │ Commands │ │ Commands │ │ Commands │           │    │
│  │  └──────────┘ └──────────┘ └──────────┘ └──────────┘           │    │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐                        │    │
│  │  │   Lua    │ │ Database │ │ Cluster  │                        │    │
│  │  │  Script  │ │ Commands │ │ Commands │                        │    │
│  │  └──────────┘ └──────────┘ └──────────┘                        │    │
│  └─────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────┬───────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                              存储层                                      │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                     StorageEngine Trait                          │    │
│  │  • get_value() / set_value()     - 通用值操作                   │    │
│  │  • update_value()                - 原子更新                     │    │
│  │  • delete_and_get()              - 原子删除                     │    │
│  │  • keys() / scan()               - 键空间操作                   │    │
│  │  • flush_db() / swap_db()        - 数据库操作                   │    │
│  │  • TTL 管理                      - 过期时间管理                 │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                  │                                       │
│              ┌───────────────────┼───────────────────┐                  │
│              ▼                                       ▼                  │
│  ┌───────────────────────┐             ┌───────────────────────┐        │
│  │   MemoryAdapter       │             │   AiDbAdapter         │        │
│  │   (内存存储)          │             │   (持久化存储)        │        │
│  │   • HashMap 数据结构   │             │   • LSM-Tree 引擎     │        │
│  │   • 最高性能          │             │   • WAL + SSTable     │        │
│  │   • 无持久化          │             │   • Bloom Filter      │        │
│  │   • 适合缓存场景      │             │   • Snappy 压缩       │        │
│  └───────────────────────┘             └───────────────────────┘        │
└─────────────────────────────────────────────────────────────────────────┘
```

### 集群架构 (Cluster Mode)

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     AiKv Cluster (3 节点示例)                           │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐      │
│  │     Node 1       │  │     Node 2       │  │     Node 3       │      │
│  │  Slots: 0-5460   │  │ Slots: 5461-10922│  │ Slots: 10923-16383│     │
│  │                  │  │                  │  │                  │      │
│  │  ┌────────────┐  │  │  ┌────────────┐  │  │  ┌────────────┐  │      │
│  │  │ RESP Layer │  │  │  │ RESP Layer │  │  │  │ RESP Layer │  │      │
│  │  │   :6379    │  │  │  │   :6380    │  │  │  │   :6381    │  │      │
│  │  └─────┬──────┘  │  │  └─────┬──────┘  │  │  └─────┬──────┘  │      │
│  │        │         │  │        │         │  │        │         │      │
│  │  ┌─────┴──────┐  │  │  ┌─────┴──────┐  │  │  ┌─────┴──────┐  │      │
│  │  │ ClusterNode│  │  │  │ ClusterNode│  │  │  │ ClusterNode│  │      │
│  │  │  • SlotMap │  │  │  │  • SlotMap │  │  │  │  • SlotMap │  │      │
│  │  │  • Router  │  │  │  │  • Router  │  │  │  │  • Router  │  │      │
│  │  │  • MOVED   │  │  │  │  • MOVED   │  │  │  │  • MOVED   │  │      │
│  │  └─────┬──────┘  │  │  └─────┬──────┘  │  │  └─────┬──────┘  │      │
│  │        │         │  │        │         │  │        │         │      │
│  │  ┌─────┴──────┐  │  │  ┌─────┴──────┐  │  │  ┌─────┴──────┐  │      │
│  │  │Cluster Bus │◄─┼──┼─►│Cluster Bus │◄─┼──┼─►│Cluster Bus │  │      │
│  │  │   :16379   │  │  │  │   :16380   │  │  │  │   :16381   │  │      │
│  │  └────────────┘  │  │  └────────────┘  │  │  └────────────┘  │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘      │
│                                                                         │
│                         Gossip Protocol                                 │
│                    ◄───────────────────────►                           │
│                      心跳检测 + 故障发现                                │
└─────────────────────────────────────────────────────────────────────────┘
```

## 核心模块

### 1. 协议层 (Protocol Layer)

**位置**: `src/protocol/`

协议层负责 RESP 协议的解析和编码。

```rust
// 核心类型
pub enum RespValue {
    SimpleString(String),
    Error(String),
    Integer(i64),
    BulkString(Option<Bytes>),
    Array(Option<Vec<RespValue>>),
    // RESP3 扩展类型
    Null,
    Boolean(bool),
    Double(f64),
    BigNumber(String),
    VerbatimString(String, Bytes),
    Map(Vec<(RespValue, RespValue)>),
    Set(Vec<RespValue>),
    Push(Vec<RespValue>),
}
```

**设计特点**:
- 零拷贝解析：使用 `Bytes` 避免不必要的内存复制
- 流式处理：支持大数据的分块传输
- 协议协商：通过 HELLO 命令自动切换 RESP2/RESP3

### 2. 命令层 (Command Layer)

**位置**: `src/command/`

命令层实现所有 Redis 命令的业务逻辑。

**设计原则**:
- **单一职责**: 每个命令模块只负责一类数据类型的命令
- **类型安全**: 使用 Rust 类型系统确保数据类型正确
- **原子性**: 复杂操作通过事务保证原子性

**命令模块结构**:
```
src/command/
├── mod.rs          # 命令路由和分发
├── string.rs       # String 命令 (GET, SET, MGET, MSET...)
├── list.rs         # List 命令 (LPUSH, RPUSH, LPOP...)
├── hash.rs         # Hash 命令 (HSET, HGET, HGETALL...)
├── set.rs          # Set 命令 (SADD, SREM, SMEMBERS...)
├── zset.rs         # ZSet 命令 (ZADD, ZRANGE, ZSCORE...)
├── json.rs         # JSON 命令 (JSON.SET, JSON.GET...)
├── key.rs          # Key 命令 (KEYS, SCAN, EXPIRE...)
├── database.rs     # Database 命令 (SELECT, FLUSHDB...)
├── server.rs       # Server 命令 (INFO, CONFIG, TIME...)
├── script.rs       # Lua 脚本 (EVAL, EVALSHA...)
└── cluster.rs      # Cluster 命令 (CLUSTER INFO...)
```

### 3. 存储层 (Storage Layer)

**位置**: `src/storage/`

存储层提供统一的数据访问接口，支持多种存储后端。

**核心接口**:
```rust
pub trait StorageBackend {
    // 基本 CRUD 操作
    fn get_value(&self, db: usize, key: &str) -> Result<Option<StoredValue>>;
    fn set_value(&self, db: usize, key: String, value: StoredValue) -> Result<()>;
    fn delete(&self, db: usize, key: &str) -> Result<bool>;
    
    // 原子操作
    fn update_value<F>(&self, db: usize, key: &str, updater: F) -> Result<()>
    where F: FnOnce(&mut StoredValue);
    fn delete_and_get(&self, db: usize, key: &str) -> Result<Option<StoredValue>>;
    
    // 键空间操作
    fn keys(&self, db: usize, pattern: Option<&str>) -> Result<Vec<String>>;
    fn scan(&self, db: usize, cursor: u64, pattern: Option<&str>, count: usize) 
        -> Result<(u64, Vec<String>)>;
    
    // TTL 管理
    fn set_expiration(&self, db: usize, key: &str, expire_at_ms: u64) -> Result<bool>;
    fn get_expiration(&self, db: usize, key: &str) -> Result<Option<u64>>;
}
```

**值类型**:
```rust
pub enum ValueType {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<String, Bytes>),
    Set(HashSet<Vec<u8>>),
    ZSet(BTreeMap<Vec<u8>, f64>),
}

pub struct StoredValue {
    value: ValueType,
    expires_at: Option<u64>,
}
```

### 4. 可观测性模块 (Observability)

**位置**: `src/observability/`

提供日志、指标和追踪功能。

**组件**:
- **LoggingManager**: 结构化日志，支持动态日志级别调整
- **Metrics**: Prometheus 格式指标导出
- **SlowLog**: 慢查询日志记录

**指标示例**:
```
# HELP aikv_commands_total Total number of commands processed
# TYPE aikv_commands_total counter
aikv_commands_total 12345

# HELP aikv_commands_duration_avg_us Average command duration in microseconds
# TYPE aikv_commands_duration_avg_us gauge
aikv_commands_duration_avg_us 42.5

# HELP aikv_connected_clients Current number of connected clients
# TYPE aikv_connected_clients gauge
aikv_connected_clients 10
```

### 5. 集群模块 (Cluster Module)

**位置**: `src/cluster/`

实现 Redis Cluster 协议兼容的分布式功能。

**核心组件**:
- **ClusterState**: 集群状态管理
- **SlotMap**: 16384 槽位映射
- **Router**: 请求路由和重定向
- **MigrationManager**: 在线槽迁移
- **ClusterBus**: 节点间通信

## 数据流

### 请求处理流程

```
1. 客户端连接
   │
   ▼
2. Tokio 接受连接，创建任务
   │
   ▼
3. 读取 TCP 数据流
   │
   ▼
4. RESP 协议解析
   │
   ├── 解析失败 → 返回错误响应
   │
   ▼
5. 命令路由
   │
   ├── 集群模式：检查槽位归属
   │   ├── 本地处理
   │   └── 返回 MOVED/ASK 重定向
   │
   ▼
6. 命令处理
   │
   ├── 参数验证
   ├── 类型检查
   ├── 业务逻辑执行
   └── 存储层操作
   │
   ▼
7. 响应编码
   │
   ▼
8. TCP 发送响应
```

### 写入流程 (AiDb 存储)

```
1. 命令层调用 set_value()
   │
   ▼
2. 序列化 StoredValue 为 bincode 格式
   │
   ▼
3. 写入 WAL (Write-Ahead Log)
   │
   ▼
4. 写入 MemTable
   │
   ▼
5. 响应客户端 (异步持久化)
   │
   ▼
6. MemTable 满时触发 Compaction
   │
   ▼
7. 生成 SSTable 文件
```

## 设计模式

### 1. 适配器模式 (Adapter Pattern)

存储层使用适配器模式支持多种存储后端：

```rust
pub enum StorageEngine {
    Memory(Arc<MemoryAdapter>),
    AiDb(Arc<AiDbStorageAdapter>),
}

impl StorageEngine {
    pub fn get_value(&self, db: usize, key: &str) -> Result<Option<StoredValue>> {
        match self {
            StorageEngine::Memory(adapter) => adapter.get_value(db, key),
            StorageEngine::AiDb(adapter) => adapter.get_value(db, key),
        }
    }
}
```

### 2. 命令模式 (Command Pattern)

每个 Redis 命令封装为独立的处理函数：

```rust
impl StringCommands {
    pub fn get(&self, args: &[Bytes], db: usize) -> Result<RespValue>;
    pub fn set(&self, args: &[Bytes], db: usize) -> Result<RespValue>;
    pub fn mget(&self, args: &[Bytes], db: usize) -> Result<RespValue>;
}
```

### 3. 观察者模式 (Observer Pattern)

MONITOR 命令使用广播模式：

```rust
pub struct MonitorBroadcaster {
    senders: RwLock<Vec<tokio::sync::mpsc::Sender<MonitorMessage>>>,
}

impl MonitorBroadcaster {
    pub fn broadcast(&self, message: MonitorMessage) {
        // 向所有订阅者发送消息
    }
}
```

## 性能优化

### 1. 零拷贝

- 使用 `bytes::Bytes` 进行引用计数的内存共享
- RESP 解析时避免不必要的字符串分配
- 批量操作时重用缓冲区

### 2. 并发优化

- 使用 `RwLock` 允许并发读取
- 细粒度锁：按数据库分区锁定
- 无锁数据结构用于高频操作

### 3. 内存效率

- 使用 `VecDeque` 优化列表两端操作
- `BTreeMap` 用于有序集合的范围查询
- 延迟删除和惰性过期检查

### 4. I/O 优化

- Tokio 异步 I/O 最大化吞吐量
- 批量写入 WAL 减少磁盘 I/O
- 后台 Compaction 避免阻塞请求

## 扩展性

### 添加新命令

1. 在对应的命令模块中添加处理函数
2. 在命令路由中注册新命令
3. 添加单元测试

### 添加新存储后端

1. 实现 `StorageBackend` trait
2. 在 `StorageEngine` 枚举中添加变体
3. 更新配置解析

### 添加新数据类型

1. 在 `ValueType` 枚举中添加变体
2. 在 `StoredValue` 中添加访问方法
3. 实现序列化/反序列化
4. 添加相关命令

## 参考资料

- [Redis Protocol Specification](https://redis.io/docs/reference/protocol-spec/)
- [Redis Cluster Specification](https://redis.io/docs/reference/cluster-spec/)
- [AiDb Documentation](https://github.com/wiqun/AiDb)
- [Tokio Runtime](https://tokio.rs/)

---

**最后更新**: 2025-12-02  
**版本**: v0.1.0  
**维护者**: @Genuineh, @copilot
