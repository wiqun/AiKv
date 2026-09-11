//! loadgen 入口: CLI 解析 → 引擎与探活后台任务 → HTTP 控制面.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use arc_swap::ArcSwap;
use clap::Parser;
use tokio_util::sync::CancellationToken;

use loadgen::api;
use loadgen::config::{self, WorkloadConfig};
use loadgen::engine::RuntimeState;

#[derive(Debug, Parser)]
#[command(
    name = "loadgen",
    about = "AiKv 交互式加压工具 (本地/裸机, 单页控制台)"
)]
struct Cli {
    /// 控制台监听地址
    #[arg(long, default_value = "127.0.0.1:8787", env = "LOADGEN_BIND")]
    bind: String,
    /// 目标模式
    #[arg(long, value_enum, default_value = "cluster")]
    mode: config::TargetMode,
    /// 目标地址列表 (逗号分隔)
    #[arg(long, default_value = "127.0.0.1:6379", value_delimiter = ',')]
    endpoints: Vec<String>,
    /// 日志过滤 (RUST_LOG 语义)
    #[arg(long, default_value = "info", env = "LOADGEN_LOG")]
    log: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .init();

    let cfg = WorkloadConfig {
        mode: cli.mode,
        endpoints: cli.endpoints.clone(),
        ..Default::default()
    };
    cfg.validate()
        .map_err(|err| anyhow::anyhow!("启动配置非法: {err}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(cli.bind, cfg))
}

async fn run(bind: String, cfg: WorkloadConfig) -> anyhow::Result<()> {
    let cfg = Arc::new(ArcSwap::from_pointee(cfg));
    let state = Arc::new(RuntimeState::default());
    let engine = Arc::new(loadgen::engine::Engine::new(cfg.clone(), state.clone()));

    let stop = CancellationToken::new();
    let engine_task = tokio::spawn(engine.clone().run(stop.clone()));
    let probe_task = tokio::spawn(loadgen::engine::probe_loop(
        cfg.clone(),
        state.clone(),
        stop.clone(),
    ));

    let app = api::router(api::AppState {
        cfg: cfg.clone(),
        state,
    });
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("无法绑定 {bind}"))?;
    tracing::info!(%bind, "loadgen 控制台已就绪");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(stop.clone()))
        .await?;

    stop.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), engine_task).await;
    probe_task.abort();
    tracing::info!("loadgen 已退出");
    Ok(())
}

async fn shutdown_signal(stop: CancellationToken) {
    let ctrl_c = tokio::signal::ctrl_c();
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("收到退出信号, 优雅停止...");
    stop.cancel();
}
