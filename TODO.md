# AiKv 项目待办事项 (TODO)

## 当前状态

已完成 v0.1.0 基础实现 + 大量增强功能：

### 核心功能
- ✅ RESP2/RESP3 协议支持（完整实现所有类型）
- ✅ String 命令 (8个)
- ✅ JSON 命令 (7个)
- ✅ List 命令 (10个)
- ✅ Hash 命令 (12个)
- ✅ Set 命令 (13个)
- ✅ Sorted Set 命令 (12个)
- ✅ Database 命令 (6个)
- ✅ Key 命令 (17个)
- ✅ Server 命令 (9个)
- ✅ Lua 脚本支持 (6个命令 + 事务性支持)
- ✅ Cluster 命令 (基础框架，feature 启用)

### 存储引擎
- ✅ 双存储引擎支持（内存 + AiDb v0.4.1）
- ✅ AiDb LSM-Tree 持久化存储
- ✅ 多数据库支持（16 个数据库）
- ✅ 键过期机制（TTL 支持）
- ✅ 完整的数据类型序列化

### 测试覆盖
- ✅ 96 个单元测试全部通过
- ✅ 集成测试套件
- ✅ 性能基准测试

### 代码质量
- ✅ 存储层架构重构完成
- ✅ CI/CD 流水线（GitHub Actions）
- ✅ 安全检查（cargo-audit, cargo-deny）

---

## 优先级 1 - 立即完成 (CI/CD & 代码规范)

### 1.1 GitHub Actions CI/CD 流水线
- [x] 创建 `.github/workflows/ci.yml` - 持续集成流水线
  - [x] 代码格式检查 (`cargo fmt --check`)
  - [x] 代码 lint 检查 (`cargo clippy`)
  - [x] 编译检查 (debug 和 release)
  - [x] 运行所有单元测试
  - [x] 运行集成测试
  - ~~[x] 代码覆盖率报告~~ (已移除，简化CI流程)
  - ~~[x] 多平台构建 (Linux, macOS)~~ (已简化为仅Linux)
  - [ ] 多 Rust 版本测试 (stable, beta, nightly) - 当前仅支持 stable

- [x] 创建 `.github/workflows/release.yml` - 发布流水线
  - [x] 自动创建 release
  - [x] 构建二进制文件
  - [x] 上传构建产物

- [x] 创建 `.github/workflows/security.yml` - 安全检查
  - [x] cargo-audit 依赖安全扫描
  - [x] cargo-deny 许可证检查

### 1.2 代码格式和规范
- [x] 创建 `rustfmt.toml` - Rust 格式化配置
- [x] 创建 `clippy.toml` - Clippy lint 配置
- [x] 创建 `.editorconfig` - 编辑器配置
- [x] 添加 pre-commit hooks 配置
- [x] 更新 `CONTRIBUTING.md` - 贡献指南（包含代码规范）
- [x] 创建 `deny.toml` - cargo-deny 配置（许可证和安全检查）

### 1.3 测试增强
- [x] 创建 `tests/integration_test.rs` - 集成测试套件
- [x] 添加性能基准测试 (`benches/` 目录)
- [ ] 添加模糊测试 (fuzzing tests)
- [x] 配置 `cargo-tarpaulin` 代码覆盖率
- [x] 添加端到端测试脚本

---

## 优先级 2 - RESP3 协议支持

### 2.1 RESP3 协议实现
- [x] 扩展 `src/protocol/types.rs` 支持 RESP3 新类型
  - [x] Null type (distinct from null bulk string)
  - [x] Boolean type
  - [x] Double type (floating point)
  - [x] Big number type
  - [x] Bulk error type
  - [x] Verbatim string type
  - [x] Map type
  - [x] Set type
  - [x] Push type (server-initiated messages)

- [x] 更新 `src/protocol/parser.rs` 解析 RESP3
  - [x] 支持新的类型标记符号
  - [x] 向后兼容 RESP2
  - [x] 协议版本协商

- [x] 添加 RESP3 序列化支持
- [x] 添加 RESP3 单元测试
- [x] 更新文档说明 RESP3 支持

### 2.2 RESP3 特性
- [x] 实现 `HELLO` 命令 (协议版本切换)
- [x] 支持属性 (Attributes) 功能
- [x] 支持流式响应

---

## 优先级 3 - Redis 基础命令支持

### 3.1 Database (DB) 相关命令
- [x] `SELECT` - 切换数据库
- [x] `DBSIZE` - 获取当前数据库键数量
- [x] `FLUSHDB` - 清空当前数据库
- [x] `FLUSHALL` - 清空所有数据库
- [x] `SWAPDB` - 交换两个数据库
- [x] `MOVE` - 移动键到其他数据库

**实现要点:**
- [x] 修改 `StorageAdapter` 支持多数据库
- [x] 添加数据库索引管理
- [x] 更新连接状态跟踪当前数据库

### 3.2 Key 相关通用命令
- [x] `KEYS` - 查找匹配模式的键
- [x] `SCAN` - 迭代数据库中的键（游标）
- [x] `RANDOMKEY` - 返回随机键
- [x] `RENAME` - 重命名键
- [x] `RENAMENX` - 仅当新键名不存在时重命名
- [x] `TYPE` - 返回键的类型
- [ ] `DUMP` - 序列化键的值
- [ ] `RESTORE` - 反序列化并创建键
- [x] `COPY` - 复制键 (Redis 6.2+)
- [ ] `MIGRATE` - 原子性迁移键到另一个实例

### 3.3 Key 过期相关命令
- [x] `EXPIRE` - 设置键过期时间（秒）
- [x] `EXPIREAT` - 设置键过期时间戳（秒）
- [x] `PEXPIRE` - 设置键过期时间（毫秒）
- [x] `PEXPIREAT` - 设置键过期时间戳（毫秒）
- [x] `TTL` - 获取键剩余生存时间（秒）
- [x] `PTTL` - 获取键剩余生存时间（毫秒）
- [x] `PERSIST` - 移除键的过期时间
- [x] `EXPIRETIME` - 获取键过期时间戳（秒）(Redis 7.0+)
- [x] `PEXPIRETIME` - 获取键过期时间戳（毫秒）(Redis 7.0+)

**实现要点:**
- [x] 实现 TTL 管理器（后台定期清理过期键）
- [x] 修改 `StorageAdapter` 存储过期时间
- [x] 实现懒惰删除和主动删除策略
- [ ] 添加过期键统计

### 3.4 Key 排序命令
- [ ] `SORT` - 排序列表、集合或有序集合
- [ ] `SORT_RO` - 只读排序 (Redis 7.0+)

