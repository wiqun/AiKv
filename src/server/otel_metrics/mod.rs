//! OTel metrics instruments: `aikv_*` 生产指标的 OTLP 唯一出口.
//!
//! # Invariant
//!
//! - 热路径只写 `ServerMetrics`; `aikv_*` 由 `refresh_runtime_metrics` →
//!   `info_catalog::sync_otel_from_server_metrics` 读真源、算 delta 后写 OTLP
//!   (相对 INFO 最多滞后 ~15s); 禁止在热路径直接写 OTel.
//! - OTel counter 仅支持 `add(delta)`: `SyncSnapshot` 保存上次 cumulative 快照,
//!   `sync_counters` / `sync_commandstats` 逐字段差分后 add.
//! - Gauge 类 (used_memory_bytes / instantaneous_ops_per_sec 等) 经 Observable
//!   回调读 `GaugeSnapshot` atomics.
//!
//! TODO(exemplars): metrics↔traces 跳转需 OTel Rust SDK exemplar 采集; 当前 0.32 仍不支持.
//!
//! 子模块: `helpers` (counter/updown delta) / `testutil` (InMemory exporter).

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};
use opentelemetry::KeyValue;

mod helpers;
pub mod testutil;

#[cfg(feature = "cluster")]
use self::helpers::cluster_redirect_type_from_cmd;
use self::helpers::{counter_delta, updown_delta};

const CMD_DURATION_BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

pub(crate) const ATTR_COMMAND: &str = "aikv.command.name";
const ATTR_STATUS: &str = "aikv.command.status";
const ATTR_DB_INDEX: &str = "aikv.db.index";
pub(crate) const ATTR_REDIRECT_TYPE: &str = "aikv.cluster.redirect.type";
const ATTR_CPU_MODE: &str = "cpu.mode";

/// Counter / histogram sync 上次 cumulative 快照 (OTel counter 仅支持 add(delta)).
#[derive(Default)]
pub(crate) struct SyncSnapshot {
    keyspace_hits: u64,
    keyspace_misses: u64,
    connections_total: u64,
    connected_clients: i64,
    rejected_connections: u64,
    expired_keys: u64,
    evicted_keys: u64,
    net_input_bytes: u64,
    net_output_bytes: u64,
    lua_execution_count: u64,
    lua_execution_duration_us: u64,
    gossip_messages: u64,
    failover: u64,
    commands_ok: HashMap<String, u64>,
    commands_err: HashMap<String, u64>,
    commands_usec: HashMap<String, u64>,
    commands_slowlog: HashMap<String, u64>,
    json_commands: HashMap<String, u64>,
    #[cfg(feature = "cluster")]
    cluster_redirects: HashMap<String, u64>,
}

/// Gauge 类指标快照 (Observable 回调读取).
#[derive(Default)]
struct GaugeSnapshot {
    used_memory_bytes: AtomicI64,
    used_memory_peak_bytes: AtomicI64,
    instantaneous_ops_per_sec: AtomicI64,
    blocked_clients: AtomicI64,
    uptime_seconds: AtomicI64,
    process_resident_memory_bytes: AtomicI64,
}

/// OTel 指标集合.
pub struct OtelMetrics {
    gauges: Arc<GaugeSnapshot>,
    db_keys_gauge: Gauge<f64>,
    commands_total: Counter<u64>,
    command_duration_seconds: Histogram<f64>,
    connections_total: Counter<u64>,
    connected_clients: UpDownCounter<i64>,
    rejected_connections_total: Counter<u64>,
    keyspace_hits_total: Counter<u64>,
    keyspace_misses_total: Counter<u64>,
    expired_keys_total: Counter<u64>,
    evicted_keys_total: Counter<u64>,
    net_input_bytes_total: Counter<u64>,
    net_output_bytes_total: Counter<u64>,
    slow_queries_total: Counter<u64>,
    process_cpu_milliseconds_total: Counter<u64>,
    process_read_bytes_total: Counter<u64>,
    process_write_bytes_total: Counter<u64>,
    lua_scripts_total: Counter<u64>,
    lua_execution_duration_seconds: Histogram<f64>,
    json_commands_total: Counter<u64>,
    #[cfg(feature = "cluster")]
    cluster_redirects_total: Counter<u64>,
    #[cfg(feature = "cluster")]
    gossip_messages_total: Counter<u64>,
    #[cfg(feature = "cluster")]
    failover_total: Counter<u64>,
    process_cpu_time: Counter<f64>,
    process_memory_usage: Gauge<f64>,
    process_disk_io: Counter<u64>,
    rebuild_counters_duration_seconds: Gauge<f64>,
    process_threads: Gauge<f64>,
    process_context_switches_total: Counter<u64>,
    sync_snapshot: Mutex<SyncSnapshot>,
}

