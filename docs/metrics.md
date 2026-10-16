---
name: aikv-metrics
description: AiKv 全量 OTel 指标总表 (仅当前已存在指标). 查 aikv_* 与进程级指标的名称, 标签基数, 类型, 单位, 数据源, 代码位置与典型 PromQL 时读本文; bench 选指标或改埋点前定位调用点也读本文.
---

# AiKv Metrics (指标总表)

## 何时读本文

- 查询 `aikv_*` 与进程级指标的名称, 标签与基数, 类型, 单位, 埋点位置与典型 PromQL
- bench / 监控面板选指标, 或改埋点前定位代码
- **不覆盖**: INFO 字段 ↔ PromQL ↔ redis_exporter 对照 → [observability-reference.md](modules/08-observability-reference.md)
- **不覆盖**: 待新增指标 (缺口清单) 与复合指标计算方式 → 工作区根目录 `bench-metrics-audit.md`
- **不覆盖**: `aidb_*` 引擎指标 (经 aikv 进程统一导出) → 末节链接

## 命名与采样口径

- 指标名统一使用 **Prometheus 渲染名** (下划线形式); OTLP 原生属性为点号形式 (`aikv.command.name`, `process.cpu.time` 等), bench 直查 OTLP 后端 (非经 Collector 转 Prometheus) 时名称不同.
- 类型统一按 Prometheus 口径标注 (Counter / Gauge / Histogram, UpDownCounter 归为 Gauge); 单位为代码中声明的 UCUM 单位.
- 全部指标需 `--features monitoring` 编译; 集群指标额外需 `--features cluster`. instrument 集中定义于 `src/server/otel_metrics/mod.rs`.
- 表格列说明:
  - **标签与基数**: 标注关键标签名, 枚举值范围及预期基数, 便于防范高基数风险.
  - **数据源与代码位置**: 保留准确的更新方式与源码行号, 便于研发定位调用与开销.
  - **说明 (实现陷阱与口径)**: 重点记录采样失真, Stub 占位, 窗口平均等底层技术陷阱.
  - **用途与典型 PromQL**: 提供直接可用的 PromQL 表达式与监控/告警用途.

**采样架构** (bench 必读):

- 热路径只写 `ServerMetrics` atomics (真源, 同时是 `INFO` 的数据源); OTel 侧由后台 **15 s 循环** (`src/main.rs:702-709`) 经 `sync_otel_from_server_metrics` 差分写入 — counter 只支持 `add(delta)`, 依赖 `SyncSnapshot` 保存上次累计值.
- OTLP `PeriodicReader` 亦硬编码 15 s (`src/server/otel.rs:222-224`). 30~60 s 单轮短跑仅 2~4 个采样点; 精确单轮数据优先用 `INFO` 前后快照差值.
- 部分指标因此是**窗口聚合值而非逐条记录**, 详见各表「说明」列.

## 服务指标

