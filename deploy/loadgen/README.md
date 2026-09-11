# AiKv Loadgen — 交互式加压工具

对已部署的 AiKv 单机/集群持续加压; 浏览器单页控制台动态调参.
**只加压, 不统计**: 不记录任何吞吐/延迟/结果数据, 观测请走 `deploy/observability` 的 Grafana 大盘.

## 快速开始

```bash
./up.sh                                            # 构建并启动 (默认 cluster, 127.0.0.1:6379)
# 浏览器打开 http://127.0.0.1:8787
./down.sh                                          # 停止
```

```bash
./up.sh --mode single --endpoints 127.0.0.1:6379   # 单机
./up.sh --bind 0.0.0.0:8787                        # 允许局域网访问控制台
./status.sh                                        # 进程/探活/当前生效参数
```

## 参数 (全部可在 UI 热更新)

| 分组 | 参数 | 默认 | 说明 |
|---|---|---|---|
| 压力 | target_ops | 20000 | 目标总速率; `0` = 不限速尽力压 |
| | connections | 32 | worker 数 (每 worker 一条连接) |
| | pipeline | 16 | 每轮在途命令数 |
| 数据 | keyspace / key_prefix | 100000 / loadgen | key 数量与前缀 (`loadgen:key:N`) |
| | value_size_min/max | 64 / 64 | SET 值大小 |
| | miss_ratio | 0.1 | 读不存在 key 的比例 |
| | ttl_ratio | 0 | SET 带 60s TTL 的比例 |
| | use_hashtag | false | true 时 key 变为 `{loadgen}:key:N`, 全部压进同一 slot |
| 混合 | set/get/del/mget/incr/expire | 40/40/5/10/3/2 | 权重自动归一化 |
| 行为 | timeout_ms / readonly | 1000 / false | 只读模式下写命令降级为读 |

- `mode` / `endpoints` / `timeout_ms` 变更会重建连接 (supervisor 0.5s 内完成), 其余参数即时生效.
- 启动 CLI: `--bind` (默认 `127.0.0.1:8787`), `--mode`, `--endpoints`; 环境变量 `LOADGEN_BIND`, `LOADGEN_LOG`.

## 边界

- 无鉴权无 TLS: 控制台与被测端口只应暴露在本机/可信网段 (对齐 AiKv v1 安全边界).
- UI 只显示: 生效参数 / worker 数 / endpoint PING 可达性 / 最近一条错误. 没有速率曲线, 没有历史.
- 引擎实际达成速率可能低于目标 (同机 CPU 不足时), 以 Grafana OPS 曲线为准.

## 开发

```bash
./build.sh --check     # fmt + clippy (-D warnings)
cargo test             # 单测 (配置校验/负载计划/令牌桶/HTTP 契约)
```

质量门禁与 aikv 主仓一致 (rustfmt/clippy 按目录层级继承); 本 crate 为独立 workspace,
不参与 `aikv` 的 `cargo test --workspace`.