impl std::fmt::Debug for OtelMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OtelMetrics")
    }
}

static GLOBAL_OTEL: OnceLock<RwLock<Option<Arc<OtelMetrics>>>> = OnceLock::new();

fn global_otel() -> &'static RwLock<Option<Arc<OtelMetrics>>> {
    GLOBAL_OTEL.get_or_init(|| RwLock::new(None))
}

impl OtelMetrics {
    /// 初始化 global `aikv` meter instruments (幂等).
    pub fn init_global(meter: Meter) -> Arc<Self> {
        let cell = global_otel();
        if let Some(existing) = cell.read().as_ref() {
            return Arc::clone(existing);
        }
        let otel = Self::new(meter);
        *cell.write() = Some(Arc::clone(&otel));
        otel
    }

    /// 测试用: 用当前 global meter provider 重建 instruments.
    pub(crate) fn install_global(meter: Meter) -> Arc<Self> {
        let otel = Self::new(meter);
        *global_otel().write() = Some(Arc::clone(&otel));
        otel
    }

    pub(crate) fn new(meter: Meter) -> Arc<Self> {
        let gauges = Arc::new(GaugeSnapshot::default());
        let otel = Arc::new(Self {
            db_keys_gauge: meter
                .f64_gauge("aikv_db_keys")
                .with_description("Approximate key count per logical DB")
                .with_unit("1")
                .build(),
            gauges: Arc::clone(&gauges),
            commands_total: meter
                .u64_counter("aikv_commands_total")
                .with_description("Total commands processed, by command name and status")
                .with_unit("1")
                .build(),
            command_duration_seconds: meter
                .f64_histogram("aikv_command_duration_seconds")
                .with_description("Command duration in seconds")
                .with_unit("s")
                .with_boundaries(CMD_DURATION_BUCKETS.to_vec())
                .build(),
            connections_total: meter
                .u64_counter("aikv_connections_total")
                .with_description("Total accepted connections")
                .with_unit("1")
                .build(),
            connected_clients: meter
                .i64_up_down_counter("aikv_connected_clients")
                .with_description("Current connected clients")
                .with_unit("1")
                .build(),
            rejected_connections_total: meter
                .u64_counter("aikv_rejected_connections_total")
                .with_description("Total rejected connections")
                .with_unit("1")
                .build(),
            keyspace_hits_total: meter
                .u64_counter("aikv_keyspace_hits_total")
                .with_description("Total keyspace hits")
                .with_unit("1")
                .build(),
            keyspace_misses_total: meter
                .u64_counter("aikv_keyspace_misses_total")
                .with_description("Total keyspace misses")
                .with_unit("1")
                .build(),
            expired_keys_total: meter
                .u64_counter("aikv_expired_keys_total")
                .with_description("Total expired keys")
                .with_unit("1")
                .build(),
            evicted_keys_total: meter
                .u64_counter("aikv_evicted_keys_total")
                .with_description("Total evicted keys")
                .with_unit("1")
                .build(),
            net_input_bytes_total: meter
                .u64_counter("aikv_net_input_bytes_total")
                .with_description("Total bytes received from clients")
                .with_unit("By")
                .build(),
            net_output_bytes_total: meter
                .u64_counter("aikv_net_output_bytes_total")
                .with_description("Total bytes sent to clients")
                .with_unit("By")
                .build(),
            slow_queries_total: meter
                .u64_counter("aikv_slow_queries_total")
                .with_description("Total slow queries by command")
                .with_unit("1")
                .build(),
            process_cpu_milliseconds_total: meter
                .u64_counter("aikv_process_cpu_milliseconds_total")
                .with_description("Total process CPU time in milliseconds (legacy)")
                .with_unit("ms")
                .build(),
            process_read_bytes_total: meter
                .u64_counter("aikv_process_read_bytes_total")
                .with_description("Total process disk read bytes")
                .with_unit("By")
                .build(),
            process_write_bytes_total: meter
                .u64_counter("aikv_process_write_bytes_total")
                .with_description("Total process disk write bytes")
                .with_unit("By")
                .build(),
            lua_scripts_total: meter
                .u64_counter("aikv_lua_scripts_total")
                .with_description("Total Lua script executions")
                .with_unit("1")
                .build(),
            lua_execution_duration_seconds: meter
                .f64_histogram("aikv_lua_execution_duration_seconds")
                .with_description("Lua script execution duration in seconds")
                .with_unit("s")
                .build(),
            json_commands_total: meter
                .u64_counter("aikv_json_commands_total")
                .with_description("Total JSON commands by sub-command")
                .with_unit("1")
                .build(),
            #[cfg(feature = "cluster")]
            cluster_redirects_total: meter
                .u64_counter("aikv_cluster_redirects_total")
                .with_description("Total cluster redirects by type")
                .with_unit("1")
                .build(),
            #[cfg(feature = "cluster")]
            gossip_messages_total: meter
                .u64_counter("aikv_gossip_messages_total")
                .with_description("Total gossip messages")
                .with_unit("1")
                .build(),
            #[cfg(feature = "cluster")]
            failover_total: meter
                .u64_counter("aikv_failover_total")
                .with_description("Total failover events")
                .with_unit("1")
                .build(),
            process_cpu_time: meter
                .f64_counter("process.cpu.time")
                .with_description("Total CPU seconds by mode (OTel semconv)")
                .with_unit("s")
                .build(),
            process_memory_usage: meter
                .f64_gauge("process.memory.usage")
                .with_description("Process resident memory")
                .with_unit("By")
                .build(),
            process_disk_io: meter
                .u64_counter("process.disk.io")
                .with_description("Process disk I/O bytes")
                .with_unit("By")
                .build(),
            rebuild_counters_duration_seconds: meter
                .f64_gauge("aikv_recovery_rebuild_counters_duration_seconds")
                .with_description("键计数器全库重建恢复耗时")
                .with_unit("s")
                .build(),
            process_threads: meter
                .f64_gauge("aikv_process_threads")
                .with_description("进程当前 OS 线程总数")
                .with_unit("1")
                .build(),
            process_context_switches_total: meter
                .u64_counter("aikv_process_context_switches_total")
                .with_description("进程上下文切换累计次数")
                .with_unit("1")
                .build(),
            sync_snapshot: Mutex::new(SyncSnapshot::default()),
        });

        register_aikv_observable_gauges(&meter, &gauges);
        otel
    }

