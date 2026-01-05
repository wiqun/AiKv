//! Cluster management module - Start, init, stop, and status for AiKv cluster

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

/// One-click cluster setup: generate files, build image, start, and initialize
pub async fn one_click_setup(project_dir: &Path, output_dir: &Path) -> Result<()> {
    println!("🚀 AiKv Cluster One-Click Setup");
    println!("================================\n");

    // Step 1: Generate deployment files
    println!("Step 1: Generating deployment files...");
    crate::deploy::generate(project_dir, "cluster", output_dir, None).await?;
    println!("  ✅ Deployment files generated\n");

    // Step 2: Build Docker image with cluster feature
    println!("Step 2: Building Docker image with cluster feature...");
    crate::commands::build_docker(project_dir, true, "cluster").await?;
    println!("  ✅ Docker image built\n");

    // Step 3: Start the cluster
    println!("Step 3: Starting cluster...");
    start_cluster(output_dir, 15).await?;
    println!("  ✅ Cluster started\n");

    // Step 4: Initialize cluster with MetaRaft membership
    println!("Step 4: Initializing cluster...");
    init_cluster(output_dir).await?;
    println!("  ✅ Cluster initialized\n");

    println!("================================");
    println!("✅ Cluster setup complete!");
    println!("================================\n");
    println!("Connect with: redis-cli -c -h 127.0.0.1 -p 6379");
    println!("Check status: redis-cli -p 6379 CLUSTER INFO");
    println!("View nodes:   redis-cli -p 6379 CLUSTER NODES");
    println!("MetaRaft:     redis-cli -p 6379 CLUSTER METARAFT MEMBERS");

    Ok(())
}

/// Start the cluster containers
pub async fn start_cluster(deploy_dir: &Path, wait_secs: u64) -> Result<()> {
    println!("Starting AiKv cluster...");

    // Check if docker-compose.yml exists
    let compose_file = deploy_dir.join("docker-compose.yml");
    if !compose_file.exists() {
        return Err(anyhow!(
            "docker-compose.yml not found in {:?}. Run 'aikv-tool deploy -t cluster' first.",
            deploy_dir
        ));
    }

    // Determine docker-compose command
    let dc_cmd = get_docker_compose_cmd().await?;

    // Start containers
    let status = Command::new(&dc_cmd[0])
        .args(&dc_cmd[1..])
        .current_dir(deploy_dir)
        .arg("up")
        .arg("-d")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        return Err(anyhow!("Failed to start cluster containers"));
    }

    println!("Waiting {} seconds for nodes to be ready...", wait_secs);
    sleep(Duration::from_secs(wait_secs)).await;

    // Check if all nodes are running
    let output = Command::new(&dc_cmd[0])
        .args(&dc_cmd[1..])
        .current_dir(deploy_dir)
        .arg("ps")
        .output()
        .await?;

    let ps_output = String::from_utf8_lossy(&output.stdout);
    let running_count = ps_output.matches("Up").count();

    if running_count >= 6 {
        println!("✅ All 6 nodes are running!");
    } else {
        println!("⚠️  Only {} nodes are running. Some nodes may still be starting.", running_count);
    }

    Ok(())
}

/// Initialize cluster with MetaRaft membership and slot assignment
pub async fn init_cluster(deploy_dir: &Path) -> Result<()> {
    println!("Initializing AiKv cluster...\n");

    // Check if init-cluster.sh exists
    let init_script = deploy_dir.join("init-cluster.sh");
    if !init_script.exists() {
        return Err(anyhow!(
            "init-cluster.sh not found in {:?}. Run 'aikv-tool deploy -t cluster' first.",
            deploy_dir
        ));
    }

    // Run the init script
    let status = Command::new("bash")
        .arg(&init_script)
        .current_dir(deploy_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        return Err(anyhow!("Cluster initialization failed"));
    }

    Ok(())
}

/// Show cluster status
pub async fn show_cluster_status() -> Result<()> {
    println!("📊 AiKv Cluster Status\n");

    // Check CLUSTER INFO
    println!("=== Cluster Info ===");
    let output = Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "CLUSTER", "INFO"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                println!("{}", String::from_utf8_lossy(&out.stdout));
            } else {
                println!("❌ Failed to get cluster info");
                println!("{}", String::from_utf8_lossy(&out.stderr));
            }
        }
        Err(e) => {
            println!("❌ redis-cli not found or failed: {}", e);
            println!("   Install redis-cli or check that the cluster is running.");
        }
    }

    // Check CLUSTER NODES
    println!("\n=== Cluster Nodes ===");
    let output = Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "CLUSTER", "NODES"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                println!("{}", String::from_utf8_lossy(&out.stdout));
            } else {
                println!("❌ Failed to get cluster nodes");
            }
        }
        Err(_) => {}
    }

    // Check MetaRaft Members
    println!("\n=== MetaRaft Members ===");
    let output = Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "CLUSTER", "METARAFT", "MEMBERS"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                println!("{}", String::from_utf8_lossy(&out.stdout));
            } else {
                println!("❌ Failed to get MetaRaft members");
            }
        }
        Err(_) => {}
    }

    // Check Docker status
    println!("\n=== Docker Container Status ===");
    let dc_cmd = get_docker_compose_cmd().await.unwrap_or_else(|_| vec!["docker-compose".to_string()]);
    
    let output = Command::new(&dc_cmd[0])
        .args(&dc_cmd[1..])
        .arg("ps")
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                println!("{}", String::from_utf8_lossy(&out.stdout));
            } else {
                println!("❌ docker-compose ps failed");
            }
        }
        Err(e) => {
            println!("❌ docker-compose not found: {}", e);
        }
    }

    Ok(())
}

/// Stop the cluster
pub async fn stop_cluster(deploy_dir: &Path, remove_volumes: bool) -> Result<()> {
    println!("Stopping AiKv cluster...");

    // Check if docker-compose.yml exists
    let compose_file = deploy_dir.join("docker-compose.yml");
    if !compose_file.exists() {
        return Err(anyhow!(
            "docker-compose.yml not found in {:?}",
            deploy_dir
        ));
    }

    // Determine docker-compose command
    let dc_cmd = get_docker_compose_cmd().await?;

    // Stop containers
    let mut cmd = Command::new(&dc_cmd[0]);
    cmd.args(&dc_cmd[1..]);
    cmd.current_dir(deploy_dir);
    cmd.arg("down");

    if remove_volumes {
        cmd.arg("-v");
        println!("  (removing data volumes)");
    }

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if status.success() {
        println!("✅ Cluster stopped successfully");
    } else {
        return Err(anyhow!("Failed to stop cluster"));
    }

    Ok(())
}

/// Get the appropriate docker-compose command
async fn get_docker_compose_cmd() -> Result<Vec<String>> {
    // Try docker compose (v2)
    let output = Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await;

    if output.is_ok() && output.unwrap().status.success() {
        return Ok(vec!["docker".to_string(), "compose".to_string()]);
    }

    // Try docker-compose (v1)
    let output = Command::new("docker-compose")
        .arg("version")
        .output()
        .await;

    if output.is_ok() && output.unwrap().status.success() {
        return Ok(vec!["docker-compose".to_string()]);
    }

    Err(anyhow!(
        "Neither 'docker compose' nor 'docker-compose' found. Please install Docker Compose."
    ))
}
