//! Commands module - Execute build, docker, benchmark commands

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Build result containing output logs
pub struct CommandResult {
    /// Whether the command completed successfully
    pub success: bool,
    /// Log lines captured from the command output (stdout and stderr)
    pub logs: Vec<String>,
}

/// Build AiKv project with output capture for TUI
pub async fn build_with_output(
    project_dir: &Path,
    cluster: bool,
    release: bool,
) -> Result<CommandResult> {
    let mut logs = Vec::new();
    logs.push("Building AiKv...".to_string());

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_dir);
    cmd.arg("build");

    if release {
        cmd.arg("--release");
        logs.push("  Mode: release".to_string());
    } else {
        logs.push("  Mode: debug".to_string());
    }

    if cluster {
        cmd.arg("--features").arg("cluster");
        logs.push("  Feature: cluster".to_string());
    }

    let output = cmd.output().await?;

    // Capture stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.trim().is_empty() {
            logs.push(line.to_string());
        }
    }

    // Capture stderr (cargo outputs to stderr)
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        if !line.trim().is_empty() {
            logs.push(line.to_string());
        }
    }

    if output.status.success() {
        let mode = if release { "release" } else { "debug" };
        let features = if cluster { " (with cluster)" } else { "" };
        logs.push(format!(
            "✅ Build completed successfully! ({} mode{})",
            mode, features
        ));
        logs.push(format!(
            "   Binary location: target/{}/aikv",
            if release { "release" } else { "debug" }
        ));
        Ok(CommandResult {
            success: true,
            logs,
        })
    } else {
        logs.push("❌ Build failed".to_string());
        Ok(CommandResult {
            success: false,
            logs,
        })
    }
}

/// Build AiKv project
pub async fn build(project_dir: &Path, cluster: bool, release: bool) -> Result<()> {
    println!("Building AiKv...");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_dir);
    cmd.arg("build");

    if release {
        cmd.arg("--release");
    }

    if cluster {
        cmd.arg("--features").arg("cluster");
    }

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if status.success() {
        let mode = if release { "release" } else { "debug" };
        let features = if cluster { " (with cluster)" } else { "" };
        println!(
            "\n✅ Build completed successfully! ({} mode{})",
            mode, features
        );
        println!(
            "   Binary location: target/{}/aikv",
            if release { "release" } else { "debug" }
        );
        Ok(())
    } else {
        Err(anyhow!("Build failed"))
    }
}

/// Build Docker image with output capture for TUI
pub async fn build_docker_with_output(
    project_dir: &Path,
    cluster: bool,
    tag: &str,
) -> Result<CommandResult> {
    let mut logs = Vec::new();
    logs.push("Building Docker image...".to_string());
    logs.push(format!("  Image tag: aikv:{}", tag));

    // Check if docker is available
    let docker_check = Command::new("docker").arg("--version").output().await;

    if docker_check.is_err() {
        logs.push("❌ Docker is not installed or not in PATH".to_string());
        return Ok(CommandResult {
            success: false,
            logs,
        });
    }

    if cluster {
        logs.push("  Feature: cluster".to_string());
    }

    let mut cmd = Command::new("docker");
    cmd.current_dir(project_dir);
    cmd.arg("build");
    cmd.arg("-t").arg(format!("aikv:{}", tag));

    if cluster {
        cmd.arg("--build-arg").arg("FEATURES=cluster");
    }

    cmd.arg(".");

    let output = cmd.output().await?;

    // Capture stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.trim().is_empty() {
            logs.push(line.to_string());
        }
    }

    // Capture stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        if !line.trim().is_empty() {
            logs.push(line.to_string());
        }
    }

    if output.status.success() {
        logs.push("✅ Docker image built successfully!".to_string());
        logs.push(format!("   Image: aikv:{}", tag));
        logs.push(format!("   To run: docker run -d -p 6379:6379 aikv:{}", tag));
        Ok(CommandResult {
            success: true,
            logs,
        })
    } else {
        logs.push("❌ Docker build failed".to_string());
        Ok(CommandResult {
            success: false,
            logs,
        })
    }
}

/// Build Docker image
pub async fn build_docker(project_dir: &Path, cluster: bool, tag: &str) -> Result<()> {
    println!("Building Docker image...");

    // Check if docker is available
    let docker_check = Command::new("docker").arg("--version").output().await;

    if docker_check.is_err() {
        return Err(anyhow!(
            "Docker is not installed or not in PATH. Please install Docker first."
        ));
    }

    let mut cmd = Command::new("docker");
    cmd.current_dir(project_dir);
    cmd.arg("build");
    cmd.arg("-t").arg(format!("aikv:{}", tag));

    if cluster {
        cmd.arg("--build-arg").arg("FEATURES=cluster");
    }

    cmd.arg(".");

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if status.success() {
        println!("\n✅ Docker image built successfully!");
        println!("   Image: aikv:{}", tag);
        println!("\n   To run:");
        println!("   docker run -d -p 6379:6379 aikv:{}", tag);
        Ok(())
    } else {
        Err(anyhow!("Docker build failed"))
    }
}

