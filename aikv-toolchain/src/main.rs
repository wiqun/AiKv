//! AiKv Toolchain - Project Management TUI Tool
//!
//! A comprehensive toolchain for building, deploying, and managing AiKv.
//! Features:
//! - **One-click cluster setup**: `cluster setup` does everything
//! - Build AiKv (single-node and cluster modes)
//! - Build Docker images
//! - Generate deployment configurations
//! - **Cluster management: start, init, status, stop**
//! - View configuration documentation
//! - Run benchmarks
//! - Display optimization suggestions

mod app;
mod cluster;
mod commands;
mod config;
mod deploy;
mod docs;
mod tui;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// AiKv Toolchain - Project Management CLI/TUI
///
/// One-click deployment tool for AiKv cluster.
/// 
/// Quick start:
///   aikv-tool cluster setup    # Deploy a 6-node cluster in one command!
///   aikv-tool cluster status   # Check cluster health
///   aikv-tool cluster stop     # Stop the cluster
#[derive(Parser)]
#[command(name = "aikv-tool")]
#[command(author = "Jerry")]
#[command(version = "0.2.0")]
#[command(about = "AiKv 一键部署工具 - 傻瓜式集群管理", long_about = None)]
struct Cli {
    /// Path to AiKv project root (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    project_dir: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the interactive TUI interface
    Tui,

    /// Build AiKv project
    Build {
        /// Build with cluster feature
        #[arg(short, long)]
        cluster: bool,

        /// Build in release mode
        #[arg(short, long)]
        release: bool,
    },

    /// Build Docker image
    Docker {
        /// Build with cluster feature
        #[arg(short, long)]
        cluster: bool,

        /// Image tag
        #[arg(short, long, default_value = "latest")]
        tag: String,
    },

    /// Generate deployment files
    Deploy {
        /// Deployment type: single, cluster
        #[arg(short = 't', long, default_value = "single")]
        deploy_type: String,

        /// Output directory
        #[arg(short, long, default_value = "./deploy")]
        output: PathBuf,

        /// Configuration template
        #[arg(long)]
        template: Option<String>,
    },

    /// Cluster management commands (RECOMMENDED)
    /// 
    /// Use 'aikv-tool cluster setup' for one-click deployment!
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },

    /// Show configuration documentation
    Config {
        /// Show cluster configuration
        #[arg(short, long)]
        cluster: bool,
    },

    /// Run benchmarks
    Bench {
        /// Benchmark type: quick, full, custom
        #[arg(short = 't', long, default_value = "quick")]
        bench_type: String,
    },

    /// Show optimization suggestions
    Optimize,

    /// Show project documentation
    Docs {
        /// Documentation topic
        #[arg(short, long)]
        topic: Option<String>,
    },

    /// Check project status
    Status,
    
    /// Quick alias for 'cluster setup' - one-click deploy
    #[command(name = "up", about = "Quick alias for 'cluster setup' - deploy cluster in one command")]
    Up {
        /// Output directory for deployment files
        #[arg(short, long, default_value = "./deploy")]
        output: PathBuf,
    },
    
    /// Quick alias for 'cluster stop' - stop the cluster
    #[command(name = "down", about = "Quick alias for 'cluster stop' - stop the cluster")]
    Down {
        /// Deployment directory
        #[arg(short, long, default_value = "./deploy")]
        deploy_dir: PathBuf,

        /// Also remove data volumes
        #[arg(short, long)]
        volumes: bool,
    },
}

#[derive(Subcommand)]
enum ClusterAction {
    /// 🚀 One-click cluster setup: generate files, build image, start, and initialize
    /// 
    /// This is the RECOMMENDED way to deploy AiKv cluster!
    /// It will automatically:
    ///   1. Generate deployment configuration files
    ///   2. Build Docker image with cluster feature
    ///   3. Start all 6 containers
    ///   4. Initialize MetaRaft membership
    ///   5. Assign slots and configure replication
    Setup {
        /// Output directory for deployment files
        #[arg(short, long, default_value = "./deploy")]
        output: PathBuf,
    },

