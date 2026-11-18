use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

use super::engine::EngineKind;
use super::table::{deep_merge, flatten_table, normalize_byte_sizes};
use crate::storage::DbPreset;

/// 中间态配置: 各字段均为 `Option`, 合并各层后由 `into_resolved` 产出最终值.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub engine: EngineSettings,
    #[serde(default)]
    pub observability: ObservabilitySettings,
    #[cfg(feature = "cluster")]
    #[serde(default)]
    pub cluster: ClusterSettings,
    #[cfg(not(feature = "cluster"))]
    pub cluster: Option<toml::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlowlogSettings {
    pub log_slower_than_us: Option<u64>,
    pub max_len: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSettings {
    pub off_worker: Option<String>,
    pub pool_threshold_us: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub lock_timeout_ms: Option<u64>,
    pub memory_limit_bytes: Option<usize>,
    pub max_idle_vms: Option<usize>,
    pub cache_max_scripts: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    pub bind: Option<SocketAddr>,
    pub max_clients: Option<usize>,
    /// `EVAL` / `EVALSHA` 专用线程数. `None` 表示用默认夹取.
    pub script_worker_threads: Option<usize>,
    pub read_timeout_ms: Option<u64>,
    pub idle_timeout_ms: Option<u64>,
    pub databases: Option<usize>,
    pub proto_max_bulk_len: Option<usize>,
    pub proto_max_buffer_size: Option<usize>,
    pub proto_max_parse_depth: Option<usize>,
    pub pipeline_max_pending_writes: Option<usize>,
    pub key_lock_buckets: Option<usize>,
    pub tcp_nodelay: Option<bool>,
    pub tcp_keepalive_secs: Option<u64>,
    pub resp3_enabled: Option<bool>,
    pub slowlog: Option<SlowlogSettings>,
    pub script: Option<ScriptSettings>,
    pub max_blocking_threads: Option<usize>,
    pub requirepass: Option<String>,
    pub rename_command: Option<BTreeMap<String, String>>,
    pub client_output_buffer_limit: Option<ClientOutputBufferLimitsSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputBufferClassSettings {
    pub hard_limit_bytes: Option<u64>,
    pub soft_limit_bytes: Option<u64>,
    pub soft_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientOutputBufferLimitsSettings {
    pub normal: Option<OutputBufferClassSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSettings {
    pub kind: Option<EngineKind>,
    pub data_dir: Option<PathBuf>,
    pub sync_wal: Option<bool>,
    pub aidb_preset: Option<String>,
    pub backup_dir: Option<PathBuf>,
    pub hash_max_inline_fields: Option<usize>,
    pub set_max_inline_members: Option<usize>,
    pub zset_max_inline_members: Option<usize>,
    pub local_batch_max_ops: Option<usize>,
    /// 透传到 `aidb::config::Options` (以 `aidb_preset` 为默认值基底).
    pub aidb: Option<toml::Table>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilitySettings {
    pub metrics_addr: Option<String>,
    pub metrics_port: Option<u16>,
    pub metrics_export_interval_secs: Option<u64>,
    pub json_log: Option<bool>,
    pub otlp_endpoint: Option<String>,
    pub otel_service_name: Option<String>,
    pub otel_sample_ratio: Option<f64>,
    pub log_level: Option<String>,
    pub log_dir: Option<PathBuf>,
    pub log_max_files: Option<usize>,
    pub host_label: Option<String>,
    pub deployment_env: Option<String>,
    pub otlp_headers: Option<BTreeMap<String, String>>,
}

#[cfg(feature = "cluster")]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterSettings {
    pub node_id: Option<u64>,
    pub rpc_addr: Option<String>,
    pub peers: Option<Vec<String>>,
    pub raft_election_timeout_min: Option<u64>,
    pub raft_election_timeout_max: Option<u64>,
    pub raft_rpc_timeout_ms: Option<u64>,
    pub meta_rpc_timeout_ms: Option<u64>,
    pub raft_heartbeat_interval: Option<u64>,
    pub lifecycle_tick_ms: Option<u64>,
    pub gossip_interval: Option<u64>,
    pub config_auto_save_ms: Option<u64>,
    pub cluster_data_port_offset: Option<u16>,
    pub client_addr: Option<String>,
    pub announce_mode: Option<String>,
    pub linearizable_read: Option<bool>,
    pub grpc_max_message_size: Option<u64>,
    pub max_entry_size: Option<u64>,
    pub meta_max_payload_entries: Option<u64>,
    pub data_max_payload_entries: Option<u64>,
    pub meta_snapshot_logs_since_last: Option<u64>,
    pub data_snapshot_logs_since_last: Option<u64>,
    pub snapshot_size_threshold: Option<u64>,
    pub batcher_max_ops: Option<usize>,
    pub batcher_adaptive_target_ops: Option<usize>,
    /// 透传到 `aidb::cluster::log_committer::LogCommitterConfig`.
    pub log_committer: Option<toml::Table>,
    /// 透传到 `aidb::config::MigrationConfig`.
    pub migration: Option<toml::Table>,
}

/// 最终态慢日志配置.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedSlowlog {
    pub log_slower_than_us: u64,
    pub max_len: usize,
}

/// 最终态 Lua 脚本配置.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedScript {
    pub off_worker: String,
    pub pool_threshold_us: u64,
    pub timeout_ms: u64,
    pub lock_timeout_ms: u64,
    pub memory_limit_bytes: usize,
    pub max_idle_vms: usize,
    pub cache_max_scripts: usize,
}

/// 最终态服务端扩展配置.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedServerExt {
    pub read_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub databases: usize,
    pub proto_max_bulk_len: usize,
    pub proto_max_buffer_size: usize,
    pub proto_max_parse_depth: usize,
    pub pipeline_max_pending_writes: usize,
    pub key_lock_buckets: usize,
    pub tcp_nodelay: bool,
    pub tcp_keepalive_secs: u64,
    pub resp3_enabled: bool,
    pub slowlog: ResolvedSlowlog,
    pub script: ResolvedScript,
    pub max_blocking_threads: usize,
    pub requirepass: Option<String>,
    pub rename_command: BTreeMap<String, String>,
    pub client_output_buffer_limit: ResolvedClientOutputBufferLimits,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResolvedOutputBufferLimit {
    pub hard_limit_bytes: u64,
    pub soft_limit_bytes: u64,
    pub soft_seconds: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResolvedClientOutputBufferLimits {
    pub normal: ResolvedOutputBufferLimit,
}

/// 最终态引擎扩展配置 (包含完整的 `aidb::config::Options`).
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedEngineExt {
    pub hash_max_inline_fields: usize,
    pub set_max_inline_members: usize,
    pub zset_max_inline_members: usize,
    pub local_batch_max_ops: usize,
    pub aidb: aidb::config::Options,
}

impl PartialEq for ResolvedEngineExt {
    fn eq(&self, other: &Self) -> bool {
        self.hash_max_inline_fields == other.hash_max_inline_fields
            && self.set_max_inline_members == other.set_max_inline_members
            && self.zset_max_inline_members == other.zset_max_inline_members
            && self.local_batch_max_ops == other.local_batch_max_ops
            && self.aidb.memtable_size == other.aidb.memtable_size
            && self.aidb.sync_wal == other.aidb.sync_wal
            && self.aidb.compression == other.aidb.compression
    }
}

/// 最终态配置: 扁平与结构化并存, 供 main / otel / announce / CONFIG GET 消费.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSettings {
    pub bind: SocketAddr,
    pub max_clients: usize,
    pub script_worker_threads: usize,
    pub server_ext: ResolvedServerExt,
    pub engine: EngineKind,
    pub data_dir: Option<PathBuf>,
    pub sync_wal: bool,
    pub aidb_preset: String,
    pub backup_dir: Option<PathBuf>,
    pub engine_ext: ResolvedEngineExt,
    pub observability: ResolvedObservability,
    #[cfg(feature = "cluster")]
    pub cluster: ResolvedCluster,
    pub config_file_used: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedObservability {
    pub metrics_addr: String,
    pub metrics_port: u16,
    pub metrics_export_interval_secs: u64,
    pub json_log: bool,
    pub otlp_endpoint: Option<String>,
    pub otel_service_name: String,
    pub otel_sample_ratio: f64,
    pub log_level: String,
    pub log_dir: Option<PathBuf>,
    pub log_max_files: usize,
    pub host_label: Option<String>,
    pub deployment_env: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub otlp_headers: BTreeMap<String, String>,
}

#[cfg(feature = "cluster")]
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedCluster {
    pub node_id: Option<u64>,
    pub rpc_addr: Option<String>,
    pub peers: Vec<String>,
    pub raft_election_timeout_min: u64,
    pub raft_election_timeout_max: u64,
    pub raft_rpc_timeout_ms: u64,
    pub meta_rpc_timeout_ms: u64,
    pub raft_heartbeat_interval: u64,
    pub lifecycle_tick_ms: u64,
    pub gossip_interval: u64,
    pub config_auto_save_ms: u64,
    pub cluster_data_port_offset: u16,
    pub client_addr: Option<String>,
    pub announce_mode: AnnounceMode,
    pub linearizable_read: bool,
    pub grpc_max_message_size: u64,
    pub max_entry_size: u64,
    pub meta_max_payload_entries: u64,
    pub data_max_payload_entries: u64,
    pub meta_snapshot_logs_since_last: u64,
    pub data_snapshot_logs_since_last: u64,
    pub snapshot_size_threshold: Option<u64>,
    pub batcher_max_ops: usize,
    pub batcher_adaptive_target_ops: usize,
    pub log_committer: aidb::cluster::log_committer::LogCommitterConfig,
    pub migration: aidb::config::MigrationConfig,
}

#[cfg(feature = "cluster")]
impl PartialEq for ResolvedCluster {
    fn eq(&self, other: &Self) -> bool {
        self.node_id == other.node_id
            && self.rpc_addr == other.rpc_addr
            && self.peers == other.peers
            && self.raft_election_timeout_min == other.raft_election_timeout_min
            && self.raft_election_timeout_max == other.raft_election_timeout_max
            && self.raft_rpc_timeout_ms == other.raft_rpc_timeout_ms
            && self.meta_rpc_timeout_ms == other.meta_rpc_timeout_ms
            && self.raft_heartbeat_interval == other.raft_heartbeat_interval
            && self.lifecycle_tick_ms == other.lifecycle_tick_ms
            && self.gossip_interval == other.gossip_interval
            && self.config_auto_save_ms == other.config_auto_save_ms
            && self.cluster_data_port_offset == other.cluster_data_port_offset
            && self.client_addr == other.client_addr
            && self.announce_mode == other.announce_mode
            && self.linearizable_read == other.linearizable_read
            && self.grpc_max_message_size == other.grpc_max_message_size
            && self.max_entry_size == other.max_entry_size
            && self.meta_max_payload_entries == other.meta_max_payload_entries
            && self.data_max_payload_entries == other.data_max_payload_entries
            && self.meta_snapshot_logs_since_last == other.meta_snapshot_logs_since_last
            && self.data_snapshot_logs_since_last == other.data_snapshot_logs_since_last
            && self.snapshot_size_threshold == other.snapshot_size_threshold
            && self.batcher_max_ops == other.batcher_max_ops
            && self.batcher_adaptive_target_ops == other.batcher_adaptive_target_ops
            && self.log_committer.max_entries == other.log_committer.max_entries
            && self.migration.max_batch_size == other.migration.max_batch_size
    }
}

/// 非 fatal 配置问题, 合并完成后由 main 在 init_logging 之后输出 warn 日志.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    UnknownAidbPreset { raw: String },
    UnknownAnnounceMode { raw: String },
    InvalidOtelSampleRatio { raw: String },
    ClusterSectionIgnored,
    PartialClusterConfig,
    MemoryEngineProductionHint,
    InvalidLogLevel { raw: String },
}

/// 采样率大于 0 且端点非空时才导出 trace.
pub fn should_export_traces(sample_ratio: f64, endpoint: Option<&str>) -> bool {
    sample_ratio > 0.0 && endpoint.is_some_and(|value| !value.is_empty())
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config error: {layer} {field}: {message}")]
    Field {
        layer: &'static str,
        field: &'static str,
        message: String,
    },
    #[error("config error: file {path}: {message}")]
    File { path: PathBuf, message: String },
}

pub type ResolveResult = Result<(ResolvedSettings, Vec<ConfigWarning>), ConfigError>;

/// 集群通告模式.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnounceMode {
    Fixed,
    UnknownEndpoint,
}

impl Serialize for AnnounceMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = match self {
            AnnounceMode::Fixed => "fixed",
            AnnounceMode::UnknownEndpoint => "unknown",
        };
        serializer.serialize_str(s)
    }
}

