---
name: aikv-design
description: AiKv 核心设计决策与架构权衡 (Why). 解释技术选型理由、放弃的替代方案 (Trade-offs)、YAGNI 刻意不实现、已知限制与决策总表.
---

# AiKv 设计决策

本文阐述 AiKv 各核心子系统的 **设计决策与架构权衡 (Why)**: 技术选型理由、放弃的替代方案 (Trade-offs)、YAGNI 刻意不实现与已知限制.

系统分层与工作原理 (How) 见 [ARCHITECTURE.md](../ARCHITECTURE.md); 存储与共识内核底层实现见 [AiDb 设计决策](https://github.com/wiqun/AiDb/blob/main/docs/design.md); 各模块源码地图见 [docs/modules/](modules/).

---

## 1. 产品形态与横切取舍

### 为什么是独立网络服务, 而非嵌入式存储库?

AiKv 定位为纯 Rust **Redis RESP 兼容的高性能分布式 KV 网络服务 (bin + lib)** (基于 Tokio 异步运行时).
- **分层解耦**: 底层同步 LSM 存储引擎与 MultiRaft 分布式共识委托给 [AiDb](https://github.com/wiqun/AiDb);
- **协议聚焦**: AiKv 专注于对外网络暴露、RESP2/3 协议编解码、Redis 数据类型到底层 KV 的编码映射、命令路由分发、细粒度并发锁及 Redis Cluster 客户端协议交互 (MOVED/ASK).
- **独立演进**: 使得协议层与网络服务层可独立演进与测试, 存储内核保持高内聚与可复用.

### 为什么选择 Tokio Async + `spawn_blocking`?

- **Async 网络接入**: 高并发 TCP 连接与 Redis Pipeline 流式批处理在单个 Tokio 连接上轻量级调度, 内存占用极低且并发吞吐高.
- **同步存储内核桥接**: 底层 AiDb 引擎为同步磁盘 IO 与内存结构 (`DB::put/get/scan`), 在 `AiDbEngine` 适配层中通过 `tokio::task::spawn_blocking` 进行线程池桥接, 彻底避免在协议层重写复杂的异步 LSM 状态机.
- *Trade-off*: 虽带来微小的线程池切换开销, 但保证了存储内核的极致简单与确定性.

### 为什么采用 Cargo Feature 严格解耦?

| Feature | 默认状态 | 设计理由与启用内容 |
| :--- | :--- | :--- |
| **(none)** | ❌（需 `--no-default-features`） | 极简单机 RESP 服务, 不启用任何可选 feature, 产生最小二进制体积 |
| **`cluster`** | ❌ 默认关闭 | 启用 `src/cluster/`、`ClusterDataAdapter` 与 `aidb/cluster`, 支持 Redis Cluster 协议与分布式 Raft 组 |
| **`monitoring`** | ❌ 默认关闭 | 启用 `MetricsServer` (`/health` HTTP 服务) 与 OTel 指标/链路跟踪, 生产 OTLP 管道输出 |
| **`compression`** | ✅ 默认开启 | 启用 `aidb/compression` (Snap / LZ4 块压缩), 生产默认使用 Snap; `--no-default-features` 可关闭 |

### 与 Redis 8.8 对齐: 兼容什么, 放弃什么 (YAGNI)?

**兼容目标 (Redis 8.8 对齐)**:
- RESP2 与 RESP3 双栈协议支持 (通过 `HELLO` 命令动态协商);
- 16384 槽位模型、`{...}` Hash Tag 提取、`-MOVED` / `-ASK` 重定向、`ASKING` 与 `READONLY` 连接标志;
- 核心数据结构 (String, Hash, List, Set, ZSet, JSON, Key, Database) 与服务端管理命令.

**刻意不实现 (YAGNI)**:
| 刻意不实现的特性 | 核心考量与替代方案 |
| :--- | :--- |
| **Telnet 式非数组内联命令 (`PING\r\n`)** | 仅支持标准 RESP 数组格式命令, 消除解析分支复杂度 |
| **Redis 官方 RDB/AOF 磁盘格式** | 内部持久化统一走 AiDb Checkpoint 与 LSM WAL, DUMP/RESTORE 采用内部紧凑 postcard 格式 |
| **`CONFIG REWRITE` 动态落盘** | 采用只读启动配置 + 环境变量覆盖模型, 避免复杂的运行时配置文件回写竞争 |
| **Redis 16379 P2P Gossip 故障投票共识** | 集群拓扑与故障转移权威判定统一走 MetaRaft 共识, Gossip 仅用于轻量 Leader 缓存刷新 |
| **服务端透明命令转发 (Forward Proxy)** | 严格遵循 Redis Cluster 官方规范由客户端重定向 (`redis-cli -c`), 避免服务端双倍跳步与跨网络阻塞 |

---

## 2. 协议与连接层设计决策

### 为什么支持 RESP2 与 RESP3 双栈协议?

- **RESP2**: 保证现有海量经典 Redis 客户端与工具链 (`redis-cli`, Jedis, redigo 等) 开箱即用;
- **RESP3**: 为现代客户端提供 Map, Set, Push, Attribute, Boolean, Double 等丰富类型;
- **连接级协商**: 默认建立 RESP2 连接, 客户端通过 `HELLO 3` 动态升级, 服务端在 `server::adapt_for_protocol` 中处理协议差异 (如 Null 表达 `$-1` vs `_`).

### 为什么选择 Parser 与 Encoder 彻底分离?

- `parser.rs` 专注于流式字节缓冲、深度限制、行解析与可恢复错误跳步 (`is_recoverable`);
- `encoder.rs` 专注于不可变 AST `RespValue` 到字节帧的序列化输出;
- 职责单一, 便于各自独立进行单元测试与属性测试 (Proptest).

### 为什么实行严格的解析 Limits?

为防止恶意超大帧攻击与递归栈溢出 OOM, 设定不可调的防御上限:
- `max_bulk_len = 512 MiB`
- `max_buffer_size = 64 MiB`
- `max_parse_depth = 128`
- `max_array_len = 4 MiB`
- `max_line_len = 1 MiB`
当遇到格式损坏或超限等不可恢复错误 (`is_fatal_protocol`) 时, 服务端主动断连, 防止恶意死循环写错误响应.

---

## 3. 存储适配与数据模型设计决策

### 为什么抽象 `KvStorage` Trait?

上层命令分发层仅依赖 `Arc<dyn KvStorage>`, 底层支持 `MemoryEngine` (开发测试) 与 `AiDbEngine` (持久化与集群) 自由切换, 单测与集成测试注入内存引擎即可实现秒级无盘自测.

### 为什么采用单 `DB` + `{db_index}:{user_key}` 扁平化前缀?

- **单实例设计**: 在底层 AiDb LSM 中使用 ASCII 前缀区分 Redis 逻辑数据库 (0~15), 避免为每个逻辑 DB 创建独立的 LSM 实例与文件描述符浪费;
- **多数据 Group 对应**: 在集群模式下, 物理实例由 MultiRaft Group 划分, 与分片拓扑天然对齐.

### 为什么复杂数据类型采用 Subkey 扁平化映射?

对于 Hash, List, Set, ZSet 等复杂类型:
- 元数据键: `{db_index}:{key}` 记录类型、版本号与元素数量;
- 数据键: `{db_index}:{key}:{field/index}` 存储单个字段或成员;
- **优势**: 支持超大集合的细粒度读写, 避免每次修改都整存整取导致大 Value 写入放大.

### 为什么集群写入必须走 `ClusterDataAdapter`?

在集群模式下, 属于本节点的分片写入请求**必须**打包为 `propose_group` 经过 MultiRaft 共识后落盘, **严禁**绕过 Raft 本地直接写磁盘, 从而彻底杜绝写入丢失或脏读.

---

## 4. 命令路由与并发控制设计决策

### 为什么 `CommandRouter` 作为命令分发中枢?

`CommandRouter` 统一承接:
1. 集群路由检查 (`cluster_route`): 验证 Key 所属槽位并决定是否返回 `-MOVED` / `-ASK`;
2. 指标记录: 实时统计命令调用耗时与成功/错误计数;
3. 并发加锁: 协调 `KeyLock` 获取读写锁;
4. 处理器分发: 按命令领域派发至 `string`, `hash`, `list`, `set`, `zset`, `json`, `script` 等子模块.

### 为什么 `KeyLock` 采用多 Key 字典序排序加锁?

- 针对多 Key 命令 (如 `MSET`, `RENAME`) 与 Lua 事务脚本, 必须同时持有多个 Key 的锁;
- `KeyLock` 内部维护 4096 个分桶互斥锁, 在加锁前对全部 Key 按照字典序升序排序后再依次获取锁, **数学上彻底杜绝死锁发生的可能**;
- Lua 脚本执行设置 30 秒加锁等待超时, 防止恶意死循环脚本永久挂起服务.

### 为什么 JSON 存储于 `ValueType::String`?

全量文档采用 JSON 文本保存于 String 类型, 内部借助 `serde_json` 与 `jsonpath` 库进行解析与过滤. 保持与底层通用 KV 引擎的解耦, 避免引入复杂的专有数据格式.

### 为什么 Lua 脚本基于 mlua (Lua 5.1)?

- 业务脚本按 Redis 内嵌的 Lua 5.1 编写, Kvrocks 的 LuaJIT 也是这套语义. 全局 `unpack`、单一数字类型和整数字符串格式都依赖 5.1.
- 选用 `mlua` (vendored lua51), 零系统 C 动态库依赖;
- 对标准库进行严格沙箱裁剪, 仅暴露安全算子;
- 在 `ScriptTransaction` 中通过 `WriteBatch` 收集 `redis.call` 的写入操作, 实现脚本多写操作的原子提交与失败回滚.

---

## 5. 集群协议与拓扑设计决策 (Feature `cluster`)

### 为什么采用 MetaRaft + MultiRaft 替代 Redis Gossip?

| 方案 | 一致性 | 拓扑变更确定性 | 故障恢复时间 | 选型结论 |
| :--- | :--- | :--- | :--- | :--- |
| **Redis 16379 Gossip** | 最终一致 | 弱 (可能产生裂脑与槽位冲突) | 慢 (依赖收敛周期) | 放弃 |
| **MetaRaft + MultiRaft** | **强一致** | **极高 (Raft 权威日志与状态机)** | **毫秒级选主** | **选用** |

- **MetaRaft**: 管理 16384 槽位权威映射表、节点拓扑与在线迁移状态机;
- **MultiRaft**: 按槽位 Group 负责数据分片的高吞吐复制;
- **AiKv 轻量 Gossip**: 节点间定期心跳仅用于刷新本地 Leader 路由缓存与集群指标, 不承担投票决策职责.

### 为什么由客户端处理 MOVED / ASK 重定向?

- 严格遵循 Redis Cluster 官方规范: 服务端发现 Key 不在本节点时, 返回 `-MOVED <slot> <target_ip:port>` 或 `-ASK <slot> <target_ip:port>`;
- 由智能客户端 (`redis-cli -c`, Lettuce, Jedis) 缓存槽位表并直接请求目标节点;
- **命令指标统计**: 仅在实际执行的节点累加 `commandstats`, 重定向节点不重复计入命令调用次数.

---

## 6. 可观测性设计决策 (Feature `monitoring`)

### 为什么 `ServerMetrics` 作为 INFO 的唯一权威源?

- 避免在业务热路径上进行多套数据结构的重复统计;
- 业务热路径仅更新 `ServerMetrics` 内存计数器;
- `InfoRenderer` 渲染 Redis `INFO` 命令输出时直接读取 `ServerMetrics`;
- 后台周期性任务将 `ServerMetrics` 的增量同步至 OpenTelemetry 指标管道 (`aikv_*`), 保证两者数据完全一致.

### 为什么独立 HTTP `/health` 服务于 9191 端口?

- Redis RESP 协议端口 (6379) 与 HTTP 协议不兼容;
- 在 `--metrics-addr:--metrics-port` (默认 9191) 上启动轻量级 HTTP 探活服务, 专用于 Kubernetes Liveness/Readiness 探针 (`/health`), 生产指标统一经 OTLP gRPC 管道导出至 Collector.

---

## 7. 性能优化与全局分配器

### 为什么使用 mimalloc 替代 glibc malloc?

基线 eBPF 火焰图分析显示, glibc malloc 在高并发连接与短小字符串分配下占用高达 **18.6%** 的 CPU 开销. 在 `main.rs` 中引入 `mimalloc` 作为全局内存分配器, 降低分配器锁竞争与内存碎片, 带来 **5%~9%** 的整体吞吐提升.

---

## 8. 全景决策总表 (Decision Matrix)

| 决策领域 | 选型方案 | 核心理由 | 放弃方案 / 约束限制 |
| :--- | :--- | :--- | :--- |
| **产品形态** | RESP 独立网络服务 (Tokio) | 兼容 Redis 生态, 协议与存储分离 | 嵌入式单库 / C++ 模块扩展 |
| **存储内核** | 委托 AiDb (LSM + MultiRaft) | 复用成熟 Rust 存储与共识基础设施 | 在协议层重复编写 LSM 状态机 |
| **协议支持** | RESP2 + RESP3 双栈 (HELLO 协商) | 兼顾旧客户端兼容性与现代类型表达 | 仅支持单协议 / 支持 Telnet 内联 |
| **并发锁** | KeyLock (4096 桶 + 字典序加锁) | 细粒度并发, 数学上杜绝死锁 | 单全局互斥大锁 / 无序任意加锁 |
| **存储映射** | 单 DB + Subkey 扁平化前缀 | 避免实例过多, 支持超大集合细粒度修改 | 多独立 DB 目录 / 集合整存整取 |
| **集群共识** | MetaRaft 控制面 + MultiRaft 数据面 | 强一致性拓扑保证, 彻底消除裂脑 | Redis 原生 Gossip 投票共识 |
| **重定向** | 客户端 MOVED/ASK 重试机制 | 对齐官方规范, 消除服务端代理开销 | 服务端透明转发 (Forward Proxy) |
| **持久化** | AiDb Checkpoint 硬链接秒级快照 | 零拷贝, 与 LSM 紧密配合 | 原生 Redis RDB / AOF 格式 |
| **指标管道** | OpenTelemetry OTLP Push 架构 | 标准化云原生可观测性栈 | 进程内 Prometheus Pull 端口 |
| **分配器** | mimalloc 全局内存分配器 | 降低多线程内存碎片与分配锁争用 | glibc 默认 malloc |