    /// Start the cluster (requires deploy files to exist)
    Start {
        /// Deployment directory
        #[arg(short, long, default_value = "./deploy")]
        deploy_dir: PathBuf,

        /// Seconds to wait for nodes to be ready
        #[arg(short, long, default_value = "10")]
        wait: u64,
    },

    /// Initialize cluster with MetaRaft membership and slot assignment
    Init {
        /// Deployment directory
        #[arg(short, long, default_value = "./deploy")]
        deploy_dir: PathBuf,
    },

    /// Show cluster status
    Status,

    /// Stop the cluster
    Stop {
        /// Deployment directory
        #[arg(short, long, default_value = "./deploy")]
        deploy_dir: PathBuf,

        /// Also remove data volumes
        #[arg(short, long)]
        volumes: bool,
    },
    
    /// Restart the cluster (stop + start + init)
    Restart {
        /// Deployment directory
        #[arg(short, long, default_value = "./deploy")]
        deploy_dir: PathBuf,
    },
    
    /// View cluster logs
    Logs {
        /// Deployment directory
        #[arg(short, long, default_value = "./deploy")]
        deploy_dir: PathBuf,
        
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        
        /// Number of lines to show (default: 100)
        #[arg(short, long, default_value = "100")]
        lines: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize better panic handling
    better_panic::install();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("aikv_toolchain=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Tui) | None => {
            // Start TUI interface
            tui::run(&cli.project_dir).await?;
        }
        Some(Commands::Build {
            cluster,
            release,
        }) => {
            commands::build(&cli.project_dir, cluster, release).await?;
        }
        Some(Commands::Docker {
            cluster,
            tag,
        }) => {
            commands::build_docker(&cli.project_dir, cluster, &tag).await?;
        }
        Some(Commands::Deploy {
            deploy_type,
            output,
            template,
        }) => {
            deploy::generate(&cli.project_dir, &deploy_type, &output, template.as_deref()).await?;
        }
        Some(Commands::Cluster { action }) => {
            match action {
                ClusterAction::Setup { output } => {
                    cluster::one_click_setup(&cli.project_dir, &output).await?;
                }
                ClusterAction::Start { deploy_dir, wait } => {
                    cluster::start_cluster(&deploy_dir, wait).await?;
                }
                ClusterAction::Init { deploy_dir } => {
                    cluster::init_cluster(&deploy_dir).await?;
                }
                ClusterAction::Status => {
                    cluster::show_cluster_status().await?;
                }
                ClusterAction::Stop { deploy_dir, volumes } => {
                    cluster::stop_cluster(&deploy_dir, volumes).await?;
                }
                ClusterAction::Restart { deploy_dir } => {
                    cluster::restart_cluster(&deploy_dir).await?;
                }
                ClusterAction::Logs { deploy_dir, follow, lines } => {
                    cluster::show_logs(&deploy_dir, follow, lines).await?;
                }
            }
        }
        // Quick aliases
        Some(Commands::Up { output }) => {
            cluster::one_click_setup(&cli.project_dir, &output).await?;
        }
        Some(Commands::Down { deploy_dir, volumes }) => {
            cluster::stop_cluster(&deploy_dir, volumes).await?;
        }
        Some(Commands::Config {
            cluster,
        }) => {
            config::show_config(cluster)?;
        }
        Some(Commands::Bench {
            bench_type,
        }) => {
            commands::run_benchmark(&cli.project_dir, &bench_type).await?;
        }
        Some(Commands::Optimize) => {
            docs::show_optimization_suggestions()?;
        }
        Some(Commands::Docs {
            topic,
        }) => {
            docs::show_documentation(topic.as_deref())?;
        }
        Some(Commands::Status) => {
            commands::show_status(&cli.project_dir).await?;
        }
    }

    Ok(())
}
