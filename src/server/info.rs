//! INFO 格式化: Redis 8.8 语义对齐, 单一数据源 (ServerMetrics + storage).
//!
//! # 字段策略
//!
//! - **键名**: Redis 8.8 有的键名均输出 (golden: `redis88_info_full_fields.txt`).
//! - **Stub**: 无子系统真源的字段固定 `0` / `-1` / `ok` (pubsub, AOF, allocator, eventloop 等).
//! - **配置零值**: `maxmemory:0` 等来自 CONFIG, 非 stub.
//! - **真源**: stats/commandstats/keyspace/latency/RSS 等见 [`observability-reference.md`](../../docs/modules/observability-reference.md).
//!
//! # Section 模型 (Redis 7.0+)
//!
//! - `INFO` / `INFO default`: default 段集合
//! - `INFO all`: default + commandstats + errorstats + threads + latencystats
//! - `INFO everything`: all + modules
//! - `INFO stats memory`: 多 section 拼接
//!
//! # 读数注意
//!
//! - AiDb 集群下 `used_memory` 为 memtable/cache 近似值; 数据集规模看 `INFO keyspace`.
//! - `instantaneous_*` / `slowlog_commands_*` 依赖 refresh 采样与 100ms 慢日志阈值.
//! - `errorstats` 按错误前缀聚合; 部分 RESP `-ERR` 路径尚未计入 (见 observability.md 待核实).

use std::fmt::Write as FmtWrite;

use crate::server::config::ServerSharedState;
use crate::storage::{KvStorage, StorageEngineKind};

const REDIS_COMPAT_VERSION: &str = "8.8";
const MASTER_REPLID2: &str = "0000000000000000000000000000000000000000";

/// 集群是否已初始化 (CLUSTER_STATE_MGR 已 set).
pub fn is_cluster_initialized() -> bool {
    #[cfg(feature = "cluster")]
    {
        crate::cluster::state::CLUSTER_STATE_MGR.get().is_some()
    }
    #[cfg(not(feature = "cluster"))]
    {
        false
    }
}

pub fn redis_mode() -> &'static str {
    if is_cluster_initialized() {
        "cluster"
    } else {
        "standalone"
    }
}

pub fn cluster_enabled() -> u8 {
    u8::from(is_cluster_initialized())
}

pub struct InfoRenderer<'a> {
    shared: &'a ServerSharedState,
    storage: &'a dyn KvStorage,
}

impl<'a> InfoRenderer<'a> {
    pub fn new(shared: &'a ServerSharedState, storage: &'a dyn KvStorage) -> Self {
        Self { shared, storage }
    }

    /// `sections` 为空时返回 default; 否则按 Redis 7.0+ 多 section 语义拼接.
    pub async fn render(&self, sections: &[String]) -> String {
        if sections.is_empty() {
            return self.render_default().await;
        }
        if sections.len() == 1 {
            return match sections[0].as_str() {
                "default" => self.render_default().await,
                "all" => self.render_all().await,
                "everything" => self.render_everything().await,
                name => self.render_one_section(name).await,
            };
        }
        self.render_sections(sections).await
    }

    async fn render_sections(&self, sections: &[String]) -> String {
        let mut out = String::new();
        for name in sections {
            let section = match name.as_str() {
                "default" => self.render_default().await,
                "all" => self.render_all().await,
                "everything" => self.render_everything().await,
                other => self.render_one_section(other).await,
            };
            append_section(&mut out, &section);
        }
        out
    }

    async fn render_one_section(&self, name: &str) -> String {
        match name {
            "server" => self.render_server(),
            "clients" => self.render_clients(),
            "memory" => self.render_memory().await,
            "persistence" => self.render_persistence(),
            "stats" => self.render_stats(),
            "replication" => self.render_replication(),
            "cpu" => self.render_cpu(),
            "keyspace" => self.render_keyspace().await,
            "cluster" => self.render_cluster_section(),
            "commandstats" => self.render_commandstats(),
            "errorstats" => self.render_errorstats(),
            "threads" => self.render_threads(),
            "latencystats" => self.render_latencystats(),
            "keysizes" => self.render_keysizes().await,
            "hotkeys" => self.render_hotkeys(),
            "modules" => self.render_modules(),
            "storage" => self.render_storage(),
            _ => String::new(),
        }
    }