| 指标名 | 类型 | 单位 | 标签与基数 | 数据源与代码位置 | 说明 (实现陷阱与口径) | 用途与典型 PromQL |
| --- | --- | --- | --- | --- | --- | --- |
| `aikv_commands_total` | Counter | 1 | `aikv_command_name(~50)` · `aikv_command_status(2: ok\|error)`<br>基数约 100 | `sync_commandstats` 15 s 差分 (commandstats 真源)<br>`src/server/otel_metrics/mod.rs` (定义); `sync_commandstats:497` | 命令名大写 | QPS: `rate(aikv_commands_total[1m])`<br>错误率: `sum(rate(aikv_commands_total{aikv_command_status="error"}[1m])) / sum(rate(aikv_commands_total[1m]))` |
| `aikv_command_duration_seconds` | Histogram | s | 同上 (基数约 100) | 15 s 窗口平均化: 以 `delta_usec / delta_calls` 重复记录 `delta_calls` 次<br>`mod.rs:157-162,531-542` | 14 桶 (5 ms~10 s); **观察值是窗口平均, 仅趋势参考**; 服务端精确分位数走 `INFO latencystats` (真实 21 桶直方图, 现渲染 p50/p99/p999, p95/max 已算未渲染); **P95 与 Max 只能由 bench 客户端测得** | 服务端延迟趋势参考:<br>`histogram_quantile(0.99, sum by (le, aikv_command_name) (rate(aikv_command_duration_seconds_bucket[5m])))` |
| `aikv_connections_total` | Counter | 1 | — (基数 1) | `sync_counters` 差分<br>`mod.rs:163-167,598` | 累计接受的 TCP 连接 | 连接建立速率 (新建连接/秒):<br>`rate(aikv_connections_total[1m])` |
| `aikv_connected_clients` | Gauge | 1 | — (基数 1) | `updown_delta` 差分 (UpDownCounter)<br>`mod.rs:168-172,603` | 当前活跃客户端连接 | 并发连接数与并发阶梯压测监测:<br>`aikv_connected_clients` |
| `aikv_rejected_connections_total` | Counter | 1 | — (基数 1) | 差分写入<br>`mod.rs:173-177,621` | 因 `max_clients` 上限被拒 | 连接过载告警 / 性能悬崖:<br>`rate(aikv_rejected_connections_total[1m]) > 0` |
| `aikv_keyspace_hits_total` | Counter | 1 | — (基数 1) | 差分写入<br>`mod.rs:178-182,611` | 键查找成功命中总数 | 键命中率分子:<br>`rate(aikv_keyspace_hits_total[1m]) / (rate(aikv_keyspace_hits_total[1m]) + rate(aikv_keyspace_misses_total[1m]))` |
| `aikv_keyspace_misses_total` | Counter | 1 | — (基数 1) | 差分写入<br>`mod.rs:183-187,616` | 键查找未命中总数 | 键命中率分母项 (配合 hits 算命中率) |
| `aikv_expired_keys_total` | Counter | 1 | — (基数 1) | `drain_expired_keys` 批量差分<br>`mod.rs:188-192,629` | TTL 过期删除总数 | 过期清理速率:<br>`rate(aikv_expired_keys_total[1m])` |
| `aikv_evicted_keys_total` | Counter | 1 | — (基数 1) | 差分写入<br>`mod.rs:193-197,634` | **Stub 恒 0** (无 LRU 内存驱逐, atomics 无递增点); Redis 兼容字段, 勿当真实数据 | 占位兼容字段 (避免面板报空) |
| `aikv_net_input_bytes_total` | Counter | By | — (基数 1) | 差分写入<br>`mod.rs:198-202,639` | 客户端网络入站字节 | 网络入站带宽 (MB/s):<br>`rate(aikv_net_input_bytes_total[1m]) / 1024 / 1024` |
| `aikv_net_output_bytes_total` | Counter | By | — (基数 1) | 差分写入<br>`mod.rs:203-207,644` | 客户端网络出站字节 | 网络出站带宽 (MB/s):<br>`rate(aikv_net_output_bytes_total[1m]) / 1024 / 1024` |
| `aikv_slow_queries_total` | Counter | 1 | `aikv_command_name(~50)`<br>基数约 50 | commandstats `slowlog_count` 差分<br>`mod.rs:208-212,544` | 阈值由 `slowlog-log-slower-than` 控制, 默认 100 ms (`src/server/slowlog.rs:8`) | 慢查询频次与命令分布:<br>`rate(aikv_slow_queries_total[1m])` |
| `aikv_lua_scripts_total` | Counter | 1 | — (基数 1) | `lua_execution_count` 差分<br>`mod.rs:228-232,649` | Lua 脚本执行次数 | Lua 执行速率:<br>`rate(aikv_lua_scripts_total[1m])` |
| `aikv_lua_execution_duration_seconds` | Histogram | s | — (基数 1) | 单次 observation = 15 s 窗口内**累计**时长 (`delta_us / 1e6`)<br>`mod.rs:233-237,654` | **非单次执行耗时分布, 桶语义为 15 s 窗口内总耗时** | Lua 耗时趋势参考 (勿用于计算单次 P99) |
| `aikv_json_commands_total` | Counter | 1 | `aikv_command_name(~20)`<br>基数约 20 | `JSON.*` 子命令计数派生 (大写)<br>`mod.rs:238-242,555-567` | JSON 模块子命令计数 | JSON 命令用量与分布:<br>`sum by (aikv_command_name) (rate(aikv_json_commands_total[1m]))` |
| `aikv_db_keys` | Gauge | 1 | `aikv_db_index(16: 0~15)`<br>基数 16 | `storage.db_key_counts()` 15 s 轮询<br>`mod.rs:146-150,290` | 各逻辑 DB 的 `len()`, 引擎条目估值, 含尚未惰性清理的过期 key, 不是精确用户 key 数 | 逻辑数据规模 / SA 分母素材:<br>`sum(aikv_db_keys)` |
| `aikv_used_memory_bytes` | Gauge | By | — (基数 1) | `storage.memory_usage_bytes()` 轮询 → Observable 回调<br>`mod.rs:687`; `sync_stats_gauges:475` | memtable/cache 近似 | 数据集内存占用规模 (MB):<br>`aikv_used_memory_bytes / 1024 / 1024` |
| `aikv_used_memory_peak_bytes` | Gauge | By | — (基数 1) | 同上<br>`mod.rs:696` | 历史峰值 | 内存峰值观测:<br>`aikv_used_memory_peak_bytes` |
| `aikv_instantaneous_ops_per_sec` | Gauge | {request}/s | — (基数 1) | 15 s 滑动窗口采样 → Observable 回调<br>`mod.rs:705` | 15 s 窗口计算, 短跑压测容易失真 | 瞬时 QPS 参考 (平稳运行时直读) |
| `aikv_blocked_clients` | Gauge | 1 | — (基数 1) | atomics → Observable 回调<br>`mod.rs:714` | BLPOP 等阻塞等待连接 | 并发阶梯压测监测阻塞:<br>`aikv_blocked_clients > 0` |
| `aikv_uptime_seconds` | Gauge | s | — (基数 1) | 启动时间计算 → Observable 回调<br>`mod.rs:723` | 进程运行秒数 | 冷启动恢复 RTO 判定辅助 (配合 `/health`):<br>`aikv_uptime_seconds` |