const DEFAULT_BIND: &str = "127.0.0.1:6379";
const DEFAULT_MAX_CLIENTS: usize = 10000;
const DEFAULT_MAX_BLOCKING_THREADS: usize = 512;
const SCRIPT_WORKER_THREADS_FALLBACK: usize = 8;

/// 未配置 `script_worker_threads` 时的线程数: 兼顾 I/O 等待, 机器并行度*4 夹到 8..=32, 探测失败为 8.
pub fn default_script_worker_threads() -> usize {
    match std::thread::available_parallelism() {
        Ok(n) => (n.get() * 4).clamp(8, 32),
        Err(_) => SCRIPT_WORKER_THREADS_FALLBACK,
    }
}

const DEFAULT_METRICS_ADDR: &str = "127.0.0.1";
const DEFAULT_METRICS_PORT: u16 = 9191;
const DEFAULT_METRICS_EXPORT_INTERVAL_SECS: u64 = 15;
const DEFAULT_OTEL_SERVICE_NAME: &str = "aikv";
const DEFAULT_OTEL_SAMPLE_RATIO: f64 = 0.0;
const DEFAULT_AIDB_PRESET: &str = "default";
const DEFAULT_LOG_LEVEL: &str = "info";

#[cfg(feature = "cluster")]
const DEFAULT_RAFT_ELECTION_TIMEOUT_MIN: u64 = 1000;
#[cfg(feature = "cluster")]
const DEFAULT_RAFT_ELECTION_TIMEOUT_MAX: u64 = 2000;
#[cfg(feature = "cluster")]
const DEFAULT_RAFT_RPC_TIMEOUT_MS: u64 = 500;
#[cfg(feature = "cluster")]
const DEFAULT_META_RPC_TIMEOUT_MS: u64 = 100;
#[cfg(feature = "cluster")]
const DEFAULT_RAFT_HEARTBEAT_INTERVAL: u64 = 300;
#[cfg(feature = "cluster")]
const DEFAULT_LIFECYCLE_TICK_MS: u64 = 1000;
#[cfg(feature = "cluster")]
const DEFAULT_GOSSIP_INTERVAL: u64 = 1;
#[cfg(feature = "cluster")]
const DEFAULT_CONFIG_AUTO_SAVE_MS: u64 = 2000;
#[cfg(feature = "cluster")]
const DEFAULT_CLUSTER_DATA_PORT_OFFSET: u16 = 10000;

