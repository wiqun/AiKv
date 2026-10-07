//! 配置分层合并: TOML 文件、环境变量、CLI 四层优先级 (见 `docs/deployment.md`).

pub mod cli;
pub mod engine;
pub mod env;
pub mod file;
pub mod settings;

pub use cli::{Cli, CliOverrides};
pub use engine::EngineKind;
pub use file::{discover_config_path, discover_config_path_in, load_settings_from_file};
pub use settings::{
    print_resolved_config, should_export_traces, AnnounceMode, ConfigError, ConfigWarning,
    EngineSettings, LogLevel, ObservabilitySettings, ResolveResult, ResolvedObservability,
    ResolvedSettings, ServerSettings, Settings,
};

#[cfg(feature = "cluster")]
pub use settings::{ClusterSettings, ResolvedCluster};

use settings::{ConfigError as SettingsConfigError, Settings as InnerSettings};

/// 测试 seam: inline TOML 内容 (非文件路径).
pub fn resolve_from_parts(
    toml_inline: Option<&str>,
    env: impl IntoIterator<Item = (String, String)>,
    cli: &CliOverrides,
) -> ResolveResult {
    let mut settings = InnerSettings::default();

    if let Some(content) = toml_inline {
        let file_settings: InnerSettings =
            toml::from_str(content).map_err(|e| SettingsConfigError::Field {
                layer: "file",
                field: "toml",
                message: e.to_string(),
            })?;
        settings.merge_file(file_settings);
    }

    let mut warnings = settings.merge_env(env);
    settings.merge_cli(cli);

    let resolved = settings.into_resolved(&mut warnings)?;
    Ok((resolved, warnings))
}

/// 运行时: 文件发现 + env + cli.
pub fn resolve(cli: &Cli) -> ResolveResult {
    let config_path = discover_config_path(cli.config.as_deref())?;
    let mut settings = InnerSettings::default();

    if let Some(ref path) = config_path {
        let file_settings = load_settings_from_file(path)?;
        settings.merge_file(file_settings);
    }

    let mut warnings = settings.merge_env(std::env::vars());
    settings.merge_cli(&cli.overrides);

    let mut resolved = settings.into_resolved(&mut warnings)?;
    resolved.config_file_used = config_path;
    Ok((resolved, warnings))
}

#[cfg(test)]
mod resolve_tests {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::path::PathBuf;

    use super::*;
    use crate::config::engine::EngineKind;

    fn resolve_parts(
        toml: Option<&str>,
        env: HashMap<String, String>,
        cli: CliOverrides,
    ) -> ResolveResult {
        resolve_from_parts(toml, env, &cli)
    }