### 3.5 Server 管理命令
- [x] `PING` - ✅ 已实现（支持可选 message 参数：无参数返回 PONG，有参数返回参数的副本）
- [x] `ECHO` - ✅ 已实现
- [x] `INFO` - 服务器信息
- [x] `CONFIG GET` - 获取配置参数
- [x] `CONFIG SET` - 设置配置参数
- [ ] `CONFIG REWRITE` - 重写配置文件
- [ ] `COMMAND` - 获取命令详细信息
- [ ] `COMMAND COUNT` - 获取命令总数
- [ ] `COMMAND INFO` - 获取特定命令信息
- [x] `TIME` - 返回服务器时间
- [x] `CLIENT LIST` - 列出客户端连接
- [x] `CLIENT SETNAME` - 设置客户端名称
- [x] `CLIENT GETNAME` - 获取客户端名称
- [ ] `SHUTDOWN` - 关闭服务器
- [ ] `SAVE` - 同步保存数据到磁盘
- [ ] `BGSAVE` - 后台保存数据到磁盘
- [ ] `LASTSAVE` - 获取最后保存时间

---

## 优先级 4 - 存储引擎集成

### 4.1 AiDb 集成
- [x] 研究 AiDb v0.1.0 API
- [x] 实现真实的 AiDb 存储适配器
- [x] 添加 AiDb 配置选项
- [x] 性能测试和优化
- [x] 添加 AiDb 集成测试

### 4.2 持久化支持
- [x] 实现 RDB 快照持久化
- [x] 实现 AOF 日志持久化
- [x] 添加持久化配置选项
- [ ] 实现数据恢复功能
- [x] 添加数据一致性检查

---

## 优先级 5 - 更多 Redis 数据类型

### 5.1 List 列表类型
- [x] `LPUSH`, `RPUSH` - 左/右推入
- [x] `LPOP`, `RPOP` - 左/右弹出
- [x] `LLEN` - 列表长度
- [x] `LRANGE` - 获取范围元素
- [x] `LINDEX` - 获取指定索引元素
- [x] `LSET` - 设置指定索引元素
- [ ] `LINSERT` - 插入元素
- [x] `LREM` - 删除元素
- [x] `LTRIM` - 修剪列表
- [ ] `BLPOP`, `BRPOP` - 阻塞弹出
- [ ] `LMOVE`, `BLMOVE` - 移动元素

### 5.2 Set 集合类型
- [x] `SADD` - 添加成员
- [x] `SREM` - 删除成员
- [x] `SISMEMBER` - 检查成员存在
- [x] `SMEMBERS` - 获取所有成员
- [x] `SCARD` - 集合大小
- [x] `SPOP` - 随机弹出成员
- [x] `SRANDMEMBER` - 随机获取成员
- [ ] `SMOVE` - 移动成员
- [x] `SUNION`, `SINTER`, `SDIFF` - 集合运算
- [x] `SUNIONSTORE`, `SINTERSTORE`, `SDIFFSTORE` - 集合运算并存储

### 5.3 Hash 哈希类型
- [x] `HSET`, `HSETNX` - 设置字段
- [x] `HGET` - 获取字段值
- [x] `HMGET` - 批量操作 (HMSET is deprecated, use HSET)
- [x] `HDEL` - 删除字段
- [x] `HEXISTS` - 检查字段存在
- [x] `HLEN` - 字段数量
- [x] `HKEYS`, `HVALS` - 获取所有键/值
- [x] `HGETALL` - 获取所有字段和值
- [x] `HINCRBY`, `HINCRBYFLOAT` - 增量操作
- [ ] `HSCAN` - 迭代字段

### 5.4 Sorted Set 有序集合类型
- [x] `ZADD` - 添加成员
- [x] `ZREM` - 删除成员
- [x] `ZSCORE` - 获取成员分数
- [x] `ZRANK`, `ZREVRANK` - 获取排名
- [x] `ZRANGE`, `ZREVRANGE` - 范围查询
- [x] `ZRANGEBYSCORE`, `ZREVRANGEBYSCORE` - 按分数范围
- [x] `ZCARD` - 集合大小
- [x] `ZCOUNT` - 统计范围内成员数
- [x] `ZINCRBY` - 增加分数
- [ ] `ZUNION`, `ZINTER`, `ZDIFF` - 集合运算

**实现要点:**
- [x] 扩展 `StoredValue` 支持多种数据类型 (String, List, Hash, Set, ZSet)
- [x] 实现所有主要的 List 命令 (10个)
- [x] 实现所有主要的 Hash 命令 (12个)
- [x] 实现所有主要的 Set 命令 (13个)
- [x] 实现所有主要的 Sorted Set 命令 (12个)
- [x] 添加数据类型检查和错误处理
- [x] 通过 clippy 和格式化检查
- [x] 所有测试通过

---

## 优先级 6 - 高级特性

### 6.1 事务支持
- [ ] `MULTI` - 开始事务
- [ ] `EXEC` - 执行事务
- [ ] `DISCARD` - 取消事务
- [ ] `WATCH` - 监视键
- [ ] `UNWATCH` - 取消监视

### 6.2 Pub/Sub 发布订阅
- [ ] `PUBLISH` - 发布消息
- [ ] `SUBSCRIBE` - 订阅频道
- [ ] `UNSUBSCRIBE` - 取消订阅
- [ ] `PSUBSCRIBE` - 模式订阅
- [ ] `PUNSUBSCRIBE` - 取消模式订阅
- [ ] `PUBSUB` - 查询订阅信息

### 6.3 Lua 脚本
- [x] `EVAL` - 执行脚本
- [x] `EVALSHA` - 执行缓存脚本
- [x] `SCRIPT LOAD` - 加载脚本
- [x] `SCRIPT EXISTS` - 检查脚本存在
- [x] `SCRIPT FLUSH` - 清空脚本缓存
- [x] `SCRIPT KILL` - 终止脚本
- [x] **事务性支持** - 脚本执行自动回滚
  - [x] 写操作缓冲区机制
  - [x] 读自己的写语义
  - [x] 成功时自动提交
  - [x] 失败时自动回滚
  - [x] 完整测试覆盖
  - [x] 文档更新

**实现说明**: 实现了基于写缓冲区的事务机制，所有脚本内的写操作先写入缓冲区，成功时批量提交，失败时自动丢弃，确保数据一致性。详见 `docs/LUA_TRANSACTION_DESIGN.md`。

### 6.4 Stream 流数据类型
- [ ] `XADD` - 添加消息
- [ ] `XREAD` - 读取消息
- [ ] `XRANGE` - 范围查询
- [ ] `XLEN` - 流长度
- [ ] `XDEL` - 删除消息
- [ ] `XTRIM` - 修剪流
- [ ] Consumer Groups 支持

---

## 优先级 7 - 性能优化

### 7.1 性能基准测试
- [x] 使用 `redis-benchmark` 测试
- [x] 创建自定义性能测试套件
- [x] 对比 Redis 性能基准
- [x] 生成性能报告

### 7.2 性能优化
- [x] 优化 RESP 协议解析性能
- [x] 优化内存分配和使用
- [x] 实现连接池优化
- [x] 添加命令流水线（pipelining）支持
- [x] 实现批量操作优化
- [x] 添加缓存层