## 进程级指标

自研 `aikv_process_*` 系列与 OTel semconv 标准系列, 均由 15 s 循环读取 `/proc` 差分写入 (`src/server/metrics.rs:455` `refresh_process_metrics`; 读取实现在 `src/server/process_metrics.rs`).

| 指标名 | 类型 | 单位 | 标签与基数 | 数据源与代码位置 | 说明 (实现陷阱与口径) | 用途与典型 PromQL |
| --- | --- | --- | --- | --- | --- | --- |
| `aikv_process_resident_memory_bytes` | Gauge | By | — (基数 1) | `/proc/self/status` VmRSS → Observable 回调<br>`mod.rs:732`; `process_metrics.rs:7` | 进程真实物理内存 (VmRSS) | 进程物理内存占用 (MB):<br>`aikv_process_resident_memory_bytes / 1024 / 1024` |
| `aikv_process_cpu_milliseconds_total` | Counter | ms | — (基数 1) | `/proc/self/stat` utime+stime 差分<br>`mod.rs:213-217,416-432` | legacy, 与 `process_cpu_time_seconds_total` 同源 | CPU 消耗 (旧面板兼容):<br>`rate(aikv_process_cpu_milliseconds_total[1m]) / 1000` |
| `aikv_process_read_bytes_total` | Counter | By | — (基数 1) | `/proc/self/io` `read_bytes` 差分<br>`mod.rs:218-222,434-445` | 真正下块层的读字节; 集群模式混入 Raft 字节 | 磁盘读带宽 (MB/s):<br>`rate(aikv_process_read_bytes_total[1m]) / 1024 / 1024`<br>RA 粗粒度过渡分子 |
| `aikv_process_write_bytes_total` | Counter | By | — (基数 1) | `/proc/self/io` `write_bytes` 差分<br>`mod.rs:223-227,434-445` | 真正下块层的写字节; 集群模式混入 Raft 复制/快照字节 | 磁盘写带宽 (MB/s):<br>`rate(aikv_process_write_bytes_total[1m]) / 1024 / 1024`<br>WA 粗粒度过渡分子 |
| `process_cpu_time_seconds_total` | Counter | s | `cpu_mode(2: user\|system)`<br>基数 2 | `/proc/self/stat` 差分 (OTLP 名 `process.cpu.time`)<br>`mod.rs:261-265,416-425` | OTel semconv 标准语义 | CPU usr/sys 占比与每秒核数消耗:<br>`sum by (cpu_mode) (rate(process_cpu_time_seconds_total[1m]))` |
| `process_memory_usage_bytes` | Gauge | By | `process_memory_state(1: used)`<br>基数 1 | `/proc/self/status` VmRSS (OTLP 名 `process.memory.usage`)<br>`mod.rs:266-270,406-414,483` | 代码两处写入均带标签; **查询时留意并存的无标签孪生 series** (来源为导出链路, 代码中不存在) | 进程 RSS (标准语义):<br>`process_memory_usage_bytes{process_memory_state="used"}` |
| `process_disk_io_bytes_total` | Counter | By | `disk_io_direction(2: read\|write)`<br>基数 2 | `/proc/self/io` 差分 (OTLP 名 `process.disk.io`)<br>`mod.rs:271-275,434-445` | OTel semconv, 与自研 read/write 双写 | 进程磁盘 I/O (标准语义):<br>`sum by (disk_io_direction) (rate(process_disk_io_bytes_total[1m]))` |