    async fn render_default(&self) -> String {
        let mut out = String::new();
        append_section(&mut out, &self.render_server());
        append_section(&mut out, &self.render_clients());
        append_section(&mut out, &self.render_memory().await);
        append_section(&mut out, &self.render_persistence());
        append_section(&mut out, &self.render_stats());
        append_section(&mut out, &self.render_replication());
        append_section(&mut out, &self.render_cpu());
        append_section(&mut out, &self.render_cluster_section());
        append_section(&mut out, &self.render_keyspace().await);
        append_section(&mut out, &self.render_storage());
        out
    }

    async fn render_all(&self) -> String {
        let mut out = self.render_default().await;
        append_section(&mut out, &self.render_commandstats());
        append_section(&mut out, &self.render_errorstats());
        append_section(&mut out, &self.render_threads());
        append_section(&mut out, &self.render_latencystats());
        out
    }

    async fn render_everything(&self) -> String {
        let mut out = self.render_all().await;
        append_section(&mut out, &self.render_modules());
        out
    }

    fn render_server(&self) -> String {
        let engine = match self.shared.engine_kind {
            StorageEngineKind::Memory => "memory",
            StorageEngineKind::AiDb => "aidb",
        };
        let persistent = u8::from(self.shared.engine_kind == StorageEngineKind::AiDb);
        let uptime = self.shared.uptime_secs();
        let uptime_days = uptime / 86_400;
        let server_time_usec = server_time_usec();
        let arch_bits = std::mem::size_of::<usize>() * 8;
        let executable =
            crate::server::process_metrics::read_executable_path().unwrap_or_else(|| {
                std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            });
        let multiplexing = if cfg!(target_os = "linux") {
            "epoll"
        } else {
            "poll"
        };
        let mut out = String::from("# Server\r\n");
        append_kv(&mut out, "redis_version", env!("CARGO_PKG_VERSION"));
        append_kv(&mut out, "redis_git_sha1", "0000000");
        append_kv(&mut out, "redis_git_dirty", "0");
        append_kv(&mut out, "redis_build_id", "aikv");
        append_kv(&mut out, "redis_compatible_version", REDIS_COMPAT_VERSION);
        append_kv(&mut out, "redis_mode", redis_mode());
        append_kv(&mut out, "os", std::env::consts::OS);
        append_kv_u64(&mut out, "arch_bits", arch_bits as u64);
        append_kv(&mut out, "monotonic_clock", "POSIX clock_gettime");
        append_kv(&mut out, "multiplexing_api", multiplexing);
        append_kv(&mut out, "atomicvar_api", "c11-builtin");
        append_kv(&mut out, "gcc_version", "-");
        append_kv_u64(&mut out, "process_id", std::process::id() as u64);
        append_kv(&mut out, "process_supervised", "no");
        append_kv(&mut out, "run_id", &self.shared.run_id);
        append_kv_u64(&mut out, "tcp_port", self.shared.tcp_port as u64);
        append_kv_u64(&mut out, "server_time_usec", server_time_usec);
        append_kv_u64(&mut out, "uptime_in_seconds", uptime);
        append_kv_u64(&mut out, "uptime_in_days", uptime_days);
        append_kv_u64(&mut out, "hz", 10);
        append_kv_u64(&mut out, "configured_hz", 10);
        append_kv_u64(&mut out, "lru_clock", lru_clock());
        append_kv(&mut out, "executable", &executable);
        append_kv(&mut out, "config_file", "");
        append_kv_u64(&mut out, "io_threads_active", 0);
        append_kv(&mut out, "storage_engine", engine);
        append_kv_u64(&mut out, "persistent", persistent as u64);
        out
    }

    fn render_clients(&self) -> String {
        let m = self.shared.metrics();
        let mut out = String::from("# Clients\r\n");
        append_kv_u64(&mut out, "connected_clients", m.connected_clients() as u64);
        append_kv_u64(&mut out, "cluster_connections", 0);
        append_kv_u64(
            &mut out,
            "maxclients",
            self.shared.connection_config.max_clients as u64,
        );
        append_kv_u64(&mut out, "client_recent_max_input_buffer", 0);
        append_kv_u64(&mut out, "client_recent_max_output_buffer", 0);
        append_kv_u64(&mut out, "blocked_clients", m.blocked_clients() as u64);
        append_kv_u64(&mut out, "tracking_clients", 0);
        append_kv_u64(&mut out, "pubsub_clients", 0);
        append_kv_u64(&mut out, "watching_clients", 0);
        append_kv_u64(&mut out, "clients_in_timeout_table", 0);
        append_kv_u64(&mut out, "active_clients", m.connected_clients() as u64);
        append_kv_u64(&mut out, "total_watched_keys", 0);
        append_kv_u64(&mut out, "total_blocking_keys", 0);
        append_kv_u64(&mut out, "total_blocking_keys_on_nokey", 0);
        out
    }