impl Settings {
    /// 通过 `toml::Table` 深度合并另一层 `Settings` (标量/数组覆盖, 子表递归合并).
    pub fn merge_settings(&mut self, other: Settings) -> Result<(), ConfigError> {
        let mut base = toml::Table::try_from(&*self).map_err(|e| ConfigError::Field {
            layer: "merge",
            field: "base",
            message: e.to_string(),
        })?;
        let overlay = toml::Table::try_from(other).map_err(|e| ConfigError::Field {
            layer: "merge",
            field: "overlay",
            message: e.to_string(),
        })?;
        deep_merge(&mut base, overlay);
        *self = toml::Value::Table(base)
            .try_into::<Settings>()
            .map_err(|e| ConfigError::Field {
                layer: "merge",
                field: "toml",
                message: e.to_string(),
            })?;
        Ok(())
    }

    /// 用 TOML 文件层覆盖已合并的下层字段; 仅 `Some` 生效.
    pub fn merge_file(&mut self, file: Settings) {
        if let Err(e) = self.merge_settings(file) {
            tracing::warn!(error = %e, "merge_file failed unexpectedly");
        }
    }

    /// 合并完成后一次性校验, 应用默认值, 产出 `ResolvedSettings`.
    pub fn into_resolved(
        &self,
        warnings: &mut Vec<ConfigWarning>,
    ) -> Result<ResolvedSettings, ConfigError> {
        #[cfg(not(feature = "cluster"))]
        if self.cluster.is_some() {
            warnings.push(ConfigWarning::ClusterSectionIgnored);
        }

        let bind = self
            .server
            .bind
            .unwrap_or_else(|| DEFAULT_BIND.parse().expect("valid default bind"));
        let max_clients = self.server.max_clients.unwrap_or(DEFAULT_MAX_CLIENTS);
        let script_worker_threads = match self.server.script_worker_threads {
            None => default_script_worker_threads(),
            Some(0) => {
                return Err(ConfigError::Field {
                    layer: "server",
                    field: "script_worker_threads",
                    message: "must be >= 1".to_string(),
                });
            }
            Some(n) => n,
        };

        if matches!(self.server.max_blocking_threads, Some(0)) {
            return Err(ConfigError::Field {
                layer: "server",
                field: "max_blocking_threads",
                message: "must be >= 1".to_string(),
            });
        }

        let rename_command = resolve_rename_commands(self.server.rename_command.as_ref())?;
        let server_ext = resolve_server_ext(&self.server, rename_command);

        let engine = self.engine.kind.unwrap_or(EngineKind::Memory);
        if engine == EngineKind::Memory {
            warnings.push(ConfigWarning::MemoryEngineProductionHint);
        }
        let sync_wal = self.engine.sync_wal.unwrap_or(false);

        let aidb_preset_raw = self
            .engine
            .aidb_preset
            .clone()
            .unwrap_or_else(|| DEFAULT_AIDB_PRESET.to_string());
        let aidb_preset = if DbPreset::parse(&aidb_preset_raw).is_some() {
            aidb_preset_raw
        } else {
            warnings.push(ConfigWarning::UnknownAidbPreset {
                raw: aidb_preset_raw,
            });
            DEFAULT_AIDB_PRESET.to_string()
        };

        let data_dir = self.engine.data_dir.clone();
        if engine == EngineKind::AiDb && data_dir.is_none() {
            return Err(ConfigError::Field {
                layer: "validation",
                field: "engine.data_dir",
                message: "required when engine is aidb".to_string(),
            });
        }

        let backup_dir = match (&self.engine.backup_dir, &data_dir) {
            (Some(dir), _) => Some(dir.clone()),
            (None, Some(dd)) if engine == EngineKind::AiDb => Some(dd.join("backup")),
            _ => None,
        };

        let engine_ext = resolve_engine_ext(&self.engine, &aidb_preset, sync_wal)?;
        let observability = resolve_observability(&self.observability, warnings)?;

        #[cfg(feature = "cluster")]
        let cluster = resolve_cluster(&self.cluster, warnings)?;

        #[cfg(feature = "cluster")]
        {
            let cluster_enabled = cluster.node_id.is_some() && cluster.rpc_addr.is_some();
            if cluster_enabled && engine != EngineKind::AiDb {
                return Err(ConfigError::Field {
                    layer: "validation",
                    field: "engine.kind",
                    message: "cluster mode requires engine aidb, got memory".to_string(),
                });
            }
        }

        Ok(ResolvedSettings {
            bind,
            max_clients,
            script_worker_threads,
            server_ext,
            engine,
            data_dir,
            sync_wal,
            aidb_preset,
            backup_dir,
            engine_ext,
            observability,
            #[cfg(feature = "cluster")]
            cluster,
            config_file_used: None,
        })
    }
}