### 7.3 并发优化
- [x] 分析并发瓶颈
- [x] 优化锁策略
- [x] 实现无锁数据结构
- [x] 调整 Tokio runtime 配置

---

## 优先级 8 - 监控和可观测性

### 8.1 日志增强
- [ ] 添加结构化日志
- [ ] 实现日志级别动态调整
- [ ] 添加慢查询日志
- [ ] 实现日志轮转和归档

### 8.2 Metrics 指标
- [ ] 集成 Prometheus metrics
- [ ] 添加命令执行统计
- [ ] 添加连接统计
- [ ] 添加内存使用统计
- [ ] 添加性能指标

### 8.3 追踪 (Tracing)
- [ ] 集成分布式追踪 (OpenTelemetry)
- [ ] 添加请求追踪
- [ ] 性能分析工具集成

---

## 优先级 9 - 集群和高可用 (基于 AiDb v0.4.1 MultiRaft API)

**设计理念**: AiDb v0.4.1 已提供完整的 MultiRaft 集群管理 API，AiKv 应直接使用这些 API 实现 Redis Cluster 协议适配，最大化代码复用，最小化自主开发。

**核心依赖**: 通过 `aidb/raft-cluster` feature 启用集群功能

```toml
[features]
cluster = ["aidb/raft-cluster"]
```

### 9.1 AiDb MultiRaft API 组件导入

启用 `cluster` feature 后，可通过 `aidb::cluster` 导入以下组件：

```rust
use aidb::cluster::{
    // 核心节点管理
    MultiRaftNode,        // 多 Raft Group 节点管理
    MetaRaftNode,         // 集群元数据 Raft 管理
    
    // 路由和分片
    Router,               // key→slot→group 路由器
    SLOT_COUNT,           // slot 总数 (16384)
    
    // 迁移管理
    MigrationManager,     // 在线 slot 迁移
    MigrationConfig,      // 迁移配置
    
    // 成员管理
    MembershipCoordinator, // 成员变更协调
    ReplicaAllocator,      // 副本分配算法
    
    // 数据结构
    ClusterMeta,          // 集群元数据
    GroupMeta,            // Raft Group 元数据
    NodeInfo,             // 节点信息
    NodeStatus,           // 节点状态
    SlotMigration,        // 迁移状态
    SlotMigrationState,   // 迁移状态枚举
    
    // 类型别名
    NodeId,               // 节点 ID 类型 (u64)
    GroupId,              // Group ID 类型 (u64)
};
```

### 9.2 Redis Cluster 协议胶水层实现 (AiKv 开发内容)

AiKv 只需实现 **RESP 协议解析** 和 **Redis Cluster 命令到 AiDb API 的映射**：

#### 9.2.1 集群信息命令映射

| Redis 命令 | AiDb API | AiKv 胶水层实现 |
|-----------|----------|----------------|
| `CLUSTER INFO` | `meta_raft.get_cluster_meta()` | 解析 `ClusterMeta` 生成 Redis 格式输出 |
| `CLUSTER NODES` | `meta_raft.get_cluster_meta().nodes` | 遍历节点，格式化为 Redis CLUSTER NODES 格式 |
| `CLUSTER SLOTS` | `meta_raft.get_cluster_meta().slots` + `.groups` | 组合 slots 和 groups 生成 Redis 格式 |
| `CLUSTER MYID` | `multi_raft_node.node_id()` | 返回当前节点 ID |
| `CLUSTER KEYSLOT key` | `Router::key_to_slot(key)` | 直接调用，CRC16/XMODEM 与 Redis 完全兼容 |

#### 9.2.2 节点管理命令映射

| Redis 命令 | AiDb API | AiKv 胶水层实现 |
|-----------|----------|----------------|
| `CLUSTER MEET ip port` | `meta_raft.add_node(node_id, addr)` | 生成 node_id，添加节点 |
| `CLUSTER FORGET node_id` | `meta_raft.remove_node(node_id)` | 从集群移除节点 |

#### 9.2.3 Slot 管理命令映射

| Redis 命令 | AiDb API | AiKv 胶水层实现 |
|-----------|----------|----------------|
| `CLUSTER ADDSLOTS slot...` | `meta_raft.update_slots(start, end, group_id)` | 批量分配 slots |
| `CLUSTER DELSLOTS slot...` | `meta_raft.update_slots(start, end, 0)` | 标记为未分配 |
| `CLUSTER SETSLOT slot NODE` | `meta_raft.update_slots(slot, slot+1, group_id)` | 分配单个 slot |
| `CLUSTER SETSLOT MIGRATING` | `migration_manager.start_migration(slot, from, to)` | 开始迁移 |
| `CLUSTER SETSLOT IMPORTING` | 自动处理 | 由 MigrationManager 内部管理 |
| `CLUSTER GETKEYSINSLOT` | `state_machine.scan_slot_keys_sync(group, slot)` | 扫描 slot 内 keys |

#### 9.2.4 成员管理命令映射

| Redis 命令 | AiDb API | AiKv 胶水层实现 |
|-----------|----------|----------------|
| `CLUSTER REPLICATE` | `membership_coordinator.add_learner()` | 添加副本 |
| `CLUSTER FAILOVER` | `raft.trigger_elect()` (openraft) | 触发选举 |

#### 9.2.5 数据操作路由

| Redis 命令 | AiDb API | 说明 |
|-----------|----------|------|
| `SET key value` | `multi_raft_node.put(key, value)` | 自动路由 |
| `GET key` | `multi_raft_node.get(&key)` | 自动路由 |
| `DEL key` | `multi_raft_node.delete(&key)` | 自动路由 |
| `MIGRATE` | `migration_manager.migrate_key()` | 单 key 迁移 |

### 9.3 AiKv 胶水层架构

```
┌─────────────────────────────────────────────────────────────────────┐
│                     AiKv RESP Listener (6379)                       │
│                             ↓                                       │
│                   Command Parser (现有代码)                          │
│                             ↓                                       │
│    ┌────────────────────────────────────────────────────────┐       │
│    │           Redis Cluster 协议胶水层 (新增)               │       │
│    │  • ClusterCommands: CLUSTER INFO/NODES/SLOTS/KEYSLOT   │       │
│    │  • SlotRedirector: -MOVED/-ASK 错误处理                │       │
│    │  • ClusterBus: 节点间通信 (端口 +10000)                │       │
│    └────────────────────────────────────────────────────────┘       │
│                             ↓                                       │
│    ┌────────────────────────────────────────────────────────┐       │
│    │           AiDb MultiRaft API (v0.4.1)                   │       │
│    │  • MultiRaftNode: 节点管理、自动路由                   │       │
│    │  • MetaRaftNode: 元数据 Raft                           │       │
│    │  • Router: CRC16 槽路由                                │       │
│    │  • MigrationManager: 在线迁移                          │       │
│    │  • MembershipCoordinator: 成员变更                     │       │
│    └────────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────────┘
```

### 9.4 实现任务清单

