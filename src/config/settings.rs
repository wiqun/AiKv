use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

use super::engine::EngineKind;
use crate::storage::DbPreset;

/// 中间态配置: 各字段均为 `Option`, 合并各层后由 `into_resolved` 产出最终值 (Task 5).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    pub bind: Option<SocketAddr>,
    pub max_clients: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSettings {
    pub kind: Option<EngineKind>,
    pub data_dir: Option<PathBuf>,
    pub sync_wal: Option<bool>,
    pub aidb_preset: Option<String>,
    pub backup_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
    pub host_label: Option<String>,
    pub deployment_env: Option<String>,
}

#[cfg(feature = "cluster")]
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
}

/// 最终态配置: 扁平结构, 供 main / otel / announce 消费.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSettings {
    pub bind: SocketAddr,
    pub max_clients: usize,
    pub engine: EngineKind,
    pub data_dir: Option<PathBuf>,
    pub sync_wal: bool,
    pub aidb_preset: String,
    pub backup_dir: Option<PathBuf>,
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
    pub log_level: LogLevel,
    pub host_label: Option<String>,
    pub deployment_env: Option<String>,
}

#[cfg(feature = "cluster")]
#[derive(Debug, Clone, PartialEq, Serialize)]
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

/// 进程日志级别. 只接受这五个名字, 大小写不敏感.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "error" => Some(LogLevel::Error),
            "warn" => Some(LogLevel::Warn),
            "info" => Some(LogLevel::Info),
            "debug" => Some(LogLevel::Debug),
            "trace" => Some(LogLevel::Trace),
            _ => None,
        }
    }
}

impl Serialize for LogLevel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
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

/// 集群通告模式 (定义 in config, cluster/announce 复用或 thin wrapper).
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
const DEFAULT_METRICS_ADDR: &str = "127.0.0.1";
const DEFAULT_METRICS_PORT: u16 = 9191;
const DEFAULT_METRICS_EXPORT_INTERVAL_SECS: u64 = 15;
const DEFAULT_OTEL_SERVICE_NAME: &str = "aikv";
const DEFAULT_OTEL_SAMPLE_RATIO: f64 = 0.0;
const DEFAULT_AIDB_PRESET: &str = "default";

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
    /// 用 TOML 文件层覆盖已合并的下层字段; 仅 `Some` 生效.
    pub fn merge_file(&mut self, file: Settings) {
        self.merge_server(&file.server);
        self.merge_engine(&file.engine);
        self.merge_observability(&file.observability);
        #[cfg(feature = "cluster")]
        self.merge_cluster(&file.cluster);
        #[cfg(not(feature = "cluster"))]
        {
            if file.cluster.is_some() {
                self.cluster = file.cluster;
            }
        }
    }

    fn merge_server(&mut self, other: &ServerSettings) {
        if let Some(v) = other.bind {
            self.server.bind = Some(v);
        }
        if let Some(v) = other.max_clients {
            self.server.max_clients = Some(v);
        }
    }

    fn merge_engine(&mut self, other: &EngineSettings) {
        if let Some(v) = other.kind {
            self.engine.kind = Some(v);
        }
        if let Some(ref v) = other.data_dir {
            self.engine.data_dir = Some(v.clone());
        }
        if let Some(v) = other.sync_wal {
            self.engine.sync_wal = Some(v);
        }
        if let Some(ref v) = other.aidb_preset {
            self.engine.aidb_preset = Some(v.clone());
        }
        if let Some(ref v) = other.backup_dir {
            self.engine.backup_dir = Some(v.clone());
        }
    }

    fn merge_observability(&mut self, other: &ObservabilitySettings) {
        if let Some(ref v) = other.metrics_addr {
            self.observability.metrics_addr = Some(v.clone());
        }
        if let Some(v) = other.metrics_port {
            self.observability.metrics_port = Some(v);
        }
        if let Some(v) = other.metrics_export_interval_secs {
            self.observability.metrics_export_interval_secs = Some(v);
        }
        if let Some(v) = other.json_log {
            self.observability.json_log = Some(v);
        }
        if let Some(ref v) = other.otlp_endpoint {
            self.observability.otlp_endpoint = Some(v.clone());
        }
        if let Some(ref v) = other.otel_service_name {
            self.observability.otel_service_name = Some(v.clone());
        }
        if let Some(v) = other.otel_sample_ratio {
            self.observability.otel_sample_ratio = Some(v);
        }
        if let Some(ref v) = other.log_level {
            self.observability.log_level = Some(v.clone());
        }
        if let Some(ref v) = other.host_label {
            self.observability.host_label = Some(v.clone());
        }
        if let Some(ref v) = other.deployment_env {
            self.observability.deployment_env = Some(v.clone());
        }
    }

    #[cfg(feature = "cluster")]
    fn merge_cluster(&mut self, other: &ClusterSettings) {
        if let Some(v) = other.node_id {
            self.cluster.node_id = Some(v);
        }
        if let Some(ref v) = other.rpc_addr {
            self.cluster.rpc_addr = Some(v.clone());
        }
        if let Some(ref v) = other.peers {
            self.cluster.peers = Some(v.clone());
        }
        if let Some(v) = other.raft_election_timeout_min {
            self.cluster.raft_election_timeout_min = Some(v);
        }
        if let Some(v) = other.raft_election_timeout_max {
            self.cluster.raft_election_timeout_max = Some(v);
        }
        if let Some(v) = other.raft_rpc_timeout_ms {
            self.cluster.raft_rpc_timeout_ms = Some(v);
        }
        if let Some(v) = other.meta_rpc_timeout_ms {
            self.cluster.meta_rpc_timeout_ms = Some(v);
        }
        if let Some(v) = other.raft_heartbeat_interval {
            self.cluster.raft_heartbeat_interval = Some(v);
        }
        if let Some(v) = other.lifecycle_tick_ms {
            self.cluster.lifecycle_tick_ms = Some(v);
        }
        if let Some(v) = other.gossip_interval {
            self.cluster.gossip_interval = Some(v);
        }
        if let Some(v) = other.config_auto_save_ms {
            self.cluster.config_auto_save_ms = Some(v);
        }
        if let Some(v) = other.cluster_data_port_offset {
            self.cluster.cluster_data_port_offset = Some(v);
        }
        if let Some(ref v) = other.client_addr {
            self.cluster.client_addr = Some(v.clone());
        }
        if let Some(ref v) = other.announce_mode {
            self.cluster.announce_mode = Some(v.clone());
        }
        if let Some(v) = other.linearizable_read {
            self.cluster.linearizable_read = Some(v);
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
            engine,
            data_dir,
            sync_wal,
            aidb_preset,
            backup_dir,
            observability,
            #[cfg(feature = "cluster")]
            cluster,
            config_file_used: None,
        })
    }
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

    let log_level = match obs.log_level.as_deref() {
        None => LogLevel::Info,
        Some(raw) => match LogLevel::parse(raw) {
            Some(level) => level,
            None => {
                warnings.push(ConfigWarning::InvalidLogLevel {
                    raw: raw.to_string(),
                });
                LogLevel::Info
            }
        },
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
        host_label: obs.host_label.clone(),
        deployment_env: obs.deployment_env.clone(),
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

/// TOML stdout 输出结构 (不含 `config_file_used` 元数据).
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
}

