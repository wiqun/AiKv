# AiKv 项目待办事项 (TODO)

## 当前状态

已完成 v0.1.0 基础实现：
- ✅ RESP2 协议支持
- ✅ String 命令 (8个)
- ✅ JSON 命令 (7个)
- ✅ 基础 TCP 服务器
- ✅ 内存存储适配器
- ✅ 单元测试 (28个)

---

## 优先级 1 - 立即完成 (CI/CD & 代码规范)

### 1.1 GitHub Actions CI/CD 流水线
- [x] 创建 `.github/workflows/ci.yml` - 持续集成流水线
  - [x] 代码格式检查 (`cargo fmt --check`)
  - [x] 代码 lint 检查 (`cargo clippy`)
  - [x] 编译检查 (debug 和 release)
  - [x] 运行所有单元测试
  - [x] 运行集成测试
  - [x] 代码覆盖率报告
  - [x] 多平台构建 (Linux, macOS)
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
- [ ] `SELECT` - 切换数据库
- [ ] `DBSIZE` - 获取当前数据库键数量
- [ ] `FLUSHDB` - 清空当前数据库
- [ ] `FLUSHALL` - 清空所有数据库
- [ ] `SWAPDB` - 交换两个数据库
- [ ] `MOVE` - 移动键到其他数据库

**实现要点:**
- [ ] 修改 `StorageAdapter` 支持多数据库
- [ ] 添加数据库索引管理
- [ ] 更新连接状态跟踪当前数据库

### 3.2 Key 相关通用命令
- [ ] `KEYS` - 查找匹配模式的键
- [ ] `SCAN` - 迭代数据库中的键（游标）
- [ ] `RANDOMKEY` - 返回随机键
- [ ] `RENAME` - 重命名键
- [ ] `RENAMENX` - 仅当新键名不存在时重命名
- [ ] `TYPE` - 返回键的类型
- [ ] `DUMP` - 序列化键的值
- [ ] `RESTORE` - 反序列化并创建键
- [ ] `COPY` - 复制键 (Redis 6.2+)
- [ ] `MIGRATE` - 原子性迁移键到另一个实例

### 3.3 Key 过期相关命令
- [ ] `EXPIRE` - 设置键过期时间（秒）
- [ ] `EXPIREAT` - 设置键过期时间戳（秒）
- [ ] `PEXPIRE` - 设置键过期时间（毫秒）
- [ ] `PEXPIREAT` - 设置键过期时间戳（毫秒）
- [ ] `TTL` - 获取键剩余生存时间（秒）
- [ ] `PTTL` - 获取键剩余生存时间（毫秒）
- [ ] `PERSIST` - 移除键的过期时间
- [ ] `EXPIRETIME` - 获取键过期时间戳（秒）(Redis 7.0+)
- [ ] `PEXPIRETIME` - 获取键过期时间戳（毫秒）(Redis 7.0+)

**实现要点:**
- [ ] 实现 TTL 管理器（后台定期清理过期键）
- [ ] 修改 `StorageAdapter` 存储过期时间
- [ ] 实现懒惰删除和主动删除策略
- [ ] 添加过期键统计

### 3.4 Key 排序命令
- [ ] `SORT` - 排序列表、集合或有序集合
- [ ] `SORT_RO` - 只读排序 (Redis 7.0+)

### 3.5 Server 管理命令
- [ ] `PING` - ✅ 已实现
- [ ] `ECHO` - ✅ 已实现
- [ ] `INFO` - 服务器信息
- [ ] `CONFIG GET` - 获取配置参数
- [ ] `CONFIG SET` - 设置配置参数
- [ ] `CONFIG REWRITE` - 重写配置文件
- [ ] `COMMAND` - 获取命令详细信息
- [ ] `COMMAND COUNT` - 获取命令总数
- [ ] `COMMAND INFO` - 获取特定命令信息
- [ ] `TIME` - 返回服务器时间
- [ ] `CLIENT LIST` - 列出客户端连接
- [ ] `CLIENT SETNAME` - 设置客户端名称
- [ ] `CLIENT GETNAME` - 获取客户端名称
- [ ] `SHUTDOWN` - 关闭服务器
- [ ] `SAVE` - 同步保存数据到磁盘
- [ ] `BGSAVE` - 后台保存数据到磁盘
- [ ] `LASTSAVE` - 获取最后保存时间

---

## 优先级 4 - 存储引擎集成

### 4.1 AiDb 集成
- [ ] 研究 AiDb v0.1.0 API
- [ ] 实现真实的 AiDb 存储适配器
- [ ] 添加 AiDb 配置选项
- [ ] 性能测试和优化
- [ ] 添加 AiDb 集成测试

### 4.2 持久化支持
- [ ] 实现 RDB 快照持久化
- [ ] 实现 AOF 日志持久化
- [ ] 添加持久化配置选项
- [ ] 实现数据恢复功能
- [ ] 添加数据一致性检查

---

## 优先级 5 - 更多 Redis 数据类型