- [x] **阶段 A: 基础集成** (对应 Stage 0-1)
  - [x] 添加 `cluster` feature 和 AiDb v0.4.1 依赖
  - [x] 创建 `src/cluster/mod.rs` 模块
  - [x] 实现 `ClusterNode` 封装 `MultiRaftNode`
  - [x] 实现 `CLUSTER KEYSLOT` 命令 (使用 `Router::key_to_slot`)
  - [x] 实现 `-MOVED` 重定向逻辑

- [x] **阶段 B: 集群命令** (对应 Stage 2)
  - [x] 实现 `CLUSTER INFO` 命令 (基础版本)
  - [x] 实现 `CLUSTER NODES` 命令 (基础版本)
  - [x] 实现 `CLUSTER SLOTS` 命令 (基础版本)
  - [x] 实现 `CLUSTER MYID` 命令
  - [x] 实现 `CLUSTER MEET` 命令
  - [x] 实现 `CLUSTER FORGET` 命令
  - [x] 实现 `CLUSTER ADDSLOTS/DELSLOTS` 命令
  - [x] 实现 `CLUSTER SETSLOT` 命令

- [x] **阶段 C: 槽迁移** (对应 Stage 3)
  - [x] 实现 `CLUSTER GETKEYSINSLOT` 命令
  - [x] 实现迁移状态查询 (`CLUSTER SETSLOT ... IMPORTING/MIGRATING`)
  - [x] 实现 `-ASK` 重定向逻辑
  - [x] 集成 `MigrationManager` 实现在线迁移
  - [ ] 实现 `MIGRATE` 命令 (需要网络层支持，移至后续版本)

- [x] **阶段 D: 高可用** (对应 Stage 4)
  - [x] 实现 `CLUSTER REPLICATE` 命令
  - [x] 实现 `CLUSTER FAILOVER` 命令 (支持 FORCE/TAKEOVER 模式)
  - [x] 实现 `READONLY/READWRITE` 命令
  - [x] 实现 `CLUSTER REPLICAS` / `CLUSTER SLAVES` 命令
  - [x] 集成 `MembershipCoordinator` (通过 ClusterState 的 replica_map 实现)
  - [x] 添加 FailoverMode 枚举
  - [x] 添加 NodeInfo.new_replica() 构造器
  - [x] 添加 ClusterState 副本管理方法 (add_replica, remove_replica, promote_replica)
  - [x] 添加完整的单元测试覆盖

- [ ] **阶段 E: Cluster Bus** (可选优化)
  - [ ] 实现节点间 gossip 协议
  - [ ] 实现心跳检测
  - [ ] 实现故障检测

### 9.5 代码量估算

| 模块 | 估算行数 | 说明 |
|------|----------|------|
| ClusterCommands | ~500 行 | Redis 命令解析和响应格式化 |
| SlotRedirector | ~200 行 | -MOVED/-ASK 处理 |
| ClusterBus | ~300 行 | 节点间通信 (可选) |
| **总计** | ~1000 行 | 纯胶水层代码，核心逻辑由 AiDb 提供 |

**对比**: 如果不使用 AiDb，从零实现 MultiRaft + 迁移 + 成员管理需要 ~10000+ 行代码

---


## 优先级 10 - 文档和工具

### 10.1 文档完善
- [ ] 更新开发文档包含新特性
- [ ] 添加架构设计文档
- [ ] 添加性能调优指南
- [ ] 添加故障排查指南
- [ ] 生成 API 文档（rustdoc）

### 10.2 开发工具
- [ ] 创建开发环境设置脚本
- [ ] 添加 Docker 开发环境
- [ ] 创建调试工具
- [ ] 添加性能分析工具

### 10.3 示例和教程
- [ ] 更多客户端示例
- [ ] 使用场景教程
- [ ] 最佳实践文档
- [ ] 性能优化案例

---

## 代码质量改进

### 当前问题审查
- [ ] 审查现有代码，识别可改进的地方
- [ ] 优化错误处理机制
- [ ] 改进代码注释和文档字符串
- [ ] 重构复杂函数
- [ ] 消除代码重复

### 依赖管理
- [ ] 审查和更新依赖版本
- [ ] 移除未使用的依赖
- [ ] 评估依赖安全性

---

## 优先级 0 - 存储层架构重构 (架构修正)

### 问题描述
当前存储层（StorageAdapter）承担了过多不属于它的职责，包含了大量命令级别的逻辑实现。这违反了单一职责原则和关注点分离原则，导致：
- 存储层有 52+ 个命令特定方法（如 `mset_in_db`, `list_lpush_in_db`, `hash_set_in_db` 等）
- 命令逻辑与存储逻辑混合，难以维护和测试
- 存储接口不够正交和精简
- 切换存储引擎（如从内存到 AiDb）需要重新实现所有命令逻辑

### 架构目标
1. **存储层**: 只提供最基本的正交存储操作接口（CRUD + 过期管理）
2. **命令层**: 所有命令相关的业务逻辑应在各自的命令实现类中完成
3. **清晰的分层**: 存储层负责数据持久化，命令层负责业务逻辑

### 0.1 架构分析阶段
- [x] 分析当前存储层的所有公开方法（52+ 方法）
- [x] 将方法分类为：
  - [x] 核心存储操作（应保留）
  - [x] 命令特定逻辑（应移至命令层）
  - [x] 辅助功能（需重新设计）
- [x] 识别存储层的最小正交接口集合
- [x] 记录当前依赖关系和影响范围

**当前存储层方法清单**:
- 基础操作: `get_from_db`, `set_in_db`, `delete_from_db`, `exists_in_db`
- 过期管理: `set_expire_in_db`, `get_ttl_in_db`, `persist_in_db`, `get_expire_time_in_db`, `set_expire_at_in_db`
- 数据库操作: `dbsize_in_db`, `flush_db`, `flush_all`, `swap_db`, `get_all_keys_in_db`
- 键管理: `rename_in_db`, `rename_nx_in_db`, `copy_in_db`, `move_key`, `random_key_in_db`
- 批量字符串操作: `mget_from_db`, `mset_in_db` (命令特定)
- List 操作 (9个): `list_lpush_in_db`, `list_rpush_in_db`, `list_lpop_in_db`, `list_rpop_in_db`, `list_len_in_db`, `list_range_in_db`, `list_index_in_db`, `list_set_in_db`, `list_rem_in_db`, `list_trim_in_db` (全部命令特定)
- Hash 操作 (10个): `hash_set_in_db`, `hash_setnx_in_db`, `hash_get_in_db`, `hash_mget_in_db`, `hash_del_in_db`, `hash_exists_in_db`, `hash_len_in_db`, `hash_keys_in_db`, `hash_vals_in_db`, `hash_getall_in_db`, `hash_incrby_in_db`, `hash_incrbyfloat_in_db` (全部命令特定)
- Set 操作 (11个): `set_add_in_db`, `set_rem_in_db`, `set_ismember_in_db`, `set_members_in_db`, `set_card_in_db`, `set_pop_in_db`, `set_randmember_in_db`, `set_union_in_db`, `set_inter_in_db`, `set_diff_in_db`, `set_unionstore_in_db`, `set_interstore_in_db`, `set_diffstore_in_db` (全部命令特定)
- ZSet 操作 (10个): `zset_add_in_db`, `zset_rem_in_db`, `zset_score_in_db`, `zset_rank_in_db`, `zset_range_in_db`, `zset_rangebyscore_in_db`, `zset_card_in_db`, `zset_count_in_db`, `zset_incrby_in_db` (全部命令特定)

