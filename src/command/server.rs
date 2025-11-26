use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// AiKv version - the actual version of this server
const AIKV_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Redis-compatible version to report for client compatibility
/// We report a modern Redis version to ensure clients like StackExchange.Redis work correctly
const REDIS_COMPAT_VERSION: &str = "7.2.4";

/// Client info structure
#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub id: usize,
    pub name: Option<String>,
    pub addr: String,
}

/// Server command handler
pub struct ServerCommands {
    clients: Arc<RwLock<HashMap<usize, ClientInfo>>>,
    config: Arc<RwLock<HashMap<String, String>>>,
    start_time: Instant,
    run_id: String,
    tcp_port: u16,
}

/// Generate a random 40-character hex string for run_id (similar to Redis)
fn generate_run_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut result = String::with_capacity(40);
    let hasher_builder = RandomState::new();

    // Generate enough random data for a 40-char hex string
    // Include loop index to ensure uniqueness even when called rapidly
    for i in 0..5 {
        let mut hasher = hasher_builder.build_hasher();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        hasher.write_u64(nanos.wrapping_add(i as u64));
        result.push_str(&format!("{:016x}", hasher.finish()));
    }

    result.truncate(40);
    result
}

impl ServerCommands {
    pub fn new() -> Self {
        Self::with_port(6379)
    }

