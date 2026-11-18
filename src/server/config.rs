//! 服务与连接配置

use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::command::CommandRouter;
use crate::server::latency::LatencyStats;
use crate::server::metrics::ServerMetrics;
use crate::server::slowlog::{SlowQueryLog, DEFAULT_SLOWLOG_MAX_LEN, DEFAULT_SLOWLOG_THRESHOLD_US};
use crate::storage::{KvStorage, StorageEngineKind, StorageObservation};

pub(crate) type TransactionGate = Arc<tokio::sync::RwLock<()>>;

/// 支持运行期动态热更新的服务端原子配置句柄 (方案 A).
#[derive(Debug)]
pub struct RuntimeMutableConfig {
    pub max_clients: AtomicUsize,
    pub idle_timeout_ms: AtomicU64,
    pub read_timeout_ms: AtomicU64,
    pub requirepass: RwLock<Option<String>>,
    pub output_buffer_normal_hard_bytes: AtomicU64,
    pub output_buffer_normal_soft_bytes: AtomicU64,
    pub output_buffer_normal_soft_secs: AtomicU64,
}

impl RuntimeMutableConfig {
    pub fn from_connection_config(cfg: &ConnectionConfig) -> Self {
        Self {
            max_clients: AtomicUsize::new(cfg.max_clients),
            idle_timeout_ms: AtomicU64::new(
                cfg.idle_timeout.map(|d| d.as_millis() as u64).unwrap_or(0),
            ),
            read_timeout_ms: AtomicU64::new(
                cfg.read_timeout.map(|d| d.as_millis() as u64).unwrap_or(0),
            ),
            requirepass: RwLock::new(None),
            output_buffer_normal_hard_bytes: AtomicU64::new(0),
            output_buffer_normal_soft_bytes: AtomicU64::new(0),
            output_buffer_normal_soft_secs: AtomicU64::new(0),
        }
    }

    pub fn idle_timeout(&self) -> Option<std::time::Duration> {
        let ms = self.idle_timeout_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| std::time::Duration::from_millis(ms))
    }

    pub fn read_timeout(&self) -> Option<std::time::Duration> {
        let ms = self.read_timeout_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| std::time::Duration::from_millis(ms))
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub read_timeout: Option<std::time::Duration>,
    pub idle_timeout: Option<std::time::Duration>,
    /// 最大并发连接数; 0 表示不限制.
    pub max_clients: usize,
    /// `EVAL` / `EVALSHA` 专用线程数.
    pub script_worker_threads: usize,
    pub tcp_nodelay: bool,
    pub tcp_keepalive_secs: u64,
    pub resp3_enabled: bool,
    pub proto_max_bulk_len: usize,
    pub proto_max_buffer_size: usize,
    pub proto_max_parse_depth: usize,
    pub pipeline_max_pending_writes: usize,
    pub key_lock_buckets: usize,
    pub script: crate::config::ResolvedScript,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            read_timeout: Some(std::time::Duration::from_secs(60)),
            idle_timeout: Some(std::time::Duration::from_secs(300)),
            max_clients: 10_000,
            script_worker_threads: crate::config::settings::default_script_worker_threads(),
            tcp_nodelay: true,
            tcp_keepalive_secs: 0,
            resp3_enabled: true,
            proto_max_bulk_len: 512 * 1024 * 1024,
            proto_max_buffer_size: 64 * 1024 * 1024,
            proto_max_parse_depth: 128,
            pipeline_max_pending_writes: 256,
            key_lock_buckets: 4096,
            script: crate::config::ResolvedScript {
                off_worker: "auto".to_string(),
                pool_threshold_us: 1000,
                timeout_ms: 5000,
                lock_timeout_ms: 30_000,
                memory_limit_bytes: 128 * 1024 * 1024,
                max_idle_vms: 64,
                cache_max_scripts: 256,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: usize,
    pub addr: SocketAddr,
    pub name: Option<String>,
    pub db: usize,
}

pub type LogReloadCallback = Arc<dyn Fn(&str) -> std::result::Result<(), String> + Send + Sync>;