### 0.2 新架构设计阶段
- [x] 设计最小化存储层接口
  - [x] 定义核心存储方法（实现为具体方法而非 trait）
  - [x] 包含的基本操作：
    - [x] `get_value(db: usize, key: &str) -> Result<Option<StoredValue>>`
    - [x] `set_value(db: usize, key: String, value: StoredValue) -> Result<()>`
    - [x] `delete_and_get(db: usize, key: &str) -> Result<Option<StoredValue>>`
    - [x] `update_value<F>(db: usize, key: &str, f: F) -> Result<bool>`
    - [x] 保留已有的 `delete(db: usize, key: &str) -> Result<bool>`
    - [x] 保留已有的 `exists(db: usize, key: &str) -> Result<bool>`
  - [x] 数据库级操作（保持不变）：
    - [x] `flush_db(db: usize) -> Result<()>`
    - [x] `flush_all() -> Result<()>`
    - [x] `db_size(db: usize) -> Result<usize>`
    - [x] `swap_db(db1: usize, db2: usize) -> Result<()>`
  - [x] 过期管理（保留在存储层，已存在）：
    - [x] `set_expire_in_db(db: usize, key: &str, expire_ms: u64) -> Result<bool>`
    - [x] `get_ttl_in_db(db: usize, key: &str) -> Result<i64>`
    - [x] `persist_in_db(db: usize, key: &str) -> Result<bool>`

- [x] 设计 `StoredValue` 作为通用值容器
  - [x] 支持所有数据类型: String, List, Hash, Set, ZSet
  - [x] 提供类型检查和转换方法
  - [x] 暴露底层数据结构供命令层直接操作

- [x] 为命令层设计辅助结构
  - [x] 公开 `StoredValue` 和 `ValueType`
  - [x] 提供类型安全的值访问和修改模式（`as_string()`, `as_list_mut()` 等）
  - [x] 确保原子性操作的支持（通过 `update_value` 闭包）

### 0.3 重构实现计划
- [x] **阶段 1: 准备工作** ✅ (Commit: 3f568b6)
  - [x] 使 `StoredValue` 和 `ValueType` 公开
  - [x] 为 `StoredValue` 添加公开访问方法 (`as_string()`, `as_list()`, `as_hash()`, `as_set()`, `as_zset()`)
  - [x] 添加可变访问方法 (`as_list_mut()`, `as_hash_mut()`, `as_set_mut()`, `as_zset_mut()`)
  - [x] 添加最小化存储接口 (`get_value()`, `set_value()`, `update_value()`, `delete_and_get()`)
  - [x] 确保所有现有测试仍然通过 (78 个单元测试全部通过)
  
- [x] **阶段 2: String 命令迁移** ✅ (Commit: 650ed9d)
  - [x] 将 `mset_in_db` 逻辑移到 `StringCommands::mset`
  - [x] 将 `mget_from_db` 逻辑移到 `StringCommands::mget`
  - [x] 更新相关测试
  - [x] 验证功能正确性

- [x] **阶段 3: List 命令迁移** ✅ (Commit: 3dcca1c)
  - [x] 将所有 `list_*_in_db` 方法的逻辑移到 `ListCommands` (10 个命令)
  - [x] 在命令层直接操作 `VecDeque<Bytes>`
  - [x] 实现: LPUSH, RPUSH, LPOP, RPOP, LLEN, LRANGE, LINDEX, LSET, LREM, LTRIM
  - [x] 更新相关测试
  - [x] 验证功能正确性

- [x] **阶段 4: Hash 命令迁移** ✅ (Commits: e692a93, 60db157)
  - [x] 将所有 `hash_*_in_db` 方法的逻辑移到 `HashCommands` (12 个命令)
  - [x] 在命令层直接操作 `HashMap<String, Bytes>`
  - [x] 处理 `hincrby` 和 `hincrbyfloat` 的原子性（在命令层解析-修改-存储）
  - [x] 实现: HSET, HSETNX, HGET, HMGET, HDEL, HEXISTS, HLEN, HKEYS, HVALS, HGETALL, HINCRBY, HINCRBYFLOAT
  - [x] 使用 Entry API 优化 HSETNX (修复 clippy 警告)
  - [x] 更新相关测试
  - [x] 验证功能正确性

- [x] **阶段 5: Set 命令迁移** ✅ (Commit: 80594b3 - 13 个命令)
  - [x] 将所有 `set_*_in_db` 方法的逻辑移到 `SetCommands`
  - [x] 在命令层直接操作 `HashSet<Vec<u8>>`
  - [x] 实现集合运算逻辑（union, inter, diff）
  - [x] 命令: SADD, SREM, SISMEMBER, SMEMBERS, SCARD, SPOP, SRANDMEMBER, SUNION, SINTER, SDIFF, SUNIONSTORE, SINTERSTORE, SDIFFSTORE
  - [x] 更新相关测试
  - [x] 验证功能正确性

- [x] **阶段 6: ZSet 命令迁移** ✅ (Commit: 963afec - 10 个命令)
  - [x] 将所有 `zset_*_in_db` 方法的逻辑移到 `ZSetCommands`
  - [x] 在命令层直接操作 `BTreeMap<Vec<u8>, f64>`
  - [x] 实现排序和范围查询逻辑
  - [x] 命令: ZADD, ZREM, ZSCORE, ZRANK, ZREVRANK, ZRANGE, ZREVRANGE, ZRANGEBYSCORE, ZREVRANGEBYSCORE, ZCARD, ZCOUNT, ZINCRBY
  - [x] 更新相关测试
  - [x] 验证功能正确性

- [x] **阶段 7: 清理和优化** ✅ (Commit: a94412f)
  - [x] 从 `StorageAdapter` 中移除已迁移的命令特定方法 (46 个方法)
  - [x] 更新存储层测试使用新接口
  - [x] 更新性能基准测试使用新接口
  - [x] 代码从 2649 行减少到 878 行 (-1771 行, -67%)
  - [x] 运行完整测试套件 (96 个测试全部通过)
  - [x] 通过 clippy 检查 (零警告)

**迁移完成统计**:
- ✅ String: 2/2 (100%)
- ✅ List: 10/10 (100%)
- ✅ Hash: 12/12 (100%)
- ✅ Set: 13/13 (100%)
- ✅ ZSet: 10/10 (100%)
- **总计**: 47/47 命令已迁移 (100%)