    fn cmd_attrs(command: &str, status: &str) -> [KeyValue; 2] {
        [
            KeyValue::new(ATTR_COMMAND, command.to_string()),
            KeyValue::new(ATTR_STATUS, status.to_string()),
        ]
    }

    pub fn set_db_key_count(&self, db: usize, count: u64) {
        self.db_keys_gauge.record(
            count as f64,
            &[KeyValue::new(ATTR_DB_INDEX, db.to_string())],
        );
    }

    pub fn set_rebuild_counters_duration(&self, duration_us: u64) {
        self.rebuild_counters_duration_seconds
            .record(duration_us as f64 / 1_000_000.0, &[]);
    }

    pub fn set_process_threads(&self, threads: u64) {
        self.process_threads.record(threads as f64, &[]);
    }

    pub fn add_process_context_switches(&self, vol_delta: u64, nonvol_delta: u64) {
        if vol_delta > 0 {
            self.process_context_switches_total.add(
                vol_delta,
                &[KeyValue::new("aikv_context_switch_type", "voluntary")],
            );
        }
        if nonvol_delta > 0 {
            self.process_context_switches_total.add(
                nonvol_delta,
                &[KeyValue::new("aikv_context_switch_type", "nonvoluntary")],
            );
        }
    }

    pub fn on_connect(&self) {
        self.connections_total.add(1, &[]);
        self.connected_clients.add(1, &[]);
    }

