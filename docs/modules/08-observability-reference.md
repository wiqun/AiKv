---
name: aikv-observability-reference
description: AiKv OTel aikv_* 指标目录与 INFO 字段对照表. 查询指标名、标签, 或 aikv 可观测性的 INFO↔PromQL 映射时查阅.
---

# AiKv Observability Reference (可观测性参考)

> **主架构文档**: [observability.md](07-observability.md) · **AiDb 存储指标**: [AiDb Observability 文档](https://github.com/wiqun/AiDb/blob/main/docs/modules/05-observability.md).

---

## 1. OTel Resource 与 Trace

- **Scope Name**: `otel_scope_name = "aikv"`
- **UCUM 统一单位**: `s` (秒), `ms` (毫秒), `By` (字节), `1` (计数/比率), `{request}/s` (速率)
- **PromQL Label 映射**: OTel 属性中的点号 `.` 在 Prometheus 中映射为下划线 `_` (例如 `aikv.command.status` → `aikv_command_status`).

---

## 2. 全量 `aikv_*` OTel 指标字典 (`feature = "monitoring"`)

| 指标名称 | 指标类型 | 单位 (UCUM) | PromQL 标签 (Labels) | 对应 `ServerMetrics` 数据源 | 详细说明 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `aikv_commands_total` | Counter | `1` | `aikv_command_name`, `aikv_command_status` | `on_command` | 累计处理的命令总数 (分命令与 status: ok/error) |
| `aikv_command_duration_seconds` | Histogram | `s` | `aikv_command_name`, `aikv_command_status` | `on_command_duration` | 命令执行耗时直方图 (14 个分桶: 5ms~10s) |
| `aikv_connections_total` | Counter | `1` | — | `on_connect` | 累计建立的客户端 TCP 连接总数 |
| `aikv_connected_clients` | UpDownCounter | `1` | — | `connect` / `disconnect` | 当前活跃的客户端连接数 |
| `aikv_rejected_connections_total` | Counter | `1` | — | `on_rejected_connection` | 因达到 `max_clients` 上限而被拒绝的连接数 |
| `aikv_used_memory_bytes` | Gauge | `By` | — | `set_memory_bytes` | 当前进程已使用的内存字节数 (RSS) |
| `aikv_used_memory_peak_bytes` | Gauge | `By` | — | `set_memory_bytes` | 进程历史内存使用峰值字节数 |
| `aikv_keyspace_hits_total` | Counter | `1` | — | `on_keyspace_hit` | Key 查询命中总次数 |
| `aikv_keyspace_misses_total` | Counter | `1` | — | `on_keyspace_miss` | Key 查询未命中总次数 |
| `aikv_expired_keys_total` | Counter | `1` | — | `record_expired_keys` | 累计 TTL 过期删除的 Key 总数 |
| `aikv_evicted_keys_total` | Counter | `1` | — | 恒为 `0` | 内存淘汰 Key 计数 (AiKv 无 LRU 驱逐, 恒为 0) |
| `aikv_instantaneous_ops_per_sec` | Gauge | `{ops}/s` | — | `sample_instantaneous_ops` | 最近 15 秒瞬时 QPS 采样值 |
| `aikv_blocked_clients` | Gauge | `1` | — | `on_client_blocked` | 当前处于阻塞等待 (BLPOP 等) 的客户端连接数 |
| `aikv_db_keys` | Gauge | `1` | `aikv_db_index` | `set_db_key_count` | 各逻辑数据库当前保存的 Key 总数 |
| `aikv_net_input_bytes_total` | Counter | `By` | — | `on_net_input_bytes` | 网络接收字节总数 |
| `aikv_net_output_bytes_total` | Counter | `By` | — | `on_net_output_bytes` | 网络发送字节总数 |
| `aikv_slow_queries_total` | Counter | `1` | `aikv_command_name` | `on_slow_query` | 超过慢查询阈值 (100ms) 的命令总数 |
| `aikv_uptime_seconds` | Gauge | `s` | — | 启动时间计算 | 服务连续运行时长 (秒) |
| `aikv_lua_scripts_total` | Counter | `1` | — | `on_lua_command` | Lua 脚本执行总次数 |
| `aikv_lua_execution_duration_seconds` | Histogram | `s` | — | `on_lua_execution` | Lua 脚本执行耗时直方图 |
| `aikv_json_commands_total` | Counter | `1` | `aikv_command_name` | `on_json_command` | JSON 系列命令调用总次数 |
| `aikv_cluster_redirects_total` | Counter | `1` | `aikv_cluster_redirect_type` | `on_cluster_redirect` | 集群重定向次数 (`type: moved/ask/crossslot`) |
| `aikv_gossip_messages_total` | Counter | `1` | — | `on_gossip_refresh` | Gossip 拓扑刷新心跳总数 |
| `aikv_failover_total` | Counter | `1` | — | `on_failover` | 集群发生的主从切换事件总数 |

---

## 3. 标准进程级指标 (Process Semconv)

| OTel 标准指标名 | 类型 | PromQL 导出名称 | 标签 (Labels) | 采集来源 |
| :--- | :--- | :--- | :--- | :--- |
| `process.cpu.time` | Counter | `process_cpu_time_seconds_total` | `cpu_mode="user"`, `cpu_mode="system"` | `/proc/self/stat` |
| `process.memory.usage` | Gauge | `process_memory_usage_bytes` | `process_memory_state="used"` | `/proc/self/status` VmRSS |
| `process.disk.io` | Counter | `process_disk_io_bytes_total` | `disk_io_direction="read"`, `"write"` | `/proc/self/io` |

---

## 4. INFO ↔ `aikv_*` ↔ redis_exporter 对照表 (Redis 8.8 基线)

### 4.1 Stats 段

| INFO 字段名 | `aikv_*` PromQL 映射 | redis_exporter 对应字段 | 字段性质与说明 |
| :--- | :--- | :--- | :--- |
| `total_commands_processed` | `sum(aikv_commands_total)` | `redis_commands_processed_total` | **真实值**: 累计执行成功的客户端命令总数 |
| `total_error_replies` | `sum(aikv_commands_total{aikv_command_status="error"})` | `redis_error_replies_total` | **真实值**: 执行出错的命令总数 |
| `keyspace_hits` | `aikv_keyspace_hits_total` | `redis_keyspace_hits_total` | **真实值**: Key 查询命中次数 |
| `keyspace_misses` | `aikv_keyspace_misses_total` | `redis_keyspace_misses_total` | **真实值**: Key 查询未命中次数 |
| `expired_keys` | `aikv_expired_keys_total` | `redis_expired_keys_total` | **真实值**: 惰性及定时过期的 Key 总数 |
| `evicted_keys` | `aikv_evicted_keys_total` | `redis_evicted_keys_total` | **Stub (0)**: AiKv 不做 LRU 内存驱逐 |
| `instantaneous_ops_per_sec` | `aikv_instantaneous_ops_per_sec` | `redis_instantaneous_ops_per_sec` | **真实值**: 15 秒滑动窗口 QPS |
| `total_net_input_bytes` | `aikv_net_input_bytes_total` | `redis_net_input_bytes_total` | **真实值**: 网络输入总字节数 |
| `total_net_output_bytes` | `aikv_net_output_bytes_total` | `redis_net_output_bytes_total` | **真实值**: 网络输出总字节数 |
| `rejected_connections` | `aikv_rejected_connections_total` | `redis_rejected_connections_total` | **真实值**: 因 max_clients 拒绝的连接数 |
| `slowlog_commands_count` | `sum(aikv_slow_queries_total)` | `redis_slowlog_commands_count` | **真实值**: 触发 Slowlog 的慢查询总数 |

### 4.2 Clients / Memory 段

| INFO 字段名 | `aikv_*` PromQL 映射 | redis_exporter 对应字段 | 字段性质与说明 |
| :--- | :--- | :--- | :--- |
| `connected_clients` | `aikv_connected_clients` | `redis_connected_clients` | **真实值**: 当前活跃 TCP 连接数 |
| `maxclients` | — | `redis_max_clients` | **真实值**: 配置的最大连接数 (`--max-clients`) |
| `blocked_clients` | `aikv_blocked_clients` | `redis_blocked_clients` | **真实值**: 阻塞在 BLPOP 等命令的连接数 |
| `used_memory` | `aikv_used_memory_bytes` | `redis_memory_used_bytes` | **真实值**: 进程 RSS 物理内存 |
| `used_memory_peak` | `aikv_used_memory_peak_bytes` | `redis_memory_used_peak_bytes` | **真实值**: 历史最高内存使用 |
| `mem_fragmentation_ratio` | — | `redis_mem_fragmentation_ratio` | **Stub (1.00)**: 碎片率基准值 |

### 4.3 Server / Replication / Cluster 段

| INFO 字段名 | 实际输出示例 | 字段性质与说明 |
| :--- | :--- | :--- |
| `redis_version` | `8.8` | **真实值**: 客户端用来判断命令能力的 Redis 兼容版本。包版本不放在这个字段，否则只认 `redis_version >= 2.8` 的客户端不会发 SCAN |
| `redis_compatible_version` | `8.8` | **真实值**: 对齐的 Redis 兼容协议版本 |
| `role` | `master` / `slave` | **真实值**: 节点在当前分片中的角色 |
| `cluster_enabled` | `1` / `0` | **真实值**: 是否启用了 Redis Cluster 模式 |
| `cluster_state` | `ok` / `fail` | **真实值**: 动态计算所有槽位是否覆盖且均有 Leader |