/// 将有效配置序列化为 TOML 字符串 (stdout 内容).
pub fn print_resolved_config(settings: &ResolvedSettings) -> Result<String, toml::ser::Error> {
    let output = ResolvedSettingsToml {
        server: ResolvedServerToml {
            bind: &settings.bind,
            max_clients: settings.max_clients,
        },
        engine: ResolvedEngineToml {
            kind: settings.engine,
            data_dir: settings.data_dir.as_ref(),
            sync_wal: settings.sync_wal,
            aidb_preset: &settings.aidb_preset,
            backup_dir: settings.backup_dir.as_ref(),
        },
        observability: &settings.observability,
        #[cfg(feature = "cluster")]
        cluster: &settings.cluster,
    };
    toml::to_string_pretty(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_all_unset() {
        let s = Settings::default();
        assert!(s.server.bind.is_none());
        assert!(s.server.max_clients.is_none());
        assert!(s.engine.kind.is_none());
        assert!(s.engine.data_dir.is_none());
        assert!(s.engine.sync_wal.is_none());
        assert!(s.engine.aidb_preset.is_none());
        assert!(s.engine.backup_dir.is_none());
        assert!(s.observability.metrics_addr.is_none());
        assert!(s.observability.metrics_port.is_none());
        assert!(s.observability.json_log.is_none());
        assert!(s.observability.otlp_endpoint.is_none());
        assert!(s.observability.otel_service_name.is_none());
        assert!(s.observability.otel_sample_ratio.is_none());
        assert!(s.observability.host_label.is_none());
        assert!(s.observability.deployment_env.is_none());
        #[cfg(feature = "cluster")]
        {
            assert!(s.cluster.node_id.is_none());
            assert!(s.cluster.rpc_addr.is_none());
            assert!(s.cluster.peers.is_none());
            assert!(s.cluster.announce_mode.is_none());
            assert!(s.cluster.linearizable_read.is_none());
        }
        #[cfg(not(feature = "cluster"))]
        {
            assert!(s.cluster.is_none());
        }
    }
}
