# AiKv 项目待办事项 (TODO)

> **最后更新**: 2026-01-07  
> **当前版本**: v0.2.0-dev  
> **目标版本**: v1.0.0 (2026.03.31)

---

## 📋 目录

1. [当前状态概览](#当前状态概览)
2. [待完成项 - 近期优先](#待完成项---近期优先)
3. [待完成项 - 中期计划](#待完成项---中期计划)
4. [待完成项 - 长期规划](#待完成项---长期规划)
5. [版本路线图](#版本路线图)
6. [已归档完成项](#已归档完成项)

---

## 当前状态概览

### ✅ 已完成功能统计

| 类别 | 数量 | 状态 |
|------|------|------|
| **协议支持** | RESP2 + RESP3 | ✅ 完成 |
| **String 命令** | 8 个 | ✅ 完成 |
| **JSON 命令** | 7 个 | ✅ 完成 |
| **List 命令** | 12 个 | ✅ 完成 |
| **Hash 命令** | 14 个 | ✅ 完成 |
| **Set 命令** | 13 个 | ✅ 完成 |
| **Sorted Set 命令** | 12 个 | ✅ 完成 |
| **Database 命令** | 6 个 | ✅ 完成 |
| **Key 命令** | 20 个 | ✅ 完成 |
| **Server 命令** | 16 个 | ✅ 完成 |
| **Lua 脚本命令** | 6 个 + 事务性 | ✅ 完成 |
| **Cluster 命令** | 17 个 (框架) | ✅ 完成 |
| **Cluster Bus** | 心跳 + 故障检测 | ✅ 完成 |
| **单元测试** | 177+ 个 | ✅ 全部通过 |

### 核心能力

- ✅ **双存储引擎**: 内存 + AiDb v0.6.1 LSM-Tree 持久化
- ✅ **多数据库**: 16 个数据库，完整 TTL 支持
- ✅ **架构重构**: 存储层 100% 完成，代码减少 67%
- ✅ **CI/CD**: GitHub Actions 流水线 + 安全检查
- ✅ **集群框架**: 阶段 A-D 完成 (90%)
- ✅ **Cluster Bus**: 心跳检测 + 故障检测 (AiDb 胶水层)

---

## 待完成项 - 近期优先

### ✅ 集群 Multi-Raft 集成 (v0.2.0) - 已完成

> 预计时间: 2 周
> 完成时间: 2025-11-26

- [x] 添加 `cluster` feature (`aidb/raft-cluster`)
- [x] 封装 `MultiRaftNode` 初始化
- [x] 封装 `MetaRaftNode` 初始化
- [x] 实现 3 节点启动和验证

**实现说明:**
- `ClusterNode` 现在封装了 AiDb 的 `MultiRaftNode` 和 `MetaRaftNode`
- 支持通过 `initialize()` 方法初始化 Multi-Raft 节点
- 支持通过 `bootstrap_meta_cluster()` 方法引导 MetaRaft 集群
- 支持通过 `start_cluster()` 方法创建多个 Raft 组进行数据分片
- 新增 `ClusterConfig` 配置结构
- 新增完整的单元测试验证 3 节点集群功能

### ✅ P0: Cluster Bus 实现 (AiDb 胶水层) - 已完成

> 预计时间: 1 周
> 完成时间: 2025-11-27
> 注：由于 AiDb 已实现 OpenRaft，不需要独立实现 gossip 协议。
> 心跳和故障检测通过对接 AiDb API 完成，保持胶水层设计理念。

- [x] ~~实现节点间 gossip 协议~~ (不需要 - AiDb 的 Raft 共识处理集群元数据同步)
- [x] 对接 AiDb 心跳检测 API (`MetaRaftNode` leader heartbeat)
- [x] 对接 AiDb 故障检测 API (`NodeStatus::Online/Offline` + election timeout)

**实现说明:**
- `ClusterBus` 模块封装了 AiDb 的 `MetaRaftNode` 和 `NodeStatus`
- 支持通过 `is_leader()` 和 `get_leader()` 方法检测领导者心跳
- 支持基于 `NodeStatus::Online/Offline` 的节点状态追踪
- 支持基于选举超时 (`election_timeout`) 的故障检测
- 新增 `ClusterBusConfig` 配置结构用于健康检查参数
- 新增 `NodeHealthInfo` 和 `NodeHealthStatus` 用于节点健康状态管理
- 完整的单元测试覆盖

### ✅ P0: Redis 集群协议兼容性 - Multi-Raft 方案 (已实现)

> 状态: **已完成** - 使用 Multi-Raft 替代 Gossip 协议
> 完成时间: 2025-12-08
> 详见: [CLUSTER_BUS_ANALYSIS.md](docs/CLUSTER_BUS_ANALYSIS.md)

**解决方案: 使用 AiDb Multi-Raft 实现集群状态同步**

这是比 Redis Gossip 协议更优雅的方案（强一致性 vs 最终一致性）：

- [x] ✅ `cluster_enabled:1` 在 INFO 中正确报告
- [x] ✅ CLUSTER 命令 (MEET, ADDSLOTS, NODES 等) 已实现
- [x] ✅ 本地集群状态存储 (`ClusterState`)
- [x] ✅ `MetaRaftClient` 封装 AiDb MetaRaftNode API
- [x] ✅ `CLUSTER MEET` 通过 Raft 共识提议节点加入
- [x] ✅ `CLUSTER FORGET` 通过 Raft 共识提议节点移除
- [x] ✅ 节点心跳任务 (通过 OpenRaft 内置机制)
- [x] ✅ `get_cluster_view()` 从 MetaRaft 读取集群状态
- [x] ✅ `ClusterCommands` 集成 `MetaRaftClient`

**实现说明:**

新增 `MetaRaftClient` 模块 (`src/cluster/metaraft.rs`)：
- `propose_node_join()` - 通过 Raft 提议添加节点
- `propose_node_leave()` - 通过 Raft 提议移除节点
- `get_cluster_view()` - 从 Raft 状态机读取集群视图
- `start_heartbeat()` - 启动心跳任务
- `is_leader()` / `get_leader()` - 查询 Raft 领导者

集成到 `ClusterCommands` (`src/cluster/commands.rs`)：
- `with_meta_raft_client()` - 使用 MetaRaftClient 创建 ClusterCommands
- `set_meta_raft_client()` - 设置 MetaRaftClient
- `meta_raft_client()` - 获取 MetaRaftClient 引用
- `meet()` - 优先使用 MetaRaftClient 添加节点
- `forget()` - 优先使用 MetaRaftClient 移除节点

**核心优势:**
- ❌ **不需要端口 16379** - 无需 gossip 协议
- ✅ **强一致性** - Raft 共识优于 gossip 的最终一致性
- ✅ **复用现有基础设施** - 使用 AiDb 的 Multi-Raft
- ✅ **100% Redis 命令兼容** - 客户端无感知

**架构图:**
```
Redis Client (redis-cli)
    │
    ▼ CLUSTER MEET / FORGET / NODES
    │
ClusterCommands (with MetaRaftClient)
    │
    ▼ propose_node_join() / propose_node_leave()
    │
MetaRaftClient  ←─────────────────────────┐
    │                                      │
    ▼ add_node() / remove_node()          │ Raft 日志复制
    │                                      │
AiDb MetaRaftNode (Group 0) ──────────────┘
```

### 🟠 P1: 核心命令补全

**Key 命令** (已完成):
- [x] `DUMP` - 序列化键的值 (返回 RDB 格式) ✅
- [x] `RESTORE` - 反序列化并创建键 (接受 RDB 格式 + 可选 TTL) ✅
- [x] `MIGRATE` - 原子性迁移键到另一个 Redis 实例 ✅

**Key 排序命令** (2 个):
- [ ] `SORT` - 排序列表、集合或有序集合
- [ ] `SORT_RO` - 只读排序 (Redis 7.0+)

**List 命令** (2 个待完成):
- [x] `LINSERT` - 插入元素 ✅
- [ ] `BLPOP`, `BRPOP` - 阻塞弹出
- [x] `LMOVE` - 移动元素 ✅
- [ ] `BLMOVE` - 阻塞移动元素

**Set 命令** (1 个待完成):
- [ ] `SMOVE` - 移动成员

**Hash 命令** (已完成):
- [x] `HSCAN` - 迭代字段 ✅
- [x] `HMSET` - 批量设置字段 ✅

**Sorted Set 命令** (3 个待完成):
- [ ] `ZUNION`, `ZINTER`, `ZDIFF` - 集合运算

### 🔴 P0: 自动故障转移 (v0.7.0 核心)

> 这是生产环境必备功能，使 AiKv 成为真正的高可用系统

- [ ] 节点健康检测定时任务 (基于 OpenRaft heartbeat)
- [ ] 主节点失联时自动触发 failover
- [ ] 副本自动提升为新主节点
- [ ] 配置 `cluster-node-timeout` 参数 (默认 15s)
- [ ] 故障转移事件日志和通知
- [ ] 测试：杀进程 → 观察自动切换 < 10s

### 🔴 P0: WAIT 命令实现

> 支持同步复制确认，确保数据安全

- [ ] `WAIT numreplicas timeout` - 等待指定数量副本确认
- [ ] 返回成功同步的副本数量
- [ ] 超时处理

### 🔴 P0: Lua 脚本增强 (Key 级锁 + 并行化)

> 目标: 同 key 串行化，不同 keys 并行化，提高并发性能
> 方案: 利用 AiDb 的 WriteBatch 先写入缓冲区不刷入磁盘，最后原子批量刷入

**已完成功能:**
- [x] 写缓冲区机制 (ScriptTransaction)
- [x] 自动回滚 (脚本失败时丢弃缓冲区)
- [x] AiDb WriteBatch 原子批量提交
- [x] 读自己的写 (read-your-own-writes)

**待实现功能:**

#### 1. Key 级锁机制
- [ ] 实现 `KeyLockManager` - 管理 key 级别的读写锁
- [ ] EVAL/EVALSHA 执行前根据 KEYS 参数加锁
- [ ] 同一 key 的脚本串行执行
- [ ] 不同 keys 的脚本可以并行执行
- [ ] 锁超时机制 (防止死锁)
- [ ] 锁等待队列 (公平调度)

#### 2. Lua 脚本命令扩展
当前只支持 GET/SET/DEL/EXISTS，需要扩展支持：
- [ ] String: INCR, DECR, INCRBY, DECRBY, INCRBYFLOAT, APPEND, STRLEN
- [ ] Hash: HGET, HSET, HDEL, HGETALL, HMGET, HMSET, HINCRBY, HEXISTS, HLEN
- [ ] List: LPUSH, RPUSH, LPOP, RPOP, LLEN, LRANGE, LINDEX
- [ ] Set: SADD, SREM, SMEMBERS, SISMEMBER, SCARD
- [ ] ZSet: ZADD, ZREM, ZSCORE, ZRANK, ZRANGE, ZCARD

#### 3. 复杂类型事务支持
- [ ] 扩展 BatchOp 支持复杂类型 (List, Hash, Set, ZSet)
- [ ] 复杂类型的读写缓冲
- [ ] 批量提交时序列化处理

---

## 待完成项 - 中期计划

### 🟠 P1: 基础数据类型命令补全

> 目标: 达到 Redis 核心命令完整覆盖

#### String 命令补全 (12 个待实现)
- [ ] `INCR` - 键值加 1
- [ ] `DECR` - 键值减 1
- [ ] `INCRBY` - 键值加指定整数
- [ ] `DECRBY` - 键值减指定整数
- [ ] `INCRBYFLOAT` - 键值加指定浮点数
- [ ] `GETRANGE` - 获取子字符串
- [ ] `SETRANGE` - 覆盖子字符串
- [ ] `GETEX` - 获取并设置过期时间
- [ ] `GETDEL` - 获取并删除
- [ ] `SETNX` - 不存在时设置 (等同于 SET NX)
- [ ] `SETEX` - 设置带过期时间 (等同于 SET EX)
- [ ] `PSETEX` - 设置带毫秒过期时间

#### List 命令补全 (5 个待实现)
- [ ] `LPOS` - 查找元素位置
- [ ] `LMPOP` - 从多个列表弹出
- [ ] `LMOVE` - 列表间移动元素 (部分已实现)
- [ ] `BLPOP`, `BRPOP` - 阻塞弹出
- [ ] `BLMOVE` - 阻塞移动

#### Set 命令补全 (2 个待实现)
- [ ] `SSCAN` - 迭代集合成员
- [ ] `SMOVE` - 移动成员到另一个集合

#### Sorted Set 命令补全 (10 个待实现)
- [ ] `ZSCAN` - 迭代有序集合成员
- [ ] `ZPOPMIN`, `ZPOPMAX` - 弹出最小/最大分数成员
- [ ] `BZPOPMIN`, `BZPOPMAX` - 阻塞弹出
- [ ] `ZRANGEBYLEX`, `ZREVRANGEBYLEX` - 按字典序范围查询
- [ ] `ZLEXCOUNT` - 字典序范围计数
- [ ] `ZMPOP` - 从多个有序集合弹出
- [ ] `ZUNION`, `ZINTER`, `ZDIFF` - 集合运算

### 🟡 P2: 事务支持 (v0.8.0)

- [ ] `MULTI` - 开始事务
- [ ] `EXEC` - 执行事务
- [ ] `DISCARD` - 取消事务
- [ ] `WATCH` - 监视键
- [ ] `UNWATCH` - 取消监视

### 🟡 P2: Pub/Sub 发布订阅

- [ ] `PUBLISH` - 发布消息
- [ ] `SUBSCRIBE` - 订阅频道
- [ ] `UNSUBSCRIBE` - 取消订阅
- [ ] `PSUBSCRIBE` - 模式订阅
- [ ] `PUNSUBSCRIBE` - 取消模式订阅
- [ ] `PUBSUB` - 查询订阅信息

### 🟡 P2: Stream 流数据类型

- [ ] `XADD` - 添加消息
- [ ] `XREAD` - 读取消息
- [ ] `XRANGE` - 范围查询
- [ ] `XLEN` - 流长度
- [ ] `XDEL` - 删除消息
- [ ] `XTRIM` - 修剪流
- [ ] Consumer Groups 支持

### ✅ P2: Server 命令补全 - 已完成

> 完成时间: 2025-12-01

- [x] `MONITOR` - 实时命令监控 (支持 Redis 桌面客户端 Profiler 功能)
- [x] `CONFIG REWRITE` - 重写配置文件 ✅
- [x] `COMMAND` - 获取命令详细信息 ✅
- [x] `COMMAND COUNT` - 获取命令总数 ✅
- [x] `COMMAND INFO` - 获取特定命令信息 ✅
- [x] `SHUTDOWN` - 关闭服务器 ✅
- [x] `SAVE` / `BGSAVE` - 保存数据到磁盘 ✅
- [x] `LASTSAVE` - 获取最后保存时间 ✅

**实现说明:**
- `COMMAND` 命令返回所有支持命令的详细信息（名称、参数数量、标志、键位置等）
- `COMMAND COUNT` 返回支持的命令总数
- `COMMAND INFO` 返回指定命令的详细信息
- `COMMAND DOCS` 返回命令文档
- `COMMAND GETKEYS` 从完整命令中提取键名
- `COMMAND HELP` 显示帮助信息
- `CONFIG REWRITE` 重写配置文件（存根实现，返回 OK）
- `SAVE` 同步保存数据到磁盘（更新 last_save 时间戳）
- `BGSAVE` 异步保存数据到磁盘（更新 last_save 时间戳）
- `LASTSAVE` 返回上次成功保存的 Unix 时间戳
- `SHUTDOWN` 请求关闭服务器（支持 NOSAVE/SAVE/NOW/FORCE/ABORT 选项）

---

## 待完成项 - 长期规划

### ✅ P3: 监控和可观测性 - 已完成

> 完成时间: 2025-11-27

**日志增强**:
- [x] 添加结构化日志 (`LogFormat::Json` 支持)
- [x] 实现日志级别动态调整 (`CONFIG SET loglevel` 命令)
- [x] 添加慢查询日志 (`SLOWLOG GET/LEN/RESET` 命令)
- [x] 实现日志轮转和归档 (`LogConfig` 配置结构)

**Metrics 指标**:
- [x] 集成 Prometheus metrics (`Metrics::export_prometheus()`)
- [x] 添加命令执行统计 (`CommandMetrics`)
- [x] 添加连接统计 (`ConnectionMetrics`)
- [x] 添加内存使用统计 (`MemoryMetrics`)

**追踪 (Tracing)**:
- [x] 集成分布式追踪 (`TracingConfig` + OpenTelemetry 配置)
- [x] 添加请求追踪 (`CommandSpan` + `TraceContext`)

**实现说明:**
- 新增 `src/observability/` 模块包含 `logging.rs`, `metrics.rs`, `tracing_setup.rs`
- 支持 `CONFIG SET loglevel <level>` 动态调整日志级别
- 支持 `CONFIG SET slowlog-log-slower-than <us>` 设置慢查询阈值
- 支持 `CONFIG SET slowlog-max-len <len>` 设置慢查询日志最大长度
- 实现 Redis 兼容的 `SLOWLOG GET/LEN/RESET/HELP` 命令
- 实现 Prometheus 文本格式指标导出
- 实现 W3C traceparent 格式的分布式追踪上下文
- 完整的单元测试覆盖

### ✅ P3: 文档和工具 - 已完成

> 完成时间: 2025-12-02

- [x] 添加架构设计文档 (`docs/ARCHITECTURE.md`)
- [x] 添加性能调优指南 (`docs/PERFORMANCE_TUNING.md`)
- [x] 添加故障排查指南 (`docs/TROUBLESHOOTING.md`)
- [x] 生成 API 文档（rustdoc）- 通过 `cargo doc` 生成
- [x] 创建 Docker 开发环境 (`Dockerfile`, `docker-compose.yml`, `docker-compose.dev.yml`)
- [x] 更多客户端示例 (`examples/data_types_example.rs`, `examples/cluster_example.rs`, `examples/pipeline_example.rs`)
- [x] 最佳实践文档 (`docs/BEST_PRACTICES.md`)

**实现说明:**
- 架构设计文档详细描述了系统架构、核心组件、数据流和设计模式
- 性能调优指南覆盖系统层面、应用层面和集群优化建议
- 故障排查指南提供完整的问题诊断流程和解决方案
- Docker 环境支持生产部署和开发调试两种场景
- 新增客户端示例展示所有数据类型、集群操作和管道性能优化

### 🔵 P3: 代码质量

- [ ] 添加模糊测试 (fuzzing tests)
- [ ] 多 Rust 版本测试 (stable, beta, nightly)
- [ ] 添加过期键统计
- [ ] 性能对比分析（重构前后）

### 🔵 P3: 后续优化

- [ ] 考虑为高频操作添加专门的优化路径
- [ ] 评估是否需要批量操作接口（如 `batch_get`, `batch_set`）
- [ ] 考虑引入事务支持的存储接口
- [ ] 优化锁粒度和并发性能

### ✅ P3: 集群高级特性 (Future Enhancements) - 已完成

> 完成时间: 2025-12-15

- [x] **动态 MetaRaft 成员变更** (Dynamic MetaRaft Membership Changes) - **已实现**
  - **解决方案**: 实现了完整的 learner → voter 工作流
  - **新增 API**:
    - `ClusterNode::add_meta_learner()` - 添加节点为 MetaRaft learner
    - `ClusterNode::promote_meta_voter()` - 提升 learner 为 voter
    - `ClusterNode::change_meta_membership()` - 直接变更 MetaRaft 成员
  - **新增 Redis 命令**:
    - `CLUSTER METARAFT ADDLEARNER node_id addr` - 添加 learner
    - `CLUSTER METARAFT PROMOTE node_id [node_id ...]` - 提升为 voter
    - `CLUSTER METARAFT MEMBERS` - 查看 voters 和 learners
  - **实现步骤**:
    1. ✅ Bootstrap 节点以单节点模式初始化 MetaRaft
    2. ✅ 其他主节点启动后可作为 learner 加入
    3. ✅ 通过 CLUSTER METARAFT PROMOTE 命令将 learner 提升为 voter
    4. ✅ 最终所有主节点都是 MetaRaft voters，可以提议 Raft 变更
  - **参考**: AiDb v0.5.2 Multi-Raft API (OpenRaft `add_learner()` + `change_membership()`)
  - **测试**: 新增 5 个测试用例验证完整工作流
  - **优先级**: ✅ **已完成** - 解除多主节点功能阻塞

---

## 版本路线图

### v0.1.0 ✅ 已完成

- ✅ RESP2/RESP3 协议完整支持
- ✅ 100+ 命令实现
- ✅ 双存储引擎
- ✅ 存储层架构重构
- ✅ Lua 脚本 + 事务性
- ✅ 集群命令框架 (90%)

### v0.2.0 ✅ Multi-Raft 集成 (已完成)

- [x] `cluster` feature 和 AiDb v0.5.0 集成
- [x] `MultiRaftNode` / `MetaRaftNode` 封装
- [x] 3 节点启动验证

### v0.3.0 - 槽路由 (周 3-4) - 已完成

- [x] 16384 槽映射 ✅
- [x] `-MOVED` 重定向 ✅
- [x] `Router::key_to_slot()` 集成 ✅

**实现说明:**
- 在 `ClusterCommands` 中添加了 `check_key_slot()` 和 `check_keys_slot()` 方法用于检查键的槽是否属于当前节点
- 在 `CommandExecutor::execute()` 中集成了槽路由检查，所有数据命令执行前会检查键的归属
- 对于单键命令 (GET, SET, LPUSH 等)，检查该键的槽是否属于当前节点
- 对于多键命令 (MGET, MSET, SUNION 等)，检查所有键是否在同一个槽，并且槽属于当前节点
- 如果键不属于当前节点，返回 `-MOVED slot ip:port` 错误，告诉客户端正确的节点地址
- 如果多键命令的键在不同槽，返回 `-CROSSSLOT` 错误

### v0.4.0 - CLUSTER 命令完善 (周 5-6) - 已完成 ✅

- [x] 完善集群信息命令 ✅
- [x] 节点管理命令 ✅
- [x] Slot 管理命令 ✅

### v0.5.0 - 在线迁移 (周 7-9) - 大部分完成

- [x] `CLUSTER GETKEYSINSLOT` ✅
- [x] `CLUSTER SETSLOT ... MIGRATING/IMPORTING` ✅
- [x] `-ASK` 重定向 ✅
- [ ] `MIGRATE` 命令 (完整网络层迁移)

### v0.6.0 - 高可用 (周 10-12) - 已完成 ✅

- [x] `CLUSTER REPLICATE` ✅
- [x] `CLUSTER FAILOVER` ✅
- [x] `READONLY/READWRITE` ✅
- [x] `CLUSTER REPLICAS` ✅

### v0.7.0 - 功能完善 (周 13-14) ⭐ 当前阶段

> **目标**: 让功能更加完善且健全

#### 🔴 P0: 核心稳定性 (必须)
- [ ] 自动故障转移 (Auto Failover) - 节点失联时自动提升副本
- [ ] `MIGRATE` 命令完整实现 - 跨节点键迁移网络传输
- [ ] `WAIT` 命令 - 同步复制确认

#### 🟠 P1: 阻塞命令 (重要)
- [ ] `BLPOP`, `BRPOP` - 阻塞列表弹出
- [ ] `BLMOVE` - 阻塞列表移动
- [ ] 连接级阻塞队列管理

#### 🟡 P2: 命令补全 (完整性)
- [ ] `ZUNION`, `ZINTER`, `ZDIFF` - Sorted Set 集合运算
- [ ] `SMOVE` - Set 成员移动
- [ ] `SORT`, `SORT_RO` - 排序命令

### v0.8.0 - 事务和 Pub/Sub (周 15-16)

#### 事务支持
- [ ] `MULTI` - 开始事务
- [ ] `EXEC` - 执行事务
- [ ] `DISCARD` - 丢弃事务
- [ ] `WATCH` / `UNWATCH` - 乐观锁

#### 发布订阅
- [ ] `PUBLISH`, `SUBSCRIBE`, `UNSUBSCRIBE`
- [ ] `PSUBSCRIBE`, `PUNSUBSCRIBE` - 模式订阅
- [ ] `PUBSUB` - 订阅信息查询
- [ ] 集群模式跨节点消息转发

### v0.9.0 - 测试和优化 (周 17-18)

- [ ] 极限压测和性能调优
- [ ] Redis 官方测试套件兼容
- [ ] YCSB 基准测试报告
- [ ] 故障转移场景测试
- [ ] 网络分区恢复测试

### v1.0.0 🎯 正式发布 (2026.03.31)

- [ ] Docker 官方镜像发布
- [ ] Helm Chart for Kubernetes
- [ ] Prometheus Exporter 完善
- [ ] 完整文档和运维指南
- [ ] 性能对比报告

#### v1.0.0 性能目标

| 指标 | 目标值 |
|------|--------|
| 3 节点聚合吞吐 (50%读50%写) | ≥ 420k ops/sec (总和) |
| 单节点吞吐 | ≥ 220k ops/sec |
| 槽迁移速度 (1000 槽) | < 25 秒 |
| 自动故障转移时间 | < 10 秒 |
| 副本延迟 (99.9%) | < 50 ms |

---

## 已归档完成项

<details>
<summary><b>📦 v0.1.0 已完成功能 (点击展开)</b></summary>

### 协议支持
- ✅ RESP2/RESP3 完整实现
- ✅ `HELLO` 命令 (协议版本切换)
- ✅ 属性 (Attributes) 支持
- ✅ 流式响应

### 数据类型命令

**String (8 个)**: GET, SET, DEL, EXISTS, MGET, MSET, STRLEN, APPEND

**JSON (7 个)**: JSON.GET, JSON.SET, JSON.DEL, JSON.TYPE, JSON.STRLEN, JSON.ARRLEN, JSON.OBJLEN

**List (10 个)**: LPUSH, RPUSH, LPOP, RPOP, LLEN, LRANGE, LINDEX, LSET, LREM, LTRIM

**Hash (12 个)**: HSET, HSETNX, HGET, HMGET, HDEL, HEXISTS, HLEN, HKEYS, HVALS, HGETALL, HINCRBY, HINCRBYFLOAT

**Set (13 个)**: SADD, SREM, SISMEMBER, SMEMBERS, SCARD, SPOP, SRANDMEMBER, SUNION, SINTER, SDIFF, SUNIONSTORE, SINTERSTORE, SDIFFSTORE

**Sorted Set (12 个)**: ZADD, ZREM, ZSCORE, ZRANK, ZREVRANK, ZRANGE, ZREVRANGE, ZRANGEBYSCORE, ZREVRANGEBYSCORE, ZCARD, ZCOUNT, ZINCRBY

### 系统命令

**Database (6 个)**: SELECT, DBSIZE, FLUSHDB, FLUSHALL, SWAPDB, MOVE

**Key (20 个)**: KEYS, SCAN, RANDOMKEY, RENAME, RENAMENX, TYPE, COPY, EXPIRE, EXPIREAT, PEXPIRE, PEXPIREAT, TTL, PTTL, PERSIST, EXPIRETIME, PEXPIRETIME, DEL/EXISTS, DUMP, RESTORE, MIGRATE

**Server (9 个)**: PING, ECHO, INFO, CONFIG GET/SET, TIME, CLIENT LIST/SETNAME/GETNAME

**Lua 脚本 (6 个)**: EVAL, EVALSHA, SCRIPT LOAD/EXISTS/FLUSH/KILL (含事务性回滚)

### 存储引擎
- ✅ 双存储引擎: 内存 + AiDb v0.5.0 LSM-Tree
- ✅ 多数据库支持 (16 个)
- ✅ TTL/过期管理
- ✅ 完整数据类型序列化

### CI/CD 和代码质量
- ✅ GitHub Actions CI/CD
- ✅ 安全检查 (cargo-audit, cargo-deny)
- ✅ 代码格式化 (rustfmt, clippy)
- ✅ 集成测试和性能基准测试

</details>

<details>
<summary><b>📦 集群框架已完成项 (阶段 A-D, 90%)</b></summary>

### 阶段 A: 基础集成 ✅
- ✅ `cluster` feature 框架
- ✅ `src/cluster/mod.rs` 模块
- ✅ `ClusterNode` 封装
- ✅ `CLUSTER KEYSLOT` 命令
- ✅ `-MOVED` 重定向逻辑

### 阶段 B: 集群命令 ✅
- ✅ `CLUSTER INFO/NODES/SLOTS/MYID`
- ✅ `CLUSTER MEET/FORGET`
- ✅ `CLUSTER ADDSLOTS/DELSLOTS`
- ✅ `CLUSTER SETSLOT`

### 阶段 C: 槽迁移 ✅
- ✅ `CLUSTER GETKEYSINSLOT`
- ✅ `CLUSTER COUNTKEYSINSLOT`
- ✅ `CLUSTER SETSLOT ... MIGRATING/IMPORTING`
- ✅ `-ASK` 重定向逻辑
- ✅ `MigrationProgress` 进度追踪
- ✅ `KeyScanner` 可插拔键扫描接口

### 阶段 D: 高可用 ✅
- ✅ `CLUSTER REPLICATE`
- ✅ `CLUSTER FAILOVER` (FORCE/TAKEOVER 模式)
- ✅ `READONLY/READWRITE`
- ✅ `CLUSTER REPLICAS/SLAVES`
- ✅ `FailoverMode` 枚举
- ✅ `ClusterState` 副本管理

</details>

<details>
<summary><b>📦 存储层架构重构 (100% 完成)</b></summary>

### 重构成果
- ✅ 47/47 命令迁移完成 (String 2, List 10, Hash 12, Set 13, ZSet 10)
- ✅ 代码减少 67% (2649 行 → 878 行)
- ✅ 移除 50+ 命令特定方法
- ✅ 清晰的存储层/命令层分离
- ✅ 完整 API 文档 (rustdoc)

### 新架构接口
- `get_value()` / `set_value()`
- `update_value()` / `delete_and_get()`
- `StoredValue` 类型安全访问器
- 数据库级操作 (flush, swap, size)

详见: `docs/ARCHITECTURE_REFACTORING.md`

</details>

---

## 参考文档

| 文档 | 说明 |
|------|------|
| [ARCHITECTURE_REFACTORING.md](docs/ARCHITECTURE_REFACTORING.md) | 存储层架构重构详情 |
| [AIDB_CLUSTER_API_REFERENCE.md](docs/AIDB_CLUSTER_API_REFERENCE.md) | 集群 API 参考 |
| [CLUSTER_BUS_ANALYSIS.md](docs/CLUSTER_BUS_ANALYSIS.md) | **集群总线协议分析 - 初始化问题根因** |
| [LUA_TRANSACTION_DESIGN.md](docs/LUA_TRANSACTION_DESIGN.md) | Lua 脚本事务设计 |
| [CHANGELOG.md](CHANGELOG.md) | 版本变更记录 |

---

## 注意事项

1. **优先级排序**: Multi-Raft 集成是当前最高优先级
2. **测试驱动**: 每个新特性都应有对应测试
3. **文档同步**: 代码变更时同步更新文档
4. **向后兼容**: 尽量保持 API 向后兼容
5. **性能关注**: 实现时关注性能影响

---

**负责人**: @Genuineh, @copilot