## 集群指标 (`monitoring` + `cluster`)

| 指标名 | 类型 | 单位 | 标签与基数 | 数据源与代码位置 | 说明 (实现陷阱与口径) | 用途与典型 PromQL |
| --- | --- | --- | --- | --- | --- | --- |
| `aikv_cluster_redirects_total` | Counter | 1 | `aikv_cluster_redirect_type(2: moved\|ask)`<br>基数 2 | `CLUSTER.redirect.*` 命令计数派生<br>`mod.rs:243-248,569-584`; `helpers.rs:10` | **实际取值仅 `moved\|ask`** (小写, `command/router/mod.rs:395,401` 仅两处调用) | 集群重定向频次 (客户端路由命中率):<br>`sum by (aikv_cluster_redirect_type) (rate(aikv_cluster_redirects_total[1m]))` |
| `aikv_gossip_messages_total` | Counter | 1 | — (基数 1) | `command_ok_count("GOSSIP.tick")` 派生<br>`mod.rs:249-254,663-671` | 非独立埋点, 由命令计数派生 | Gossip 拓扑心跳速率:<br>`rate(aikv_gossip_messages_total[1m])` |
| `aikv_failover_total` | Counter | 1 | — (基数 1) | `command_ok_count("CLUSTER.failover")` 派生<br>`mod.rs:255-260,672-679` | 非独立埋点, 由命令计数派生 | 主从切换事件总数:<br>`aikv_failover_total` |

## aidb_* 引擎指标 (经 aikv 统一导出)

aikv 在 `init_otel` 设置 global `MeterProvider` 后调用 `aidb::metrics::init()` (`src/server/otel.rs:232`), `aidb_*` 引擎指标与 `aikv_*` 走同一 15 s PeriodicReader 与 OTLP endpoint (aikv 进程是唯一出口).

其名称, 标签与埋点位置见 [../aidb/docs/metrics.md](../../aidb/docs/metrics.md).