    async fn render_memory(&self) -> String {
        let used_memory = self
            .storage
            .memory_usage_bytes()
            .await
            .unwrap_or_else(|_| self.shared.metrics().used_memory_bytes());
        let peak = self.shared.metrics().used_memory_peak_bytes();
        let rss = self.shared.metrics().cached_rss_bytes();
        let total_system = self.shared.metrics().cached_total_system_memory_bytes();
        let maxmemory = config_u64(self.shared, "maxmemory");
        let maxmemory_policy =
            config_string(self.shared, "maxmemory-policy").unwrap_or_else(|| "noeviction".into());
        let (frag_ratio, frag_bytes) = mem_fragmentation(used_memory, rss);
        let peak_perc = if peak > 0 {
            (used_memory as f64 / peak as f64) * 100.0
        } else {
            100.0
        };
        let dataset_perc = if used_memory > 0 { 100.0 } else { 0.0 };

        let mut out = String::from("# Memory\r\n");
        append_kv_u64(&mut out, "used_memory", used_memory);
        append_kv(&mut out, "used_memory_human", &human_bytes(used_memory));
        append_kv_u64(&mut out, "used_memory_rss", rss);
        append_kv(&mut out, "used_memory_rss_human", &human_bytes(rss));
        append_kv_u64(&mut out, "used_memory_peak", peak);
        append_kv(&mut out, "used_memory_peak_human", &human_bytes(peak));
        append_kv_u64(&mut out, "used_memory_peak_time", 0);
        append_kv_f64(&mut out, "used_memory_peak_perc", peak_perc);
        append_kv_u64(&mut out, "used_memory_overhead", 0);
        append_kv_u64(&mut out, "used_memory_startup", used_memory);
        append_kv_u64(&mut out, "used_memory_dataset", used_memory);
        append_kv_f64(&mut out, "used_memory_dataset_perc", dataset_perc);
        append_kv_u64(&mut out, "allocator_allocated", 0);
        append_kv_u64(&mut out, "allocator_active", 0);
        append_kv_u64(&mut out, "allocator_resident", rss);
        append_kv_u64(&mut out, "allocator_muzzy", 0);
        append_kv_u64(&mut out, "total_system_memory", total_system);
        append_kv(
            &mut out,
            "total_system_memory_human",
            &human_bytes(total_system),
        );
        append_kv_u64(&mut out, "used_memory_lua", 0);
        append_kv_u64(&mut out, "used_memory_vm_eval", 0);
        append_kv(&mut out, "used_memory_lua_human", "0B");
        append_kv_u64(&mut out, "used_memory_scripts_eval", 0);
        append_kv_u64(&mut out, "number_of_cached_scripts", 0);
        append_kv_u64(&mut out, "number_of_functions", 0);
        append_kv_u64(&mut out, "number_of_libraries", 0);
        append_kv_u64(&mut out, "used_memory_vm_functions", 0);
        append_kv_u64(&mut out, "used_memory_vm_total", 0);
        append_kv(&mut out, "used_memory_vm_total_human", "0B");
        append_kv_u64(&mut out, "used_memory_functions", 0);
        append_kv_u64(&mut out, "used_memory_scripts", 0);
        append_kv(&mut out, "used_memory_scripts_human", "0B");
        append_kv_u64(&mut out, "maxmemory", maxmemory);
        append_kv(&mut out, "maxmemory_human", &human_bytes(maxmemory));
        append_kv(&mut out, "maxmemory_policy", &maxmemory_policy);
        append_kv_f64(&mut out, "allocator_frag_ratio", 1.0);
        append_kv_u64(&mut out, "allocator_frag_bytes", 0);
        append_kv_f64(&mut out, "allocator_rss_ratio", 1.0);
        append_kv_u64(&mut out, "allocator_rss_bytes", 0);
        append_kv_f64(&mut out, "rss_overhead_ratio", 1.0);
        append_kv_u64(&mut out, "rss_overhead_bytes", 0);
        append_kv_f64(&mut out, "mem_fragmentation_ratio", frag_ratio);
        append_kv_i64(&mut out, "mem_fragmentation_bytes", frag_bytes);
        append_kv_u64(&mut out, "mem_not_counted_for_evict", 0);
        append_kv_u64(&mut out, "mem_replication_backlog", 0);
        append_kv_u64(&mut out, "mem_total_replication_buffers", 0);
        append_kv_u64(&mut out, "mem_replica_full_sync_buffer", 0);
        append_kv_u64(&mut out, "mem_clients_slaves", 0);
        append_kv_u64(&mut out, "mem_clients_normal", 0);
        append_kv_u64(&mut out, "mem_clients_normal_shared", 0);
        append_kv_u64(&mut out, "mem_clients_normal_unshared", 0);
        append_kv_u64(&mut out, "mem_cluster_slot_migration_output_buffer", 0);
        append_kv_u64(&mut out, "mem_cluster_slot_migration_input_buffer", 0);
        append_kv_u64(&mut out, "mem_cluster_slot_migration_input_buffer_peak", 0);
        append_kv_u64(&mut out, "mem_cluster_links", 0);
        append_kv_u64(&mut out, "mem_aof_buffer", 0);
        append_kv(&mut out, "mem_allocator", "libc");
        append_kv_u64(&mut out, "mem_overhead_db_hashtable_rehashing", 0);
        append_kv_u64(&mut out, "active_defrag_running", 0);
        append_kv_u64(&mut out, "lazyfree_pending_objects", 0);
        append_kv_u64(&mut out, "lazyfreed_objects", 0);
        out
    }

