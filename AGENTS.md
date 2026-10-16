# AiKv — AI 助手操作指南

> **Role & Positioning**: 你是 AiKv 的核心系统开发工程师. AiKv 是纯 Rust 实现的高性能、轻量级 **Redis RESP 兼容的分布式键值网络服务** (bin + lib). 对外提供 RESP2/RESP3 协议与 Redis 命令语义; 底层持久化存储与 MultiRaft 分布式共识委托给 **[wiqun/AiDb](https://github.com/wiqun/AiDb)**.

---

## 1. 行为边界与硬约束 (Guardrails & Constraints)

### 🔴 绝对禁止 (Never)
- **禁止在 AiKv 重复实现存储与共识内核**: WAL 重放、MemTable 组织、SSTable Compaction 与 Raft 日志状态机均在 AiDb 实现, 严禁在 AiKv 内部编写 LSM 状态机或自研 Raft.
- **禁止服务端透明命令转发**: 严格遵循 Redis Cluster 官方规范, 非本地槽位必须向客户端返回 `-MOVED` 或 `-ASK` 重定向错误, 严禁在服务端代客户端透明 TCP 代理转发.
- **禁止破坏 KeyLock 字典序加锁契约**: 多 Key 命令与 Lua 事务必须严格按 Key 字典序升序排序加锁, 严禁任意无序获取锁引发并发死锁.
- **禁止绕过 Raft 直接写存储**: 在集群模式下, 分配槽位的写操作必须通过 `ClusterDataAdapter` 进行 Raft 批处理 propose, 严禁绕过共识直接写入底层本地引擎.
- **禁止允许名单以外的 Span**: 只允许命令入口 `kv_command`. 不得再加 `#[instrument]`. 采样关闭时不创建该 span, 字段不含用户 key.
- **禁止测试并发污染**: 集成测试与集群测试必须带 `--test-threads=1`, 避免端口与数据目录冲突.
- **禁止随意修改持久化编码格式**: `StoredValue` 编码、Subkey 扁平化规则与 DUMP 内部 postcard 格式变更属于高风险操作, 须提前评估兼容性.

### 🟢 必须遵守 (Always)
- **指标前缀与单一真源规范**: 生产指标统一使用 `aikv_*` 前缀经 OTLP 导出; `ServerMetrics` 作为 `INFO` 输出与 OTel 镜像的唯一真实数据源, 杜绝双计数.
- **Redis 8.8 对齐**: `INFO` 字段键名与格式全面对齐 Redis 8.8, 暂未支持的指标必须明确标注为 Stub 值 (`0` 或 `-1`).
- **测试纪律**: 所有 bug 修复 (`fix:`) 必须在同一 PR 附带回归测试, 并在测试函数正上方添加中文 `///` 注释说明 bug 现象、期望与 Issue 编号.
- **文档同步 (强制)**: 修改公共 API、核心行为、CLI 参数或模块边界必须同步更新对应 [`docs/modules/`](docs/modules/) 模块文档与根目录文档; commit 消息修 bug 须带 Issue 引用 (`Fixes #NN`).
- **代码质量门禁**: 提交前必须确保 `cargo fmt --check` 与 `RUSTFLAGS='-D warnings' cargo clippy --all-targets --features cluster,monitoring` 零警告通过.

---

## 2. 技术选型与参考基准 (防误猜对照表)

仓库领域术语定义见 [CONTEXT.md](CONTEXT.md).

| 维度 | 本项目选型 | 主流参考 | 核心差异与 AI 行为指引 (Do NOT Guess) |
| :--- | :--- | :--- | :--- |
| **RESP 协议栈** | 自研 `src/protocol/` (RESP2/3 双栈, Pipeline) | Redis RESP 规范 / resp-rs | **未采用 resp-rs**; 流式解析限制 512MB bulk / 64MB buffer / 128 深度; **不支持 Telnet 内联非数组命令** |
| **数据结构映射** | `StoredValue` + Subkey 扁平化前缀 | Kvrocks (Redis-on-LSM) | 存储引擎是 AiDb 自研 LSM (非 Kvrocks/RocksDB 直用); Hash/List 等采用 Subkey 降低写放大 |
| **并发锁模型** | `KeyLock` (4096 桶 + 字典序排序加锁) | Redis 单线程 / 全局锁 | 多 Key 与 Lua 事务按字节序升序加锁, 设 30s 超时; 读写锁隔离 |
| **集群路由** | 16384 Slot, CRC16, `{...}` Hash Tag, MOVED/ASK | Redis Cluster Spec | **无服务端透明转发**; 命令统计仅在实际执行节点累加; 客户端需 `redis-cli -c` |
| **集群共识** | 委托 AiDb MetaRaft (控制面) + MultiRaft (数据面) | Redis 16379 Gossip | **拓扑与故障转移权威判定走 MetaRaft**, 非 Gossip 投票; AiKv Gossip 仅用于轻量 Leader 缓存刷新 |
| **可观测性** | INFO 对齐 Redis 8.8; 生产指标 OTLP `aikv_*` | Redis INFO / redis_exporter | `ServerMetrics` 为 INFO 唯一真源; OTel 为其镜像; HTTP `:9191` 仅提供 `/health` 探活 |
| **全局分配器** | `mimalloc` | glibc 默认 malloc | 针对高并发小内存分配消除锁竞争与内存碎片 |

---

## 3. 核心开发与验证命令 (Command-First)

### 本地快速门禁 (推送前必跑)
```bash
export RUSTFLAGS='-D warnings'
cargo fmt --check
cargo clippy --all-targets --features cluster,monitoring
cargo test --workspace --features cluster,monitoring -- --test-threads=1
```

### 慢测与压力测试
```bash
cargo test --test server --features cluster -- --ignored --test-threads=1
cargo test --test commands --features cluster -- --ignored --test-threads=1
cargo test --test stress_ttl --features cluster -- --ignored --test-threads=1
```

### E2E 黑盒验收测试 (需先部署单机或集群服务)
```bash
pytest e2e/function/ -v
```

---

## 4. 任务上下文与模块导航 (Task Routing)

> **文档总览**
- 架构设计详见: [ARCHITECTURE.md](ARCHITECTURE.md);
- 设计决策与权衡详见: [docs/design.md](docs/design.md);
- 部署与运维详见: [docs/deployment.md](docs/deployment.md);
- 贡献规范见: [CONTRIBUTING.md](CONTRIBUTING.md);
- 完整文档索引见: [docs/README.md](docs/README.md).

修改具体子系统代码时, AI **必须优先查阅**对应的模块文档:

| 开发任务 / 涉及领域 | 优先阅读文档 | 核心关注点 |
| :--- | :--- | :--- |
| **RESP 编解码 / Parser / Limits** | [docs/modules/01-protocol.md](docs/modules/01-protocol.md) | 流式分帧、`is_recoverable` 错误跳步、`is_fatal_protocol` 断连、RESP2/3 格式转换 |
| **TCP 连接 / HELLO / ATOM 事务** | [docs/modules/02-server.md](docs/modules/02-server.md) | Connection 生命周期、`protocol_negotiated`、MULTI/EXEC/WATCH 事务队列与 `max_clients` |
| **存储适配 / Subkey / Raft 批写** | [docs/modules/03-storage.md](docs/modules/03-storage.md) | `KvStorage` trait、Subkey 扁平化编码、`ClusterDataAdapter` 批处理与 `DEFAULT_EAGER_FLUSH` |
| **核心数据结构命令 / KeyLock** | [docs/modules/04-commands-core.md](docs/modules/04-commands-core.md) | String~ZSet 命令实现、`CommandFlags`、`KeyLock` 字典序加锁防死锁、WRONGTYPE 处理 |
| **JSON / Lua / 阻塞队列 / 运维** | [docs/modules/05-commands-extended.md](docs/modules/05-commands-extended.md) | JSONPath 引擎、Lua 沙箱与 `ScriptTransaction`、`BlockingRegistry` (BLPOP)、SAVE Checkpoint |
| **Redis Cluster 协议 / Slot 迁移** | [docs/modules/06-cluster.md](docs/modules/06-cluster.md) | CRC16 `{...}` Hash Tag、MOVED/ASK 重定向、CLUSTER 子命令、与 AiDb MetaRaft 拓扑同步 |
| **可观测性架构 / INFO / Tracing** | [docs/modules/07-observability.md](docs/modules/07-observability.md) | `SlowQueryLog` 环形缓冲、`LatencyStats`、`InfoRenderer` 渲染、OTel 管道与 `kv_command` span |
| **指标字典 / INFO 8.8 对照表** | [docs/modules/08-observability-reference.md](docs/modules/08-observability-reference.md) | `aikv_*` 全部 OTel 指标名与标签、INFO 8.8 全字段对照表与 Stub 标注 |
| **配置分层 / TOML / env / CLI 优先级** | [docs/modules/09-config.md](docs/modules/09-config.md) | `src/config/` merge 语义、`ResolvedSettings`、`ConfigWarning`、文件发现与测试 seam |
| **LSM / Raft 内核排查** | [AiDb AGENTS.md](https://github.com/wiqun/AiDb/blob/main/AGENTS.md) | 涉及底层 LSM 存储、WAL 恢复、Compaction 或 Raft 共识故障时跳转查看 |