    pub fn on_disconnect(&self) {
        self.connected_clients.add(-1, &[]);
    }

    pub fn on_client_blocked(&self) {
        self.gauges.blocked_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_client_unblocked(&self) {
        self.gauges.blocked_clients.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn on_command(&self, command: &str, ok: bool) {
        let status = if ok { "ok" } else { "error" };
        let cmd = command.to_ascii_uppercase();
        self.commands_total.add(1, &Self::cmd_attrs(&cmd, status));
    }

    pub fn on_command_duration(&self, command: &str, duration_us: u64, ok: bool) {
        let status = if ok { "ok" } else { "error" };
        let cmd = command.to_ascii_uppercase();
        let secs = duration_us as f64 / 1_000_000.0;
        self.command_duration_seconds
            .record(secs, &Self::cmd_attrs(&cmd, status));
    }

    pub fn on_lua_scripts(&self) {
        self.lua_scripts_total.add(1, &[]);
    }

    pub fn on_lua_execution(&self, duration_secs: f64) {
        self.lua_execution_duration_seconds
            .record(duration_secs, &[]);
    }

    pub fn on_slow_query(&self, command: &str) {
        self.slow_queries_total.add(
            1,
            &[KeyValue::new(ATTR_COMMAND, command.to_ascii_uppercase())],
        );
    }

    pub fn on_json_command(&self, command: &str) {
        self.json_commands_total.add(
            1,
            &[KeyValue::new(ATTR_COMMAND, command.to_ascii_uppercase())],
        );
    }

    pub fn on_rejected_connection(&self) {
        self.rejected_connections_total.add(1, &[]);
    }

    pub fn on_keyspace_hit(&self) {
        self.keyspace_hits_total.add(1, &[]);
    }

    pub fn on_keyspace_miss(&self) {
        self.keyspace_misses_total.add(1, &[]);
    }

    pub fn record_expired_keys(&self, count: u64) {
        if count > 0 {
            self.expired_keys_total.add(count, &[]);
        }
    }

    pub fn on_expired_key(&self) {
        self.expired_keys_total.add(1, &[]);
    }

    pub fn set_uptime_secs(&self, secs: u64) {
        self.gauges
            .uptime_seconds
            .store(secs as i64, Ordering::Relaxed);
    }

    pub fn set_memory_bytes(&self, current: u64, peak: u64) {
        self.gauges
            .used_memory_bytes
            .store(current as i64, Ordering::Relaxed);
        self.gauges
            .used_memory_peak_bytes
            .store(peak as i64, Ordering::Relaxed);
    }

    pub fn set_instantaneous_ops(&self, ops: u64) {
        self.gauges
            .instantaneous_ops_per_sec
            .store(ops as i64, Ordering::Relaxed);
    }

    pub fn on_net_input_bytes(&self, bytes: u64) {
        if bytes > 0 {
            self.net_input_bytes_total.add(bytes, &[]);
        }
    }

    pub fn on_net_output_bytes(&self, bytes: u64) {
        if bytes > 0 {
            self.net_output_bytes_total.add(bytes, &[]);
        }
    }

    pub fn set_process_rss(&self, bytes: u64) {
        self.gauges
            .process_resident_memory_bytes
            .store(bytes as i64, Ordering::Relaxed);
        self.process_memory_usage.record(
            bytes as f64,
            &[KeyValue::new("process.memory.state", "used")],
        );
    }

    pub fn add_process_cpu_delta(&self, user_secs: f64, sys_secs: f64) {
        if user_secs > 0.0 {
            self.process_cpu_time
                .add(user_secs, &[KeyValue::new(ATTR_CPU_MODE, "user")]);
        }
        if sys_secs > 0.0 {
            self.process_cpu_time
                .add(sys_secs, &[KeyValue::new(ATTR_CPU_MODE, "system")]);
        }
        let total_secs = user_secs + sys_secs;
        if total_secs > 0.0 {
            let delta_ms = (total_secs * 1000.0).round() as u64;
            if delta_ms > 0 {
                self.process_cpu_milliseconds_total.add(delta_ms, &[]);
            }
        }
    }

    pub fn add_process_io(&self, read_delta: u64, write_delta: u64) {
        if read_delta > 0 {
            self.process_read_bytes_total.add(read_delta, &[]);
            self.process_disk_io
                .add(read_delta, &[KeyValue::new("disk.io.direction", "read")]);
        }
        if write_delta > 0 {
            self.process_write_bytes_total.add(write_delta, &[]);
            self.process_disk_io
                .add(write_delta, &[KeyValue::new("disk.io.direction", "write")]);
        }
    }

    pub fn sync_blocked_clients(&self, count: usize) {
        self.gauges
            .blocked_clients
            .store(count as i64, Ordering::Relaxed);
    }

    #[cfg(feature = "cluster")]
    pub fn on_cluster_redirect(&self, redirect_type: &str) {
        self.cluster_redirects_total.add(
            1,
            &[KeyValue::new(ATTR_REDIRECT_TYPE, redirect_type.to_string())],
        );
    }

    #[cfg(feature = "cluster")]
    pub fn on_gossip_message(&self) {
        self.gossip_messages_total.add(1, &[]);
    }

    #[cfg(feature = "cluster")]
    pub fn on_failover(&self) {
        self.failover_total.add(1, &[]);
    }

    /// 将 observable gauge 快照与 `ServerMetrics` atomics 对齐.
    pub fn sync_stats_gauges(&self, metrics: &crate::server::metrics::ServerMetrics) {
        self.sync_blocked_clients(metrics.blocked_clients());
        self.set_instantaneous_ops(metrics.instantaneous_ops_per_sec());
        self.set_memory_bytes(
            metrics.used_memory_bytes(),
            metrics.used_memory_peak_bytes(),
        );
        self.set_uptime_secs(metrics.uptime_secs());
        self.gauges
            .process_resident_memory_bytes
            .store(metrics.cached_rss_bytes() as i64, Ordering::Relaxed);
        self.process_memory_usage
            .record(metrics.cached_rss_bytes() as f64, &[]);
        for (db, count) in metrics.db_key_counts() {
            self.set_db_key_count(db, count);
        }
    }

    /// 读 `ServerMetrics` atomics, 与 snapshot 做差, `counter.add(delta)`.
    pub fn sync_counters(&self, metrics: &crate::server::metrics::ServerMetrics) {
        let mut snap = self.sync_snapshot.lock();
        self.sync_counters_locked(metrics, &mut snap);
    }

    /// 遍历 command totals, 对 ok/err/usec/slowlog 分别 delta 到 OTel.
    pub fn sync_commandstats(&self, metrics: &crate::server::metrics::ServerMetrics) {
        let processed = metrics.total_commands_processed();
        let all_calls: u64 = metrics
            .all_command_totals()
            .iter()
            .map(|(_, totals)| totals.ok + totals.err)
            .sum();
        debug_assert_eq!(
            processed, all_calls,
            "total_commands_processed must match sum of all commandstats calls"
        );

        let mut snap = self.sync_snapshot.lock();
        for (cmd, totals) in metrics.all_command_totals() {
            let prev_ok = snap.commands_ok.get(&cmd).copied().unwrap_or(0);
            let prev_err = snap.commands_err.get(&cmd).copied().unwrap_or(0);
            let prev_usec = snap.commands_usec.get(&cmd).copied().unwrap_or(0);
            let prev_slow = snap.commands_slowlog.get(&cmd).copied().unwrap_or(0);

            let delta_ok = totals.ok.saturating_sub(prev_ok);
            let delta_err = totals.err.saturating_sub(prev_err);
            let delta_usec = totals.usec.saturating_sub(prev_usec);
            let delta_calls = delta_ok + delta_err;
            let delta_slow = totals.slowlog_count.saturating_sub(prev_slow);

            if delta_ok > 0 {
                self.commands_total
                    .add(delta_ok, &Self::cmd_attrs(&cmd, "ok"));
            }
            if delta_err > 0 {
                self.commands_total
                    .add(delta_err, &Self::cmd_attrs(&cmd, "error"));
            }

            if delta_calls > 0 && delta_usec > 0 {
                let avg_secs = (delta_usec as f64 / delta_calls as f64) / 1_000_000.0;
                let status = if delta_err > 0 && delta_ok == 0 {
                    "error"
                } else {
                    "ok"
                };
                for _ in 0..delta_calls {
                    self.command_duration_seconds
                        .record(avg_secs, &Self::cmd_attrs(&cmd, status));
                }
            }

            if delta_slow > 0 {
                self.slow_queries_total
                    .add(delta_slow, &[KeyValue::new(ATTR_COMMAND, cmd.clone())]);
            }

            snap.commands_ok.insert(cmd.clone(), totals.ok);
            snap.commands_err.insert(cmd.clone(), totals.err);
            snap.commands_usec.insert(cmd.clone(), totals.usec);
            snap.commands_slowlog
                .insert(cmd.clone(), totals.slowlog_count);

            if let Some(sub) = cmd.strip_prefix("JSON.") {
                let json_name = sub.to_ascii_uppercase();
                let prev_json = snap.json_commands.get(&json_name).copied().unwrap_or(0);
                let current = totals.ok + totals.err;
                let delta_json = current.saturating_sub(prev_json);
                if delta_json > 0 {
                    self.json_commands_total.add(
                        delta_json,
                        &[KeyValue::new(ATTR_COMMAND, json_name.clone())],
                    );
                }
                snap.json_commands.insert(json_name, current);
            }

            #[cfg(feature = "cluster")]
            if let Some(redirect_type) = cluster_redirect_type_from_cmd(&cmd) {
                let prev = snap
                    .cluster_redirects
                    .get(&redirect_type)
                    .copied()
                    .unwrap_or(0);
                let delta = totals.ok.saturating_sub(prev);
                if delta > 0 {
                    self.cluster_redirects_total.add(
                        delta,
                        &[KeyValue::new(ATTR_REDIRECT_TYPE, redirect_type.clone())],
                    );
                }
                snap.cluster_redirects.insert(redirect_type, totals.ok);
            }
        }
    }

    /// 测试用: 重置 sync 快照, 避免跨测试污染 global `OtelMetrics`.
    pub(crate) fn reset_sync_snapshot_for_test(&self) {
        *self.sync_snapshot.lock() = SyncSnapshot::default();
    }

    fn sync_counters_locked(
        &self,
        metrics: &crate::server::metrics::ServerMetrics,
        snap: &mut SyncSnapshot,
    ) {
        let delta = counter_delta(metrics.connections_total(), &mut snap.connections_total);
        if delta > 0 {
            self.connections_total.add(delta, &[]);
        }

        let client_delta = updown_delta(
            metrics.connected_clients() as i64,
            &mut snap.connected_clients,
        );
        if client_delta != 0 {
            self.connected_clients.add(client_delta, &[]);
        }

        let delta = counter_delta(metrics.keyspace_hits(), &mut snap.keyspace_hits);
        if delta > 0 {
            self.keyspace_hits_total.add(delta, &[]);
        }

        let delta = counter_delta(metrics.keyspace_misses(), &mut snap.keyspace_misses);
        if delta > 0 {
            self.keyspace_misses_total.add(delta, &[]);
        }

        let delta = counter_delta(
            metrics.rejected_connections(),
            &mut snap.rejected_connections,
        );
        if delta > 0 {
            self.rejected_connections_total.add(delta, &[]);
        }

        let delta = counter_delta(metrics.expired_keys(), &mut snap.expired_keys);
        if delta > 0 {
            self.expired_keys_total.add(delta, &[]);
        }

        let delta = counter_delta(metrics.evicted_keys(), &mut snap.evicted_keys);
        if delta > 0 {
            self.evicted_keys_total.add(delta, &[]);
        }

        let delta = counter_delta(metrics.net_input_bytes(), &mut snap.net_input_bytes);
        if delta > 0 {
            self.net_input_bytes_total.add(delta, &[]);
        }

        let delta = counter_delta(metrics.net_output_bytes(), &mut snap.net_output_bytes);
        if delta > 0 {
            self.net_output_bytes_total.add(delta, &[]);
        }

        let delta = counter_delta(metrics.lua_execution_count(), &mut snap.lua_execution_count);
        if delta > 0 {
            self.lua_scripts_total.add(delta, &[]);
        }

        let delta = counter_delta(
            metrics.lua_execution_duration_us(),
            &mut snap.lua_execution_duration_us,
        );
        if delta > 0 {
            self.lua_execution_duration_seconds
                .record(delta as f64 / 1_000_000.0, &[]);
        }

        #[cfg(feature = "cluster")]
        {
            let delta = counter_delta(
                metrics.command_ok_count("GOSSIP.tick"),
                &mut snap.gossip_messages,
            );
            if delta > 0 {
                self.gossip_messages_total.add(delta, &[]);
            }

            let delta = counter_delta(
                metrics.command_ok_count("CLUSTER.failover"),
                &mut snap.failover,
            );
            if delta > 0 {
                self.failover_total.add(delta, &[]);
            }
        }
    }
}

fn register_aikv_observable_gauges(meter: &Meter, gauges: &Arc<GaugeSnapshot>) {
    let g = Arc::clone(gauges);
    meter
        .i64_observable_gauge("aikv_used_memory_bytes")
        .with_unit("By")
        .with_callback(move |inst| {
            inst.observe(g.used_memory_bytes.load(Ordering::Relaxed), &[]);
        })
        .build();

    let g = Arc::clone(gauges);
    meter
        .i64_observable_gauge("aikv_used_memory_peak_bytes")
        .with_unit("By")
        .with_callback(move |inst| {
            inst.observe(g.used_memory_peak_bytes.load(Ordering::Relaxed), &[]);
        })
        .build();

    let g = Arc::clone(gauges);
    meter
        .i64_observable_gauge("aikv_instantaneous_ops_per_sec")
        .with_unit("{request}/s")
        .with_callback(move |inst| {
            inst.observe(g.instantaneous_ops_per_sec.load(Ordering::Relaxed), &[]);
        })
        .build();

    let g = Arc::clone(gauges);
    meter
        .i64_observable_gauge("aikv_blocked_clients")
        .with_unit("1")
        .with_callback(move |inst| {
            inst.observe(g.blocked_clients.load(Ordering::Relaxed), &[]);
        })
        .build();

    let g = Arc::clone(gauges);
    meter
        .i64_observable_gauge("aikv_uptime_seconds")
        .with_unit("s")
        .with_callback(move |inst| {
            inst.observe(g.uptime_seconds.load(Ordering::Relaxed), &[]);
        })
        .build();

    let g = Arc::clone(gauges);
    meter
        .i64_observable_gauge("aikv_process_resident_memory_bytes")
        .with_unit("By")
        .with_callback(move |inst| {
            inst.observe(g.process_resident_memory_bytes.load(Ordering::Relaxed), &[]);
        })
        .build();
}

/// 契约测试: 核心 aikv_* 指标名.
pub const AIKV_METRIC_NAMES: &[&str] = &[
    "aikv_commands_total",
    "aikv_command_duration_seconds",
    "aikv_connections_total",
    "aikv_connected_clients",
    "aikv_used_memory_bytes",
    "aikv_keyspace_hits_total",
    "aikv_uptime_seconds",
    "aikv_db_keys",
];