    fn render_stats(&self) -> String {
        let m = self.shared.metrics();
        let mut out = String::from("# Stats\r\n");
        append_kv_u64(
            &mut out,
            "total_connections_received",
            m.connections_total(),
        );
        append_kv_u64(
            &mut out,
            "total_commands_processed",
            m.total_commands_processed(),
        );
        append_kv_u64(
            &mut out,
            "instantaneous_ops_per_sec",
            m.instantaneous_ops_per_sec(),
        );
        append_kv_u64(&mut out, "total_net_input_bytes", m.net_input_bytes());
        append_kv_u64(&mut out, "total_net_output_bytes", m.net_output_bytes());
        append_kv_u64(&mut out, "total_net_repl_input_bytes", 0);
        append_kv_u64(&mut out, "total_net_repl_output_bytes", 0);
        append_kv_u64(
            &mut out,
            "instantaneous_input_kbps",
            m.instantaneous_input_kbps(),
        );
        append_kv_u64(
            &mut out,
            "instantaneous_output_kbps",
            m.instantaneous_output_kbps(),
        );
        append_kv_u64(&mut out, "instantaneous_input_repl_kbps", 0);
        append_kv_u64(&mut out, "instantaneous_output_repl_kbps", 0);
        append_kv_u64(&mut out, "rejected_connections", m.rejected_connections());
        append_kv_u64(&mut out, "sync_full", 0);
        append_kv_u64(&mut out, "sync_partial_ok", 0);
        append_kv_u64(&mut out, "sync_partial_err", 0);
        append_kv_u64(&mut out, "expired_subkeys", 0);
        append_kv_u64(&mut out, "expired_subkeys_active", 0);
        append_kv_u64(&mut out, "expired_keys", m.expired_keys());
        append_kv_u64(&mut out, "expired_keys_active", 0);
        append_kv_u64(&mut out, "expired_stale_perc", 0);
        append_kv_u64(&mut out, "expired_time_cap_reached_count", 0);
        append_kv_u64(&mut out, "expire_cycle_cpu_milliseconds", 0);
        append_kv_u64(&mut out, "evicted_keys", m.evicted_keys());
        append_kv_u64(&mut out, "evicted_clients", 0);
        append_kv_u64(&mut out, "evicted_scripts", 0);
        append_kv_u64(&mut out, "total_eviction_exceeded_time", 0);
        append_kv_u64(&mut out, "current_eviction_exceeded_time", 0);
        append_kv_u64(&mut out, "keyspace_hits", m.keyspace_hits());
        append_kv_u64(&mut out, "keyspace_misses", m.keyspace_misses());
        append_kv_u64(&mut out, "pubsub_channels", 0);
        append_kv_u64(&mut out, "pubsub_patterns", 0);
        append_kv_u64(&mut out, "pubsubshard_channels", 0);
        append_kv_u64(&mut out, "latest_fork_usec", 0);
        append_kv_u64(&mut out, "total_forks", 0);
        append_kv_u64(&mut out, "migrate_cached_sockets", 0);
        append_kv_u64(&mut out, "slave_expires_tracked_keys", 0);
        append_kv_u64(&mut out, "active_defrag_hits", 0);
        append_kv_u64(&mut out, "active_defrag_misses", 0);
        append_kv_u64(&mut out, "active_defrag_key_hits", 0);
        append_kv_u64(&mut out, "active_defrag_key_misses", 0);
        append_kv_u64(&mut out, "total_active_defrag_time", 0);
        append_kv_u64(&mut out, "current_active_defrag_time", 0);
        append_kv_u64(&mut out, "tracking_total_keys", 0);
        append_kv_u64(&mut out, "tracking_total_items", 0);
        append_kv_u64(&mut out, "tracking_total_prefixes", 0);
        append_kv_u64(&mut out, "unexpected_error_replies", 0);
        append_kv_u64(&mut out, "total_error_replies", m.total_error_replies());
        append_kv_u64(&mut out, "dump_payload_sanitizations", 0);
        append_kv_u64(&mut out, "total_reads_processed", m.connections_total());
        append_kv_u64(
            &mut out,
            "total_writes_processed",
            m.total_commands_processed(),
        );
        append_kv_u64(&mut out, "io_threaded_reads_processed", 0);
        append_kv_u64(&mut out, "io_threaded_writes_processed", 0);
        append_kv_u64(&mut out, "io_threaded_total_prefetch_batches", 0);
        append_kv_u64(&mut out, "io_threaded_total_prefetch_entries", 0);
        append_kv_u64(&mut out, "client_query_buffer_limit_disconnections", 0);
        append_kv_u64(&mut out, "client_output_buffer_limit_disconnections", 0);
        append_kv_u64(&mut out, "reply_buffer_shrinks", 0);
        append_kv_u64(&mut out, "reply_buffer_expands", 0);
        append_kv_u64(&mut out, "eventloop_cycles", 0);
        append_kv_u64(&mut out, "eventloop_duration_sum", 0);
        append_kv_u64(&mut out, "eventloop_duration_cmd_sum", 0);
        append_kv_u64(&mut out, "instantaneous_eventloop_cycles_per_sec", 0);
        append_kv_u64(&mut out, "instantaneous_eventloop_duration_usec", 0);
        append_kv_u64(&mut out, "eventloop_cycles_with_clients_processing", 0);
        append_kv_u64(&mut out, "total_client_processing_events", 0);
        append_kv_u64(&mut out, "avg_pipeline_length_sum", 0);
        append_kv_u64(&mut out, "avg_pipeline_length_cnt", 0);
        append_kv_f64(&mut out, "avg_pipeline_length", 0.0);
        append_kv_u64(
            &mut out,
            "slowlog_commands_count",
            m.slowlog_commands_count(),
        );
        append_kv_u64(
            &mut out,
            "slowlog_commands_time_ms_max",
            m.slowlog_commands_time_ms_max(),
        );
        append_kv_u64(
            &mut out,
            "slowlog_commands_time_ms_sum",
            m.slowlog_commands_time_ms_sum(),
        );
        append_kv_u64(&mut out, "acl_access_denied_auth", 0);
        append_kv_u64(&mut out, "acl_access_denied_cmd", 0);
        append_kv_u64(&mut out, "acl_access_denied_key", 0);
        append_kv_u64(&mut out, "acl_access_denied_channel", 0);
        append_kv_u64(&mut out, "acl_access_denied_tls_cert", 0);
        out
    }

