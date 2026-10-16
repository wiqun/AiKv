use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

use super::engine::EngineKind;
use super::settings::Settings;

/// CLI 入口: 唯一 parse 点 (Task 6 替换 main 内旧 `Args`).
#[derive(Parser, Debug)]
#[command(name = "aikv", about = "Redis RESP compatible KV server")]
pub struct Cli {
    #[arg(long, help = "TOML 配置文件路径")]
    pub config: Option<PathBuf>,
    #[arg(long, help = "合并后配置打印到 stdout (TOML), 然后继续启动")]
    pub print_config: bool,
    #[command(flatten)]
    pub overrides: CliOverrides,
}

/// CLI 覆盖项: 全部为 `Option`, 不设 `default_value`, 未传参不覆盖下层.
#[derive(Parser, Debug, Default)]
pub struct CliOverrides {
    #[arg(long, help = "监听地址 (default: 127.0.0.1:6379)")]
    pub bind: Option<SocketAddr>,
    #[arg(long, value_enum, help = "存储引擎 (default: memory)")]
    pub engine: Option<EngineKind>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        help = "每条写后 fsync WAL (default: false)"
    )]
    pub sync_wal: Option<bool>,
    #[arg(long, help = "AiDb LSM preset (default: default)")]
    pub aidb_preset: Option<String>,
    #[arg(long, help = "BGSAVE 目录")]
    pub backup_dir: Option<PathBuf>,
    #[arg(long, help = "Metrics HTTP 端口 (default: 9191)")]
    pub metrics_port: Option<u16>,
    #[arg(long, help = "Metrics HTTP 地址 (default: 127.0.0.1)")]
    pub metrics_addr: Option<String>,
    #[arg(long, help = "指标导出间隔秒数 (default: 15)")]
    pub metrics_export_interval_secs: Option<u64>,
    #[arg(long, help = "日志级别 (default: info)")]
    pub log_level: Option<String>,
    #[arg(long, help = "最大并发连接 (default: 10000, 0=不限)")]
    pub max_clients: Option<usize>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub cluster_node_id: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub cluster_rpc_addr: Option<String>,
    #[cfg(feature = "cluster")]
    #[arg(long, value_delimiter = ',')]
    pub cluster_peers: Option<Vec<String>>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub raft_election_timeout_min: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub raft_election_timeout_max: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub raft_rpc_timeout_ms: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub meta_rpc_timeout_ms: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub raft_heartbeat_interval: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub lifecycle_tick_ms: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub gossip_interval: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub config_auto_save_ms: Option<u64>,
    #[cfg(feature = "cluster")]
    #[arg(long)]
    pub cluster_data_port_offset: Option<u16>,
}

impl Settings {
    /// 用 CLI 覆盖已合并的下层字段; 仅 `Some` 生效, `cluster_peers` 整表替换.
    pub fn merge_cli(&mut self, cli: &CliOverrides) {
        if let Some(v) = cli.bind {
            self.server.bind = Some(v);
        }
        if let Some(v) = cli.max_clients {
            self.server.max_clients = Some(v);
        }

        if let Some(v) = cli.engine {
            self.engine.kind = Some(v);
        }
        if let Some(ref v) = cli.data_dir {
            self.engine.data_dir = Some(v.clone());
        }
        if let Some(v) = cli.sync_wal {
            self.engine.sync_wal = Some(v);
        }
        if let Some(ref v) = cli.aidb_preset {
            self.engine.aidb_preset = Some(v.clone());
        }
        if let Some(ref v) = cli.backup_dir {
            self.engine.backup_dir = Some(v.clone());
        }

        if let Some(ref v) = cli.metrics_addr {
            self.observability.metrics_addr = Some(v.clone());
        }
        if let Some(v) = cli.metrics_port {
            self.observability.metrics_port = Some(v);
        }
        if let Some(v) = cli.metrics_export_interval_secs {
            self.observability.metrics_export_interval_secs = Some(v);
        }
        if let Some(ref v) = cli.log_level {
            self.observability.log_level = Some(v.clone());
        }

        #[cfg(feature = "cluster")]
        self.merge_cli_cluster(cli);
    }

