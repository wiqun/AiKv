# AiKv 项目待办事项 (TODO)

> **最后更新**: 2025-11-27  
> **当前版本**: v0.1.0  
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
| **Key 命令** | 17 个 | ✅ 完成 |
| **Server 命令** | 9 个 | ✅ 完成 |
| **Lua 脚本命令** | 6 个 + 事务性 | ✅ 完成 |
| **Cluster 命令** | 17 个 (框架) | ✅ 完成 |
| **Cluster Bus** | 心跳 + 故障检测 | ✅ 完成 |
| **单元测试** | 96+ 个 | ✅ 全部通过 |

### 核心能力

- ✅ **双存储引擎**: 内存 + AiDb v0.4.1 LSM-Tree 持久化
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

### 🟠 P1: 核心命令补全

**Key 命令** (3 个待完成):
- [ ] `DUMP` - 序列化键的值 (返回 RDB 格式)
- [ ] `RESTORE` - 反序列化并创建键 (接受 RDB 格式 + 可选 TTL)
- [ ] `MIGRATE` - 原子性迁移键到另一个 Redis 实例

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

---

## 待完成项 - 中期计划

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

### 🟡 P2: Server 命令补全

- [x] `MONITOR` - 实时命令监控 (支持 Redis 桌面客户端 Profiler 功能)
- [ ] `CONFIG REWRITE` - 重写配置文件
- [ ] `COMMAND` - 获取命令详细信息
- [ ] `COMMAND COUNT` - 获取命令总数
- [ ] `COMMAND INFO` - 获取特定命令信息
- [ ] `SHUTDOWN` - 关闭服务器
- [ ] `SAVE` / `BGSAVE` - 保存数据到磁盘
- [ ] `LASTSAVE` - 获取最后保存时间

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

### 🔵 P3: 文档和工具

- [ ] 添加架构设计文档
- [ ] 添加性能调优指南
- [ ] 添加故障排查指南
- [ ] 生成 API 文档（rustdoc）
- [ ] 创建 Docker 开发环境
- [ ] 更多客户端示例
- [ ] 最佳实践文档

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

- [x] `cluster` feature 和 AiDb v0.4.1 集成
- [x] `MultiRaftNode` / `MetaRaftNode` 封装
- [x] 3 节点启动验证

### v0.3.0 - 槽路由 (周 3-4)

- [ ] 16384 槽映射
- [ ] `-MOVED` 重定向
- [ ] `Router::key_to_slot()` 集成

### v0.4.0 - CLUSTER 命令完善 (周 5-6)

- [ ] 完善集群信息命令
- [ ] 节点管理命令
- [ ] Slot 管理命令

### v0.5.0 - 在线迁移 (周 7-9)

- [x] `CLUSTER GETKEYSINSLOT` ✅
- [x] `CLUSTER SETSLOT ... MIGRATING/IMPORTING` ✅
- [x] `-ASK` 重定向 ✅
- [ ] `MIGRATE` 命令 (网络层)

### v0.6.0 - 高可用 (周 10-12)

- [x] `CLUSTER REPLICATE` ✅
- [x] `CLUSTER FAILOVER` ✅
- [x] `READONLY/READWRITE` ✅
- [x] `CLUSTER REPLICAS` ✅

### v0.8.0 - 高级功能 (周 13-15)

- [ ] 事务 (MULTI/EXEC/WATCH)
- [ ] Pub/Sub
- [ ] 跨槽支持

### v0.9.0 - 测试优化 (周 16-17)

- [ ] 极限压测
- [ ] 官方测试套件
- [ ] 性能调优

### v1.0.0 🎯 正式发布 (2026.03.31)

- [ ] Docker 官方镜像
- [ ] Helm Chart
- [ ] Prometheus Exporter
- [ ] 完整文档
- [ ] YCSB 性能报告

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

**Key (17 个)**: KEYS, SCAN, RANDOMKEY, RENAME, RENAMENX, TYPE, COPY, EXPIRE, EXPIREAT, PEXPIRE, PEXPIREAT, TTL, PTTL, PERSIST, EXPIRETIME, PEXPIRETIME, DEL/EXISTS

**Server (9 个)**: PING, ECHO, INFO, CONFIG GET/SET, TIME, CLIENT LIST/SETNAME/GETNAME

**Lua 脚本 (6 个)**: EVAL, EVALSHA, SCRIPT LOAD/EXISTS/FLUSH/KILL (含事务性回滚)

### 存储引擎
- ✅ 双存储引擎: 内存 + AiDb v0.4.1 LSM-Tree
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