    fn render_replication(&self) -> String {
        let (role, slaves) = replication_snapshot();
        let mut out = String::from("# Replication\r\n");
        append_kv(&mut out, "role", role);
        append_kv(&mut out, "master_failover_state", "no-failover");
        append_kv(&mut out, "master_replid", &self.shared.run_id);
        append_kv(&mut out, "master_replid2", MASTER_REPLID2);
        append_kv_u64(&mut out, "master_repl_offset", 0);
        append_kv_i64(&mut out, "second_repl_offset", -1);
        append_kv_u64(&mut out, "repl_backlog_active", 0);
        append_kv_u64(&mut out, "repl_backlog_size", 1_048_576);
        append_kv_u64(&mut out, "repl_backlog_first_byte_offset", 0);
        append_kv_u64(&mut out, "repl_backlog_histlen", 0);
        append_kv_u64(&mut out, "connected_slaves", slaves);
        append_kv_u64(&mut out, "min_slaves_good_slaves", 0);
        out
    }

    fn render_cpu(&self) -> String {
        let (user, sys) = cpu_seconds();
        let mut out = String::from("# CPU\r\n");
        append_kv_f64(&mut out, "used_cpu_sys", sys);
        append_kv_f64(&mut out, "used_cpu_user", user);
        append_kv_f64(&mut out, "used_cpu_sys_children", 0.0);
        append_kv_f64(&mut out, "used_cpu_user_children", 0.0);
        append_kv_f64(&mut out, "used_cpu_sys_main_thread", sys);
        append_kv_f64(&mut out, "used_cpu_user_main_thread", user);
        out
    }