### 0.4 文档和验证
- [x] 更新架构文档说明新的分层设计 ✅ (Commit: a959c93, 最终更新: 2025-11-14)
  - [x] 创建 `docs/ARCHITECTURE_REFACTORING.md` - 完整的重构计划和实施状态
  - [x] 更新 `CHANGELOG.md` - 记录所有重构变更
  - [x] 文档化 AiDbStorageAdapter 的当前限制和未来工作
  - [x] 记录 Section 0.4 完成的工作
- [x] 创建存储层 API 文档（rustdoc）✅ (2025-11-14)
  - [x] 为 `aidb_adapter.rs` 添加模块级文档
  - [x] 为 `memory_adapter.rs` 添加模块级文档
  - [x] 为核心方法添加详细文档和示例
  - [x] 为公共类型添加文档
- [x] 清理 AiDbStorageAdapter 冗余代码 ✅ (2025-11-14)
  - [x] 移除 `mget_from_db`, `mget`, `mset_in_db`, `mset` 方法
  - [x] 更新测试使用新接口
  - [x] 更新示例文件使用新接口
- [ ] 添加架构决策记录 (ADR) ⏳ (可选)
- [ ] 编写迁移指南（如果有外部依赖者）⏳ (暂不需要)
- [x] 进行全面的回归测试 ✅
  - [x] 89 个单元测试全部通过
  - [x] 5 个集成测试全部通过
  - [x] cargo clippy 零警告
  - [x] cargo fmt 已格式化
  - [x] codeql 安全扫描零问题
- [ ] 性能对比分析（重构前后）⏳ (可选)

### 0.5 后续优化
- [ ] 考虑为高频操作添加专门的优化路径
- [ ] 评估是否需要批量操作接口（如 `batch_get`, `batch_set`）
- [ ] 考虑引入事务支持的存储接口
- [ ] 优化锁粒度和并发性能
- [ ] 评估使用 RwLock 以外的并发原语

---

### 📊 重构进度总结 (更新于 2025-11-26)

**整体进度**: 阶段 0.1-0.4 全部完成 - **架构重构 100% 完成** ✅

**已完成**:
- ✅ 阶段 0.1: 架构分析 - 完成
- ✅ 阶段 0.2: 新架构设计 - 完成
- ✅ 阶段 0.3.1: 准备工作 - 完成 (Commit: 3f568b6)
- ✅ 阶段 0.3.2: String 命令迁移 (2/2) - 完成 (Commit: 650ed9d)
- ✅ 阶段 0.3.3: List 命令迁移 (10/10) - 完成 (Commit: 3dcca1c)
- ✅ 阶段 0.3.4: Hash 命令迁移 (12/12) - 完成 (Commits: e692a93, 60db157)
- ✅ 阶段 0.3.5: Set 命令迁移 (13/13) - 完成 (Commit: 80594b3)
- ✅ 阶段 0.3.6: ZSet 命令迁移 (10/10) - 完成 (Commit: 963afec)
- ✅ 阶段 0.3.7: 清理和优化 - 完成 (Commit: a94412f)
- ✅ 阶段 0.4: 文档和验证 - **完成** ✅

**命令迁移统计**:
- String: 2/2 ✅ (100%)
- List: 10/10 ✅ (100%)
- Hash: 12/12 ✅ (100%)
- Set: 13/13 ✅ (100%)
- ZSet: 10/10 ✅ (100%)
- **总计**: 47/47 命令 (100%)

**测试状态**:
- ✅ 96 个单元测试全部通过
- ✅ 集成测试全部通过
- ✅ cargo clippy 零警告
- ✅ cargo fmt 代码格式化

**代码改进**:
- ✅ 移除 50+ 个命令特定方法
- ✅ memory_adapter.rs: 2649 行 → 878 行 (-67%)
- ✅ aidb_adapter.rs: 1318 行 → 1297 行 (-21 行)
- ✅ 总计删除 ~1800+ 行代码
- ✅ 架构更清晰，职责更明确
- ✅ 完整的 API 文档

**完成工作**:
1. ✅ 清理冗余代码：移除 mget/mset 相关方法
2. ✅ 添加 rustdoc 文档：模块级、方法级、类型级全覆盖
3. ✅ 更新示例：aidb_storage_example.rs 使用新接口
4. ✅ 更新文档：ARCHITECTURE_REFACTORING.md, TODO.md
5. ✅ 全面验证：测试、格式、lint 全部通过

**AiDbStorageAdapter 扩展完成** ✅:
- 使用 bincode 实现高性能序列化/反序列化
- 支持所有数据类型（String, List, Hash, Set, ZSet）
- 新增 11 个测试用例，所有 96 个测试通过
- 与 MemoryAdapter 功能完全对等
- 详细信息见 `docs/ARCHITECTURE_REFACTORING.md`

**参考文档**:
- 详细计划: `docs/ARCHITECTURE_REFACTORING.md`
- 变更记录: `CHANGELOG.md`

---

### 预期收益
1. **清晰的架构**: 存储层和命令层职责明确
2. **易于维护**: 命令逻辑集中在命令类中，便于修改和测试
3. **灵活性**: 可以轻松切换存储引擎而不影响命令实现
4. **可测试性**: 存储层和命令层可以独立测试
5. **性能**: 减少不必要的抽象层，可能提升性能
6. **扩展性**: 新增命令只需要使用基础存储接口，不需要修改存储层

### 风险和缓解
- **风险**: 大规模重构可能引入 bug
  - **缓解**: 分阶段实施，每个阶段都有完整测试
- **风险**: 性能可能受影响
  - **缓解**: 每个阶段进行性能基准测试，及时调整
- **风险**: 代码量可能增加
  - **缓解**: 通过辅助函数和宏减少重复代码
- **风险**: 原子性操作可能变复杂
  - **缓解**: 在命令层使用适当的锁策略，或在存储层提供事务接口

---

## AiKv v1.0.0 完整终极规划 (2025.11.26 – 2026.03.31)

**目标**: 发布全球第一个 100% Redis Cluster 协议兼容 + 完全 Rust 原生 + 基于 AiDb v0.4.1 Multi-Raft 的生产级分布式 KV 引擎

**核心策略**: 最大化利用 AiDb v0.4.1 已提供的 MultiRaft API，AiKv 只需实现 Redis Cluster 协议胶水层

**最终版本号**: AiKv v1.0.0 (2026 年 3 月 31 日发布)

### 技术架构 (基于 AiDb v0.4.1 API)

