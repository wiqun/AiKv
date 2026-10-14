//! 连接级指标 (`ServerMetrics`, Phase 8 Atomic): INFO 与业务计数的唯一热路径真源.
//! monitoring 下经 `info_catalog` refresh delta 同步到 OTel (`OtelMetrics`), 相对 INFO
//! 最多滞后 ~15s. `error_stats` 供 INFO errorstats (错误前缀, 非命令名); 全局
//! `slowlog_commands_*` 与逐命令 commandstats `slowlog_*` 字段并存
//! (Redis 8.8 stats + commandstats 语义).
//!
//! # Invariant
//!
//! - INFO 字段须与同期 `ServerMetrics` atomics 一致; 禁止在 `InfoRenderer` 内独立计数公式.
//! - 钩子分工: 命令出口 `on_command_done` 只记一次 calls 和真实耗时.
//!   点查 hit/miss 在存储 `get_typed` 返回时记, 命令层不再为同一次点查加第二次.
//! - 跟踪排除: `PING|ECHO|HELLO|QUIT|MONITOR|SLOWLOG` 不经 `record_command_observability`.
//! - 客户端 commandstats: `is_client_command` 过滤含 `.` 的内部伪命令
//!   (如 `GOSSIP.tick`), 不进 INFO commandstats.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use aidb::statistics::{LatencyHistogram, LatencySnapshot, COMMAND_DURATION_BOUNDS_US};

use crate::server::latency::CommandLatencySnapshot;
use crate::storage::observation::PointReadCounter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    Moved,
    Ask,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CommandTotals {
    pub(crate) ok: u64,
    pub(crate) err: u64,
    pub(crate) usec: u64,
    pub(crate) rejected: u64,
    pub(crate) slowlog_count: u64,
    pub(crate) slowlog_time_ms_sum: u64,
    pub(crate) slowlog_time_ms_max: u64,
}

/// 客户端可见命令统计 (排除 GOSSIP.tick 等内部伪命令).
fn is_client_command(command: &str) -> bool {
    !command.contains('.')
}

#[derive(Debug)]
struct CommandLatencySlot {
    ok: LatencyHistogram,
    err: LatencyHistogram,
    max_us: AtomicU64,
}

impl CommandLatencySlot {
    fn new() -> Self {
        Self {
            ok: LatencyHistogram::default(),
            err: LatencyHistogram::default(),
            max_us: AtomicU64::new(0),
        }
    }

    fn note_max(&self, duration_us: u64) {
        let mut current = self.max_us.load(Ordering::Relaxed);
        while duration_us > current {
            match self.max_us.compare_exchange(
                current,
                duration_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(v) => current = v,
            }
        }
    }
}

