# AiKv Loadgen — 交互式加压工具

对已部署的 AiKv 单机/集群持续加压; 浏览器单页控制台随时改表单, 点启动才开任务.
**只加压, 不统计**: 不记录任何吞吐/延迟/结果数据, 观测请走 `deploy/observability` 的 Grafana 大盘.

打开控制台时的默认值, 侧栏预设, 命令占比默认胶囊来自同目录配置文件. 仓库提供模板 [`loadgen.example.toml`](./loadgen.example.toml), 可复制为 `loadgen.toml` 进行自定义. 改 TOML 后在 `aikv/deploy/` 下重启进程即可 (`./loadgen.sh down && ./loadgen.sh up`), 不必重新编译. 没有这份文件时使用编译进二进制的同一套默认值; 文件存在但写坏了会拒绝启动.

控制台页面在 `web/` 目录 (`index.html`, `app.css`, `app.js`). 改 UI 后刷新浏览器即可, 不必重编; 缺这四个文件 (含 `wiqun.svg`) 会拒绝启动. 改 Rust 源码才需要在 `aikv/deploy/` 下执行 `./loadgen.sh build`.

---

## 快速开始

在 `aikv/deploy/` 目录下执行:

```bash
./loadgen.sh up                                    # 读 loadgen/loadgen.toml (若存在) 并启动
# 浏览器打开 http://127.0.0.1:8787
./loadgen.sh down                                  # 停止
```

```bash
# 复制并编辑 loadgen/loadgen.toml 后重启
cp loadgen/loadgen.example.toml loadgen/loadgen.toml
./loadgen.sh up -b 0.0.0.0:8787                    # 允许局域网访问控制台
./loadgen.sh up -f path/to/loadgen.toml            # 指定配置文件
./loadgen.sh status                                # 查看进程/探活/当前生效参数
```

支持通过环境变量 `LOADGEN_BIND` (或在 `deploy/.env` 中定义) 配置默认监听端口.

---

## 参数 (随时可改, 只有点启动才作用到任务)

`loadgen.toml` 的各分块配置与下表一致, 可按机器性能调整:

| 分组 | 参数 | 默认 | 说明 |
| :--- | :--- | :--- | :--- |
| **压力** | `target_ops` | 5000 | 限速上限; `0` = 不限速尽力压 |
| | `connections` | 8 | 并行 worker 数 (每个 worker 维护一条独立连接) |
| | `pipeline` | 8 | 每个 worker 一次网络往返打包的命令条数 |
| **数据** | `keyspace` / `key_prefix` | 100000 / `loadgen` | key 数量与前缀 (`loadgen:key:N` / `loadgen:churn:N` 等) |
| | `value_size_min` / `max` | 64 / 256 | SET 写入值字节大小区间 |
| | `miss_ratio` | 0.1 | 读未命中比例 (前端呈现为键命中率 `0.9`); 引擎采用意图分层隔离机制 (key/miss/churn/ttl/int), 保证主数据段命中率精准恒定, 彻底消除 DEL/TTL 腐蚀 |
| | `ttl_ratio` / `ttl_seconds` | 0 / 60 | SET 带 TTL 的写入比例与过期时长 (秒) |
| | `use_hashtag` / `target_slot` | `false` / `null` | 集中单槽及目标槽位 (`0..=16383`); `target_slot` 为 `null` 时跟随 `key_prefix` 默认哈希 |
| **混合** | `set/get/del/mget/incr/expire` 及扩展命令 | 40/40/5/10/3/2 | 权重自动归一化; 默认胶囊 = `defaults.mix` 里权重大于 0 的命令. 控制台从 `/api/commands` 动态拉取引擎可发压命令. `KEYS` 等全表扫描命令不在目录中; 选 INFO / SMEMBERS 等重命令会弹窗提醒, 仍可加入 mix |
| **行为** | `timeout_ms` | 1000 | 命令超时时间 (毫秒) |

`[[presets]]` 预设可自由增删. 预设中定义了 `commands` 则整表替换, 未列出的命令权重自动视为 0.

- 改表单不影响正在运行的任务, 也不会自动停止. 运行中或暂停时修改草稿, 顶栏会出现「草稿已改 · 终止后点启动才生效」. 顶栏主操作按钮包含: 未运行「启动」(按表单开任务), 运行中「暂停」(停发, 保留连接), 暂停「继续」(恢复派发). 终止任务点击状态徽章上的 ×, 销毁 worker 回到未运行状态. 悬停叹号可查看当前任务全参数快照.
- 令牌桶按 `target_ops` 限速发卡, 这是发压上限而非机器性能承诺; 实际发出速率还受 `连接数 × pipeline / 往返时延` 限制. 设为 `0` 表示不限速全速发压.
- 部署脚本 (`aikv/deploy/loadgen.sh`): 支持 `build`, `up`, `down`, `status`; `up` 命令支持 `-b|--bind` 与 `-f|--config`. 二进制另有 `--log` 与 `--web-dir`, 脚本内部已自动固定 web 目录. 加压目标 (mode, 地址, ops, mix, 预设) 均来自配置文件或内置默认值. 集群模式只需填任意一个节点入口, 其余节点由 `CLUSTER SLOTS` 自动感知拓扑.

---

## 运行边界

- **无鉴权无 TLS**: 控制台与被测端口默认设计为仅在 loopback 或可信私有网段暴露 (严格对齐 AiKv v1 安全边界).
- **UI 纯状态展示**: 界面仅展示探活状态, 拓扑信息与加压泵状态; 失败信息使用底部浮窗提示, 不在客户端内做性能统计图表, 性能表现以 Grafana 为准.
- **实际达成速率**: 引擎实际达成速率 = min(限速目标, 连接数 × pipeline / 往返时延, 引擎处理能力); 以 Grafana 采集的物理 OPS 为准.
- **探活机制**: 探活后台每 30 秒对 endpoint 发起 PING (集群模式额外读取 `CLUSTER INFO`); 点击启动前若种子节点不通或 `cluster_state` 非 `ok`, 会直接拒绝启动任务.
- **故障自愈与熔断**: 运行中单个从节点离线不会中断全局发压; 全部节点不可达或集群陷入 `fail` 状态时自动暂停派发 (worker 连接池保持), 探活恢复后点击「继续」即可无缝恢复. 如需彻底释放所有连接, 点击状态徽章上的 × 即可.

---

## 开发与测试

```bash
cargo fmt --check
RUSTFLAGS='-D warnings' cargo clippy --all-targets
cargo test             # 单元测试 (配置校验, 负载计划, 令牌桶, HTTP 契约)
```

代码质量门禁与 `aikv` 主仓标准完全一致 (按目录层级继承 `rustfmt` 与 `clippy` 规则); 本 crate 为独立 workspace, 不参与主仓库的 `cargo test --workspace`.