    fn render_persistence(&self) -> String {
        let in_progress = u8::from(self.shared.bgsave_in_progress());
        let last = self.shared.last_save_time();
        let status = self.shared.last_bgsave_status.read().clone();
        let mut out = String::from("# Persistence\r\n");
        append_kv_u64(&mut out, "loading", 0);
        append_kv_u64(&mut out, "async_loading", 0);
        append_kv_u64(&mut out, "current_cow_peak", 0);
        append_kv_u64(&mut out, "current_cow_size", 0);
        append_kv_u64(&mut out, "current_cow_size_age", 0);
        append_kv_u64(&mut out, "current_fork_perc", 0);
        append_kv_u64(&mut out, "current_save_keys_processed", 0);
        append_kv_u64(&mut out, "current_save_keys_total", 0);
        append_kv_u64(&mut out, "rdb_changes_since_last_save", 0);
        append_kv_u64(&mut out, "rdb_bgsave_in_progress", in_progress as u64);
        append_kv_u64(&mut out, "rdb_last_save_time", last);
        append_kv(&mut out, "rdb_last_bgsave_status", &status);
        append_kv_i64(&mut out, "rdb_last_bgsave_time_sec", last as i64);
        append_kv_i64(&mut out, "rdb_current_bgsave_time_sec", -1);
        append_kv_u64(&mut out, "rdb_saves", 0);
        append_kv_u64(&mut out, "rdb_saves_consecutive_failures", 0);
        append_kv_u64(&mut out, "rdb_last_cow_size", 0);
        append_kv_u64(&mut out, "rdb_last_load_keys_expired", 0);
        append_kv_u64(&mut out, "rdb_last_load_keys_loaded", 0);
        append_kv_u64(&mut out, "aof_enabled", 0);
        append_kv_u64(&mut out, "aof_rewrite_in_progress", 0);
        append_kv_u64(&mut out, "aof_rewrite_scheduled", 0);
        append_kv_i64(&mut out, "aof_last_rewrite_time_sec", -1);
        append_kv_i64(&mut out, "aof_current_rewrite_time_sec", -1);
        append_kv(&mut out, "aof_last_bgrewrite_status", "ok");
        append_kv_u64(&mut out, "aof_rewrites", 0);
        append_kv_u64(&mut out, "aof_rewrites_consecutive_failures", 0);
        append_kv(&mut out, "aof_last_write_status", "ok");
        append_kv_u64(&mut out, "aof_last_cow_size", 0);
        append_kv_u64(&mut out, "module_fork_in_progress", 0);
        append_kv_u64(&mut out, "module_fork_last_cow_size", 0);
        out
    }

    async fn render_keyspace(&self) -> String {
        use crate::storage::types::DB_COUNT;

        let mut out = String::from("# Keyspace\r\n");
        let db_count = self.storage.db_count().await.unwrap_or(0).min(DB_COUNT);
        for db in 0..db_count {
            if let Ok(stats) = self.storage.keyspace_stats(db).await {
                let _ = write!(
                    out,
                    "db{db}:keys={},expires={},avg_ttl={},subexpiry=0\r\n",
                    stats.keys, stats.expires, stats.avg_ttl
                );
            }
        }
        out
    }

    fn render_cluster_section(&self) -> String {
        let mut out = String::from("# Cluster\r\n");
        append_kv_u64(&mut out, "cluster_enabled", cluster_enabled() as u64);
        out
    }

    fn render_commandstats(&self) -> String {
        let mut out = String::from("# Commandstats\r\n");
        for (cmd, totals) in self.shared.metrics().client_command_totals() {
            let calls = totals.ok + totals.err;
            if calls == 0 {
                continue;
            }
            let usec_per_call = totals.usec as f64 / calls as f64;
            let stat_name = commandstat_name(&cmd);
            let _ = write!(
                out,
                "cmdstat_{stat_name}:calls={calls},usec={},usec_per_call={usec_per_call:.2},\
                 rejected_calls={},failed_calls={},\
                 slowlog_count={},slowlog_time_ms_sum={},slowlog_time_ms_max={}\r\n",
                totals.usec,
                totals.rejected,
                totals.err,
                totals.slowlog_count,
                totals.slowlog_time_ms_sum,
                totals.slowlog_time_ms_max,
            );
        }
        out
    }

    fn render_errorstats(&self) -> String {
        let mut out = String::from("# Errorstats\r\n");
        for (prefix, count) in self.shared.metrics().error_stat_totals() {
            let _ = write!(out, "errorstat_{prefix}:count={count}\r\n");
        }
        out
    }

