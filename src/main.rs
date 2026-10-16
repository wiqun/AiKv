//! aikv 入口: 加载配置, 初始化 tracing, 启动 TCP 服务.
#![recursion_limit = "256"]

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "cluster")]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(feature = "cluster")]
#[allow(unused_imports)]
use std::path::Path;

use aikv::command::blocking;
use aikv::config::{
    print_resolved_config, resolve, should_export_traces, Cli, EngineKind, ResolvedSettings,
};
use aikv::server::{ConnectionConfig, Server, ServerSharedState};
use aikv::storage::ttl_filter::TtlExpireFilter;
use aikv::storage::{
    server_db_options_with_preset, AiDbEngine, DbPreset, KvStorage, KvStorageAdapter, MemoryEngine,
    StorageEngineKind, StorageObservation,
};
use clap::Parser;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

/// Storage 构建结果: (storage, engine_kind, data_dir, 可选的 DB 引用)
type StorageBuildResult = Result<
    (
        Arc<dyn KvStorage>,
        StorageEngineKind,
        Option<PathBuf>,
        Option<Arc<aidb::DB>>,
    ),
    String,
>;

async fn build_storage(
    settings: &ResolvedSettings,
    observation: Arc<StorageObservation>,
) -> StorageBuildResult {
    let preset = DbPreset::parse(&settings.aidb_preset).unwrap_or(DbPreset::Default);

    match settings.engine {
        EngineKind::Memory => Ok((
            MemoryEngine::with_observation(16, Some(observation)),
            StorageEngineKind::Memory,
            None,
            None,
        )),
        EngineKind::AiDb => {
            let path = settings
                .data_dir
                .as_ref()
                .expect("aidb engine requires data_dir after resolve");
            let engine = AiDbEngine::open_with_options(
                path,
                server_db_options_with_preset(settings.sync_wal, preset),
            )
            .map_err(|e| e.to_string())?;
            engine
                .db
                .set_compaction_filter(Some(std::sync::Arc::new(TtlExpireFilter)));
            let db = engine.db.clone();
            let adapter: Arc<dyn aikv::storage::StorageAdapter> = engine;
            #[cfg(feature = "cluster")]
            let adapter: Arc<dyn aikv::storage::StorageAdapter> =
                aikv::storage::cluster_adapter::ClusterDataAdapter::new(
                    adapter,
                    aikv::storage::cluster_adapter::ClusterDataAdapter::DEFAULT_EAGER_FLUSH,
                );
            Ok((
                KvStorageAdapter::open(adapter, Some(observation))
                    .await
                    .map_err(|e| e.to_string())?,
                StorageEngineKind::AiDb,
                Some(path.clone()),
                Some(db),
            ))
        }
    }
}

#[cfg(feature = "cluster")]
fn spawn_client_addr_sync(
    meta_raft: std::sync::Arc<aidb::cluster::MetaRaftNode>,
    node_id: u64,
    external_client_addr: String,
) {
    use aidb::cluster::meta_types::MetaRequest;

    tokio::spawn(async move {
        for attempt in 1u32..=20 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let meta = meta_raft.get_cluster_meta();
            match meta.nodes.get(&node_id) {
                Some(node) if node.client_addr.as_deref() != Some(&external_client_addr) => {
                    match meta_raft
                        .propose(MetaRequest::UpdateNodeClientAddr {
                            node_id,
                            client_addr: Some(external_client_addr.clone()),
                        })
                        .await
                    {
                        Ok(_) => {
                            tracing::info!(
                              client_addr = %external_client_addr,
                              attempt,
                              "client_addr updated in MetaRaft"
                            );
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                              attempt,
                              error = %e,
                              "failed to update client_addr, will retry"
                            );
                        }
                    }
                }
                Some(_) => {
                    tracing::info!("client_addr already correct, skipping update");
                    break;
                }
                None => {
                    // bootstrap 或尚未 MEET 注册, 继续等待.
                }
            }
        }
    });
}