### 5.1 List 列表类型
- [ ] `LPUSH`, `RPUSH` - 左/右推入
- [ ] `LPOP`, `RPOP` - 左/右弹出
- [ ] `LLEN` - 列表长度
- [ ] `LRANGE` - 获取范围元素
- [ ] `LINDEX` - 获取指定索引元素
- [ ] `LSET` - 设置指定索引元素
- [ ] `LINSERT` - 插入元素
- [ ] `LREM` - 删除元素
- [ ] `LTRIM` - 修剪列表
- [ ] `BLPOP`, `BRPOP` - 阻塞弹出
- [ ] `LMOVE`, `BLMOVE` - 移动元素

### 5.2 Set 集合类型
- [ ] `SADD` - 添加成员
- [ ] `SREM` - 删除成员
- [ ] `SISMEMBER` - 检查成员存在
- [ ] `SMEMBERS` - 获取所有成员
- [ ] `SCARD` - 集合大小
- [ ] `SPOP` - 随机弹出成员
- [ ] `SRANDMEMBER` - 随机获取成员
- [ ] `SMOVE` - 移动成员
- [ ] `SUNION`, `SINTER`, `SDIFF` - 集合运算
- [ ] `SUNIONSTORE`, `SINTERSTORE`, `SDIFFSTORE` - 集合运算并存储

### 5.3 Hash 哈希类型
- [ ] `HSET`, `HSETNX` - 设置字段
- [ ] `HGET` - 获取字段值
- [ ] `HMSET`, `HMGET` - 批量操作
- [ ] `HDEL` - 删除字段
- [ ] `HEXISTS` - 检查字段存在
- [ ] `HLEN` - 字段数量
- [ ] `HKEYS`, `HVALS` - 获取所有键/值
- [ ] `HGETALL` - 获取所有字段和值
- [ ] `HINCRBY`, `HINCRBYFLOAT` - 增量操作
- [ ] `HSCAN` - 迭代字段

### 5.4 Sorted Set 有序集合类型
- [ ] `ZADD` - 添加成员
- [ ] `ZREM` - 删除成员
- [ ] `ZSCORE` - 获取成员分数
- [ ] `ZRANK`, `ZREVRANK` - 获取排名
- [ ] `ZRANGE`, `ZREVRANGE` - 范围查询
- [ ] `ZRANGEBYSCORE`, `ZREVRANGEBYSCORE` - 按分数范围
- [ ] `ZCARD` - 集合大小
- [ ] `ZCOUNT` - 统计范围内成员数
- [ ] `ZINCRBY` - 增加分数
- [ ] `ZUNION`, `ZINTER`, `ZDIFF` - 集合运算

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
- [ ] `EVAL` - 执行脚本
- [ ] `EVALSHA` - 执行缓存脚本
- [ ] `SCRIPT LOAD` - 加载脚本
- [ ] `SCRIPT EXISTS` - 检查脚本存在
- [ ] `SCRIPT FLUSH` - 清空脚本缓存
- [ ] `SCRIPT KILL` - 终止脚本

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
- [ ] 使用 `redis-benchmark` 测试
- [ ] 创建自定义性能测试套件
- [ ] 对比 Redis 性能基准
- [ ] 生成性能报告

### 7.2 性能优化
- [ ] 优化 RESP 协议解析性能
- [ ] 优化内存分配和使用
- [ ] 实现连接池优化
- [ ] 添加命令流水线（pipelining）支持
- [ ] 实现批量操作优化
- [ ] 添加缓存层

### 7.3 并发优化
- [ ] 分析并发瓶颈
- [ ] 优化锁策略
- [ ] 实现无锁数据结构
- [ ] 调整 Tokio runtime 配置

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

## 优先级 9 - 集群和高可用

### 9.1 主从复制
- [ ] 实现主从复制协议
- [ ] 支持增量复制
- [ ] 实现复制偏移量追踪
- [ ] 添加主从切换支持

### 9.2 哨兵模式 (Sentinel)
- [ ] 实现哨兵协议
- [ ] 自动故障转移
- [ ] 主节点选举

### 9.3 集群模式 (Cluster)
- [ ] 实现 Redis Cluster 协议
- [ ] 数据分片支持
- [ ] 节点间通信
- [ ] 集群重配置

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

## 版本规划

### v0.2.0 (近期)
- CI/CD 流水线
- 代码规范和格式化
- RESP3 支持
- DB 和 Key 基础命令
- 过期时间支持
- 集成测试套件

### v0.3.0 (中期)
- AiDb 完整集成
- List, Set, Hash 数据类型
- 持久化支持
- 性能优化

### v0.4.0 (长期)
- Sorted Set 数据类型
- 事务支持
- Pub/Sub
- 主从复制

### v1.0.0 (远期)
- 完整 Redis 兼容性
- 集群支持
- 生产级稳定性
- 完善的文档和工具

---

## 注意事项

1. **优先级排序**: 按照用户需求，CI/CD 和代码规范是最高优先级
2. **测试驱动**: 每个新特性都应该有对应的测试
3. **文档同步**: 代码变更时同步更新文档
4. **向后兼容**: 尽量保持 API 向后兼容
5. **性能关注**: 实现时始终关注性能影响
6. **安全第一**: 定期进行安全审计

---

**最后更新**: 2025-11-11
**负责人**: @Genuineh, @copilot