/// 将单一日志级别扩展为带噪声模块压制的标准 `EnvFilter` 表达式.
pub fn expand_log_filter(raw: &str) -> std::result::Result<tracing_subscriber::EnvFilter, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("loglevel cannot be empty".to_string());
    }
    let expr = if !trimmed.contains(',') && !trimmed.contains('=') {
        format!(
            "{trimmed},openraft=warn,tonic=warn,h2=warn,hyper=warn,opentelemetry=warn,reqwest=warn"
        )
    } else {
        trimmed.to_string()
    };
    tracing_subscriber::EnvFilter::try_new(&expr)
        .map_err(|e| format!("Invalid loglevel '{raw}': {e}"))
}

pub struct ServerSharedState {
    pub connection_config: Arc<ConnectionConfig>,
    pub runtime_config: Arc<RuntimeMutableConfig>,
    pub metrics: Arc<ServerMetrics>,
    pub slow_query_log: Arc<SlowQueryLog>,
    pub latency_stats: Arc<LatencyStats>,
    pub storage: Arc<dyn KvStorage>,
    router: OnceLock<Arc<CommandRouter>>,
    pub config_map: Arc<RwLock<HashMap<String, String>>>,
    pub clients: Arc<RwLock<HashMap<usize, ClientInfo>>>,
    pub monitor_tx: broadcast::Sender<String>,
    pub start_time_secs: u64,
    pub run_id: String,
    pub tcp_port: u16,
    pub shutdown: CancellationToken,
    pub engine_kind: StorageEngineKind,
    pub data_dir: Option<PathBuf>,
    pub backup_dir: PathBuf,
    pub last_save_time: AtomicU64,
    pub bgsave_in_progress: AtomicBool,
    pub last_bgsave_status: RwLock<String>,
    pub metrics_port: u16,
    pub metrics_addr: String,
    next_client_id: AtomicUsize,
    pub(crate) transaction_gate: TransactionGate,
    storage_observation: Arc<StorageObservation>,
    log_reloader: RwLock<Option<LogReloadCallback>>,
    rename_table: RwLock<CommandRenameTable>,
}

#[derive(Debug, Default, Clone)]
pub struct CommandRenameTable {
    alias_to_canonical: HashMap<String, String>,
    canonical_to_alias: HashMap<String, String>,
    disabled: std::collections::HashSet<String>,
}

impl CommandRenameTable {
    pub fn from_map(map: &std::collections::BTreeMap<String, String>) -> Self {
        let mut table = Self::default();
        for (orig, target) in map {
            if target.is_empty() {
                table.disabled.insert(orig.clone());
            } else {
                table
                    .alias_to_canonical
                    .insert(target.clone(), orig.clone());
                table
                    .canonical_to_alias
                    .insert(orig.clone(), target.clone());
            }
        }
        table
    }

    pub fn resolve<'a>(&self, cmd_upper: &'a str) -> Option<std::borrow::Cow<'a, str>> {
        if let Some(canonical) = self.alias_to_canonical.get(cmd_upper) {
            return Some(std::borrow::Cow::Owned(canonical.clone()));
        }
        if self.disabled.contains(cmd_upper) || self.canonical_to_alias.contains_key(cmd_upper) {
            return None;
        }
        Some(std::borrow::Cow::Borrowed(cmd_upper))
    }

    pub fn display_name(&self, canonical_upper: &str) -> Option<String> {
        if self.disabled.contains(canonical_upper) {
            return None;
        }
        if let Some(alias) = self.canonical_to_alias.get(canonical_upper) {
            return Some(alias.clone());
        }
        Some(canonical_upper.to_string())
    }

    pub fn disabled_count(&self) -> usize {
        self.disabled.len()
    }
}

impl ServerSharedState {
    pub fn new(
        connection_config: ConnectionConfig,
        storage: Arc<dyn KvStorage>,
        tcp_port: u16,
    ) -> Arc<Self> {
        Self::new_with_engine(
            connection_config,
            storage,
            tcp_port,
            StorageEngineKind::Memory,
            None,
        )
    }

