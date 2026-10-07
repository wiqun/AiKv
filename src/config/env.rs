use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use super::engine::EngineKind;
use super::settings::{ConfigWarning, Settings};

/// 宽松布尔解析: 大小写不敏感, 无法识别时返回 `None` (继承下层).
pub fn parse_bool_lenient(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn nonempty<'a>(map: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    map.get(key).map(|s| s.as_str()).filter(|s| !s.is_empty())
}

fn parse_engine_kind(raw: &str) -> Option<EngineKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "memory" => Some(EngineKind::Memory),
        "aidb" => Some(EngineKind::AiDb),
        _ => None,
    }
}

#[cfg(feature = "cluster")]
fn parse_peers(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

impl Settings {
    /// 用环境变量覆盖已合并的下层字段; 一般空字符串视为未设置, 返回 env 层 warnings.
    pub fn merge_env(
        &mut self,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Vec<ConfigWarning> {
        let map: HashMap<String, String> = vars.into_iter().collect();
        let mut warnings = Vec::new();

        if let Some(v) = nonempty(&map, "AIKV_BIND") {
            if let Ok(addr) = v.parse::<SocketAddr>() {
                self.server.bind = Some(addr);
            }
        }
        if let Some(v) = nonempty(&map, "AIKV_MAX_CLIENTS") {
            if let Ok(n) = v.parse::<usize>() {
                self.server.max_clients = Some(n);
            }
        }

        if let Some(v) = nonempty(&map, "AIKV_ENGINE") {
            if let Some(kind) = parse_engine_kind(v) {
                self.engine.kind = Some(kind);
            }
        }
        if let Some(v) = nonempty(&map, "AIKV_DATA_DIR") {
            self.engine.data_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = nonempty(&map, "AIKV_SYNC_WAL") {
            if let Some(b) = parse_bool_lenient(v) {
                self.engine.sync_wal = Some(b);
            }
        }
        if let Some(v) = nonempty(&map, "AIKV_AIDB_PRESET") {
            self.engine.aidb_preset = Some(v.to_string());
        }
        if let Some(v) = nonempty(&map, "AIKV_BACKUP_DIR") {
            self.engine.backup_dir = Some(PathBuf::from(v));
        }

        if let Some(v) = nonempty(&map, "AIKV_METRICS_ADDR") {
            self.observability.metrics_addr = Some(v.to_string());
        }
        if let Some(v) = nonempty(&map, "AIKV_METRICS_PORT") {
            if let Ok(p) = v.parse::<u16>() {
                self.observability.metrics_port = Some(p);
            }
        }
        if let Some(v) = nonempty(&map, "AIKV_METRICS_EXPORT_INTERVAL_SECS") {
            if let Ok(secs) = v.parse::<u64>() {
                self.observability.metrics_export_interval_secs = Some(secs);
            }
        }
        if let Some(v) = nonempty(&map, "AIKV_JSON_LOG") {
            if let Some(b) = parse_bool_lenient(v) {
                self.observability.json_log = Some(b);
            }
        }
        if let Some(v) = nonempty(&map, "OTEL_EXPORTER_OTLP_ENDPOINT")
            .or_else(|| nonempty(&map, "AIKV_OTLP_ENDPOINT"))
        {
            self.observability.otlp_endpoint = Some(v.to_string());
        }
        if let Some(v) =
            nonempty(&map, "OTEL_SERVICE_NAME").or_else(|| nonempty(&map, "AIKV_OTEL_SERVICE_NAME"))
        {
            self.observability.otel_service_name = Some(v.to_string());
        }
        if let Some(v) = nonempty(&map, "AIKV_OTEL_SAMPLE_RATIO") {
            match v.trim().parse::<f64>() {
                Ok(r) => {
                    self.observability.otel_sample_ratio = Some(r);
                }
                Err(_) => {
                    self.observability.otel_sample_ratio = Some(0.0);
                    warnings.push(ConfigWarning::InvalidOtelSampleRatio { raw: v.to_string() });
                }
            }
        }
        if let Some(v) = nonempty(&map, "AIKV_LOG_LEVEL") {
            self.observability.log_level = Some(v.to_string());
        }
        if let Some(v) = nonempty(&map, "AIKV_HOST_LABEL") {
            self.observability.host_label = Some(v.to_string());
        }
        if let Some(v) = nonempty(&map, "OTEL_DEPLOYMENT_ENVIRONMENT")
            .or_else(|| nonempty(&map, "AIKV_DEPLOYMENT_ENV"))
        {
            self.observability.deployment_env = Some(v.to_string());
        }

        #[cfg(feature = "cluster")]
        self.merge_env_cluster(&map);

        warnings
    }

    #[cfg(feature = "cluster")]
    fn merge_env_cluster(&mut self, map: &HashMap<String, String>) {
        if let Some(v) = nonempty(map, "AIKV_CLUSTER_NODE_ID") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.node_id = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_CLUSTER_RPC_ADDR") {
            self.cluster.rpc_addr = Some(v.to_string());
        }
        if let Some(v) = nonempty(map, "AIKV_CLUSTER_PEERS") {
            self.cluster.peers = Some(parse_peers(v));
        }
        if let Some(v) = nonempty(map, "AIKV_RAFT_ELECTION_TIMEOUT_MIN") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.raft_election_timeout_min = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_RAFT_ELECTION_TIMEOUT_MAX") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.raft_election_timeout_max = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_RAFT_RPC_TIMEOUT_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.raft_rpc_timeout_ms = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_META_RPC_TIMEOUT_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.meta_rpc_timeout_ms = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_RAFT_HEARTBEAT_INTERVAL") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.raft_heartbeat_interval = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_LIFECYCLE_TICK_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.lifecycle_tick_ms = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_GOSSIP_INTERVAL") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.gossip_interval = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_CONFIG_AUTO_SAVE_MS") {
            if let Ok(n) = v.parse::<u64>() {
                self.cluster.config_auto_save_ms = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_CLUSTER_DATA_PORT_OFFSET") {
            if let Ok(n) = v.parse::<u16>() {
                self.cluster.cluster_data_port_offset = Some(n);
            }
        }
        if let Some(v) = nonempty(map, "AIKV_CLIENT_ADDR") {
            self.cluster.client_addr = Some(v.to_string());
        }
        if let Some(v) = nonempty(map, "AIKV_CLUSTER_ANNOUNCE_MODE") {
            self.cluster.announce_mode = Some(v.to_string());
        }
        if let Some(v) = map.get("AIKV_LINEARIZABLE_READ") {
            self.cluster.linearizable_read = Some(v == "1" || v.eq_ignore_ascii_case("true"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr};

    use super::*;

    #[test]
    fn parse_bool_lenient_truthy() {
        for raw in ["1", "true", "TRUE", "yes", "YES", "on", "ON"] {
            assert_eq!(parse_bool_lenient(raw), Some(true), "raw={raw}");
        }
    }

    #[test]
    fn parse_bool_lenient_falsy() {
        for raw in ["0", "false", "FALSE", "no", "NO", "off", "OFF"] {
            assert_eq!(parse_bool_lenient(raw), Some(false), "raw={raw}");
        }
    }

    #[test]
    fn parse_bool_lenient_invalid_returns_none() {
        assert_eq!(parse_bool_lenient("maybe"), None);
        assert_eq!(parse_bool_lenient(""), None);
    }

    #[test]
    fn merge_env_bind_overrides() {
        let mut settings = Settings::default();
        let vars = HashMap::from([("AIKV_BIND".to_string(), "192.168.1.1:6380".to_string())]);
        settings.merge_env(vars);
        assert_eq!(
            settings.server.bind,
            Some(SocketAddr::from((Ipv4Addr::new(192, 168, 1, 1), 6380)))
        );
    }

    #[test]
    fn merge_env_empty_string_skipped() {
        let mut settings = Settings::default();
        settings.server.bind = Some("127.0.0.1:6379".parse().unwrap());
        let vars = HashMap::from([("AIKV_BIND".to_string(), String::new())]);
        settings.merge_env(vars);
        assert_eq!(
            settings.server.bind,
            Some("127.0.0.1:6379".parse().unwrap())
        );
    }

    #[test]
    fn merge_env_otel_endpoint_precedence() {
        let mut settings = Settings::default();
        let vars = HashMap::from([
            (
                "AIKV_OTLP_ENDPOINT".to_string(),
                "http://aikv:4317".to_string(),
            ),
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                "http://otel:4317".to_string(),
            ),
        ]);
        settings.merge_env(vars);
        assert_eq!(
            settings.observability.otlp_endpoint.as_deref(),
            Some("http://otel:4317")
        );
    }

    #[test]
    fn merge_env_otel_endpoint_falls_back_to_aikv() {
        let mut settings = Settings::default();
        let vars = HashMap::from([(
            "AIKV_OTLP_ENDPOINT".to_string(),
            "http://aikv:4317".to_string(),
        )]);
        settings.merge_env(vars);
        assert_eq!(
            settings.observability.otlp_endpoint.as_deref(),
            Some("http://aikv:4317")
        );
    }

    #[test]
    fn merge_env_otel_service_name_precedence() {
        let mut settings = Settings::default();
        let vars = HashMap::from([
            ("AIKV_OTEL_SERVICE_NAME".to_string(), "aikv-a".to_string()),
            ("OTEL_SERVICE_NAME".to_string(), "aikv-b".to_string()),
        ]);
        settings.merge_env(vars);
        assert_eq!(
            settings.observability.otel_service_name.as_deref(),
            Some("aikv-b")
        );
    }

    #[test]
    fn merge_env_deployment_env_precedence() {
        let mut settings = Settings::default();
        let vars = HashMap::from([
            ("AIKV_DEPLOYMENT_ENV".to_string(), "staging".to_string()),
            (
                "OTEL_DEPLOYMENT_ENVIRONMENT".to_string(),
                "production".to_string(),
            ),
        ]);
        settings.merge_env(vars);
        assert_eq!(
            settings.observability.deployment_env.as_deref(),
            Some("production")
        );
    }

    #[test]
    fn merge_env_json_log_lenient() {
        let mut settings = Settings::default();
        let vars = HashMap::from([("AIKV_JSON_LOG".to_string(), "yes".to_string())]);
        settings.merge_env(vars);
        assert_eq!(settings.observability.json_log, Some(true));
    }

    #[test]
    fn merge_env_sync_wal_invalid_skipped() {
        let mut settings = Settings::default();
        settings.engine.sync_wal = Some(false);
        let vars = HashMap::from([("AIKV_SYNC_WAL".to_string(), "maybe".to_string())]);
        settings.merge_env(vars);
        assert_eq!(settings.engine.sync_wal, Some(false));
    }

    /// 回归测试: `AIKV_LINEARIZABLE_READ` 保留旧版严格布尔语义.
    #[cfg(feature = "cluster")]
    #[test]
    fn merge_env_linearizable_read_uses_legacy_strict_semantics() {
        for raw in ["0", "false", "maybe", ""] {
            let mut settings = Settings::default();
            let vars = HashMap::from([("AIKV_LINEARIZABLE_READ".to_string(), raw.to_string())]);
            settings.merge_env(vars);
            assert_eq!(settings.cluster.linearizable_read, Some(false), "raw={raw}");
        }

        for raw in ["1", "true", "TRUE"] {
            let mut settings = Settings::default();
            let vars = HashMap::from([("AIKV_LINEARIZABLE_READ".to_string(), raw.to_string())]);
            settings.merge_env(vars);
            assert_eq!(settings.cluster.linearizable_read, Some(true), "raw={raw}");
        }
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn merge_env_cluster_peers_comma_separated() {
        let mut settings = Settings::default();
        let vars = HashMap::from([(
            "AIKV_CLUSTER_PEERS".to_string(),
            "127.0.0.1:16380, 127.0.0.1:16381".to_string(),
        )]);
        settings.merge_env(vars);
        assert_eq!(
            settings.cluster.peers,
            Some(vec![
                "127.0.0.1:16380".to_string(),
                "127.0.0.1:16381".to_string(),
            ])
        );
    }
}