    pub fn with_port(port: u16) -> Self {
        let mut default_config = HashMap::new();
        default_config.insert("server".to_string(), "aikv".to_string());
        default_config.insert("version".to_string(), AIKV_VERSION.to_string());
        default_config.insert("port".to_string(), port.to_string());
        default_config.insert("databases".to_string(), "16".to_string());

        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(default_config)),
            start_time: Instant::now(),
            run_id: generate_run_id(),
            tcp_port: port,
        }
    }

    /// Get server uptime in seconds
    fn uptime_seconds(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Build the Server section info lines
    fn build_server_info(&self) -> Vec<String> {
        let uptime_secs = self.uptime_seconds();
        let uptime_days = uptime_secs / 86400;
        let pid = std::process::id();

        vec![
            "# Server".to_string(),
            format!("redis_version:{}", REDIS_COMPAT_VERSION),
            "redis_git_sha1:00000000".to_string(),
            "redis_git_dirty:0".to_string(),
            format!("redis_build_id:aikv{}", AIKV_VERSION.replace('.', "")),
            "redis_mode:standalone".to_string(),
            format!("os:{} {} {}", std::env::consts::OS, std::env::consts::FAMILY, std::env::consts::ARCH),
            format!("arch_bits:{}", if cfg!(target_pointer_width = "64") { 64 } else { 32 }),
            "multiplexing_api:tokio".to_string(),
            format!("process_id:{}", pid),
            format!("run_id:{}", self.run_id),
            format!("tcp_port:{}", self.tcp_port),
            format!("uptime_in_seconds:{}", uptime_secs),
            format!("uptime_in_days:{}", uptime_days),
            "hz:10".to_string(),
            "configured_hz:10".to_string(),
            "lru_clock:0".to_string(),
            "executable:aikv".to_string(),
            "config_file:".to_string(),
            "io_threads_active:0".to_string(),
            format!("aikv_version:{}", AIKV_VERSION),
        ]
    }

    /// Build the Clients section info lines
    fn build_clients_info(&self) -> Result<Vec<String>> {
        let clients = self
            .clients
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        Ok(vec![
            "# Clients".to_string(),
            format!("connected_clients:{}", clients.len()),
            "cluster_connections:0".to_string(),
            "maxclients:10000".to_string(),
            "client_recent_max_input_buffer:0".to_string(),
            "client_recent_max_output_buffer:0".to_string(),
            "blocked_clients:0".to_string(),
            "tracking_clients:0".to_string(),
            "clients_in_timeout_table:0".to_string(),
        ])
    }

    /// Build the Memory section info lines
    fn build_memory_info(&self) -> Vec<String> {
        vec![
            "# Memory".to_string(),
            "used_memory:1024000".to_string(),
            "used_memory_human:1000.00K".to_string(),
            "used_memory_rss:2048000".to_string(),
            "used_memory_rss_human:2.00M".to_string(),
            "used_memory_peak:1024000".to_string(),
            "used_memory_peak_human:1000.00K".to_string(),
            "used_memory_peak_perc:100.00%".to_string(),
            "used_memory_overhead:1000000".to_string(),
            "used_memory_startup:1000000".to_string(),
            "used_memory_dataset:24000".to_string(),
            "used_memory_dataset_perc:0.00%".to_string(),
            "allocator_allocated:1024000".to_string(),
            "allocator_active:2048000".to_string(),
            "allocator_resident:2048000".to_string(),
            "total_system_memory:8589934592".to_string(),
            "total_system_memory_human:8.00G".to_string(),
            "used_memory_lua:31744".to_string(),
            "used_memory_lua_human:31.00K".to_string(),
            "used_memory_scripts:0".to_string(),
            "used_memory_scripts_human:0B".to_string(),
            "maxmemory:0".to_string(),
            "maxmemory_human:0B".to_string(),
            "maxmemory_policy:noeviction".to_string(),
            "allocator_frag_ratio:1.00".to_string(),
            "allocator_frag_bytes:0".to_string(),
            "allocator_rss_ratio:1.00".to_string(),
            "allocator_rss_bytes:0".to_string(),
            "rss_overhead_ratio:1.00".to_string(),
            "rss_overhead_bytes:0".to_string(),
            "mem_fragmentation_ratio:2.00".to_string(),
            "mem_fragmentation_bytes:1024000".to_string(),
            "mem_not_counted_for_evict:0".to_string(),
            "mem_replication_backlog:0".to_string(),
            "mem_clients_slaves:0".to_string(),
            "mem_clients_normal:0".to_string(),
            "mem_aof_buffer:0".to_string(),
            "mem_allocator:jemalloc-5.3.0".to_string(),
            "active_defrag_running:0".to_string(),
            "lazyfree_pending_objects:0".to_string(),
            "lazyfreed_objects:0".to_string(),
        ]
    }

    /// Build the Stats section info lines
    fn build_stats_info(&self) -> Vec<String> {
        vec![
            "# Stats".to_string(),
            "total_connections_received:1".to_string(),
            "total_commands_processed:1".to_string(),
            "instantaneous_ops_per_sec:0".to_string(),
            "total_net_input_bytes:0".to_string(),
            "total_net_output_bytes:0".to_string(),
            "instantaneous_input_kbps:0.00".to_string(),
            "instantaneous_output_kbps:0.00".to_string(),
            "rejected_connections:0".to_string(),
            "sync_full:0".to_string(),
            "sync_partial_ok:0".to_string(),
            "sync_partial_err:0".to_string(),
            "expired_keys:0".to_string(),
            "expired_stale_perc:0.00".to_string(),
            "expired_time_cap_reached_count:0".to_string(),
            "expire_cycle_cpu_milliseconds:0".to_string(),
            "evicted_keys:0".to_string(),
            "keyspace_hits:0".to_string(),
            "keyspace_misses:0".to_string(),
            "pubsub_channels:0".to_string(),
            "pubsub_patterns:0".to_string(),
            "latest_fork_usec:0".to_string(),
            "total_forks:0".to_string(),
            "migrate_cached_sockets:0".to_string(),
            "slave_expires_tracked_keys:0".to_string(),
            "active_defrag_hits:0".to_string(),
            "active_defrag_misses:0".to_string(),
            "active_defrag_key_hits:0".to_string(),
            "active_defrag_key_misses:0".to_string(),
            "tracking_total_keys:0".to_string(),
            "tracking_total_items:0".to_string(),
            "tracking_total_prefixes:0".to_string(),
            "unexpected_error_replies:0".to_string(),
            "total_error_replies:0".to_string(),
            "dump_payload_sanitizations:0".to_string(),
            "total_reads_processed:1".to_string(),
            "total_writes_processed:1".to_string(),
            "io_threaded_reads_processed:0".to_string(),
            "io_threaded_writes_processed:0".to_string(),
        ]
    }

    /// Build the Replication section info lines
    fn build_replication_info(&self) -> Vec<String> {
        vec![
            "# Replication".to_string(),
            "role:master".to_string(),
            "connected_slaves:0".to_string(),
            "master_failover_state:no-failover".to_string(),
            "master_replid:0000000000000000000000000000000000000000".to_string(),
            "master_replid2:0000000000000000000000000000000000000000".to_string(),
            "master_repl_offset:0".to_string(),
            "second_repl_offset:-1".to_string(),
            "repl_backlog_active:0".to_string(),
            "repl_backlog_size:1048576".to_string(),
            "repl_backlog_first_byte_offset:0".to_string(),
            "repl_backlog_histlen:0".to_string(),
        ]
    }

    /// Build the CPU section info lines
    fn build_cpu_info(&self) -> Vec<String> {
        vec![
            "# CPU".to_string(),
            "used_cpu_sys:0.000000".to_string(),
            "used_cpu_user:0.000000".to_string(),
            "used_cpu_sys_children:0.000000".to_string(),
            "used_cpu_user_children:0.000000".to_string(),
            "used_cpu_sys_main_thread:0.000000".to_string(),
            "used_cpu_user_main_thread:0.000000".to_string(),
        ]
    }

    /// Build the Modules section info lines
    fn build_modules_info(&self) -> Vec<String> {
        vec![
            "# Modules".to_string(),
        ]
    }

    /// Build the Errorstats section info lines
    fn build_errorstats_info(&self) -> Vec<String> {
        vec![
            "# Errorstats".to_string(),
        ]
    }

    /// Build the Cluster section info lines
    fn build_cluster_info(&self) -> Vec<String> {
        vec![
            "# Cluster".to_string(),
            "cluster_enabled:0".to_string(),
        ]
    }

    /// Build the Keyspace section info lines
    fn build_keyspace_info(&self) -> Vec<String> {
        vec![
            "# Keyspace".to_string(),
        ]
    }

    /// Build the Persistence section info lines
    fn build_persistence_info(&self) -> Vec<String> {
        vec![
            "# Persistence".to_string(),
            "loading:0".to_string(),
            "current_cow_size:0".to_string(),
            "current_cow_size_age:0".to_string(),
            "current_fork_perc:0.00".to_string(),
            "current_save_keys_processed:0".to_string(),
            "current_save_keys_total:0".to_string(),
            "rdb_changes_since_last_save:0".to_string(),
            "rdb_bgsave_in_progress:0".to_string(),
            "rdb_last_save_time:0".to_string(),
            "rdb_last_bgsave_status:ok".to_string(),
            "rdb_last_bgsave_time_sec:-1".to_string(),
            "rdb_current_bgsave_time_sec:-1".to_string(),
            "rdb_last_cow_size:0".to_string(),
            "aof_enabled:0".to_string(),
            "aof_rewrite_in_progress:0".to_string(),
            "aof_rewrite_scheduled:0".to_string(),
            "aof_last_rewrite_time_sec:-1".to_string(),
            "aof_current_rewrite_time_sec:-1".to_string(),
            "aof_last_bgrewrite_status:ok".to_string(),
            "aof_last_write_status:ok".to_string(),
            "aof_last_cow_size:0".to_string(),
            "module_fork_in_progress:0".to_string(),
            "module_fork_last_cow_size:0".to_string(),
        ]
    }

    /// INFO \[section\] - Get server information
    pub fn info(&self, args: &[Bytes]) -> Result<RespValue> {
        let section = if args.is_empty() {
            "default"
        } else {
            &String::from_utf8_lossy(&args[0])
        };

        let mut info_lines = Vec::new();

        match section.to_lowercase().as_str() {
            // "default" returns just server section (same as no args in Redis)
            "default" | "server" => {
                info_lines.extend(self.build_server_info());
            }
            "clients" => {
                info_lines.extend(self.build_clients_info()?);
            }
            "memory" => {
                info_lines.extend(self.build_memory_info());
            }
            "stats" => {
                info_lines.extend(self.build_stats_info());
            }
            "replication" => {
                info_lines.extend(self.build_replication_info());
            }
            "cpu" => {
                info_lines.extend(self.build_cpu_info());
            }
            "modules" => {
                info_lines.extend(self.build_modules_info());
            }
            "errorstats" => {
                info_lines.extend(self.build_errorstats_info());
            }
            "cluster" => {
                info_lines.extend(self.build_cluster_info());
            }
            "keyspace" => {
                info_lines.extend(self.build_keyspace_info());
            }
            "persistence" => {
                info_lines.extend(self.build_persistence_info());
            }
            // "all" or "everything" returns all sections
            "all" | "everything" => {
                info_lines.extend(self.build_server_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_clients_info()?);
                info_lines.push(String::new());
                info_lines.extend(self.build_memory_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_persistence_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_stats_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_replication_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_cpu_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_modules_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_errorstats_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_cluster_info());
                info_lines.push(String::new());
                info_lines.extend(self.build_keyspace_info());
            }
            // For unknown sections, return empty result (Redis behavior)
            // This is important for compatibility - Redis returns empty for unknown sections
            _ => {
                // Return empty bulk string for unknown sections to maintain compatibility
            }
        }

        let info_str = info_lines.join("\r\n");
        Ok(RespValue::bulk_string(info_str))
    }

    /// CONFIG GET parameter - Get configuration value
    pub fn config_get(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CONFIG GET".to_string()));
        }

        let parameter = String::from_utf8_lossy(&args[0]).to_string();
        let config = self
            .config
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut results = Vec::new();

        // Support wildcard matching
        if parameter == "*" {
            for (key, value) in config.iter() {
                results.push(RespValue::bulk_string(key.clone()));
                results.push(RespValue::bulk_string(value.clone()));
            }
        } else if let Some(value) = config.get(&parameter) {
            results.push(RespValue::bulk_string(parameter.clone()));
            results.push(RespValue::bulk_string(value.clone()));
        }

        Ok(RespValue::array(results))
    }

    /// CONFIG SET parameter value - Set configuration value
    pub fn config_set(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("CONFIG SET".to_string()));
        }

        let parameter = String::from_utf8_lossy(&args[0]).to_string();
        let value = String::from_utf8_lossy(&args[1]).to_string();

        let mut config = self
            .config
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        // For safety, only allow certain parameters to be set
        match parameter.as_str() {
            "server" | "version" | "port" => {
                return Err(AikvError::InvalidArgument(
                    "ERR configuration parameter is read-only".to_string(),
                ));
            }
            _ => {}
        }

        config.insert(parameter, value);
        Ok(RespValue::ok())
    }

    /// TIME - Return the current server time
    pub fn time(&self, _args: &[Bytes]) -> Result<RespValue> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| AikvError::Storage(format!("Time error: {}", e)))?;

        let seconds = now.as_secs();
        let microseconds = now.subsec_micros();

        Ok(RespValue::array(vec![
            RespValue::bulk_string(seconds.to_string()),
            RespValue::bulk_string(microseconds.to_string()),
        ]))
    }

    /// CLIENT LIST - List all client connections
    pub fn client_list(&self, _args: &[Bytes]) -> Result<RespValue> {
        let clients = self
            .clients
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut client_lines = Vec::new();
        for (id, client) in clients.iter() {
            let name = client
                .name
                .as_ref()
                .map(|n| format!(" name={}", n))
                .unwrap_or_default();
            client_lines.push(format!("id={} addr={}{}", id, client.addr, name));
        }

        let client_str = client_lines.join("\n");
        Ok(RespValue::bulk_string(client_str))
    }

    /// CLIENT SETNAME name - Set client name
    pub fn client_setname(&self, args: &[Bytes], client_id: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("CLIENT SETNAME".to_string()));
        }

        let name = String::from_utf8_lossy(&args[0]).to_string();

        let mut clients = self
            .clients
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(client) = clients.get_mut(&client_id) {
            client.name = Some(name);
        }

        Ok(RespValue::ok())
    }

    /// CLIENT GETNAME - Get client name
    pub fn client_getname(&self, _args: &[Bytes], client_id: usize) -> Result<RespValue> {
        let clients = self
            .clients
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(client) = clients.get(&client_id) {
            if let Some(name) = &client.name {
                return Ok(RespValue::bulk_string(name.clone()));
            }
        }

        Ok(RespValue::null_bulk_string())
    }

    /// Register a client
    pub fn register_client(&self, id: usize, addr: String) -> Result<()> {
        let mut clients = self
            .clients
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        clients.insert(
            id,
            ClientInfo {
                id,
                name: None,
                addr,
            },
        );
        Ok(())
    }

    /// Unregister a client
    pub fn unregister_client(&self, id: usize) -> Result<()> {
        let mut clients = self
            .clients
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        clients.remove(&id);
        Ok(())
    }
}

impl Default for ServerCommands {
    fn default() -> Self {
        Self::new()
    }
}