    #[cfg(feature = "cluster")]
    fn merge_cli_cluster(&mut self, cli: &CliOverrides) {
        if let Some(v) = cli.cluster_node_id {
            self.cluster.node_id = Some(v);
        }
        if let Some(ref v) = cli.cluster_rpc_addr {
            self.cluster.rpc_addr = Some(v.clone());
        }
        if let Some(ref v) = cli.cluster_peers {
            self.cluster.peers = Some(v.clone());
        }
        if let Some(v) = cli.raft_election_timeout_min {
            self.cluster.raft_election_timeout_min = Some(v);
        }
        if let Some(v) = cli.raft_election_timeout_max {
            self.cluster.raft_election_timeout_max = Some(v);
        }
        if let Some(v) = cli.raft_rpc_timeout_ms {
            self.cluster.raft_rpc_timeout_ms = Some(v);
        }
        if let Some(v) = cli.meta_rpc_timeout_ms {
            self.cluster.meta_rpc_timeout_ms = Some(v);
        }
        if let Some(v) = cli.raft_heartbeat_interval {
            self.cluster.raft_heartbeat_interval = Some(v);
        }
        if let Some(v) = cli.lifecycle_tick_ms {
            self.cluster.lifecycle_tick_ms = Some(v);
        }
        if let Some(v) = cli.gossip_interval {
            self.cluster.gossip_interval = Some(v);
        }
        if let Some(v) = cli.config_auto_save_ms {
            self.cluster.config_auto_save_ms = Some(v);
        }
        if let Some(v) = cli.cluster_data_port_offset {
            self.cluster.cluster_data_port_offset = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr};

    use super::*;

    #[test]
    fn merge_cli_skips_unset_bind() {
        let original = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 1), 6379));
        let mut settings = Settings::default();
        settings.server.bind = Some(original);

        settings.merge_cli(&CliOverrides::default());

        assert_eq!(settings.server.bind, Some(original));
    }

    #[test]
    fn merge_cli_bind_overrides_when_set() {
        let mut settings = Settings::default();
        settings.server.bind = Some("127.0.0.1:6379".parse().unwrap());

        let cli = CliOverrides {
            bind: Some("192.168.1.1:6380".parse().unwrap()),
            ..CliOverrides::default()
        };
        settings.merge_cli(&cli);

        assert_eq!(
            settings.server.bind,
            Some(SocketAddr::from((Ipv4Addr::new(192, 168, 1, 1), 6380)))
        );
    }

    /// 回归测试: 未传 `--sync-wal` 必须保留 `None`, 以便继承下层配置.
    #[test]
    fn parse_sync_wal_is_unset_when_flag_is_absent() {
        let cli = Cli::try_parse_from(["aikv"]).unwrap();

        assert_eq!(cli.overrides.sync_wal, None);
    }

    /// 回归测试: 裸 `--sync-wal` 必须解析为 `Some(true)`.
    #[test]
    fn parse_sync_wal_bare_flag_enables_sync() {
        let cli = Cli::try_parse_from(["aikv", "--sync-wal"]).unwrap();

        assert_eq!(cli.overrides.sync_wal, Some(true));
    }

    /// 回归测试: CLI 裸 flag 必须覆盖 TOML/env 的 `false`.
    #[test]
    fn parse_sync_wal_bare_flag_overrides_toml_and_env_false() {
        let cli = Cli::try_parse_from(["aikv", "--sync-wal"]).unwrap();
        let env = HashMap::from([("AIKV_SYNC_WAL".to_string(), "false".to_string())]);
        let (resolved, _) = crate::config::resolve_from_parts(
            Some("[engine]\nsync_wal = false\n"),
            env,
            &cli.overrides,
        )
        .unwrap();

        assert!(resolved.sync_wal);
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn merge_cli_cluster_peers_replace() {
        let mut settings = Settings::default();
        settings.cluster.peers = Some(vec![
            "127.0.0.1:16380".to_string(),
            "127.0.0.1:16381".to_string(),
        ]);

        let cli = CliOverrides {
            cluster_peers: Some(vec!["10.0.0.2:16380".to_string()]),
            ..CliOverrides::default()
        };
        settings.merge_cli(&cli);

        assert_eq!(
            settings.cluster.peers,
            Some(vec!["10.0.0.2:16380".to_string()])
        );
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn merge_cli_cluster_peers_unset_preserves_existing() {
        let existing = vec!["127.0.0.1:16380".to_string()];
        let mut settings = Settings::default();
        settings.cluster.peers = Some(existing.clone());

        settings.merge_cli(&CliOverrides::default());

        assert_eq!(settings.cluster.peers, Some(existing));
    }
}