fn is_builtin_command(upper: &str) -> bool {
    crate::command::lookup(upper).is_some()
        || matches!(
            upper,
            "AUTH"
                | "PING"
                | "ECHO"
                | "HELLO"
                | "QUIT"
                | "MONITOR"
                | "MULTI"
                | "EXEC"
                | "DISCARD"
                | "WATCH"
                | "UNWATCH"
                | "ATOM.MULTI"
                | "ATOM.EXEC"
                | "ATOM.DISCARD"
                | "ATOM.WATCH"
                | "ATOM.UNWATCH"
                | "ASKING"
                | "READONLY"
                | "READWRITE"
        )
}

fn resolve_rename_commands(
    raw: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let Some(map) = raw else {
        return Ok(BTreeMap::new());
    };
    let mut normalized = BTreeMap::new();
    for (orig, target) in map {
        let orig_up = orig.trim().to_ascii_uppercase();
        if orig_up.is_empty() {
            return Err(ConfigError::Field {
                layer: "server",
                field: "rename_command",
                message: "original command name cannot be empty".to_string(),
            });
        }
        let target_up = target.trim().to_ascii_uppercase();
        if let Some(prev) = normalized.insert(orig_up.clone(), target_up) {
            let _ = prev;
        }
    }
    let mut seen_targets = std::collections::HashSet::new();
    for (orig, target) in &normalized {
        if target.is_empty() {
            continue;
        }
        if !seen_targets.insert(target.clone()) {
            return Err(ConfigError::Field {
                layer: "server",
                field: "rename_command",
                message: format!("duplicate target command name '{target}'"),
            });
        }
        if is_builtin_command(target) && target != orig && !normalized.contains_key(target) {
            return Err(ConfigError::Field {
                layer: "server",
                field: "rename_command",
                message: format!("target '{target}' conflicts with existing built-in command"),
            });
        }
    }
    Ok(normalized)
}