    pub fn new_with_engine(
        connection_config: ConnectionConfig,
        storage: Arc<dyn KvStorage>,
        tcp_port: u16,
        engine_kind: StorageEngineKind,
        data_dir: Option<PathBuf>,
    ) -> Arc<Self> {
        Self::new_with_backup_dir(
            connection_config,
            storage,
            tcp_port,
            engine_kind,
            data_dir,
            None,
            9191,
            "127.0.0.1".into(),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_backup_dir(
        connection_config: ConnectionConfig,
        storage: Arc<dyn KvStorage>,
        tcp_port: u16,
        engine_kind: StorageEngineKind,
        data_dir: Option<PathBuf>,
        backup_dir_override: Option<PathBuf>,
        metrics_port: u16,
        metrics_addr: String,
        storage_observation: Option<Arc<StorageObservation>>,
    ) -> Arc<Self> {
        let backup_dir = backup_dir_override.unwrap_or_else(|| {
            data_dir
                .as_ref()
                .map(|p| p.join("backup"))
                .unwrap_or_else(|| PathBuf::from("./backup"))
        });
        let start_time_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let run_id: String = (0..40)
            .map(|_| {
                const HEX: &[u8] = b"0123456789abcdef";
                HEX[rand::random::<usize>() % 16] as char
            })
            .collect();
        let (monitor_tx, _) = broadcast::channel(256);
        let config_map = Arc::new(RwLock::new(HashMap::from([
            ("port".into(), tcp_port.to_string()),
            ("dir".into(), ".".into()),
            ("dbfilename".into(), "dump.rdb".into()),
            // "save" — 仅消除 redis-benchmark CONFIG GET 探测警告, aikv 使用 BGSAVE/SAVE 命令
            ("appendonly".into(), "no".into()),
            ("save".into(), "".into()),
            ("maxmemory".into(), "0".into()),
            ("maxmemory-policy".into(), "noeviction".into()),
            (
                "timeout".into(),
                connection_config
                    .idle_timeout
                    .map(|d| d.as_secs().to_string())
                    .unwrap_or_else(|| "0".into()),
            ),
            (
                "slowlog-log-slower-than".into(),
                DEFAULT_SLOWLOG_THRESHOLD_US.to_string(),
            ),
            (
                "slowlog-max-len".into(),
                DEFAULT_SLOWLOG_MAX_LEN.to_string(),
            ),
            (
                "maxclients".into(),
                connection_config.max_clients.to_string(),
            ),
            ("loglevel".into(), "info".into()),
        ])));
        let runtime_config = Arc::new(RuntimeMutableConfig::from_connection_config(
            &connection_config,
        ));
        let metrics = {
            let m = ServerMetrics::default();
            let point_reads = Arc::new(crate::storage::observation::PointReadCounter::default());
            storage.set_point_read_counter(Arc::clone(&point_reads));
            m.attach_point_reads(point_reads);
            Arc::new(m)
        };
        Arc::new(Self {
            connection_config: Arc::new(connection_config),
            runtime_config,
            metrics,
            slow_query_log: Arc::new(SlowQueryLog::new()),
            latency_stats: Arc::new(LatencyStats::default()),
            storage,
            router: OnceLock::new(),
            config_map,
            clients: Arc::new(RwLock::new(HashMap::new())),
            monitor_tx,
            start_time_secs,
            run_id,
            tcp_port,
            shutdown: CancellationToken::new(),
            engine_kind,
            data_dir,
            backup_dir,
            last_save_time: AtomicU64::new(0),
            bgsave_in_progress: AtomicBool::new(false),
            last_bgsave_status: RwLock::new("ok".into()),
            metrics_port,
            metrics_addr,
            next_client_id: AtomicUsize::new(1),
            transaction_gate: Arc::new(tokio::sync::RwLock::new(())),
            storage_observation: storage_observation.unwrap_or_default(),
            log_reloader: RwLock::new(None),
            rename_table: RwLock::new(CommandRenameTable::default()),
        })
    }

    /// 挂载运行时 `EnvFilter` 热更新回调 (由 main 初始化 subscriber 后注册).
    pub fn set_log_reloader(&self, callback: LogReloadCallback) {
        *self.log_reloader.write() = Some(callback);
    }

    /// 校验并热更新运行时日志过滤级别.
    pub fn reload_log_level(&self, raw: &str) -> std::result::Result<(), String> {
        expand_log_filter(raw)?;
        if let Some(cb) = self.log_reloader.read().clone() {
            cb(raw)?;
        }
        Ok(())
    }

    pub fn record_save_success(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_save_time.store(now, Ordering::Relaxed);
        *self.last_bgsave_status.write() = "ok".into();
        self.metrics.on_bgsave_complete(true);
    }

    pub fn record_save_failure(&self, msg: impl Into<String>) {
        *self.last_bgsave_status.write() = msg.into();
        self.metrics.on_bgsave_complete(false);
    }

    pub fn last_save_time(&self) -> u64 {
        self.last_save_time.load(Ordering::Relaxed)
    }

    pub fn bgsave_in_progress(&self) -> bool {
        self.bgsave_in_progress.load(Ordering::Relaxed)
    }

    pub fn router(self: &Arc<Self>) -> Arc<CommandRouter> {
        self.router
            .get_or_init(|| {
                Arc::new(CommandRouter::new_with_shared(
                    Arc::clone(&self.storage),
                    Arc::clone(self),
                ))
            })
            .clone()
    }

    pub fn alloc_client_id(&self) -> usize {
        self.next_client_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn register_client(&self, id: usize, addr: SocketAddr) {
        self.clients.write().insert(
            id,
            ClientInfo {
                id,
                addr,
                name: None,
                db: 0,
            },
        );
    }

    pub fn unregister_client(&self, id: usize) {
        self.clients.write().remove(&id);
    }

    pub fn set_client_db(&self, id: usize, db: usize) {
        if let Some(info) = self.clients.write().get_mut(&id) {
            info.db = db;
        }
    }

    pub fn set_client_name(&self, id: usize, name: Option<String>) {
        if let Some(info) = self.clients.write().get_mut(&id) {
            info.name = name;
        }
    }

    pub fn metrics(&self) -> &ServerMetrics {
        &self.metrics
    }

    pub fn try_register_connection(self: &Arc<Self>) -> bool {
        let max = self.runtime_config.max_clients.load(Ordering::Relaxed);
        if max > 0 && self.metrics.connected_clients() >= max {
            self.metrics.on_rejected_connection();
            return false;
        }
        self.metrics.on_connect();
        true
    }

    /// 将解析后的 `ResolvedSettings` 同步到运行态原子句柄、慢日志和 `CONFIG GET` 配置树.
    pub fn apply_resolved_settings(&self, settings: &crate::config::ResolvedSettings) {
        self.runtime_config
            .max_clients
            .store(settings.max_clients, Ordering::Relaxed);
        self.runtime_config
            .idle_timeout_ms
            .store(settings.server_ext.idle_timeout_ms, Ordering::Relaxed);
        self.runtime_config
            .read_timeout_ms
            .store(settings.server_ext.read_timeout_ms, Ordering::Relaxed);
        *self.runtime_config.requirepass.write() = settings.server_ext.requirepass.clone();
        *self.rename_table.write() =
            CommandRenameTable::from_map(&settings.server_ext.rename_command);
        let ob = &settings.server_ext.client_output_buffer_limit.normal;
        self.set_normal_output_buffer_limit(
            ob.hard_limit_bytes,
            ob.soft_limit_bytes,
            ob.soft_seconds,
        );
        self.slow_query_log
            .set_threshold_us(settings.server_ext.slowlog.log_slower_than_us);
        self.slow_query_log
            .set_max_entries(settings.server_ext.slowlog.max_len);

        let mut map = self.config_map.write();
        for (k, v) in settings.to_flat_map() {
            map.insert(k, v);
        }
        map.insert("maxclients".into(), settings.max_clients.to_string());
        map.insert(
            "timeout".into(),
            (settings.server_ext.idle_timeout_ms / 1000).to_string(),
        );
        map.insert(
            "slowlog-log-slower-than".into(),
            settings.server_ext.slowlog.log_slower_than_us.to_string(),
        );
        map.insert(
            "slowlog-max-len".into(),
            settings.server_ext.slowlog.max_len.to_string(),
        );
        map.insert("loglevel".into(), settings.observability.log_level.clone());
        let pw = settings.server_ext.requirepass.clone().unwrap_or_default();
        map.insert("requirepass".into(), pw.clone());
        map.insert("server.requirepass".into(), pw);
        let ob = &settings.server_ext.client_output_buffer_limit.normal;
        map.insert(
            "client-output-buffer-limit".into(),
            format!(
                "normal {} {} {}",
                ob.hard_limit_bytes, ob.soft_limit_bytes, ob.soft_seconds
            ),
        );
    }

    pub fn requires_auth(&self) -> bool {
        self.runtime_config.requirepass.read().is_some()
    }

    pub fn requirepass_value(&self) -> String {
        self.runtime_config
            .requirepass
            .read()
            .clone()
            .unwrap_or_default()
    }

    pub fn set_requirepass(&self, raw: &str) {
        let next = (!raw.is_empty()).then(|| raw.to_string());
        *self.runtime_config.requirepass.write() = next;
    }

    pub fn verify_auth(&self, username: Option<&str>, password: &str) -> Result<(), &'static str> {
        let guard = self.runtime_config.requirepass.read();
        let Some(expected) = guard.as_deref() else {
            return Err("ERR Client sent AUTH, but no password is set");
        };
        if let Some(user) = username {
            if user != "default" {
                return Err("WRONGPASS invalid username-password pair or user is disabled.");
            }
        }
        if password == expected {
            Ok(())
        } else {
            Err("WRONGPASS invalid username-password pair or user is disabled.")
        }
    }

    pub fn resolve_command<'a>(&self, cmd_upper: &'a str) -> Option<std::borrow::Cow<'a, str>> {
        self.rename_table.read().resolve(cmd_upper)
    }

