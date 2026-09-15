# AiKv

[![Rust 2021](https://img.shields.io/badge/Rust-2021-blue.svg)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/version-1.0.0-orange.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![CI Status](https://img.shields.io/badge/CI-passing-brightgreen.svg)](.github/workflows/ci.yml)

> **AiKv** 是一个用纯 Rust 实现的高性能, 轻量级 **RESP2/RESP3 兼容的分布式键值服务** (bin + lib).
> 对外提供 RESP2/RESP3 array framing 与 [已实现命令子集](docs/compatibility.md); 底层存储与分布式共识委托给嵌入式引擎 [wiqun/AiDb](https://github.com/wiqun/AiDb).

---

## 为什么开发 AiKv (Why AiKv?)

- **磁盘级 KV 存储, 大幅降低成本**: 原生 Redis 全量内存导致成本高昂且容量受限; AiKv 将冷热数据下沉至 SSD 优化的 LSM-Tree 引擎, 在保障高并发低延迟的同时显著降低硬件成本与运维开销.
- **原生支持 JSON 与 Lua 脚本**: 无需额外编译或维护第三方 C 扩展 (如 RedisJSON), 原生支持 RedisJSON (JSONPath) 兼容命令与 Lua 5.4 安全沙箱, 开箱即用.
- **彻底解决 Redis 集群模式痛点**: 摒弃 Redis Gossip 带来的脑裂与收敛慢问题, 引入强一致的 Raft 算法, 提供确定性拓扑管理与无损在线槽位迁移.
- **存算分离, 架构解耦**: 网络协议与命令路由由 AiKv 实现, 持久化与分布式共识由纯 Rust LSM 引擎 [wiqun/AiDb](https://github.com/wiqun/AiDb) 承载, 各层独立演进.

---

## 核心亮点 (Key Highlights)

- **RESP2/RESP3 兼容及已实现命令子集**: 支持标准 array framing 双栈协议与 registry 内命令语义; 完整清单见 [docs/compatibility.md](docs/compatibility.md), 非 registry 命令或未实现能力需客户端适配.
- **纯 Rust 实现**: 内存安全, 零 C/C++ 依赖, 采用 mimalloc 降低内存碎片.
- **丰富的数据结构与原生扩展**: 完整支持 String, Hash, List, Set, Sorted Set, 原生内置 JSON (JSONPath) 与 Lua 5.4 脚本 (EVAL / SCRIPT).
- **细粒度并发与事务**: 4096 桶按 Key 字典序排序加锁, 彻底杜绝死锁; 支持 MULTI/EXEC/WATCH 原子事务.
- **分布式集群模式**: 16384 槽位路由, Hash Tag, 在线槽位迁移, 客户端透明重定向 (redis-cli -c).
- **云原生高并发架构**: Tokio 异步运行时 + 4096 桶细粒度 KeyLock, 保证原子性与无死锁的同时最大化多核吞吐.
- **云原生可观测性**: 深度对齐 Redis 8.8 INFO 字段, 支持基于 OpenTelemetry 的指标导出 (`aikv_*`) 与全链路 tracing span 跟踪.

---

## 架构概览 (Architecture at a Glance)

```mermaid
flowchart TB
    Client([Redis 客户端 / redis-cli -c]) -->|TCP / RESP2 / RESP3| Listener[Server Listener / Connection]

    subgraph Protocol [协议与连接层]
        Listener --> Parser[RespParser 流式解析]
        Encoder[RespEncoder 序列化] --> Listener
    end

    subgraph Dispatch [命令调度与并发控制]
        Parser --> Router[CommandRouter 命令分发中枢]
        Router --> KeyLock[KeyLock 字典序加锁]
        Router --> Registry[CommandRegistry 命令表]
    end

    subgraph CommandHandlers [命令处理器]
        Router --> CoreCmds[核心命令: String / Hash / List / Set / ZSet]
        Router --> ExtCmds[扩展命令: JSON / Lua / Blocking / Migrate]
        Router --> ServerCmds[管理命令: INFO / CONFIG / CLIENT]
    end

    subgraph StorageAdapter [存储适配层]
        CoreCmds --> Adapter[KvStorage Trait]
        ExtCmds --> Adapter
        Adapter --> Subkey[Subkey 扁平化编码]
        Adapter --> Memory[MemoryEngine 内存引擎]
        Adapter --> AiDbEngine[AiDbEngine 持久化引擎]
        Adapter --> ClusterAdapter[ClusterDataAdapter Raft 批处理]
    end

    subgraph AiDbBackend [AiDb 内核 Sibling]
        AiDbEngine --> LSM[AiDb LSM 存储引擎]
        ClusterAdapter --> Raft[MetaRaft 控制面 + MultiRaft 数据面]
    end

    Router -.-> ClusterRoute[ClusterRouter 16384 槽位 MOVED/ASK 判定]
```

---

## 快速开始

### 编译构建

```bash
# 1. 编译带集群能力的发布版本
cargo build --release --features cluster

# 2. 生产全功能构建 (默认包含块压缩 + 集群 + OTel 监控)
cargo build --release --features cluster,monitoring

# 3. 无压缩兼容构建
cargo build --release --no-default-features --features cluster,monitoring
```

### 单机运行

可选: 将 [`deploy/aikv.example.toml`](deploy/aikv.example.toml) 复制为 `aikv.toml` 放置于工作目录, 或通过 `--config` 指定; 详见 [docs/modules/09-config.md](docs/modules/09-config.md).

```bash
# 启动持久化服务 (推荐生产配置)
./target/release/aikv \
  --bind 127.0.0.1:6379 \
  --engine aidb \
  --data-dir /tmp/aikv-data

# 另开终端验证
redis-cli -p 6379 PING
redis-cli -p 6379 SET mykey "hello aikv"
redis-cli -p 6379 GET mykey
```

### 容器化部署

仓库 `deploy/` 目录提供一键脚本, 基于 [`deploy/aikv.example.toml`](deploy/aikv.example.toml) 生成运行时配置:

```bash
# 默认 GitHub main 构建 (aidb)
./deploy/aikv-single.sh build
./deploy/aikv-single.sh up
./deploy/aikv-single.sh down

# 本地同层级 ../aidb 构建并启动六节点集群
./deploy/aikv-cluster.sh build --local
./deploy/aikv-cluster.sh up
./deploy/aikv-cluster.sh down --purge
```

默认 Docker 构建使用 GitHub `main` 分支上的 `aidb`; `--local` 使用 aikv 同层级的
`../aidb`. Compose 文件只引用预构建镜像, 不负责 build 或 pull; 镜像名默认为
`aikv:dev`, 可用 `AIKV_IMAGE` 覆盖.

`aikv-single.sh up` 只启动一个容器, 使用 named volume `aikv`. `aikv-cluster.sh up` 会
生成六份节点配置并启动 2 个分片、每个分片 1 主 2 从的 Redis Cluster 拓扑.
启动时会清理同一 Compose project 中旧服务名产生的 orphan 容器. 运行时配置位于
`deploy/.runtime/`, 默认 `down` 保留 named volumes; 只有显式 `--purge`
才删除数据卷. 该开发阶段快速部署参考不提供旧卷到新卷的数据迁移.
运行状态用 `docker ps` 查看.

集群容器名称为 `aikv-1` 至 `aikv-6`. 宿主机客户端端口依次为
`6379`, `6380`, `6381`, `7379`, `7380`, `7381`; MetaRaft 端口依次为
`16379`, `16380`, `16381`, `17379`, `17380`, `17381`; MultiRaft 端口依次为
`26379`, `26380`, `26381`, `27379`, `27380`, `27381`; Metrics 端口为
`9191-9196`. 本地客户端入口为 `redis-cli -c -p 6379`;
默认 `MOVED` 地址公布为 `127.0.0.1:<宿主客户端端口>`, 面向本机客户端.
远程或跨主机访问时需同时设置宿主机绑定与公布地址:

```bash
./deploy/aikv-cluster.sh up -b 0.0.0.0 -a 192.168.1.112
```

- `AIKV_BIND_IP`: Compose 端口映射的宿主机地址, 默认 `127.0.0.1`
- `AIKV_ANNOUNCE_IP`: 写入 `client_addr` / `CLUSTER MEET` 的公布 IP, 默认 `127.0.0.1`

单机 Compose (`aikv-single.sh up`) 同样支持 `-b/--bind`, 默认仍仅本机可连.

监控栈: `./deploy/observability.sh up`. 加压控制台: `./deploy/loadgen.sh up` (http://127.0.0.1:8787).

### 集群部署与运维

多节点 Redis Cluster 部署, 端口规划与拓扑初始化指南请查阅 [docs/deployment.md](docs/deployment.md).

---

## 安全、平台与升级边界

AiKv v1 不内建 `AUTH`, `ACL` 或 `TLS`. `RESP`, `MetaRaft` 和 `MultiRaft` 端口不得暴露到不可信网络. 需要跨越信任边界时, 必须使用认证/TLS proxy 或 service mesh, 并配置网络访问控制.

Linux x86_64 是正式支持平台, 其他平台仅 best-effort. 从 v1 之前版本升级时, 数据目录, `DUMP`, Raft snapshot 和已有集群不可原地升级或滚动升级; 请使用新部署及经过验证的迁移或恢复方案, 不要混用不同版本节点或持久化产物.

漏洞报告方式见 [SECURITY.md](SECURITY.md).

---

## 与 AiDb 协同开发 (Monorepo Setup)

AiKv 通过 GitHub `main` 分支引入 [wiqun/AiDb](https://github.com/wiqun/AiDb). 本地高频联调时, 在 `~/.cargo/config.toml` 中配置 Git source 的本地覆盖:

```toml
[patch."https://github.com/wiqun/AiDb.git"]
aidb = { path = "/absolute/path/to/aidb" }
```

---

## 示例 (Examples)

仓库内提供了开箱即用的示例代码:

| 示例场景 | 源码入口 | 说明 | 运行命令 |
| :--- | :--- | :--- | :--- |
| **基础操作** | [`examples/basic.rs`](examples/basic.rs) | 内嵌内存 Server 快速演示 CRUD 与 INFO | `cargo run --example basic` |
| **集群路由** | [`examples/cluster.rs`](examples/cluster.rs) | CRC16 槽位计算与 Hash Tag 提取演示 | `cargo run --features cluster --example cluster` |

---

## 功能特性矩阵 (Feature Matrix)

| Feature | 默认状态 | 核心能力 | 依赖与说明 |
| :--- | :--- | :--- | :--- |
| **(none)** | ❌ (需 `--no-default-features`) | 单机 RESP2/3 服务, 内存 / AiDb 存储引擎 | 无可选 feature 的最小构建 |
| **`cluster`** | 按需开启 | 16384 Slot 槽位计算, `-MOVED` / `-ASK` 重定向, MultiRaft 批处理 | 依赖 `aidb/cluster`, 构建需 `protoc` |
| **`monitoring`** | 按需开启 | OTel 生产指标 (`aikv_*`), Tracing 链路导出, `:9191` `/health` HTTP 探活 | 依赖 `aidb/monitoring` |
| **`compression`** | ✅ 默认开启 | 开启 SSTable 数据块压缩 (Snap / LZ4) | 依赖 `aidb/compression`; 可用 `--no-default-features` 关闭 |

完整 CLI 参数与环境变量对照见 [docs/deployment.md](docs/deployment.md).

---

## 自动化测试与质量门禁

```bash
# 1. 运行核心单元与集成测试
export RUSTFLAGS='-D warnings'
cargo fmt --check
cargo clippy --all-targets --features cluster,monitoring
cargo test --features cluster,monitoring -- --test-threads=1

# 2. 运行黑盒 E2E 测试 (需先部署服务)
pytest e2e/function/ -v
```

测试规范与矩阵说明见 [CONTRIBUTING.md](CONTRIBUTING.md).

---

## 文档导航

开发与设计文档总览见 [docs/README.md](docs/README.md).

| 文档分类 | 入口文件 | 适用场景与内容 |
| :--- | :--- | :--- |
| **系统架构** | [ARCHITECTURE.md](ARCHITECTURE.md) | 分层设计, KeyLock 并发模型, Subkey 编码, Cluster 路由原理 |
| **设计决策** | [docs/design.md](docs/design.md) | 技术选型理由, 跨模块权衡 (Why), YAGNI 与已知限制 |
| **部署与运维** | [docs/deployment.md](docs/deployment.md) | Feature 构建, CLI 参数, 集群多节点部署, OTel 监控告警 |
| **安全政策** | [SECURITY.md](SECURITY.md) | 漏洞报告入口, 支持版本与网络安全边界 |
| **开发与贡献** | [CONTRIBUTING.md](CONTRIBUTING.md) | Git 工作流, 测试矩阵, 规范要求与 PR 流程 |
| **兼容矩阵** | [compatibility.md](docs/compatibility.md) | Redis 协议边界, 已实现命令与 `all_commands()` 自动核对 |
| **AI 助手指南** | [AGENTS.md](AGENTS.md) | 编码硬约束 (Never / Always), 技术对照表与任务路由 |
| **模块文档** | [docs/modules/](docs/modules/) | 8 篇模块深度规范 (Protocol, Server, Storage, Commands, Cluster, Observability) |
| **变更记录** | [CHANGELOG.md](CHANGELOG.md) | 版本演进与特性变更日志 |

---

## 许可证

本项目采用 [MIT 许可证](LICENSE) (见 [Cargo.toml](Cargo.toml)).