```
┌─────────────────────────────────────────────────────────────────────┐
│                     AiKv RESP Listener (6379)                       │
│                             ↓                                       │
│                   Command Parser (现有代码)                          │
│                             ↓                                       │
│    ┌────────────────────────────────────────────────────────┐       │
│    │         Redis Cluster 协议胶水层 (AiKv 新增 ~1000 行)   │       │
│    │  • CLUSTER 命令处理 → 调用 AiDb MetaRaftNode API        │       │
│    │  • 槽路由 → 使用 Router::key_to_slot()                 │       │
│    │  • -MOVED/-ASK 重定向 → 使用 Router.slot_to_group()    │       │
│    │  • 迁移管理 → 使用 MigrationManager API                │       │
│    └────────────────────────────────────────────────────────┘       │
│                             ↓                                       │
│    ┌────────────────────────────────────────────────────────┐       │
│    │         AiDb MultiRaft API (v0.4.1 完整提供)            │       │
│    │  • MultiRaftNode: 自动路由、数据读写                   │       │
│    │  • MetaRaftNode: 元数据 Raft、节点管理                 │       │
│    │  • Router: CRC16 槽计算 (Redis 兼容)                   │       │
│    │  • MigrationManager: 在线迁移、双写、原子切换           │       │
│    │  • MembershipCoordinator: 成员变更、副本分配           │       │
│    │  • 16384 Slots → Raft Groups 映射                      │       │
│    └────────────────────────────────────────────────────────┘       │
│                             ↓                                       │
│               Cluster Bus 端口 16379（gossip + 心跳）               │
└─────────────────────────────────────────────────────────────────────┘
```

### AiDb API 与 Redis Cluster 命令映射速查

| Redis Cluster 命令 | AiDb v0.4.1 API | 实现复杂度 |
|-------------------|-----------------|-----------|
| `CLUSTER KEYSLOT` | `Router::key_to_slot(key)` | ⭐ 简单 |
| `CLUSTER INFO` | `meta_raft.get_cluster_meta()` | ⭐ 简单 |
| `CLUSTER NODES` | `meta_raft.get_cluster_meta().nodes` | ⭐⭐ 中等 (格式化) |
| `CLUSTER SLOTS` | `meta_raft.get_cluster_meta().slots/groups` | ⭐⭐ 中等 |
| `CLUSTER MEET` | `meta_raft.add_node(id, addr)` | ⭐ 简单 |
| `CLUSTER ADDSLOTS` | `meta_raft.update_slots(start, end, group)` | ⭐ 简单 |
| `SET/GET/DEL` | `multi_raft.put/get/delete` | ⭐ 简单 (自动路由) |
| 在线迁移 | `migration_manager.start_migration()` | ⭐⭐ 中等 |
| 副本管理 | `membership_coordinator.add_learner()` | ⭐⭐ 中等 |

### 18 周完整里程碑（基于 AiDb API 重新规划）

| 周次 | 阶段 & 里程碑 | 核心交付物 | AiDb API 使用 |
|------|---------------|------------|--------------|
| **周 1-2** | **Stage 0**: 集成 AiDb v0.4.1 cluster feature | v0.2.0 | `MultiRaftNode`, `MetaRaftNode` 初始化 |
| **周 3-4** | **Stage 1**: 槽路由 + KEYSLOT + -MOVED | v0.3.0 | `Router::key_to_slot()`, `Router.route()` |
| **周 5-6** | **Stage 2**: CLUSTER 命令实现 | v0.4.0 | `meta_raft.get_cluster_meta()`, `add_node()` |
| **周 7-9** | **Stage 3**: 在线迁移 | v0.5.0 | `MigrationManager.start_migration()` |
| **周 10-12** | **Stage 4**: 副本 + failover | v0.6.0 | `MembershipCoordinator`, `ReplicaAllocator` |
| **周 13-15** | **Stage 5**: 高级功能 | v0.8.0 | 迁移感知读写 |
| **周 16-17** | **Stage 6**: 测试 + 压测 | v0.9.0 | 完整集成测试 |
| **周 18** | **Stage 7**: 发布 | v1.0.0 | 文档 + 镜像 |

### v1.0.0 硬性指标 (CI 强制)

| 指标 | 目标值 | 测试工具 |
|------|--------|----------|
| 3 节点吞吐 (50%读50%写) | ≥ 420k ops/sec | redis-benchmark -t set,get -n 5000000 -c 500 |
| 单节点吞吐 | ≥ 220k ops/sec | 同上 |
| 槽迁移速度 (1000 槽) | < 25 秒 | redis-cli --cluster reshard |
| 自动故障转移时间 | < 10 秒 | 杀进程 + 监控切换 |
| 副本延迟 (99.9%) | < 50 ms | 自研同步延迟监控 |
| 官方 cluster test 通过率 | 100% | redis/tests/cluster |
| 内存占用 (10 亿键, 3 节点) | < 120 GB 总和 | YCSB workload C |

### v1.0.0 功能兼容清单

| 类别 | 命令/功能 | 必须支持 |
|------|-----------|----------|
| **基本命令** | GET/SET/DEL/MGET/MSET/EXPIRE/TTL 等 | ✅ |
| **高级数据类型** | List/Set/Hash/Zset/Bitmap/HLL/Stream/JSON | ✅ |
| **事务** | MULTI/EXEC/WATCH/UNWATCH | ✅ |
| **Lua 脚本** | EVAL/EVALSHA/SCRIPT LOAD/KILL | ✅ |
| **Pub/Sub** | PUBLISH/SUBSCRIBE/PSUBSCRIBE | ✅ |
| **Cluster 核心** | CLUSTER MEET/ADDSLOTS/NODES/INFO/SLOTS/KEYSLOT/SETSLOT/FAILOVER | ✅ |
| **槽迁移** | CLUSTER GETKEYSINSLOT + reshard | ✅ |
| **读写分离** | READONLY/READWRITE | ✅ |

### 一键部署方案 (v1.0.0 附带)

```yaml
# docker-compose.yml（6 节点 3 主 3 从）
version: '3.8'
services:
  aikv1:
    image: genuineh/aikv:1.0.0
    command: --multi-raft --node-id n1 --bind 0.0.0.0 --port 6379 --cluster-port 16379 --peers n2:16379,n3:16379,n4:16379,n5:16379,n6:16379
  aikv2:
    image: genuineh/aikv:1.0.0
    command: --multi-raft --node-id n2 --bind 0.0.0.0 --port 6380 --cluster-port 16380 --peers n1:16379,n3:16379,n4:16379,n5:16379,n6:16379
  aikv3:
    image: genuineh/aikv:1.0.0
    command: --multi-raft --node-id n3 --bind 0.0.0.0 --port 6381 --cluster-port 16381 --peers n1:16379,n2:16379,n4:16379,n5:16379,n6:16379
  aikv4:
    image: genuineh/aikv:1.0.0
    command: --multi-raft --node-id n4 --bind 0.0.0.0 --port 6382 --cluster-port 16382 --peers n1:16379,n2:16379,n3:16379,n5:16379,n6:16379
  aikv5:
    image: genuineh/aikv:1.0.0
    command: --multi-raft --node-id n5 --bind 0.0.0.0 --port 6383 --cluster-port 16383 --peers n1:16379,n2:16379,n3:16379,n4:16379,n6:16379
  aikv6:
    image: genuineh/aikv:1.0.0
    command: --multi-raft --node-id n6 --bind 0.0.0.0 --port 6384 --cluster-port 16384 --peers n1:16379,n2:16379,n3:16379,n4:16379,n5:16379
```