fn resolve_server_ext(
    server: &ServerSettings,
    rename_command: BTreeMap<String, String>,
) -> ResolvedServerExt {
    let slowlog = server.slowlog.as_ref();
    let script = server.script.as_ref();
    ResolvedServerExt {
        read_timeout_ms: server.read_timeout_ms.unwrap_or(60_000),
        idle_timeout_ms: server.idle_timeout_ms.unwrap_or(300_000),
        databases: server.databases.unwrap_or(16).max(1),
        proto_max_bulk_len: server.proto_max_bulk_len.unwrap_or(512 * 1024 * 1024),
        proto_max_buffer_size: server.proto_max_buffer_size.unwrap_or(64 * 1024 * 1024),
        proto_max_parse_depth: server.proto_max_parse_depth.unwrap_or(128),
        pipeline_max_pending_writes: server.pipeline_max_pending_writes.unwrap_or(256).max(1),
        key_lock_buckets: server.key_lock_buckets.unwrap_or(4096).max(1),
        tcp_nodelay: server.tcp_nodelay.unwrap_or(true),
        tcp_keepalive_secs: server.tcp_keepalive_secs.unwrap_or(0),
        resp3_enabled: server.resp3_enabled.unwrap_or(true),
        slowlog: ResolvedSlowlog {
            log_slower_than_us: slowlog
                .and_then(|s| s.log_slower_than_us)
                .unwrap_or(100_000),
            max_len: slowlog.and_then(|s| s.max_len).unwrap_or(128),
        },
        script: ResolvedScript {
            off_worker: script
                .and_then(|s| s.off_worker.clone())
                .unwrap_or_else(|| "auto".to_string()),
            pool_threshold_us: script.and_then(|s| s.pool_threshold_us).unwrap_or(1000),
            timeout_ms: script.and_then(|s| s.timeout_ms).unwrap_or(5000),
            lock_timeout_ms: script.and_then(|s| s.lock_timeout_ms).unwrap_or(30_000),
            memory_limit_bytes: script
                .and_then(|s| s.memory_limit_bytes)
                .unwrap_or(128 * 1024 * 1024),
            max_idle_vms: script.and_then(|s| s.max_idle_vms).unwrap_or(64),
            cache_max_scripts: script.and_then(|s| s.cache_max_scripts).unwrap_or(256),
        },
        max_blocking_threads: server
            .max_blocking_threads
            .unwrap_or(DEFAULT_MAX_BLOCKING_THREADS),
        requirepass: server
            .requirepass
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned),
        rename_command,
        client_output_buffer_limit: ResolvedClientOutputBufferLimits {
            normal: {
                let n = server
                    .client_output_buffer_limit
                    .as_ref()
                    .and_then(|c| c.normal.as_ref());
                ResolvedOutputBufferLimit {
                    hard_limit_bytes: n.and_then(|v| v.hard_limit_bytes).unwrap_or(0),
                    soft_limit_bytes: n.and_then(|v| v.soft_limit_bytes).unwrap_or(0),
                    soft_seconds: n.and_then(|v| v.soft_seconds).unwrap_or(0),
                }
            },
        },
    }
}