    #[test]
    fn defaults_match_legacy_clap_values() {
        let (resolved, warnings) =
            resolve_parts(None, HashMap::new(), CliOverrides::default()).unwrap();
        assert_eq!(warnings, vec![ConfigWarning::MemoryEngineProductionHint]);

        assert_eq!(
            resolved.bind,
            SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 6379))
        );
        assert_eq!(resolved.max_clients, 10000);
        assert_eq!(resolved.engine, EngineKind::Memory);
        assert!(resolved.data_dir.is_none());
        assert!(!resolved.sync_wal);
        assert_eq!(resolved.aidb_preset, "default");
        assert!(resolved.backup_dir.is_none());
        assert_eq!(resolved.observability.metrics_addr, "127.0.0.1");
        assert_eq!(resolved.observability.metrics_port, 9191);
        assert_eq!(resolved.observability.metrics_export_interval_secs, 15);
        assert!(resolved.observability.json_log);
        assert!(resolved.observability.otlp_endpoint.is_none());
        assert_eq!(resolved.observability.otel_service_name, "aikv");
        assert!((resolved.observability.otel_sample_ratio - 0.0).abs() < f64::EPSILON);
        assert_eq!(resolved.observability.log_level, LogLevel::Info);
        assert!(resolved.observability.host_label.is_none());
        assert!(resolved.observability.deployment_env.is_none());
        assert!(resolved.config_file_used.is_none());

        #[cfg(feature = "cluster")]
        {
            assert!(resolved.cluster.node_id.is_none());
            assert!(resolved.cluster.rpc_addr.is_none());
            assert!(resolved.cluster.peers.is_empty());
            assert_eq!(resolved.cluster.raft_election_timeout_min, 1000);
            assert_eq!(resolved.cluster.raft_election_timeout_max, 2000);
            assert_eq!(resolved.cluster.raft_rpc_timeout_ms, 500);
            assert_eq!(resolved.cluster.meta_rpc_timeout_ms, 100);
            assert_eq!(resolved.cluster.raft_heartbeat_interval, 300);
            assert_eq!(resolved.cluster.lifecycle_tick_ms, 1000);
            assert_eq!(resolved.cluster.gossip_interval, 1);
            assert_eq!(resolved.cluster.config_auto_save_ms, 2000);
            assert_eq!(resolved.cluster.cluster_data_port_offset, 10000);
            assert!(resolved.cluster.client_addr.is_none());
            assert_eq!(
                resolved.cluster.announce_mode,
                AnnounceMode::UnknownEndpoint
            );
            assert!(resolved.cluster.linearizable_read);
        }
    }

    #[test]
    fn metrics_export_interval_defaults_and_env() {
        let (resolved, _) = resolve_parts(None, HashMap::new(), CliOverrides::default()).unwrap();
        assert_eq!(resolved.observability.metrics_export_interval_secs, 15);
        let mut env = HashMap::new();
        env.insert(
            "AIKV_METRICS_EXPORT_INTERVAL_SECS".to_string(),
            "30".to_string(),
        );
        let (resolved, _) = resolve_parts(None, env, CliOverrides::default()).unwrap();
        assert_eq!(resolved.observability.metrics_export_interval_secs, 30);
    }

    #[test]
    fn log_level_and_sample_ratio_defaults() {
        let (resolved, _) = resolve_parts(None, HashMap::new(), CliOverrides::default()).unwrap();
        assert_eq!(resolved.observability.log_level, LogLevel::Info);
        assert!((resolved.observability.otel_sample_ratio - 0.0).abs() < f64::EPSILON);
        assert!(!should_export_traces(0.0, Some("http://127.0.0.1:4317")));
        assert!(!should_export_traces(0.1, None));
        assert!(!should_export_traces(0.1, Some("")));
        assert!(should_export_traces(0.1, Some("http://127.0.0.1:4317")));
    }

    #[test]
    fn log_level_env_then_cli() {
        let mut env = HashMap::new();
        env.insert("AIKV_LOG_LEVEL".to_string(), "debug".to_string());
        let (resolved, _) = resolve_parts(None, env, CliOverrides::default()).unwrap();
        assert_eq!(resolved.observability.log_level, LogLevel::Debug);

        let mut env = HashMap::new();
        env.insert("AIKV_LOG_LEVEL".to_string(), "debug".to_string());
        let cli = CliOverrides {
            log_level: Some("error".to_string()),
            ..CliOverrides::default()
        };
        let (resolved, _) = resolve_parts(None, env, cli).unwrap();
        assert_eq!(resolved.observability.log_level, LogLevel::Error);

        let mut env = HashMap::new();
        env.insert("AIKV_LOG_LEVEL".to_string(), "nope".to_string());
        let (resolved, warnings) = resolve_parts(None, env, CliOverrides::default()).unwrap();
        assert_eq!(resolved.observability.log_level, LogLevel::Info);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ConfigWarning::InvalidLogLevel { raw } if raw == "nope"
        )));

        let mut env = HashMap::new();
        env.insert("AIKV_OTEL_SAMPLE_RATIO".to_string(), "abc".to_string());
        let (resolved, warnings) = resolve_parts(None, env, CliOverrides::default()).unwrap();
        assert!((resolved.observability.otel_sample_ratio - 0.0).abs() < f64::EPSILON);
        assert!(warnings.contains(&ConfigWarning::InvalidOtelSampleRatio {
            raw: "abc".to_string()
        }));
    }

    #[test]
    fn four_layer_precedence_cli_wins() {
        let toml = r#"
[server]
bind = "10.0.0.1:6379"
"#;
        let env = HashMap::from([("AIKV_BIND".to_string(), "192.168.1.1:6380".to_string())]);
        let cli = CliOverrides {
            bind: Some("172.16.0.1:6381".parse().unwrap()),
            ..CliOverrides::default()
        };

        let (resolved, _) = resolve_parts(Some(toml), env, cli).unwrap();
        assert_eq!(
            resolved.bind,
            SocketAddr::from((Ipv4Addr::new(172, 16, 0, 1), 6381))
        );
    }

    #[test]
    fn env_beats_toml() {
        let toml = r#"
[server]
bind = "10.0.0.1:6379"
"#;
        let env = HashMap::from([("AIKV_BIND".to_string(), "192.168.1.1:6380".to_string())]);

        let (resolved, _) = resolve_parts(Some(toml), env, CliOverrides::default()).unwrap();
        assert_eq!(
            resolved.bind,
            SocketAddr::from((Ipv4Addr::new(192, 168, 1, 1), 6380))
        );
    }

    #[test]
    fn aidb_without_data_dir_fails() {
        let toml = r#"
[engine]
kind = "aidb"
"#;
        let err = resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Field {
                field: "engine.data_dir",
                ..
            }
        ));
    }

    /// 回归 #77: 集群启用且 `engine=memory` (无 data_dir) 必须在 resolve 阶段以 `engine.kind` 失败, 不得等到 init_cluster panic.
    #[cfg(feature = "cluster")]
    #[test]
    fn cluster_with_memory_engine_fails_validation() {
        let toml = r#"
[engine]
kind = "memory"

[cluster]
node_id = 1
rpc_addr = "127.0.0.1:16379"
"#;
        let err = resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Field {
                    field: "engine.kind",
                    ..
                }
            ),
            "expected engine.kind, got {err:?}"
        );
    }

    /// 回归 #77: 即使配了 data_dir, cluster + memory 仍必须报 `engine.kind`, 禁止 memory 绕过 Raft.
    #[cfg(feature = "cluster")]
    #[test]
    fn cluster_with_memory_engine_and_data_dir_fails_validation() {
        let toml = r#"
[engine]
kind = "memory"
data_dir = "/tmp/aikv-data"

[cluster]
node_id = 1
rpc_addr = "127.0.0.1:16379"
"#;
        let err = resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Field {
                    field: "engine.kind",
                    ..
                }
            ),
            "expected engine.kind, got {err:?}"
        );
    }

    /// 回归 #77: 只配 node_id (PartialClusterConfig) 时仍可用单机 memory, 不得当成集群启用.
    #[cfg(feature = "cluster")]
    #[test]
    fn partial_cluster_config_still_allows_memory_engine() {
        let toml = r#"
[engine]
kind = "memory"

[cluster]
node_id = 1
"#;
        let (resolved, warnings) =
            resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap();
        assert_eq!(resolved.engine, EngineKind::Memory);
        assert!(warnings.contains(&ConfigWarning::PartialClusterConfig));
    }

    /// 回归 #77: 集群 + aidb + data_dir 是合法配置, resolve 必须成功.
    #[cfg(feature = "cluster")]
    #[test]
    fn cluster_with_aidb_and_data_dir_succeeds() {
        let toml = r#"
[engine]
kind = "aidb"
data_dir = "/var/lib/aikv/data"

[cluster]
node_id = 1
rpc_addr = "127.0.0.1:16379"
"#;
        let (resolved, _) =
            resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap();
        assert_eq!(resolved.engine, EngineKind::AiDb);
        assert_eq!(
            resolved.data_dir.as_deref(),
            Some(std::path::Path::new("/var/lib/aikv/data"))
        );
        assert_eq!(resolved.cluster.node_id, Some(1));
        assert_eq!(
            resolved.cluster.rpc_addr.as_deref(),
            Some("127.0.0.1:16379")
        );
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn raft_rpc_timeout_ge_election_min_fails() {
        let toml = r#"
[cluster]
raft_election_timeout_min = 500
raft_rpc_timeout_ms = 500
"#;
        let err = resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Field {
                field: "cluster.raft_rpc_timeout_ms",
                ..
            }
        ));
    }

    #[cfg(not(feature = "cluster"))]
    #[test]
    fn warning_cluster_section_ignored() {
        let toml = r#"
[cluster]
node_id = 1
"#;
        let (_, warnings) =
            resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap();
        assert!(warnings.contains(&ConfigWarning::ClusterSectionIgnored));
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn warning_partial_cluster_config() {
        let toml = r#"
[cluster]
node_id = 1
"#;
        let (_, warnings) =
            resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap();
        assert!(warnings.contains(&ConfigWarning::PartialClusterConfig));
    }

    #[test]
    fn warning_unknown_aidb_preset() {
        let toml = r#"
[engine]
aidb_preset = "bogus"
"#;
        let (resolved, warnings) =
            resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap();
        assert!(warnings.iter().any(|w| matches!(
            w,
            ConfigWarning::UnknownAidbPreset { raw } if raw == "bogus"
        )));
        assert_eq!(resolved.aidb_preset, "default");
    }

    #[test]
    fn warning_otel_endpoint_precedence_in_resolve() {
        let env = HashMap::from([
            (
                "AIKV_OTLP_ENDPOINT".to_string(),
                "http://aikv:4317".to_string(),
            ),
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                "http://otel:4317".to_string(),
            ),
        ]);
        let (resolved, _) = resolve_parts(None, env, CliOverrides::default()).unwrap();
        assert_eq!(
            resolved.observability.otlp_endpoint.as_deref(),
            Some("http://otel:4317")
        );
    }

    /// 回归测试: 非法 OTel 采样率必须告警并回退到 `0.0`.
    #[test]
    fn warning_invalid_otel_sample_ratio_in_resolve() {
        for raw in ["abc", "1.5"] {
            let env = HashMap::from([("AIKV_OTEL_SAMPLE_RATIO".to_string(), raw.to_string())]);
            let (resolved, warnings) = resolve_parts(None, env, CliOverrides::default()).unwrap();

            assert_eq!(resolved.observability.otel_sample_ratio, 0.0, "raw={raw}");
            assert!(
                warnings.contains(&ConfigWarning::InvalidOtelSampleRatio {
                    raw: raw.to_string()
                }),
                "missing warning for raw={raw}: {warnings:?}"
            );
        }
    }

    /// 回归测试: `AIKV_LINEARIZABLE_READ` 的非法值 resolve 后也必须保持关闭.
    #[cfg(feature = "cluster")]
    #[test]
    fn linearizable_read_false_values_resolve_to_false() {
        for raw in ["0", "false", "maybe", ""] {
            let env = HashMap::from([("AIKV_LINEARIZABLE_READ".to_string(), raw.to_string())]);
            let (resolved, _) = resolve_parts(None, env, CliOverrides::default()).unwrap();
            assert!(!resolved.cluster.linearizable_read, "raw={raw}");
        }
    }

    #[test]
    fn print_resolved_config_emits_toml_sections() {
        let (resolved, _) = resolve_parts(None, HashMap::new(), CliOverrides::default()).unwrap();
        let output = print_resolved_config(&resolved).unwrap();
        assert!(output.contains("[server]"));
        assert!(output.contains("[engine]"));
        assert!(output.contains("[observability]"));
        assert!(output.contains("127.0.0.1:6379"));
        assert!(output.contains("kind = \"memory\""));
        #[cfg(feature = "cluster")]
        assert!(output.contains("[cluster]"));
    }

    #[test]
    fn resolve_from_parts_unknown_toml_key_fails() {
        let toml = "[server]\nunknown_key = 1\n";
        assert!(resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).is_err());
    }

    #[test]
    fn aidb_resolves_backup_dir_default() {
        let toml = r#"
[engine]
kind = "aidb"
data_dir = "/var/lib/aikv/data"
"#;
        let (resolved, _) =
            resolve_parts(Some(toml), HashMap::new(), CliOverrides::default()).unwrap();
        assert_eq!(
            resolved.backup_dir,
            Some(PathBuf::from("/var/lib/aikv/data/backup"))
        );
    }
}
