# AiKv Loadgen — 交互式加压工具

对已部署的 AiKv 单机/集群持续加压; 浏览器单页控制台随时改表单, 点启动才开任务.
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

## 参数 (随时可改, 只有点启动才作用到任务)

| 分组 | 参数 | 默认 | 说明 |
|---|---|---|---|
| 压力 | target_ops | 5000 | 限速上限; `0` = 不限速尽力压 |
| | connections | 8 | 并行 worker 数 (每 worker 一条逻辑连接) |
| | pipeline | 8 | 每个 worker 一次网络往返打包的命令条数 |
| 数据 | keyspace / key_prefix | 100000 / loadgen | key 数量与前缀 (`loadgen:key:N` / `loadgen:churn:N` 等) |
| | value_size_min/max | 64 / 256 | SET 值大小 |
| | miss_ratio | 0.1 | 读未命中比例 (前端呈现为键命中率 `0.9`); 引擎采用意图分层隔离机制 (key/miss/churn/ttl/int), 保证主数据段命中率精准恒定, 彻底消除 DEL/TTL 腐蚀 |
| | ttl_ratio / ttl_seconds | 0 / 60 | SET 带 TTL 的写入比例与过期时长 (秒) |
| | use_hashtag / target_slot | false / null | 集中单槽及目标槽位 (0..=16383); target_slot 为 null 时跟随 key_prefix 默认哈希 |
| 混合 | set/get/del/mget/incr/expire | 40/40/5/10/3/2 | 权重自动归一化 |
| 行为 | timeout_ms | 1000 | 命令超时时间 (毫秒) |

- 改表单不影响正在跑的任务, 也不会自动停止; 运行中状态由后台自己维护. 点「启动」才按当前表单开新任务 (若已在跑则先停再起, 整批重建 worker, 不是差值加减).
- 令牌桶按 `target_ops` 限速发卡, 这是天花板不是机器性能承诺; 实际发出速率还受 `连接数 × pipeline / 往返时延` 限制. `0` 表示不限速.
- 启动 CLI: `--bind` (默认 `127.0.0.1:8787`), `--mode`, `--endpoints` (默认一个 seed `127.0.0.1:6379`; cluster 只需入口, 其余节点由 `CLUSTER SLOTS` 发现); 环境变量 `LOADGEN_BIND`, `LOADGEN_LOG`.

## 边界

- 无鉴权无 TLS: 控制台与被测端口只应暴露在本机/可信网段 (对齐 AiKv v1 安全边界).
- UI 只显示: 生效参数 / worker 数 / seed PING 可达性. 失败用底部提示, 没有速率曲线和历史.
- 引擎实际达成速率 = min(限速, 连接数×pipeline/往返时延, 引擎处理能力); 以 Grafana OPS 为准.
- 探活每 30s 对 endpoints 做 PING (cluster 另读 `CLUSTER INFO`); 点启动前 seed 不通或 `cluster_state` 非 `ok` 会拒绝启动.
- 运行中单个节点变红不停止加压; 全部不可达或集群 `fail` 时暂停派发 (worker 保留), 只有点「停止」才真正停泵.

## 开发

```bash
./build.sh --check     # fmt + clippy (-D warnings)
cargo test             # 单测 (配置校验/负载计划/令牌桶/HTTP 契约)
```

质量门禁与 aikv 主仓一致 (rustfmt/clippy 按目录层级继承); 本 crate 为独立 workspace,
不参与 `aikv` 的 `cargo test --workspace`.
