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

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub read_timeout: Option<std::time::Duration>,
    pub idle_timeout: Option<std::time::Duration>,
    /// 最大并发连接数; 0 表示不限制.
    pub max_clients: usize,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            read_timeout: Some(std::time::Duration::from_secs(60)),
            idle_timeout: Some(std::time::Duration::from_secs(300)),
            max_clients: 10_000,
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

pub struct ServerSharedState {
    pub connection_config: Arc<ConnectionConfig>,
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
            ("timeout".into(), "300".into()),
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
        ])));
        let metrics = {
            let m = ServerMetrics::default();
            #[cfg(feature = "monitoring")]
            let m = {
                let otel = super::otel_metrics::OtelMetrics::init_global(
                    opentelemetry::global::meter("aikv"),
                );
                m.with_otel(otel)
            };
            Arc::new(m)
        };
        Arc::new(Self {
            connection_config: Arc::new(connection_config),
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
        })
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
        let max = self.connection_config.max_clients;
        if max > 0 && self.metrics.connected_clients() >= max {
            self.metrics.on_rejected_connection();
            return false;
        }
        self.metrics.on_connect();
        true
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
        #[cfg(feature = "monitoring")]
        {
            if let Some(otel) = self.metrics.otel_handle() {
                otel.set_rebuild_counters_duration(self.storage.rebuild_counters_duration_us());
            }
            crate::server::info_catalog::sync_otel_from_server_metrics(&self.metrics);
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.start_time_secs)
    }
}