impl Default for CommandLatencySlot {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ServerMetrics {
    connections_total: AtomicU64,
    connected_clients: AtomicUsize,
    keyspace_hits: AtomicU64,
    keyspace_misses: AtomicU64,
    lua_execution_duration_us: AtomicU64,
    lua_execution_count: AtomicU64,
    commands_total: DashMap<String, CommandTotals>,
    net_input_bytes: AtomicU64,
    net_output_bytes: AtomicU64,
    expired_keys: AtomicU64,
    rejected_connections: AtomicU64,
    used_memory_bytes: AtomicU64,
    used_memory_peak_bytes: AtomicU64,
    evicted_keys: AtomicU64,
    instantaneous_ops_per_sec: AtomicU64,
    ops_last_commands: AtomicU64,
    ops_last_sample_secs: AtomicU64,
    cluster_messages_sent: AtomicU64,
    cluster_messages_received: AtomicU64,
    blocked_clients: AtomicUsize,
    uptime_secs: AtomicU64,
    cached_rss_bytes: AtomicU64,
    cached_total_system_memory: AtomicU64,
    cached_process_threads: AtomicU64,
    cached_voluntary_ctxt_switches: AtomicU64,
    cached_nonvoluntary_ctxt_switches: AtomicU64,
    net_last_input_bytes: AtomicU64,
    net_last_output_bytes: AtomicU64,
    net_last_sample_secs: AtomicU64,
    instantaneous_input_kbps: AtomicU64,
    instantaneous_output_kbps: AtomicU64,
    slowlog_commands_count: AtomicU64,
    slowlog_commands_time_ms_sum: AtomicU64,
    slowlog_commands_time_ms_max: AtomicU64,
    error_stats: DashMap<String, u64>,
    db_key_counts: DashMap<usize, u64>,
    command_latency: DashMap<&'static str, CommandLatencySlot>,
    local_point_hits: AtomicU64,
    local_point_misses: AtomicU64,
    shared_point_reads: OnceLock<Arc<PointReadCounter>>,
    scan_entries: AtomicU64,
    connection_closes: DashMap<&'static str, u64>,
    accept_errors: DashMap<String, u64>,
    redirect_moved: AtomicU64,
    redirect_ask: AtomicU64,
    propose_duration: DashMap<u64, LatencyHistogram>,
    #[cfg(feature = "monitoring")]
    process_last_cpu_user_seconds_bits: AtomicU64,
    #[cfg(feature = "monitoring")]
    process_last_cpu_sys_seconds_bits: AtomicU64,
    #[cfg(feature = "monitoring")]
    process_last_read_bytes: AtomicU64,
    #[cfg(feature = "monitoring")]
    process_last_write_bytes: AtomicU64,
    #[cfg(feature = "monitoring")]
    process_last_voluntary_ctxt_switches: AtomicU64,
    #[cfg(feature = "monitoring")]
    process_last_nonvoluntary_ctxt_switches: AtomicU64,
    #[cfg(feature = "monitoring")]
    otel: Option<Arc<super::otel_metrics::OtelMetrics>>,
}

impl Default for ServerMetrics {
    fn default() -> Self {
        Self {
            connections_total: AtomicU64::new(0),
            connected_clients: AtomicUsize::new(0),
            keyspace_hits: AtomicU64::new(0),
            keyspace_misses: AtomicU64::new(0),
            lua_execution_duration_us: AtomicU64::new(0),
            lua_execution_count: AtomicU64::new(0),
            commands_total: DashMap::new(),
            net_input_bytes: AtomicU64::new(0),
            net_output_bytes: AtomicU64::new(0),
            expired_keys: AtomicU64::new(0),
            rejected_connections: AtomicU64::new(0),
            used_memory_bytes: AtomicU64::new(0),
            used_memory_peak_bytes: AtomicU64::new(0),
            evicted_keys: AtomicU64::new(0),
            instantaneous_ops_per_sec: AtomicU64::new(0),
            ops_last_commands: AtomicU64::new(0),
            ops_last_sample_secs: AtomicU64::new(0),
            cluster_messages_sent: AtomicU64::new(0),
            cluster_messages_received: AtomicU64::new(0),
            blocked_clients: AtomicUsize::new(0),
            uptime_secs: AtomicU64::new(0),
            cached_rss_bytes: AtomicU64::new(0),
            cached_total_system_memory: AtomicU64::new(0),
            cached_process_threads: AtomicU64::new(0),
            cached_voluntary_ctxt_switches: AtomicU64::new(0),
            cached_nonvoluntary_ctxt_switches: AtomicU64::new(0),
            net_last_input_bytes: AtomicU64::new(0),
            net_last_output_bytes: AtomicU64::new(0),
            net_last_sample_secs: AtomicU64::new(0),
            instantaneous_input_kbps: AtomicU64::new(0),
            instantaneous_output_kbps: AtomicU64::new(0),
            slowlog_commands_count: AtomicU64::new(0),
            slowlog_commands_time_ms_sum: AtomicU64::new(0),
            slowlog_commands_time_ms_max: AtomicU64::new(0),
            error_stats: DashMap::new(),
            db_key_counts: DashMap::new(),
            command_latency: DashMap::new(),
            local_point_hits: AtomicU64::new(0),
            local_point_misses: AtomicU64::new(0),
            shared_point_reads: OnceLock::new(),
            scan_entries: AtomicU64::new(0),
            connection_closes: DashMap::new(),
            accept_errors: DashMap::new(),
            redirect_moved: AtomicU64::new(0),
            redirect_ask: AtomicU64::new(0),
            propose_duration: DashMap::new(),
            #[cfg(feature = "monitoring")]
            process_last_cpu_user_seconds_bits: AtomicU64::new(0),
            #[cfg(feature = "monitoring")]
            process_last_cpu_sys_seconds_bits: AtomicU64::new(0),
            #[cfg(feature = "monitoring")]
            process_last_read_bytes: AtomicU64::new(0),
            #[cfg(feature = "monitoring")]
            process_last_write_bytes: AtomicU64::new(0),
            #[cfg(feature = "monitoring")]
            process_last_voluntary_ctxt_switches: AtomicU64::new(0),
            #[cfg(feature = "monitoring")]
            process_last_nonvoluntary_ctxt_switches: AtomicU64::new(0),
            #[cfg(feature = "monitoring")]
            otel: None,
        }
    }
}

impl ServerMetrics {
    /// 关联 OTel 指标实例 (仅在 monitoring feature 下生效).
    #[cfg(feature = "monitoring")]
    pub fn with_otel(mut self, otel: Arc<super::otel_metrics::OtelMetrics>) -> Self {
        self.otel = Some(otel);
        self
    }

    /// OTel 镜像句柄 (refresh / info_catalog sync).
    #[cfg(feature = "monitoring")]
    pub fn otel_handle(&self) -> Option<&Arc<super::otel_metrics::OtelMetrics>> {
        self.otel.as_ref()
    }