启动后只需一句：
```bash
redis-cli --cluster create 172.20.0.2:6379 172.20.0.3:6380 172.20.0.4:6381 \
  172.20.0.5:6382 172.20.0.6:6383 172.20.0.7:6384 --cluster-replicas 1
```

---

## 版本规划 (基于 AiDb v0.4.1 API)

### v0.1.0 (当前版本 - 已完成)
- ✅ RESP2/RESP3 协议支持
- ✅ String 命令 (8个)
- ✅ JSON 命令 (7个)
- ✅ 基础 TCP 服务器
- ✅ 内存存储适配器
- ✅ AiDb v0.4.1 集成（已升级）
- ✅ 多数据库支持 (16 个数据库)
- ✅ 键过期机制 (TTL 支持)
- ✅ 存储层架构重构完成
- ✅ List/Set/Hash/ZSet 数据类型
- ✅ Lua 脚本支持

### v0.2.0 (Stage 0: 周 1-2) - AiDb MultiRaft 集成
- [ ] 添加 `cluster` feature (`aidb/raft-cluster`)
- [ ] 创建 `src/cluster/mod.rs` 模块
- [ ] 封装 `MultiRaftNode` 初始化
- [ ] 封装 `MetaRaftNode` 初始化
- [ ] 实现 3 节点启动和验证

**关键 AiDb API**:
```rust
use aidb::cluster::{MultiRaftNode, MetaRaftNode};
let node = MultiRaftNode::new(node_id, "./data", config).await?;
node.init_meta_raft(config).await?;
```

### v0.3.0 (Stage 1: 周 3-4) - 槽路由核心
- [ ] 实现 `CLUSTER KEYSLOT` 命令 (使用 `Router::key_to_slot`)
- [ ] 实现 `-MOVED` 重定向逻辑 (使用 `Router.slot_to_group`)
- [ ] 初始化 16384 槽到 Raft Group 映射

**关键 AiDb API**:
```rust
use aidb::cluster::{Router, SLOT_COUNT};
let slot = Router::key_to_slot(b"mykey");  // 与 Redis 完全兼容
let group_id = router.slot_to_group(slot)?;
```

### v0.4.0 (Stage 2: 周 5-6) - CLUSTER 命令
- [ ] 实现 `CLUSTER INFO` (使用 `meta_raft.get_cluster_meta()`)
- [ ] 实现 `CLUSTER NODES` (格式化 `ClusterMeta.nodes`)
- [ ] 实现 `CLUSTER SLOTS` (组合 `ClusterMeta.slots` 和 `.groups`)
- [ ] 实现 `CLUSTER MEET` (使用 `meta_raft.add_node()`)
- [ ] 实现 `CLUSTER ADDSLOTS/DELSLOTS` (使用 `meta_raft.update_slots()`)

**关键 AiDb API**:
```rust
use aidb::cluster::{MetaRaftNode, ClusterMeta, NodeInfo};
let meta: ClusterMeta = meta_raft.get_cluster_meta();
meta_raft.add_node(node_id, addr).await?;
meta_raft.update_slots(0, 5461, group_id).await?;
```

### v0.5.0 (Stage 3: 周 7-9) - 在线迁移
- [x] 实现 `CLUSTER GETKEYSINSLOT` (使用 `state_machine.scan_slot_keys_sync`)
- [x] 实现 `CLUSTER SETSLOT ... MIGRATING/IMPORTING`
- [x] 实现 `-ASK` 重定向逻辑
- [x] 集成迁移监控 (`MigrationManager.get_migration_progress`)
- [x] 实现 `CLUSTER COUNTKEYSINSLOT` 命令
- [x] 实现迁移状态查询方法 (`ClusterState.get_migration_state`)
- [x] 实现 `start_migration` 和 `complete_migration` 方法
- [x] 实现 `check_redirect_with_migration` (含 `-ASK` 重定向)
- [x] 实现 `should_handle_after_asking` 支持
- [x] 添加 `MigrationProgress` 进度追踪
- [x] 添加 `KeyScanner` 可插拔键扫描接口
- [ ] 实现 `MIGRATE` 命令 (需要网络层支持，移至后续版本)

**关键 AiDb API**:
```rust
use aidb::cluster::{MigrationManager, MigrationConfig};
let manager = MigrationManager::new(config, router, state_machine);
manager.start_migration(slot, from_group, to_group).await?;
// 迁移感知读写 (双写期间)
manager.put_with_migration_awareness(&key, value)?;
manager.get_with_migration_awareness(&key)?;
```

### v0.6.0 (Stage 4: 周 10-12) - 高可用
- [x] 实现 `CLUSTER REPLICATE` (使用 ClusterState.add_replica)
- [x] 实现 `CLUSTER FAILOVER` (使用 ClusterState.promote_replica, 支持 FORCE/TAKEOVER)
- [x] 实现 `READONLY/READWRITE` 命令
- [x] 实现 `CLUSTER REPLICAS` / `CLUSTER SLAVES` 命令

**实现说明**:
- 通过 `ClusterState.replica_map` 管理副本关系
- `promote_replica` 实现自动槽迁移和角色交换
- `FailoverMode` 枚举支持 Default/Force/Takeover 三种模式
- 完整单元测试覆盖

**关键 AiDb API**:
```rust
use aidb::cluster::{MembershipCoordinator, ReplicaAllocator};
coordinator.add_learner(group_id, node_id, addr).await?;
coordinator.promote_learner(group_id, new_members).await?;
```

### v0.8.0 (Stage 5: 周 13-15) - 高级功能
- [ ] 高级数据类型跨槽支持
- [ ] EVAL/EVALSHA 多槽拆分
- [ ] PUBLISH 跨节点投递
- [ ] 事务支持 (MULTI/EXEC/WATCH)

### v0.9.0 (Stage 6: 周 16-17) - 测试和优化
- [ ] 极限压测
- [ ] 官方测试套件通过
- [ ] 性能调优
- [ ] 稳定性测试

### v1.0.0 (Stage 7: 周 18 - 2026.03.31) - 发布
- [ ] Docker 官方镜像
- [ ] Helm Chart
- [ ] Prometheus Exporter
- [ ] 完整文档
- [ ] YCSB 性能报告

---

## 注意事项

1. **优先级排序**: 按照用户需求，CI/CD 和代码规范是最高优先级
2. **测试驱动**: 每个新特性都应该有对应的测试
3. **文档同步**: 代码变更时同步更新文档
4. **向后兼容**: 尽量保持 API 向后兼容
5. **性能关注**: 实现时始终关注性能影响
6. **安全第一**: 定期进行安全审计
7. **里程碑驱动**: 严格按照 18 周计划推进，每周检查进度

---

**最后更新**: 2025-11-26
**负责人**: @Genuineh, @copilot