/// 等到 Meta 中本节点应承载的 data group 已在 MultiRaft 打开, 再扫计数.
/// `expected` 为空 (尚未 ADDSLOTS) 时立即返回; 超时则视为启动失败.
#[cfg(feature = "cluster")]
async fn wait_local_data_groups(timeout: std::time::Duration) -> Result<(), String> {
    let Some(mgr) = aikv::cluster::state::CLUSTER_STATE_MGR.get() else {
        return Err("CLUSTER_STATE_MGR not set".into());
    };
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let meta = mgr.meta_raft.get_cluster_meta();
        let expected: std::collections::HashSet<u64> = meta
            .groups
            .iter()
            .filter(|(_, g)| g.replicas.iter().any(|r| r.node_id == mgr.node_id))
            .map(|(id, _)| *id)
            .collect();
        let current: std::collections::HashSet<u64> =
            mgr.multi_raft.local_group_ids().into_iter().collect();
        if expected.is_subset(&current) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "local data groups not ready: expected {expected:?} have {current:?}"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(feature = "cluster")]
async fn init_cluster(
    settings: &ResolvedSettings,
    cluster_db: Option<Arc<aidb::DB>>,
    metrics: Arc<aikv::server::metrics::ServerMetrics>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::net::SocketAddr as NetAddr;

    use aidb::cluster::{
        membership_coordinator::MembershipCoordinator,
        meta_types::{default_slot_table, ClusterMeta, SlotMigrationState, SlotTable},
        slot_migration::SlotMigrationManager,
        LeaderChangeWatcher, LifecycleManager, MetaRaftNode, MultiRaftNode,
        RaftNetworkClientFactory, RaftServiceDispatcher, Router,
    };
    use aidb::config::MigrationConfig;

    let cluster = &settings.cluster;
    let node_id = cluster.node_id.expect("cluster node_id required");
    let rpc_addr = cluster
        .rpc_addr
        .as_deref()
        .expect("cluster rpc_addr required");
    let Some(data_dir) = settings.data_dir.as_deref() else {
        return Err("data_dir required for cluster mode".into());
    };
    let bind_addr = settings.bind;
    let aidb_preset = DbPreset::parse(&settings.aidb_preset).unwrap_or(DbPreset::Default);
    let shared_opts = server_db_options_with_preset(settings.sync_wal, aidb_preset);
    let shared_stats = match &cluster_db {
        Some(db) => db.statistics(),
        None => Arc::new(aidb::Statistics::new(shared_opts.max_levels)),
    };

    // 1. 打开/重用 DB 引擎 (WAL 必须启用)
    let db = match cluster_db {
        Some(db) => db,
        None => {
            let mut opts = shared_opts.clone();
            opts.statistics = Some(shared_stats.clone());
            aidb::DB::open(data_dir, opts)?
        }
    };

    // 2. 创建 RaftNetworkClientFactory (MetaRaft 专用, 独立快超时, 注入 shared_stats)
    let net_factory = RaftNetworkClientFactory::new_with_stats(
        node_id,
        0,
        cluster.meta_rpc_timeout_ms,
        64 * 1024 * 1024,
        Some(shared_stats.clone()),
    );
    let net_factory = Arc::new(parking_lot::RwLock::new(net_factory));

    let linearizable_read = cluster.linearizable_read;

    // 3. 创建 MetaRaftNode (控制平面, group_id=0)
    let raft_config = aidb::cluster::RaftNodeConfig {
        node_id,
        group_id: 0,
        election_timeout_min: cluster.raft_election_timeout_min,
        election_timeout_max: cluster.raft_election_timeout_max,
        heartbeat_interval: cluster.raft_heartbeat_interval,
        max_payload_entries: 100,
        snapshot_logs_since_last: 256,
        max_entry_size: 8 * 1024 * 1024,
        rpc_timeout_ms: cluster.raft_rpc_timeout_ms,
        grpc_max_message_size: 64 * 1024 * 1024,
        snapshot_size_threshold: None,
        linearizable_read,
        log_committer_config: None, // MetaRaft 使用同步路径
    };
    let factory = net_factory.read().clone(); // drop read lock before .await
    let meta_raft = Arc::new(MetaRaftNode::new(raft_config.clone(), db.clone(), factory).await?);

    // 4. 注册 peer 地址到 net_factory
    for peer_addr in &cluster.peers {
        if peer_addr == rpc_addr {
            continue;
        }
        net_factory.write().add_node(0, peer_addr.clone());
    }

    // 5. 确定本节点的 client_addr (外部可达地址).
    let external_client_addr = cluster.client_addr.clone().unwrap_or_else(|| {
        let rpc_host = rpc_addr
            .rsplit_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_default();
        format!("{rpc_host}:{}", bind_addr.port())
    });

    // 6. 首节点 bootstrap / 从节点自动发现
    if cluster.peers.is_empty() {
        meta_raft
            .initialize_with_client(vec![(
                node_id,
                rpc_addr.to_string(),
                Some(external_client_addr.clone()),
            )])
            .await?;
        tracing::info!("meta raft initialized as bootstrap node");
    } else {
        tracing::info!("meta raft joining via peers (gRPC auto-discovery)");
    }

    // 6. 创建共享 RaftServiceDispatcher (MetaRaft + MultiRaft 共用, 注入 shared_stats).
    let dispatcher: Arc<RaftServiceDispatcher> = Arc::new(RaftServiceDispatcher::new_with_stats(
        Some(shared_stats.clone()),
    ));

    // 7. 启动 MetaRaft gRPC (独立端口, 使用共享 dispatcher).
    let rpc_socket: NetAddr = tokio::net::lookup_host(rpc_addr)
        .await?
        .next()
        .ok_or_else(|| format!("cannot resolve RPC address: {rpc_addr}"))?;
    let meta_raft_clone = Arc::clone(&meta_raft);
    let dispatcher_for_meta = Arc::clone(&dispatcher);
    tokio::spawn(async move {
        if let Err(e) = meta_raft_clone
            .start_server_with_dispatcher(rpc_socket, 64 * 1024 * 1024, dispatcher_for_meta)
            .await
        {
            tracing::error!(error = %e, "meta raft gRPC server failed");
        }
    });
    tracing::info!(rpc_addr = %rpc_addr, "meta raft gRPC server started");

    // 6a. 所有节点后台同步 client_addr → MetaRaft (含 bootstrap).
    spawn_client_addr_sync(
        Arc::clone(&meta_raft),
        node_id,
        external_client_addr.clone(),
    );

    // 7. 推算 MultiRaft 总线端口 (rpc_port + cluster_data_port_offset, 与 @cport 约定一致)
    let rpc_port_u16: u16 = rpc_socket.port();
    let cluster_data_port_offset = cluster.cluster_data_port_offset;
    if rpc_port_u16 > 65535u16 - cluster_data_port_offset {
        return Err(format!(
            "RPC port {} is too large: rpc_port + {} = {} exceeds u16::MAX (65535). \
       Use a port <= {}.",
            rpc_port_u16,
            cluster_data_port_offset,
            rpc_port_u16 as u32 + cluster_data_port_offset as u32,
            65535u16 - cluster_data_port_offset,
        )
        .into());
    }
    let data_port = rpc_port_u16 + cluster_data_port_offset;
    let data_addr = format!("{}:{}", rpc_socket.ip(), data_port);

    // 8. 创建 Router (初始空表, 由 LifecycleManager 刷新)
    let router = Arc::new(Router::new(
        default_slot_table(),
        HashMap::new(),
        HashMap::new(),
    ));

    // 9. MetaRaftProvider 包装 (MetaRaftNode 未实现 trait, 需手动包装)
    struct MetaRaftProv(Arc<MetaRaftNode>);
    impl aidb::cluster::lifecycle_manager::MetaRaftProvider for MetaRaftProv {
        fn get_cluster_meta(&self) -> ClusterMeta {
            self.0.get_cluster_meta()
        }
        fn get_slot_table(&self) -> SlotTable {
            self.0.get_slot_table()
        }
        fn get_migration_state(&self) -> Option<SlotMigrationState> {
            self.0.get_migration_state()
        }
    }

    // 10. 创建 LifecycleManager + MultiRaftNode (复用共享 dispatcher).
    let meta_raft_prov = Arc::new(MetaRaftProv(Arc::clone(&meta_raft)));
    let lifecycle = LifecycleManager::new(node_id, router.clone(), meta_raft_prov)
        .with_tick_interval(std::time::Duration::from_millis(cluster.lifecycle_tick_ms));
    let multi_raft =
        MultiRaftNode::new_with_lifecycle(node_id, router.clone(), dispatcher.clone(), lifecycle);
    let multi_raft = Arc::new(multi_raft);

    // 11. 启动 Lifecycle (数据 Group 自动创建/销毁)
    let mut data_raft_config = raft_config.clone();
    data_raft_config.log_committer_config =
        Some(aidb::cluster::log_committer::LogCommitterConfig::default());
    let mut data_options = server_db_options_with_preset(settings.sync_wal, aidb_preset);
    data_options.statistics = Some(shared_stats.clone());
    let lifecycle_cfg = aidb::cluster::multi_raft_node::LifecycleConfig {
        data_dir: data_dir.to_path_buf(),
        raft_node_config: data_raft_config,
        options: data_options,
        compaction_filter: Some(std::sync::Arc::new(TtlExpireFilter)),
        compaction_removal_listener_factory: None,
    };
    let _lifecycle_shutdown = multi_raft.start_lifecycle_with_data(lifecycle_cfg);

    // 12. 启动数据面 gRPC server (spawn 后台任务)
    let data_socket: NetAddr = data_addr.parse()?;
    let mr_for_grpc = Arc::clone(&multi_raft);
    tokio::spawn(async move {
        if let Err(e) = mr_for_grpc.start(data_socket, 64 * 1024 * 1024).await {
            tracing::error!(error = %e, "multi raft gRPC server failed");
        }
    });
    tracing::info!(data_addr = %data_addr, "multi raft gRPC server starting");

    // 14. 创建 MembershipCoordinator
    let membership = Arc::new(MembershipCoordinator::new(
        meta_raft.clone(),
        multi_raft.clone(),
        router.clone(),
        node_id,
    ));

    // 15. 创建 ClusterStateManager, 注入 membership, 设置全局单例
    let mut state_mgr = aikv::cluster::state::ClusterStateManager::new(
        router.as_ref().clone(),
        meta_raft.clone(),
        multi_raft.clone(),
        node_id,
        cluster.announce_mode,
    );
    state_mgr.set_membership_coordinator(membership.clone());
    let slot_migration = Arc::new(SlotMigrationManager::new(
        meta_raft.clone(),
        multi_raft.clone(),
        router.clone(),
        node_id,
        data_dir.join("slot_migration"),
        MigrationConfig::default(),
    ));
    state_mgr.set_slot_migration_manager(slot_migration);
    state_mgr.data_dir = Some(data_dir.to_path_buf());
    state_mgr.data_port_offset = cluster_data_port_offset;
    state_mgr.metrics = Some(metrics);
    tracing::info!(
      announce_mode = ?state_mgr.announce_resolver.mode(),
      client_addr = %external_client_addr,
      "cluster announce resolver ready"
    );
    let state_mgr = Arc::new(state_mgr);
    let _ = aikv::cluster::state::CLUSTER_STATE_MGR.set(state_mgr);

    // 16. 启动拓扑后台刷新 (leader 缓存 + gossip metrics)
    let _topology_refresh_handle = aikv::cluster::gossip::start_background_refresh(
        aikv::cluster::state::CLUSTER_STATE_MGR
            .get()
            .unwrap()
            .clone(),
        cluster.gossip_interval,
    );

    // 17. 启动 LeaderChangeWatcher (tick 后同步观测 leader 到路由缓存)
    let tick_ms = (cluster.raft_election_timeout_min / 2).max(100);
    let leader_watcher = std::sync::Arc::new(LeaderChangeWatcher::new(
        node_id,
        multi_raft.clone(),
        meta_raft.clone(),
        std::time::Duration::from_millis(tick_ms),
        std::time::Duration::from_millis(cluster.raft_election_timeout_max), // lease = election_timeout_max
    ));
    let (watcher_tx, watcher_rx) = tokio::sync::watch::channel(false);
    let watcher_for_task = leader_watcher.clone();
    let tick_duration = std::time::Duration::from_millis(tick_ms);
    tokio::spawn(async move {
        let mut shutdown_rx = watcher_rx;
        loop {
            tokio::select! {
              _ = tokio::time::sleep(tick_duration) => {
                let changed = watcher_for_task.tick().await;
                if let Some(mgr) = aikv::cluster::state::CLUSTER_STATE_MGR.get() {
                  let cache = watcher_for_task.leader_cache().read();
                  for group_id in changed {
                    if let Some(leader_id) = cache.get(&group_id).and_then(|v| *v) {
                      mgr.apply_observed_group_leader(group_id, leader_id);
                    }
                  }
                  // 探活 quorum 状态注入 → 派生 cluster_state_ok (fail 时路由拒绝读写).
                  mgr.apply_observed_group_quorum(
                    watcher_for_task.leader_quorum_status(),
                    mgr.meta_raft.get_slot_table(),
                    mgr.meta_raft.get_cluster_meta(),
                  );
                }
              }
              _ = shutdown_rx.changed() => {
                tracing::info!("LeaderChangeWatcher shutting down");
                break;
              }
            }
        }
    });
    if let Some(mgr) = aikv::cluster::state::CLUSTER_STATE_MGR.get() {
        *mgr._watcher_shutdown.lock() = Some(watcher_tx);
    }
    tracing::info!(tick_ms, "LeaderChangeWatcher started");

    // 18. 启动 ConfigAutoSave
    let auto_save = aikv::cluster::config_auto_save::ConfigAutoSave::new_with_interval(
        meta_raft.clone(),
        data_dir.to_path_buf(),
        std::time::Duration::from_millis(cluster.config_auto_save_ms),
    );
    let (auto_save_tx, auto_save_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        auto_save.run(auto_save_rx).await;
    });
    if let Some(mgr) = aikv::cluster::state::CLUSTER_STATE_MGR.get() {
        *mgr._auto_save_shutdown.lock() = Some(auto_save_tx);
    }
    tracing::info!("ConfigAutoSave started");

    tracing::info!(node_id, rpc_addr, "cluster initialized successfully");
    Ok(())
}