fn resolve_engine_ext(
    engine: &EngineSettings,
    aidb_preset: &str,
    sync_wal: bool,
) -> Result<ResolvedEngineExt, ConfigError> {
    let mut aidb_opts = aidb::config::Options::from_preset(aidb_preset).unwrap_or_default();
    aidb_opts.create_if_missing = true;
    aidb_opts.sync_wal = sync_wal;
    #[cfg(not(feature = "compression"))]
    {
        aidb_opts.compression = aidb::config::CompressionType::None;
    }

    if let Some(overlay) = engine.aidb.clone() {
        let mut base = toml::Table::try_from(&aidb_opts).map_err(|e| ConfigError::Field {
            layer: "engine",
            field: "aidb",
            message: e.to_string(),
        })?;
        let mut ov = overlay;
        normalize_byte_sizes(&mut ov)?;
        deep_merge(&mut base, ov);
        aidb_opts = toml::Value::Table(base)
            .try_into::<aidb::config::Options>()
            .map_err(|e| ConfigError::Field {
                layer: "engine",
                field: "aidb",
                message: e.to_string(),
            })?;
        // 顶部 engine.sync_wal 若显式设置 (或由 preset 基底带入) 保持一致:
        // 若用户在 [engine.aidb] 里单独指定了 sync_wal, try_into 已覆盖之.
        if engine.sync_wal.is_some() {
            aidb_opts.sync_wal = sync_wal;
        }
    }

    aidb_opts.validate().map_err(|e| ConfigError::Field {
        layer: "validation",
        field: "engine.aidb",
        message: e.to_string(),
    })?;

    Ok(ResolvedEngineExt {
        hash_max_inline_fields: engine.hash_max_inline_fields.unwrap_or(64).max(1),
        set_max_inline_members: engine.set_max_inline_members.unwrap_or(64).max(1),
        zset_max_inline_members: engine.zset_max_inline_members.unwrap_or(64).max(1),
        local_batch_max_ops: engine.local_batch_max_ops.unwrap_or(512).max(1),
        aidb: aidb_opts,
    })
}