    fn render_threads(&self) -> String {
        let mut out = String::from("# Threads\r\n");
        let (threads, vol, nonvol) = self.shared.metrics.get_threads_and_context_switches();
        append_kv_u64(&mut out, "process_threads", threads);
        append_kv_u64(&mut out, "context_switches_voluntary", vol);
        append_kv_u64(&mut out, "context_switches_nonvoluntary", nonvol);
        out
    }

    fn render_latencystats(&self) -> String {
        let mut out = String::from("# Latencystats\r\n");
        for (cmd, snap) in self.shared.latency_stats.histogram_snapshots(None) {
            let _ = write!(
                out,
                "latency_percentiles_usec_{cmd}:p50={p50},p99={p99},p999={p999}\r\n",
                p50 = snap.p50_us,
                p99 = snap.p99_us,
                p999 = snap.p999_us,
            );
        }
        out
    }

    async fn render_keysizes(&self) -> String {
        use crate::storage::types::DB_COUNT;

        let mut out = String::from("# Keysizes\r\n");
        let db_count = self.storage.db_count().await.unwrap_or(0).min(DB_COUNT);
        for db in 0..db_count {
            let _ = write!(out, "db{db}_distrib_strings_sizes:\r\n");
            let _ = write!(out, "db{db}_distrib_lists_items:\r\n");
            let _ = write!(out, "db{db}_distrib_sets_items:\r\n");
            let _ = write!(out, "db{db}_distrib_hashes_items:\r\n");
            let _ = write!(out, "db{db}_distrib_zsets_items:\r\n");
        }
        out
    }

    fn render_hotkeys(&self) -> String {
        let mut out = String::from("# Hotkeys\r\n");
        append_kv_u64(&mut out, "tracking_active", 0);
        append_kv_u64(&mut out, "used_memory", 0);
        append_kv_u64(&mut out, "cpu_time", 0);
        out
    }

    fn render_modules(&self) -> String {
        "# Modules\r\n".to_string()
    }

    fn render_storage(&self) -> String {
        let mut out = String::from("# Storage\r\n");
        if let Some(stats) = self.storage.aidb_statistics() {
            append_kv(&mut out, "storage_engine", "aidb");
            let snap = stats.snapshot();
            append_kv_u64(&mut out, "aidb_block_cache_hits", snap.block_cache_hits);
            append_kv_u64(&mut out, "aidb_block_cache_misses", snap.block_cache_misses);
            append_kv_u64(&mut out, "aidb_block_cache_size", snap.block_cache_size);
            append_kv_u64(
                &mut out,
                "aidb_block_cache_capacity",
                snap.block_cache_capacity,
            );
            append_kv_u64(
                &mut out,
                "aidb_memtable_active_bytes",
                snap.memtable_size_bytes[0],
            );
            append_kv_u64(
                &mut out,
                "aidb_memtable_frozen_bytes",
                snap.memtable_size_bytes[1],
            );
            append_kv_u64(&mut out, "aidb_wal_size_bytes", snap.wal_size_bytes);
            append_kv_u64(&mut out, "aidb_total_key_count", snap.total_key_count);
            append_kv_u64(&mut out, "aidb_sequence", snap.sequence);
            append_kv_u64(&mut out, "aidb_flush_total", snap.flush_total);
            append_kv_u64(
                &mut out,
                "aidb_bloom_false_positive",
                snap.bloom_false_positive,
            );
            let sst_count: u64 = snap.sstable_count.iter().sum();
            append_kv_u64(&mut out, "aidb_sstable_count_total", sst_count);
            let sst_size: u64 = snap.sstable_size_bytes.iter().sum();
            append_kv_u64(&mut out, "aidb_sstable_size_bytes_total", sst_size);

            // 写放大 (WA) 指标
            append_kv_u64(&mut out, "aidb_wal_written_bytes", snap.wal_written_bytes);
            append_kv_u64(
                &mut out,
                "aidb_flush_written_bytes",
                snap.flush_written_bytes,
            );
            append_kv_u64(
                &mut out,
                "aidb_compaction_written_bytes",
                snap.compaction_written_bytes,
            );
            append_kv_u64(
                &mut out,
                "aidb_logical_write_bytes",
                snap.logical_write_bytes,
            );

            // 读放大 (RA) 指标
            append_kv_u64(&mut out, "aidb_block_read_bytes", snap.block_read_bytes);
            append_kv_u64(&mut out, "aidb_logical_read_bytes", snap.logical_read_bytes);
            append_kv_u64(
                &mut out,
                "aidb_compaction_read_bytes",
                snap.compaction_read_bytes,
            );
            append_kv_u64(&mut out, "aidb_bloom_useful", snap.bloom_useful);

            // Compaction 积压
            append_kv_u64(
                &mut out,
                "aidb_compaction_pending_bytes",
                snap.compaction_pending_bytes,
            );

            // 写停顿 (Write Stall) 汇总指标
            let stall_requests: u64 = snap.write_stall_requests.iter().sum();
            append_kv_u64(&mut out, "aidb_write_stall_requests", stall_requests);
            let stall_duration_us: u64 = snap.write_stall_duration_sum_us.iter().sum();
            append_kv_u64(&mut out, "aidb_write_stall_duration_us", stall_duration_us);
            append_kv_u64(
                &mut out,
                "aidb_write_stall_max_duration_us",
                snap.write_stall_max_duration_us,
            );

            // 冷启动恢复 (Recovery) 指标
            append_kv_u64(
                &mut out,
                "aidb_recovery_wal_replay_duration_us",
                snap.recovery_wal_replay_duration_us,
            );
            append_kv_u64(
                &mut out,
                "aidb_recovery_wal_replayed_bytes",
                snap.recovery_wal_replayed_bytes,
            );
            append_kv_u64(
                &mut out,
                "aidb_recovery_manifest_duration_us",
                snap.recovery_manifest_duration_us,
            );
            append_kv_u64(
                &mut out,
                "aidb_recovery_sstable_open_duration_us",
                snap.recovery_sstable_open_duration_us,
            );
            append_kv_u64(
                &mut out,
                "aikv_recovery_rebuild_counters_duration_us",
                self.storage.rebuild_counters_duration_us(),
            );
        } else {
            let engine = match self.shared.engine_kind {
                StorageEngineKind::Memory => "memory",
                StorageEngineKind::AiDb => "aidb",
            };
            append_kv(&mut out, "storage_engine", engine);
        }
        out
    }
}

