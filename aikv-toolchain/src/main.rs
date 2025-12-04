//! AiKv Toolchain - Project Management TUI Tool
//!
//! A comprehensive toolchain for building, deploying, and managing AiKv.
//! Features:
//! - Build AiKv (single-node and cluster modes)
//! - Build Docker images
//! - Generate deployment configurations
//! - View configuration documentation
//! - Run benchmarks
//! - Display optimization suggestions

mod app;
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
#[derive(Parser)]
#[command(name = "aikv-tool")]
#[command(author = "Jerry")]
#[command(version = "0.1.0")]
#[command(about = "AiKv project management toolchain with TUI interface", long_about = None)]
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
        #[arg(short, long)]
        template: Option<String>,
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