fn resolve_observability(
    obs: &ObservabilitySettings,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<ResolvedObservability, ConfigError> {
    let metrics_addr = obs
        .metrics_addr
        .clone()
        .unwrap_or_else(|| DEFAULT_METRICS_ADDR.to_string());
    let metrics_port = obs.metrics_port.unwrap_or(DEFAULT_METRICS_PORT);
    let metrics_export_interval_secs = obs
        .metrics_export_interval_secs
        .unwrap_or(DEFAULT_METRICS_EXPORT_INTERVAL_SECS);
    let json_log = obs.json_log.unwrap_or(true);
    let otlp_endpoint = obs.otlp_endpoint.clone();
    let otel_service_name = obs
        .otel_service_name
        .clone()
        .unwrap_or_else(|| DEFAULT_OTEL_SERVICE_NAME.to_string());

    let log_level = match obs.log_level.as_deref().map(str::trim) {
        None => DEFAULT_LOG_LEVEL.to_string(),
        Some("") => {
            warnings.push(ConfigWarning::InvalidLogLevel { raw: String::new() });
            DEFAULT_LOG_LEVEL.to_string()
        }
        Some(raw) => raw.to_string(),
    };

    let otel_sample_ratio = match obs.otel_sample_ratio {
        Some(r) if r.is_finite() && (0.0..=1.0).contains(&r) => r,
        Some(r) => {
            warnings.push(ConfigWarning::InvalidOtelSampleRatio { raw: r.to_string() });
            DEFAULT_OTEL_SAMPLE_RATIO
        }
        None => DEFAULT_OTEL_SAMPLE_RATIO,
    };

    Ok(ResolvedObservability {
        metrics_addr,
        metrics_port,
        metrics_export_interval_secs,
        json_log,
        otlp_endpoint,
        otel_service_name,
        otel_sample_ratio,
        log_level,
        log_dir: obs.log_dir.clone(),
        log_max_files: obs.log_max_files.unwrap_or(7).max(1),
        host_label: obs.host_label.clone(),
        deployment_env: obs.deployment_env.clone(),
        otlp_headers: obs.otlp_headers.clone().unwrap_or_default(),
    })
}

#[cfg(feature = "cluster")]
fn resolve_cluster(
    cluster: &ClusterSettings,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<ResolvedCluster, ConfigError> {
    let node_id = cluster.node_id;
    let rpc_addr = cluster.rpc_addr.clone();
    if node_id.is_some() ^ rpc_addr.is_some() {
        warnings.push(ConfigWarning::PartialClusterConfig);
    }

    let raft_election_timeout_min = cluster
        .raft_election_timeout_min
        .unwrap_or(DEFAULT_RAFT_ELECTION_TIMEOUT_MIN);
    let raft_election_timeout_max = cluster
        .raft_election_timeout_max
        .unwrap_or(DEFAULT_RAFT_ELECTION_TIMEOUT_MAX);
    let raft_rpc_timeout_ms = cluster
        .raft_rpc_timeout_ms
        .unwrap_or(DEFAULT_RAFT_RPC_TIMEOUT_MS);
    let raft_heartbeat_interval = cluster
        .raft_heartbeat_interval
        .unwrap_or(DEFAULT_RAFT_HEARTBEAT_INTERVAL);

    if raft_rpc_timeout_ms >= raft_election_timeout_min {
        return Err(ConfigError::Field {
            layer: "validation",
            field: "cluster.raft_rpc_timeout_ms",
            message: format!(
                "must be less than raft_election_timeout_min ({raft_election_timeout_min})"
            ),
        });
    }
    if raft_heartbeat_interval >= raft_election_timeout_min {
        return Err(ConfigError::Field {
            layer: "validation",
            field: "cluster.raft_heartbeat_interval",
            message: format!(
                "must be less than raft_election_timeout_min ({raft_election_timeout_min})"
            ),
        });
    }

    let announce_mode = resolve_announce_mode(cluster.announce_mode.as_deref(), warnings);

    let log_committer = merge_sub_struct(
        aidb::cluster::log_committer::LogCommitterConfig::default(),
        cluster.log_committer.clone(),
        "log_committer",
    )?;
    let migration = merge_sub_struct(
        aidb::config::MigrationConfig::default(),
        cluster.migration.clone(),
        "migration",
    )?;

    Ok(ResolvedCluster {
        node_id,
        rpc_addr,
        peers: cluster.peers.clone().unwrap_or_default(),
        raft_election_timeout_min,
        raft_election_timeout_max,
        raft_rpc_timeout_ms,
        meta_rpc_timeout_ms: cluster
            .meta_rpc_timeout_ms
            .unwrap_or(DEFAULT_META_RPC_TIMEOUT_MS),
        raft_heartbeat_interval,
        lifecycle_tick_ms: cluster
            .lifecycle_tick_ms
            .unwrap_or(DEFAULT_LIFECYCLE_TICK_MS),
        gossip_interval: cluster.gossip_interval.unwrap_or(DEFAULT_GOSSIP_INTERVAL),
        config_auto_save_ms: cluster
            .config_auto_save_ms
            .unwrap_or(DEFAULT_CONFIG_AUTO_SAVE_MS),
        cluster_data_port_offset: cluster
            .cluster_data_port_offset
            .unwrap_or(DEFAULT_CLUSTER_DATA_PORT_OFFSET),
        client_addr: cluster.client_addr.clone(),
        announce_mode,
        linearizable_read: cluster.linearizable_read.unwrap_or(true),
        grpc_max_message_size: cluster.grpc_max_message_size.unwrap_or(64 * 1024 * 1024),
        max_entry_size: cluster.max_entry_size.unwrap_or(8 * 1024 * 1024),
        meta_max_payload_entries: cluster.meta_max_payload_entries.unwrap_or(100),
        data_max_payload_entries: cluster.data_max_payload_entries.unwrap_or(1000),
        meta_snapshot_logs_since_last: cluster.meta_snapshot_logs_since_last.unwrap_or(256),
        data_snapshot_logs_since_last: cluster.data_snapshot_logs_since_last.unwrap_or(100_000),
        snapshot_size_threshold: cluster.snapshot_size_threshold.filter(|&v| v > 0),
        batcher_max_ops: cluster.batcher_max_ops.unwrap_or(512).max(1),
        batcher_adaptive_target_ops: cluster.batcher_adaptive_target_ops.unwrap_or(32).max(1),
        log_committer,
        migration,
    })
}

#[cfg(feature = "cluster")]
fn merge_sub_struct<T>(
    default: T,
    overlay: Option<toml::Table>,
    field: &'static str,
) -> Result<T, ConfigError>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let Some(mut ov) = overlay else {
        return Ok(default);
    };
    let mut base = toml::Table::try_from(default).map_err(|e| ConfigError::Field {
        layer: "cluster",
        field,
        message: e.to_string(),
    })?;
    normalize_byte_sizes(&mut ov)?;
    deep_merge(&mut base, ov);
    toml::Value::Table(base)
        .try_into::<T>()
        .map_err(|e| ConfigError::Field {
            layer: "cluster",
            field,
            message: e.to_string(),
        })
}

#[cfg(feature = "cluster")]
fn resolve_announce_mode(raw: Option<&str>, warnings: &mut Vec<ConfigWarning>) -> AnnounceMode {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => AnnounceMode::UnknownEndpoint,
        Some(s) if s.eq_ignore_ascii_case("fixed") => AnnounceMode::Fixed,
        Some(s) if s.eq_ignore_ascii_case("unknown") => AnnounceMode::UnknownEndpoint,
        Some(other) => {
            warnings.push(ConfigWarning::UnknownAnnounceMode {
                raw: other.to_string(),
            });
            AnnounceMode::UnknownEndpoint
        }
    }
}