    pub fn display_command_name(&self, canonical_upper: &str) -> Option<String> {
        self.rename_table.read().display_name(canonical_upper)
    }

    pub fn disabled_command_count(&self) -> usize {
        self.rename_table.read().disabled_count()
    }

    pub fn normal_output_buffer_limit(&self) -> (u64, u64, u64) {
        (
            self.runtime_config
                .output_buffer_normal_hard_bytes
                .load(Ordering::Relaxed),
            self.runtime_config
                .output_buffer_normal_soft_bytes
                .load(Ordering::Relaxed),
            self.runtime_config
                .output_buffer_normal_soft_secs
                .load(Ordering::Relaxed),
        )
    }

    pub fn set_normal_output_buffer_limit(&self, hard: u64, soft: u64, secs: u64) {
        self.runtime_config
            .output_buffer_normal_hard_bytes
            .store(hard, Ordering::Relaxed);
        self.runtime_config
            .output_buffer_normal_soft_bytes
            .store(soft, Ordering::Relaxed);
        self.runtime_config
            .output_buffer_normal_soft_secs
            .store(secs, Ordering::Relaxed);
    }

    pub async fn refresh_runtime_metrics(self: &Arc<Self>) {
        self.metrics.set_uptime_secs(self.uptime_secs());
        self.metrics.sample_instantaneous_ops();
        self.metrics.refresh_cached_process_info();
        self.metrics.sync_redis_aligned_gauges();
        if let Ok(counts) = self.storage.db_key_counts().await {
            for (db, count) in counts.iter().enumerate() {
                self.metrics.set_db_key_count(db, *count as u64);
            }
        }
        if let Ok(bytes) = self.storage.memory_usage_bytes().await {
            self.metrics.set_memory_bytes(bytes);
        }
        let expired = self.storage_observation.drain_expired_keys();
        self.metrics.record_expired_keys(expired);
    }

    pub fn uptime_secs(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.start_time_secs)
    }
}