fn init_logging(settings: &ResolvedSettings) {
    aidb::observability::set_traces_enabled(should_export_traces(
        settings.observability.otel_sample_ratio,
        settings.observability.otlp_endpoint.as_deref(),
    ));
    let json_enabled = settings.observability.json_log;
    #[cfg(feature = "monitoring")]
    let tcp_port = settings.bind.port();
    #[cfg(all(feature = "monitoring", feature = "cluster"))]
    let cluster_node_id = settings.cluster.node_id;
    #[cfg(all(feature = "monitoring", not(feature = "cluster")))]
    let cluster_node_id: Option<u64> = None;

    let log_level = settings.observability.log_level.as_str();

    #[cfg(feature = "monitoring")]
    if let Some(config) = aikv::server::otel::otel_config_from_settings(
        &settings.observability,
        tcp_port,
        cluster_node_id,
    ) {
        if aikv::server::otel::init_otel(&config) && aikv::server::otel::tracer_installed() {
            let tracer = opentelemetry::global::tracer("aikv");
            let otel_layer = tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
            let base = Registry::default().with(otel_layer);
            let subscriber = if json_enabled {
                base.with(
                    fmt::layer()
                        .json()
                        .flatten_event(false)
                        .with_current_span(true)
                        .with_span_list(true)
                        .with_writer(std::io::stderr)
                        .with_filter(EnvFilter::new(log_level))
                        .boxed(),
                )
            } else {
                base.with(
                    fmt::layer()
                        .compact()
                        .with_target(true)
                        .with_writer(std::io::stderr)
                        .with_filter(EnvFilter::new(log_level))
                        .boxed(),
                )
            };
            let _ = subscriber.try_init();
            return;
        }
    }

    // 无 OTel 时只初始化 fmt subscriber
    let subscriber = if json_enabled {
        Registry::default().with(
            fmt::layer()
                .json()
                .flatten_event(false)
                .with_current_span(true)
                .with_span_list(true)
                .with_writer(std::io::stderr)
                .with_filter(EnvFilter::new(log_level))
                .boxed(),
        )
    } else {
        Registry::default().with(
            fmt::layer()
                .compact()
                .with_target(true)
                .with_writer(std::io::stderr)
                .with_filter(EnvFilter::new(log_level))
                .boxed(),
        )
    };
    if let Err(e) = subscriber.try_init() {
        eprintln!("warn: tracing subscriber already initialized ({})", e);
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let (settings, warnings) = match resolve(&cli) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if cli.print_config {
        match print_resolved_config(&settings) {
            Ok(output) => print!("{output}"),
            Err(e) => {
                eprintln!("failed to serialize resolved config: {e}");
                std::process::exit(1);
            }
        }
    }
    init_logging(&settings);
    if let Some(path) = settings.config_file_used.as_deref() {
        tracing::info!("config file: {}", path.display());
    } else {
        tracing::info!("config file: none");
    }
    for w in warnings {
        tracing::warn!(target = "aikv::config", ?w, "configuration warning");
    }

    tracing::info!(?settings, "resolved config");
    tracing::info!(
        bind = %settings.bind,
        engine = ?settings.engine,
        "aikv starting"
    );

    let observation = StorageObservation::new();
    let (storage, engine_kind, data_dir, _cluster_db) =
        match build_storage(&settings, observation.clone()).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "failed to initialize storage");
                std::process::exit(1);
            }
        };
    let tcp_port = settings.bind.port();
    let connection_config = ConnectionConfig {
        max_clients: settings.max_clients,
        script_worker_threads: settings.script_worker_threads,
        ..Default::default()
    };
    let state = ServerSharedState::new_with_backup_dir(
        connection_config,
        storage,
        tcp_port,
        engine_kind,
        data_dir,
        settings.backup_dir.clone(),
        settings.observability.metrics_port,
        settings.observability.metrics_addr.clone(),
        Some(observation),
    );

    // 集群初始化 (等待 init_cluster 完成, 确保 CLUSTER_STATE_MGR 就绪)
    #[cfg(feature = "cluster")]
    if settings.cluster.node_id.is_some() && settings.cluster.rpc_addr.is_some() {
        if let Err(e) = init_cluster(&settings, _cluster_db, Arc::clone(&state.metrics)).await {
            tracing::error!(error = %e, "cluster initialization failed");
            std::process::exit(1);
        }
        let wait_timeout = std::time::Duration::from_millis(
            settings
                .cluster
                .lifecycle_tick_ms
                .saturating_mul(3)
                .saturating_add(10_000),
        );
        if let Err(e) = wait_local_data_groups(wait_timeout).await {
            tracing::error!(error = %e, "local data groups not ready after cluster init");
            std::process::exit(1);
        }
        // MultiRaft group 已打开: 再扫 group 数据校正计数 (首次 open 时 CLUSTER_STATE_MGR 尚未就绪).
        if let Err(e) = state.storage.rebuild_key_counts().await {
            tracing::error!(error = %e, "rebuild_key_counts after cluster init failed");
            std::process::exit(1);
        }
    }

    // 启动 Metrics HTTP 服务器 (仅 monitoring feature)
    #[cfg(feature = "monitoring")]
    {
        let metrics_addr_str = format!(
            "{}:{}",
            settings.observability.metrics_addr, settings.observability.metrics_port
        );
        match metrics_addr_str.parse::<std::net::SocketAddr>() {
            Ok(metrics_addr) => {
                let metrics_server = aikv::server::metrics_server::MetricsServer::new(metrics_addr);
                tokio::spawn(async move {
                    metrics_server.run().await;
                });
            }
            Err(_) => {
                tracing::warn!(addr = %metrics_addr_str, "invalid metrics address, metrics server disabled");
            }
        }

        // 一份快照同时写成 /metrics 文本. 端点为空时不推 OTLP.
        let bg_state = Arc::clone(&state);
        let export_secs = settings.observability.metrics_export_interval_secs.max(1);
        let otlp_enabled =
            aikv::server::export::export_config(settings.observability.otlp_endpoint.as_deref())
                .otlp_enabled;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(export_secs));
            let mut baseline = aikv::server::export::ExportSnapshot::default();
            loop {
                interval.tick().await;
                bg_state.refresh_runtime_metrics().await;
                bg_state.metrics().refresh_process_metrics();
                let keys_estimate = bg_state.storage.len(0).await.unwrap_or(0) as u64;
                let db_count = bg_state.storage.db_count().await.unwrap_or(0);
                let mut expires = Vec::with_capacity(db_count);
                for db in 0..db_count {
                    if let Ok(count) = bg_state.storage.expires(db).await {
                        expires.push((db, count));
                    }
                }
                let engine = bg_state.storage.aidb_statistics();
                #[cfg(feature = "cluster")]
                let raft = aikv::cluster::state::raft_export_views();
                #[cfg(not(feature = "cluster"))]
                let raft = Vec::new();
                let current = aikv::server::export::collect_export_snapshot(
                    bg_state.metrics(),
                    keys_estimate,
                    &expires,
                    engine.as_deref(),
                    &raft,
                );
                let text = aikv::server::export::render_prometheus(&current);
                aikv::server::export::publish_metrics_text(text);
                if otlp_enabled {
                    let mut sink = aikv::server::export::OtelSink;
                    aikv::server::export::push_deltas(&current, &baseline, &mut sink);
                    aikv::server::otel::flush_otel_metrics().await;
                }
                baseline = current;
            }
        });
    }

    blocking::start_background_eviction();

    if let Err(e) = Server::run(settings.bind, state).await {
        tracing::error!(error = %e, "server exited with error");
        #[cfg(feature = "monitoring")]
        aikv::server::otel::shutdown_otel();
        std::process::exit(1);
    }

    #[cfg(feature = "monitoring")]
    aikv::server::otel::shutdown_otel();
}

#[cfg(test)]
#[cfg(test)]
fn export_spans_enabled_at(level: &str) -> bool {
    aidb::observability::set_traces_enabled(true);
    let subscriber = Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(
            fmt::layer()
                .with_writer(std::io::sink)
                .with_filter(EnvFilter::new(level)),
        );
    let enabled = tracing::subscriber::with_default(subscriber, || {
        !aidb::observability::optional_span("kv_command").is_disabled()
    });
    aidb::observability::set_traces_enabled(false);
    enabled
}

#[cfg(test)]
mod logging_filter_tests {
    use super::export_spans_enabled_at;

    #[test]
    fn error_log_level_still_creates_command_span() {
        assert!(export_spans_enabled_at("error"));
    }

    #[test]
    fn init_logging_does_not_filter_the_whole_subscriber() {
        let src = include_str!("main.rs");
        let start = src.find("fn init_logging").expect("init_logging");
        let body = &src[start..start + 2800];
        assert!(
            !body.contains(".with(filter)"),
            "log filter must sit on the fmt layer, not the whole subscriber"
        );
    }
}