    pub fn on_connect(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
        self.connected_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_disconnect(&self) {
        self.connected_clients.fetch_sub(1, Ordering::Relaxed);
    }

    /// 客户端进入阻塞命令等待 (BLPOP 等).
    pub fn on_client_blocked(&self) {
        self.blocked_clients.fetch_add(1, Ordering::Relaxed);
    }

    /// 客户端离开阻塞命令等待.
    pub fn on_client_unblocked(&self) {
        self.blocked_clients.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn on_command(&self, command: &str, ok: bool) {
        let mut entry = self
            .commands_total
            .entry(command.to_ascii_uppercase())
            .or_default();
        if ok {
            entry.ok += 1;
        } else {
            entry.err += 1;
        }
    }

    /// 解析 Redis 错误前缀 (首 token), 如 `ERR`, `WRONGTYPE`.
    pub fn parse_error_prefix(message: &str) -> &str {
        message.split_whitespace().next().unwrap_or("ERR")
    }

    /// INFO errorstats 真源: 按错误前缀聚合.
    pub fn on_error_stat(&self, message: &str) {
        let prefix = Self::parse_error_prefix(message).to_ascii_uppercase();
        *self.error_stats.entry(prefix).or_insert(0) += 1;
    }

    pub fn error_stat_totals(&self) -> Vec<(String, u64)> {
        let mut out: Vec<_> = self
            .error_stats
            .iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn on_lua_command(&self, command: &str, ok: bool) {
        let key = format!("LUA.{}", command.to_ascii_lowercase());
        self.on_command(&key, ok);
    }

    pub fn on_lua_execution(&self, duration_us: u64) {
        self.lua_execution_duration_us
            .fetch_add(duration_us, Ordering::Relaxed);
        self.lua_execution_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn lua_execution_duration_us(&self) -> u64 {
        self.lua_execution_duration_us.load(Ordering::Relaxed)
    }

    pub fn lua_execution_count(&self) -> u64 {
        self.lua_execution_count.load(Ordering::Relaxed)
    }

    pub fn lua_command_ok_count(&self, command: &str) -> u64 {
        let key = format!("LUA.{}", command.to_ascii_lowercase());
        self.command_ok_count(&key)
    }

    /// 记录命令耗时 (微秒); INFO commandstats 真源.
    pub fn on_command_duration(&self, command: &str, duration_us: u64, ok: bool) {
        let mut entry = self
            .commands_total
            .entry(command.to_ascii_uppercase())
            .or_default();
        entry.usec = entry.usec.saturating_add(duration_us);
        let _ = ok;
    }

    /// 记录慢查询 (INFO commandstats slowlog_* 由 on_slowlog_command 写入).
    pub fn on_slow_query(&self, _command: &str) {}

    /// INFO commandstats slowlog_* 字段 (Redis 8.8+).
    pub fn on_slowlog_command(&self, command: &str, duration_us: u64) {
        let ms = duration_us / 1000;
        let mut entry = self
            .commands_total
            .entry(command.to_ascii_uppercase())
            .or_default();
        entry.slowlog_count += 1;
        entry.slowlog_time_ms_sum = entry.slowlog_time_ms_sum.saturating_add(ms);
        entry.slowlog_time_ms_max = entry.slowlog_time_ms_max.max(ms);
        self.slowlog_commands_count.fetch_add(1, Ordering::Relaxed);
        self.slowlog_commands_time_ms_sum
            .fetch_add(ms, Ordering::Relaxed);
        let mut current_max = self.slowlog_commands_time_ms_max.load(Ordering::Relaxed);
        while ms > current_max {
            match self.slowlog_commands_time_ms_max.compare_exchange(
                current_max,
                ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(v) => current_max = v,
            }
        }
    }

    /// 设置 uptime 秒数 (仅 monitoring feature).
    #[cfg(not(feature = "monitoring"))]
    pub fn set_uptime_secs(&self, _secs: u64) {}

    #[cfg(not(feature = "monitoring"))]
    pub fn set_db_key_count(&self, _db: usize, _count: u64) {}

    #[cfg(not(feature = "monitoring"))]
    pub fn on_net_input_bytes(&self, bytes: u64) {
        if bytes > 0 {
            self.net_input_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    #[cfg(not(feature = "monitoring"))]
    pub fn on_net_output_bytes(&self, bytes: u64) {
        if bytes > 0 {
            self.net_output_bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    #[cfg(not(feature = "monitoring"))]
    pub fn on_rejected_connection(&self) {
        self.rejected_connections.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(not(feature = "monitoring"))]
    pub fn on_expired_key(&self) {
        self.expired_keys.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(not(feature = "monitoring"))]
    pub fn record_expired_keys(&self, count: u64) {
        if count > 0 {
            self.expired_keys.fetch_add(count, Ordering::Relaxed);
        }
    }

    #[cfg(not(feature = "monitoring"))]
    pub fn set_memory_bytes(&self, current: u64) {
        self.used_memory_bytes.store(current, Ordering::Relaxed);
        let prev_peak = self.used_memory_peak_bytes.load(Ordering::Relaxed);
        if current > prev_peak {
            self.used_memory_peak_bytes
                .store(current, Ordering::Relaxed);
        }
    }

    #[cfg(not(feature = "monitoring"))]
    pub fn sample_instantaneous_ops(&self) {
        self.sample_instantaneous_ops_inner();
    }

    #[cfg(feature = "monitoring")]
    pub fn set_uptime_secs(&self, secs: u64) {
        self.uptime_secs.store(secs, Ordering::Relaxed);
    }

    /// 设置当前内存使用量 (仅 monitoring feature).
    #[cfg(feature = "monitoring")]
    pub fn set_memory_bytes(&self, current: u64) {
        self.used_memory_bytes.store(current, Ordering::Relaxed);
        let prev_peak = self.used_memory_peak_bytes.load(Ordering::Relaxed);
        if current > prev_peak {
            self.used_memory_peak_bytes
                .store(current, Ordering::Relaxed);
        }
    }

    /// 记录过期 key 驱逐 (仅 monitoring feature).
    #[cfg(feature = "monitoring")]
    pub fn on_expired_key(&self) {
        self.expired_keys.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录被拒绝的连接 (仅 monitoring feature).
    #[cfg(feature = "monitoring")]
    pub fn on_rejected_connection(&self) {
        self.rejected_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// 批量记录过期 key 驱逐 (仅 monitoring feature).
    #[cfg(feature = "monitoring")]
    pub fn record_expired_keys(&self, count: u64) {
        if count == 0 {
            return;
        }
        self.expired_keys.fetch_add(count, Ordering::Relaxed);
    }

    #[cfg(feature = "monitoring")]
    pub fn sample_instantaneous_ops(&self) {
        self.sample_instantaneous_ops_inner();
    }

    fn sample_instantaneous_ops_inner(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cmds = self.total_commands_processed();
        let prev_cmds = self.ops_last_commands.swap(cmds, Ordering::Relaxed);
        let prev_time = self.ops_last_sample_secs.swap(now, Ordering::Relaxed);
        if prev_time > 0 && now > prev_time {
            let ops = (cmds.saturating_sub(prev_cmds)) / (now - prev_time);
            self.instantaneous_ops_per_sec.store(ops, Ordering::Relaxed);
        }
        self.sample_instantaneous_net_kbps_inner(now);
    }

    fn sample_instantaneous_net_kbps_inner(&self, now: u64) {
        let input = self.net_input_bytes.load(Ordering::Relaxed);
        let output = self.net_output_bytes.load(Ordering::Relaxed);
        let prev_input = self.net_last_input_bytes.swap(input, Ordering::Relaxed);
        let prev_output = self.net_last_output_bytes.swap(output, Ordering::Relaxed);
        let prev_time = self.net_last_sample_secs.swap(now, Ordering::Relaxed);
        if prev_time > 0 && now > prev_time {
            let secs = now - prev_time;
            let in_kbps = (input.saturating_sub(prev_input) * 8) / (secs * 1024);
            let out_kbps = (output.saturating_sub(prev_output) * 8) / (secs * 1024);
            self.instantaneous_input_kbps
                .store(in_kbps, Ordering::Relaxed);
            self.instantaneous_output_kbps
                .store(out_kbps, Ordering::Relaxed);
        }
    }

    /// 缓存进程 RSS / 系统内存 (INFO memory 真源).
    pub fn refresh_cached_process_info(&self) {
        if let Some(bytes) = crate::server::process_metrics::read_resident_memory_bytes() {
            self.cached_rss_bytes.store(bytes, Ordering::Relaxed);
        }
        if let Some(bytes) = crate::server::process_metrics::read_total_system_memory_bytes() {
            self.cached_total_system_memory
                .store(bytes, Ordering::Relaxed);
        }
        if let Some((threads, vol, nonvol)) =
            crate::server::process_metrics::read_process_threads_and_context_switches()
        {
            self.cached_process_threads
                .store(threads, Ordering::Relaxed);
            self.cached_voluntary_ctxt_switches
                .store(vol, Ordering::Relaxed);
            self.cached_nonvoluntary_ctxt_switches
                .store(nonvol, Ordering::Relaxed);
        }
    }

    /// 获取进程线程数与上下文切换数 (threads, voluntary, nonvoluntary).
    ///
    /// 优先从原子缓存直读；若缓存为 0（如启动初期未完成首次周期刷新），
    /// 则兜底直读 `/proc/self/status` 并回填缓存，与 `cached_rss_bytes` 保持一致兜底语义。
    pub fn get_threads_and_context_switches(&self) -> (u64, u64, u64) {
        let threads = self.cached_process_threads.load(Ordering::Relaxed);
        let vol = self.cached_voluntary_ctxt_switches.load(Ordering::Relaxed);
        let nonvol = self
            .cached_nonvoluntary_ctxt_switches
            .load(Ordering::Relaxed);
        if threads > 0 {
            return (threads, vol, nonvol);
        }
        if let Some((t, v, nv)) =
            crate::server::process_metrics::read_process_threads_and_context_switches()
        {
            self.cached_process_threads.store(t, Ordering::Relaxed);
            self.cached_voluntary_ctxt_switches
                .store(v, Ordering::Relaxed);
            self.cached_nonvoluntary_ctxt_switches
                .store(nv, Ordering::Relaxed);
            (t, v, nv)
        } else {
            (0, 0, 0)
        }
    }

    pub fn cached_rss_bytes(&self) -> u64 {
        let cached = self.cached_rss_bytes.load(Ordering::Relaxed);
        if cached > 0 {
            return cached;
        }
        crate::server::process_metrics::read_resident_memory_bytes().unwrap_or(0)
    }

    pub fn cached_total_system_memory_bytes(&self) -> u64 {
        let cached = self.cached_total_system_memory.load(Ordering::Relaxed);
        if cached > 0 {
            return cached;
        }
        crate::server::process_metrics::read_total_system_memory_bytes().unwrap_or(0)
    }

    pub fn instantaneous_input_kbps(&self) -> u64 {
        self.instantaneous_input_kbps.load(Ordering::Relaxed)
    }

    pub fn instantaneous_output_kbps(&self) -> u64 {
        self.instantaneous_output_kbps.load(Ordering::Relaxed)
    }

    pub fn slowlog_commands_count(&self) -> u64 {
        self.slowlog_commands_count.load(Ordering::Relaxed)
    }

    pub fn slowlog_commands_time_ms_sum(&self) -> u64 {
        self.slowlog_commands_time_ms_sum.load(Ordering::Relaxed)
    }

    pub fn slowlog_commands_time_ms_max(&self) -> u64 {
        self.slowlog_commands_time_ms_max.load(Ordering::Relaxed)
    }

    /// 更新逻辑 DB key 数量 (仅 monitoring feature).
    #[cfg(feature = "monitoring")]
    pub fn set_db_key_count(&self, db: usize, count: u64) {
        self.db_key_counts.insert(db, count);
    }

    /// 记录网络入站字节.
    #[cfg(feature = "monitoring")]
    pub fn on_net_input_bytes(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.net_input_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// 刷新进程 RSS / CPU / 磁盘 IO 指标 (仅 monitoring feature).
    #[cfg(feature = "monitoring")]
    pub fn refresh_process_metrics(&self) {
        let Some(ref otel) = self.otel else {
            return;
        };
        if let Some(bytes) = crate::server::process_metrics::read_resident_memory_bytes() {
            otel.set_process_rss(bytes);
        }
        if let Some((user_secs, sys_secs)) =
            crate::server::process_metrics::read_cpu_user_sys_seconds()
        {
            let user_bits = user_secs.to_bits();
            let sys_bits = sys_secs.to_bits();
            let prev_user = self
                .process_last_cpu_user_seconds_bits
                .swap(user_bits, Ordering::Relaxed);
            let prev_sys = self
                .process_last_cpu_sys_seconds_bits
                .swap(sys_bits, Ordering::Relaxed);
            if prev_user != 0 && prev_sys != 0 {
                let prev_user_secs = f64::from_bits(prev_user);
                let prev_sys_secs = f64::from_bits(prev_sys);
                let user_delta = if user_secs > prev_user_secs {
                    user_secs - prev_user_secs
                } else {
                    0.0
                };
                let sys_delta = if sys_secs > prev_sys_secs {
                    sys_secs - prev_sys_secs
                } else {
                    0.0
                };
                if user_delta > 0.0 || sys_delta > 0.0 {
                    otel.add_process_cpu_delta(user_delta, sys_delta);
                }
            }
        }
        if let Some((read_bytes, write_bytes)) = crate::server::process_metrics::read_io_bytes() {
            let prev_read = self
                .process_last_read_bytes
                .swap(read_bytes, Ordering::Relaxed);
            if prev_read > 0 && read_bytes > prev_read {
                let delta = read_bytes - prev_read;
                otel.add_process_io(delta, 0);
            }
            let prev_write = self
                .process_last_write_bytes
                .swap(write_bytes, Ordering::Relaxed);
            if prev_write > 0 && write_bytes > prev_write {
                let delta = write_bytes - prev_write;
                otel.add_process_io(0, delta);
            }
        }
        if let Some((threads, vol, nonvol)) =
            crate::server::process_metrics::read_process_threads_and_context_switches()
        {
            otel.set_process_threads(threads);

            let prev_vol = self
                .process_last_voluntary_ctxt_switches
                .load(Ordering::Relaxed);
            if prev_vol > 0 && vol > prev_vol {
                otel.add_process_context_switches(vol - prev_vol, 0);
            }
            self.process_last_voluntary_ctxt_switches
                .store(vol, Ordering::Relaxed);

            let prev_nonvol = self
                .process_last_nonvoluntary_ctxt_switches
                .load(Ordering::Relaxed);
            if prev_nonvol > 0 && nonvol > prev_nonvol {
                otel.add_process_context_switches(0, nonvol - prev_nonvol);
            }
            self.process_last_nonvoluntary_ctxt_switches
                .store(nonvol, Ordering::Relaxed);
        }
    }

    /// 记录网络出站字节.
    #[cfg(feature = "monitoring")]
    pub fn on_net_output_bytes(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.net_output_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// 记录 gossip 拓扑刷新 (MetaRaft 同步).
    pub fn on_gossip_refresh(&self, known_nodes: usize) {
        self.cluster_messages_sent.fetch_add(1, Ordering::Relaxed);
        if known_nodes > 0 {
            self.cluster_messages_received
                .fetch_add(known_nodes as u64, Ordering::Relaxed);
        }
        self.on_command("GOSSIP.tick", true);
    }

    /// 记录 failover 事件 (仅 cluster feature).
    pub fn on_failover(&self) {
        self.on_command("CLUSTER.failover", true);
    }

    pub fn on_json_command(&self, _command: &str, _ok: bool) {}

    /// 集群重定向计数 (aikv_cluster_redirects_total 语义).
    pub fn on_cluster_redirect(&self, redirect_type: &str) {
        match redirect_type {
            "moved" => self.on_redirect(RedirectKind::Moved),
            "ask" => self.on_redirect(RedirectKind::Ask),
            _ => return,
        }
        let key = format!("CLUSTER.redirect.{}", redirect_type);
        self.on_command(&key, true);
    }

    pub fn on_redirect(&self, kind: RedirectKind) {
        match kind {
            RedirectKind::Moved => {
                self.redirect_moved.fetch_add(1, Ordering::Relaxed);
            }
            RedirectKind::Ask => {
                self.redirect_ask.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn redirect_count(&self, kind: RedirectKind) -> u64 {
        match kind {
            RedirectKind::Moved => self.redirect_moved.load(Ordering::Relaxed),
            RedirectKind::Ask => self.redirect_ask.load(Ordering::Relaxed),
        }
    }

    /// 写路径 propose 返回时记一次. 读路径传 `is_write = false`, 不加计数.
    pub fn note_propose(&self, duration_us: u64, is_write: bool) {
        self.note_propose_group(0, duration_us, is_write);
    }

    pub fn note_propose_group(&self, group: u64, duration_us: u64, is_write: bool) {
        if !is_write {
            return;
        }
        self.propose_duration
            .entry(group)
            .or_default()
            .record(duration_us);
    }

    pub fn propose_count(&self) -> u64 {
        self.propose_duration
            .iter()
            .map(|entry| entry.snapshot().count)
            .sum()
    }

    pub fn propose_rows(&self) -> Vec<(u64, LatencySnapshot)> {
        self.propose_duration
            .iter()
            .map(|entry| (*entry.key(), entry.snapshot()))
            .filter(|(_, snap)| snap.count > 0)
            .collect()
    }

    pub fn json_command_ok_count(&self, command: &str) -> u64 {
        let key = format!("JSON.{}", command.to_ascii_lowercase());
        self.command_ok_count(&key)
    }

    pub fn on_bgsave_complete(&self, ok: bool) {
        self.on_command("BGSAVE", ok);
    }

    pub fn on_keyspace_hit(&self) {
        self.keyspace_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn net_input_bytes(&self) -> u64 {
        self.net_input_bytes.load(Ordering::Relaxed)
    }

    pub fn net_output_bytes(&self) -> u64 {
        self.net_output_bytes.load(Ordering::Relaxed)
    }

    pub fn on_keyspace_miss(&self) {
        self.keyspace_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn keyspace_hits(&self) -> u64 {
        self.keyspace_hits.load(Ordering::Relaxed)
            + self.shared_point_reads.get().map(|c| c.hits()).unwrap_or(0)
    }

    pub fn keyspace_misses(&self) -> u64 {
        self.keyspace_misses.load(Ordering::Relaxed)
            + self
                .shared_point_reads
                .get()
                .map(|c| c.misses())
                .unwrap_or(0)
    }

    pub fn connections_total(&self) -> u64 {
        self.connections_total.load(Ordering::Relaxed)
    }

    pub fn connected_clients(&self) -> usize {
        self.connected_clients.load(Ordering::Relaxed)
    }

    pub fn total_commands_processed(&self) -> u64 {
        self.commands_total
            .iter()
            .map(|entry| entry.value().ok + entry.value().err)
            .sum()
    }

    pub fn total_error_replies(&self) -> u64 {
        self.commands_total
            .iter()
            .map(|entry| entry.value().err)
            .sum()
    }

    pub(crate) fn client_command_totals(&self) -> Vec<(String, CommandTotals)> {
        let mut out: Vec<_> = self
            .commands_total
            .iter()
            .filter(|entry| is_client_command(entry.key()))
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// 全部命令统计 (含内部伪命令, 供 OTel sync).
    #[cfg(feature = "monitoring")]
    pub(crate) fn all_command_totals(&self) -> Vec<(String, CommandTotals)> {
        let mut out: Vec<_> = self
            .commands_total
            .iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn uptime_secs(&self) -> u64 {
        self.uptime_secs.load(Ordering::Relaxed)
    }

    pub fn db_key_counts(&self) -> Vec<(usize, u64)> {
        let mut out: Vec<_> = self
            .db_key_counts
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();
        out.sort_by_key(|(db, _)| *db);
        out
    }

    pub fn command_ok_count(&self, command: &str) -> u64 {
        self.commands_total
            .get(&command.to_ascii_uppercase())
            .map(|t| t.ok)
            .unwrap_or(0)
    }

    pub fn expired_keys(&self) -> u64 {
        self.expired_keys.load(Ordering::Relaxed)
    }

    pub fn rejected_connections(&self) -> u64 {
        self.rejected_connections.load(Ordering::Relaxed)
    }

    pub fn used_memory_bytes(&self) -> u64 {
        self.used_memory_bytes.load(Ordering::Relaxed)
    }

    pub fn used_memory_peak_bytes(&self) -> u64 {
        self.used_memory_peak_bytes.load(Ordering::Relaxed)
    }

    pub fn evicted_keys(&self) -> u64 {
        self.evicted_keys.load(Ordering::Relaxed)
    }

    pub fn instantaneous_ops_per_sec(&self) -> u64 {
        self.instantaneous_ops_per_sec.load(Ordering::Relaxed)
    }

    pub fn cluster_messages_sent(&self) -> u64 {
        self.cluster_messages_sent.load(Ordering::Relaxed)
    }

    pub fn cluster_messages_received(&self) -> u64 {
        self.cluster_messages_received.load(Ordering::Relaxed)
    }

    pub fn blocked_clients(&self) -> usize {
        self.blocked_clients.load(Ordering::Relaxed)
    }

    /// 同步 Redis 对齐 gauge (blocked_clients 等; OTel 写入在 refresh sync).
    #[cfg(feature = "monitoring")]
    pub fn sync_redis_aligned_gauges(&self) {}

    #[cfg(not(feature = "monitoring"))]
    pub fn sync_redis_aligned_gauges(&self) {}

    pub fn attach_point_reads(&self, counter: Arc<PointReadCounter>) {
        let _ = self.shared_point_reads.set(counter);
    }

    /// 命令出口记一次. `command` 必须是 `canonical_command` 返回的静态名字.
    pub fn on_command_done(&self, command: &'static str, ok: bool, duration_us: u64) {
        self.bump_command_totals(command, ok, duration_us);
        let slot = self.command_latency.entry(command).or_default();
        if ok {
            slot.ok.record(duration_us);
        } else {
            slot.err.record(duration_us);
        }
        slot.note_max(duration_us);
    }

    pub fn command_status_count(&self, command: &str, ok: bool) -> u64 {
        self.commands_total
            .get(command)
            .map(|t| if ok { t.ok } else { t.err })
            .unwrap_or(0)
    }

    pub fn command_duration(&self, command: &str, ok: bool) -> LatencySnapshot {
        self.command_latency
            .get(command)
            .map(|slot| {
                if ok {
                    slot.ok.snapshot()
                } else {
                    slot.err.snapshot()
                }
            })
            .unwrap_or_else(|| LatencyHistogram::default().snapshot())
    }

    pub fn note_point_read(&self, hit: bool) {
        if hit {
            self.local_point_hits.fetch_add(1, Ordering::Relaxed);
            self.on_keyspace_hit();
        } else {
            self.local_point_misses.fetch_add(1, Ordering::Relaxed);
            self.on_keyspace_miss();
        }
    }

    pub fn point_read_count(&self, hit: bool) -> u64 {
        let local = if hit {
            self.local_point_hits.load(Ordering::Relaxed)
        } else {
            self.local_point_misses.load(Ordering::Relaxed)
        };
        let shared = self
            .shared_point_reads
            .get()
            .map(|c| if hit { c.hits() } else { c.misses() })
            .unwrap_or(0);
        local + shared
    }

    /// SCAN 结束时的条目数. 不改点查 hit/miss.
    pub fn note_scan_entries(&self, count: u64) {
        self.scan_entries.fetch_add(count, Ordering::Relaxed);
    }

    pub fn scan_entries(&self) -> u64 {
        self.scan_entries.load(Ordering::Relaxed)
    }

    pub fn note_connection_close(&self, reason: &'static str) {
        *self.connection_closes.entry(reason).or_insert(0) += 1;
    }

    pub fn note_accept_error(&self, errno: &str) {
        *self.accept_errors.entry(errno.to_string()).or_insert(0) += 1;
    }

    pub fn connection_close_rows(&self) -> Vec<(String, u64)> {
        self.connection_closes
            .iter()
            .map(|e| ((*e.key()).to_string(), *e.value()))
            .collect()
    }

    pub fn accept_error_rows(&self) -> Vec<(String, u64)> {
        self.accept_errors
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect()
    }

    pub fn command_counter_rows(&self) -> Vec<(String, u64, u64)> {
        self.commands_total
            .iter()
            .map(|e| (e.key().clone(), e.ok, e.err))
            .collect()
    }

    pub fn command_duration_rows(&self) -> Vec<(String, bool, LatencySnapshot)> {
        let mut out = Vec::new();
        for entry in self.command_latency.iter() {
            let name = (*entry.key()).to_string();
            let ok = entry.ok.snapshot();
            let err = entry.err.snapshot();
            if ok.count > 0 {
                out.push((name.clone(), true, ok));
            }
            if err.count > 0 {
                out.push((name, false, err));
            }
        }
        out
    }

    pub fn command_histogram_snapshots(
        &self,
        filter: Option<&[&str]>,
    ) -> Vec<(String, CommandLatencySnapshot)> {
        let mut out = Vec::new();
        for entry in self.command_latency.iter() {
            let name = *entry.key();
            if let Some(names) = filter {
                if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                    continue;
                }
            }
            let ok = entry.ok.snapshot();
            let err = entry.err.snapshot();
            let calls = ok.count + err.count;
            if calls == 0 {
                continue;
            }
            let mut buckets = Vec::with_capacity(COMMAND_DURATION_BOUNDS_US.len());
            let mut cumulative = 0u64;
            for (i, bound) in COMMAND_DURATION_BOUNDS_US.iter().enumerate() {
                cumulative += ok.buckets[i] + err.buckets[i];
                buckets.push((*bound, cumulative));
            }
            out.push((
                name.to_ascii_lowercase(),
                CommandLatencySnapshot {
                    calls,
                    total_us: ok.sum_us + err.sum_us,
                    max_us: entry.max_us.load(Ordering::Relaxed),
                    buckets,
                    p50_us: 0,
                    p95_us: 0,
                    p99_us: 0,
                    p999_us: 0,
                },
            ));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn reset_command_histograms(&self, filter: Option<&[&str]>) -> u64 {
        match filter {
            None => {
                let count = self.command_latency.len() as u64;
                self.command_latency.clear();
                count
            }
            Some(names) => {
                let mut count = 0u64;
                for name in names {
                    let key = crate::command::canonical_command(name);
                    if self.command_latency.remove(key).is_some() {
                        count += 1;
                    }
                }
                count
            }
        }
    }

    fn bump_command_totals(&self, command: &str, ok: bool, duration_us: u64) {
        if let Some(mut entry) = self.commands_total.get_mut(command) {
            if ok {
                entry.ok += 1;
            } else {
                entry.err += 1;
            }
            entry.usec = entry.usec.saturating_add(duration_us);
            return;
        }
        let mut totals = CommandTotals::default();
        if ok {
            totals.ok = 1;
        } else {
            totals.err = 1;
        }
        totals.usec = duration_us;
        self.commands_total.insert(command.to_string(), totals);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_metrics_threads_and_context_switches_cache() {
        let metrics = ServerMetrics::default();
        // 1. 验证启动初期兜底直读与回填（在 Linux 环境下大于 0）
        let (_threads, _vol, _nonvol) = metrics.get_threads_and_context_switches();
        #[cfg(target_os = "linux")]
        {
            assert!(threads > 0, "threads should be > 0 on linux");
            assert_eq!(
                metrics.cached_process_threads.load(Ordering::Relaxed),
                threads
            );
            assert_eq!(
                metrics
                    .cached_voluntary_ctxt_switches
                    .load(Ordering::Relaxed),
                vol
            );
            assert_eq!(
                metrics
                    .cached_nonvoluntary_ctxt_switches
                    .load(Ordering::Relaxed),
                nonvol
            );
        }

        // 2. 模拟缓存命中（优先返回缓存，不触发二次解析）
        metrics.cached_process_threads.store(99, Ordering::Relaxed);
        metrics
            .cached_voluntary_ctxt_switches
            .store(888, Ordering::Relaxed);
        metrics
            .cached_nonvoluntary_ctxt_switches
            .store(777, Ordering::Relaxed);
        assert_eq!(metrics.get_threads_and_context_switches(), (99, 888, 777));

        // 3. 验证周期刷新逻辑
        metrics.refresh_cached_process_info();
        #[cfg(target_os = "linux")]
        {
            let refreshed = metrics.get_threads_and_context_switches();
            assert_ne!(
                refreshed.0, 99,
                "refresh should overwrite dummy cache with real data"
            );
        }
    }

    #[test]
    fn command_done_records_once() {
        use crate::command::canonical_command;
        let metrics = ServerMetrics::default();
        let get = canonical_command("get");
        let get_upper = canonical_command("GET");
        assert!(std::ptr::eq(get, get_upper));
        assert_eq!(get, "GET");
        metrics.on_command_done(get, true, 150);
        assert_eq!(metrics.command_status_count("GET", true), 1);
        assert_eq!(metrics.command_status_count("GET", false), 0);
        let snap = metrics.command_duration("GET", true);
        assert_eq!(snap.count, 1);
        assert_eq!(snap.sum_us, 150);
    }

    #[test]
    fn unknown_command_does_not_use_the_raw_name() {
        use crate::command::canonical_command;
        assert_eq!(canonical_command("no-such"), "unknown");
    }

    #[test]
    fn json_set_counts_once() {
        use crate::command::canonical_command;
        let metrics = ServerMetrics::default();
        metrics.on_command_done(canonical_command("JSON.SET"), true, 80);
        assert_eq!(metrics.command_status_count("JSON.SET", true), 1);
        assert_eq!(metrics.command_duration("JSON.SET", true).count, 1);
    }

    #[test]
    fn point_read_miss_does_not_count_as_hit() {
        let metrics = ServerMetrics::default();
        metrics.note_point_read(false);
        assert_eq!(metrics.point_read_count(false), 1);
        assert_eq!(metrics.point_read_count(true), 0);
        metrics.note_scan_entries(3);
        assert_eq!(metrics.point_read_count(false), 1);
        assert_eq!(metrics.point_read_count(true), 0);
    }
}