/// Run benchmarks
pub async fn run_benchmark(project_dir: &Path, bench_type: &str) -> Result<()> {
    match bench_type {
        "quick" => run_quick_benchmark(project_dir).await,
        "full" => run_full_benchmark(project_dir).await,
        _ => {
            println!("Unknown benchmark type: {}", bench_type);
            println!("Available types: quick, full");
            Ok(())
        }
    }
}

async fn run_quick_benchmark(project_dir: &Path) -> Result<()> {
    println!("Running quick benchmark using cargo bench (subset)...");

    let status = Command::new("cargo")
        .current_dir(project_dir)
        .arg("bench")
        .arg("--bench")
        .arg("aikv_benchmark")
        .arg("--")
        .arg("--quick")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if status.success() {
        println!("\n✅ Quick benchmark completed!");
        Ok(())
    } else {
        // If quick fails, it might not support --quick flag
        println!("\nNote: Running basic benchmark...");
        let status = Command::new("cargo")
            .current_dir(project_dir)
            .arg("bench")
            .arg("--bench")
            .arg("aikv_benchmark")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;

        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("Benchmark failed"))
        }
    }
}

async fn run_full_benchmark(project_dir: &Path) -> Result<()> {
    println!("Running full benchmark suite...");

    let status = Command::new("cargo")
        .current_dir(project_dir)
        .arg("bench")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if status.success() {
        println!("\n✅ Full benchmark completed!");
        println!("   Results available in target/criterion/");
        Ok(())
    } else {
        Err(anyhow!("Benchmark failed"))
    }
}

/// Show project status
pub async fn show_status(project_dir: &Path) -> Result<()> {
    use sysinfo::System;

    println!("📊 AiKv Project Status");
    println!("======================\n");

    // Check project structure
    println!("📁 Project Structure:");
    let cargo_toml = project_dir.join("Cargo.toml");
    let src_dir = project_dir.join("src");
    let config_dir = project_dir.join("config");
    let docs_dir = project_dir.join("docs");
    let target_dir = project_dir.join("target");
    let release_bin = project_dir.join("target/release/aikv");

    println!(
        "   Cargo.toml: {}",
        if cargo_toml.exists() { "✅" } else { "❌" }
    );
    println!("   src/: {}", if src_dir.exists() { "✅" } else { "❌" });
    println!(
        "   config/: {}",
        if config_dir.exists() { "✅" } else { "❌" }
    );
    println!("   docs/: {}", if docs_dir.exists() { "✅" } else { "❌" });
    println!(
        "   target/: {}",
        if target_dir.exists() { "✅" } else { "❌" }
    );
    println!(
        "   Release binary: {}",
        if release_bin.exists() {
            "✅"
        } else {
            "❌ (not built)"
        }
    );

    println!();

    // System info
    let mut sys = System::new_all();
    sys.refresh_all();

    println!("💻 System Information:");
    println!("   CPU Cores: {}", sys.cpus().len());
    println!("   Total Memory: {} MB", sys.total_memory() / 1024 / 1024);
    println!(
        "   Available Memory: {} MB",
        (sys.total_memory() - sys.used_memory()) / 1024 / 1024
    );

    println!();

    // Rust info
    println!("🦀 Rust Toolchain:");
    let rustc_output = Command::new("rustc").arg("--version").output().await;
    if let Ok(output) = rustc_output {
        println!("   {}", String::from_utf8_lossy(&output.stdout).trim());
    }

    let cargo_output = Command::new("cargo").arg("--version").output().await;
    if let Ok(output) = cargo_output {
        println!("   {}", String::from_utf8_lossy(&output.stdout).trim());
    }

    println!();

    // Docker info
    println!("🐳 Docker:");
    let docker_output = Command::new("docker").arg("--version").output().await;
    if let Ok(output) = docker_output {
        if output.status.success() {
            println!("   {}", String::from_utf8_lossy(&output.stdout).trim());
        } else {
            println!("   ❌ Not available");
        }
    } else {
        println!("   ❌ Not installed");
    }

    println!();

    // Available features
    println!("✨ Available Features:");
    println!("   • Single Node Mode");
    println!("   • Cluster Mode (--features cluster)");
    println!("   • Memory Storage Engine");
    println!("   • AiDb Persistent Storage Engine");
    println!("   • RESP2/RESP3 Protocol Support");
    println!("   • 100+ Redis Commands");
    println!("   • Lua Scripting");
    println!("   • JSON Support");

    Ok(())
}