/// TOML stdout / 扁平配置树输出结构 (不含 `config_file_used` 元数据).
#[derive(Serialize)]
struct ResolvedSettingsToml<'a> {
    server: ResolvedServerToml<'a>,
    engine: ResolvedEngineToml<'a>,
    observability: &'a ResolvedObservability,
    #[cfg(feature = "cluster")]
    cluster: &'a ResolvedCluster,
}

#[derive(Serialize)]
struct ResolvedServerToml<'a> {
    bind: &'a SocketAddr,
    max_clients: usize,
    script_worker_threads: usize,
    #[serde(flatten)]
    ext: &'a ResolvedServerExt,
}

#[derive(Serialize)]
struct ResolvedEngineToml<'a> {
    kind: EngineKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_dir: Option<&'a PathBuf>,
    sync_wal: bool,
    aidb_preset: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_dir: Option<&'a PathBuf>,
    #[serde(flatten)]
    ext: &'a ResolvedEngineExt,
}

impl ResolvedSettings {
    /// 将全部生效配置扁平化为点分路径键值表 (供 `CONFIG GET` 使用).
    pub fn to_flat_map(&self) -> BTreeMap<String, String> {
        let view = ResolvedSettingsToml {
            server: ResolvedServerToml {
                bind: &self.bind,
                max_clients: self.max_clients,
                script_worker_threads: self.script_worker_threads,
                ext: &self.server_ext,
            },
            engine: ResolvedEngineToml {
                kind: self.engine,
                data_dir: self.data_dir.as_ref(),
                sync_wal: self.sync_wal,
                aidb_preset: &self.aidb_preset,
                backup_dir: self.backup_dir.as_ref(),
                ext: &self.engine_ext,
            },
            observability: &self.observability,
            #[cfg(feature = "cluster")]
            cluster: &self.cluster,
        };
        let mut out = BTreeMap::new();
        if let Ok(table) = toml::Table::try_from(view) {
            flatten_table("", &table, &mut out);
        }
        out
    }
}

/// 将有效配置序列化为 TOML 字符串 (stdout 内容).
pub fn print_resolved_config(settings: &ResolvedSettings) -> Result<String, toml::ser::Error> {
    let output = ResolvedSettingsToml {
        server: ResolvedServerToml {
            bind: &settings.bind,
            max_clients: settings.max_clients,
            script_worker_threads: settings.script_worker_threads,
            ext: &settings.server_ext,
        },
        engine: ResolvedEngineToml {
            kind: settings.engine,
            data_dir: settings.data_dir.as_ref(),
            sync_wal: settings.sync_wal,
            aidb_preset: &settings.aidb_preset,
            backup_dir: settings.backup_dir.as_ref(),
            ext: &settings.engine_ext,
        },
        observability: &settings.observability,
        #[cfg(feature = "cluster")]
        cluster: &settings.cluster,
    };
    toml::to_string_pretty(&output)
}
