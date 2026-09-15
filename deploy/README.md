# AiKv 部署与运维

本目录提供 AiKv 的轻量化容器编排, 生产级可观测性监控栈与交互式压测工具. 所有运维操作均收口至 4 个一级管理脚本, 支持 macOS 与 Linux 双系统开箱即用.

---

## 架构概览

```mermaid
flowchart TD
    subgraph Observability["可观测性监控栈 (observability.sh)"]
        Prometheus["Prometheus (:9090)"]
        Grafana["Grafana (:3000)"]
        Collector["OTel Collector (:4317/:4318)"]
        NodeExporter["Node Exporter (:9100)"]
        Collector -->|Remote Write| Prometheus
        Prometheus --> Grafana
        NodeExporter -.->|Scrape| Prometheus
    end

    subgraph StorageEngine["AiKv 存储集群 (aikv-single.sh / aikv-cluster.sh)"]
        Single["单机实例 (6379 / 9191)"]
        Cluster["分布式 6 节点集群 (6379-7381)"]
    end

    subgraph LoadgenModule["加压控制台 (loadgen.sh)"]
        WebUI["Web 控制台 (:8787)"]
        Engine["压测引擎 (Pipeline/Worker)"]
        WebUI --> Engine
    end

    Single -.->|OTLP/gRPC| Collector
    Cluster -.->|OTLP/gRPC| Collector
    Engine -->|RESP| Single
    Engine -->|RESP (Cluster Slots)| Cluster
```

---

## 核心脚本一览

| 脚本 | 部署对象 | 核心参数与命令 | 核心功能与特性 |
| :--- | :--- | :--- | :--- |
| [`aikv-single.sh`](./aikv-single.sh) | 单机容器 | `build [--local]`<br>`up [-b IP]`<br>`down [--purge]` | 单实例 AiDb 存储引擎, 自动探测并对接 OTel 收集器, 数据收口挂载至本地 `.runtime/single/data`. |
| [`aikv-cluster.sh`](./aikv-cluster.sh) | 分布式集群 | `build [--local]`<br>`up [-b IP] [-a IP]`<br>`down [--purge]` | 自动化拉起 6 节点 (2 主 4 从) Redis Cluster 架构, 自动完成 16384 槽位切分与拓扑收敛. |
| [`observability.sh`](./observability.sh) | 可观测性监控栈 | `up`<br>`down [--purge]` | 一键部署 Prometheus, Grafana, OTel Collector; 自动感知 macOS/Linux 适配 cAdvisor 与系统指标. |
| [`loadgen.sh`](./loadgen.sh) | 交互式压测控制台 | `build`<br>`up [-b ADDR:PORT] [-f PATH]`<br>`status`<br>`down` | 提供轻量单页 Web 控制台, 支持动态调参, 意图分层防穿透, 预设模板与集群自动寻路. |

---

## 端口与网络规划

所有容器默认挂载至 Docker Bridge 网络 `aikv-net`, 并在宿主机暴露对应端口:

| 服务组件 | 宿主机端口 | 协议类型 | 说明 |
| :--- | :--- | :--- | :--- |
| **AiKv 单机客户端** | `6379` | RESP (TCP) | Redis 客户端连接入口 |
| **AiKv 单机健康/指标** | `9191` | HTTP | 单机 `/health` 探活与 Prometheus 兼容指标端点 |
| **AiKv 集群客户端** | `6379` ~ `6381`, `7379` ~ `7381` | RESP (TCP) | 6 节点集群客户端访问端口 (含主从分片) |
| **AiKv 集群内部通信** | `16379` ~ `17381`, `26379` ~ `27381` | gRPC / Raft | MetaRaft 控制面与 MultiRaft 数据面复制通道 |
| **OTel Collector** | `4317` (gRPC), `4318` (HTTP) | OTLP | 接收实例推送的指标与链路数据 |
| **Prometheus** | `9090` | HTTP | 时序数据库, 内置生命周期管理与查询接口 |
| **Grafana** | `3000` | HTTP | 监控大盘控制台 (默认免密 Viewer, 管理员 `admin/admin`) |
| **Loadgen 控制台** | `8787` | HTTP | 浏览器交互式加压控制台页面与管理 API |

---

## 运行时与存储目录规范

为了方便在宿主机本地直接排查存储引擎状态并保证环境干净, 部署体系约定如下:

- **运行时目录 (`.runtime/`)**: 
  - 所有实例动态生成的配置文件 (如 `aikv.toml`) 与持久化数据统一收口在 `deploy/.runtime/` 下.
  - 单机数据目录: `deploy/.runtime/single/data`
  - 集群数据目录: `deploy/.runtime/cluster/node1/data` ~ `node6/data`
  - 压测运行日志: `deploy/.runtime/loadgen/loadgen.log`
  - 该目录由项目根目录 `.gitignore` 自动忽略, 绝不污染 Git 工作区.
- **原子清理 (`--purge`)**: 
  - 执行各脚本的 `down --purge` 命令时, 除了停止并销毁容器外, 会递归清理对应的 `.runtime/` 目录与关联存储卷, 瞬间恢复零残留初始状态.

---

## 快速上手链路

### 1. 启动监控栈 (可选推荐)
```bash
cd deploy
./observability.sh up
```
启动后访问 [http://127.0.0.1:3000](http://127.0.0.1:3000) 打开 Grafana 大盘.

### 2. 启动单机或集群实例
```bash
# 方式 A: 启动单机实例
./aikv-single.sh up

# 方式 B: 启动 6 节点分布式集群
./aikv-cluster.sh up
```

### 3. 启动交互式压测
```bash
./loadgen.sh up
```
在浏览器打开 [http://127.0.0.1:8787](http://127.0.0.1:8787) 随时调整参数并启动负载任务.

---

## 子模块详细文档

- [Loadgen 压测工具详细设计与参数手册](./loadgen/README.md)
- [可观测性监控大盘指标规范与对账契约](./observability/README.md)