fn commandstat_name(cmd: &str) -> String {
    cmd.to_ascii_lowercase().replace(' ', "|")
}

fn config_string(shared: &ServerSharedState, key: &str) -> Option<String> {
    shared.config_map.read().get(key).cloned()
}

fn config_u64(shared: &ServerSharedState, key: &str) -> u64 {
    config_string(shared, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn mem_fragmentation(used: u64, rss: u64) -> (f64, i64) {
    if used == 0 {
        return (1.0, 0);
    }
    let ratio = rss as f64 / used as f64;
    let bytes = rss as i64 - used as i64;
    (ratio, bytes)
}

fn server_time_usec() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

fn lru_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60
}

fn append_section(out: &mut String, section: &str) {
    if section.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str("\r\n");
    }
    out.push_str(section);
}

fn append_kv(out: &mut String, key: &str, value: &str) {
    let _ = write!(out, "{key}:{value}\r\n");
}

fn append_kv_u64(out: &mut String, key: &str, value: u64) {
    let _ = write!(out, "{key}:{value}\r\n");
}

fn append_kv_i64(out: &mut String, key: &str, value: i64) {
    let _ = write!(out, "{key}:{value}\r\n");
}

fn append_kv_f64(out: &mut String, key: &str, value: f64) {
    let _ = write!(out, "{key}:{value:.2}\r\n");
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.2}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.2}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

fn replication_snapshot() -> (&'static str, u64) {
    #[cfg(feature = "cluster")]
    {
        if let Some(mgr) = crate::cluster::state::CLUSTER_STATE_MGR.get() {
            let meta = mgr.meta_raft.get_cluster_meta();
            return (
                crate::cluster::replication::node_replication_role(&meta, mgr.node_id),
                crate::cluster::replication::connected_slaves_count(&meta, mgr.node_id),
            );
        }
    }
    ("master", 0)
}

fn cpu_seconds() -> (f64, f64) {
    if let Some((user, sys)) = crate::server::process_metrics::read_cpu_user_sys_seconds() {
        return (user, sys);
    }
    (0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_mode_standalone_without_cluster_mgr() {
        assert_eq!(redis_mode(), "standalone");
        assert_eq!(cluster_enabled(), 0);
    }

    #[test]
    fn commandstat_name_replaces_spaces() {
        assert_eq!(commandstat_name("ACL LIST"), "acl|list");
    }

    #[test]
    fn mem_fragmentation_zero_used() {
        assert_eq!(mem_fragmentation(0, 1024), (1.0, 0));
    }
}
