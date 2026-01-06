//! Cluster management module - Start, init, stop, and status for AiKv cluster

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

/// One-click cluster setup: generate files, build image, start, and initialize
pub async fn one_click_setup(project_dir: &Path, output_dir: &Path) -> Result<()> {
    println!("🚀 AiKv 集群一键部署");
    println!("================================\n");
    
    // Pre-flight checks
    println!("📋 前置检查...");
    check_prerequisites().await?;
    println!("  ✅ 前置检查通过\n");

    // Step 1: Generate deployment files
    println!("步骤 1/4: 生成部署文件...");
    crate::deploy::generate(project_dir, "cluster", output_dir, None).await?;
    println!("  ✅ 部署文件已生成\n");

    // Step 2: Build Docker image with cluster feature
    println!("步骤 2/4: 构建 Docker 镜像 (aikv:cluster)...");
    crate::commands::build_docker(project_dir, true, "cluster").await?;
    println!("  ✅ Docker 镜像构建完成\n");

    // Step 3: Start the cluster
    println!("步骤 3/4: 启动集群容器...");
    start_cluster(output_dir, 15).await?;
    println!("  ✅ 集群容器已启动\n");

    // Step 4: Initialize cluster with MetaRaft membership
    println!("步骤 4/4: 初始化集群配置...");
    init_cluster(output_dir).await?;
    println!("  ✅ 集群配置完成\n");

    print_success_message();

    Ok(())
}

/// Print success message with usage hints
fn print_success_message() {
    println!("================================");
    println!("🎉 集群部署成功！");
    println!("================================\n");
    
    println!("📊 集群信息:");
    println!("   节点数量: 6 (3 主节点 + 3 副本)");
    println!("   槽分布: 16384 slots");
    println!("   共识协议: MetaRaft\n");
    
    println!("🔗 连接方式:");
    println!("   redis-cli -c -h 127.0.0.1 -p 6379\n");
    
    println!("📝 常用命令:");
    println!("   PING                      # 测试连接");
    println!("   SET key value             # 写入数据");
    println!("   GET key                   # 读取数据");
    println!("   CLUSTER INFO              # 集群状态");
    println!("   CLUSTER NODES             # 节点列表\n");
    
    println!("🔧 管理命令:");
    println!("   aikv-tool cluster status  # 查看集群状态");
    println!("   aikv-tool cluster logs    # 查看日志");
    println!("   aikv-tool cluster stop    # 停止集群");
    println!("   aikv-tool cluster restart # 重启集群\n");
}

/// Check prerequisites before setup
async fn check_prerequisites() -> Result<()> {
    // Check Docker
    let docker_check = Command::new("docker").arg("--version").output().await;
    if docker_check.is_err() || !docker_check.unwrap().status.success() {
        return Err(anyhow!(
            "❌ Docker 未安装或未运行\n\n\
            请先安装 Docker:\n\
            - macOS: brew install --cask docker\n\
            - Ubuntu: sudo apt install docker.io\n\
            - Arch: sudo pacman -S docker\n\n\
            安装后运行: sudo systemctl start docker"
        ));
    }
    
    // Check Docker Compose
    let dc_cmd = get_docker_compose_cmd().await;
    if dc_cmd.is_err() {
        return Err(anyhow!(
            "❌ Docker Compose 未安装\n\n\
            请安装 Docker Compose:\n\
            - 如果使用 Docker Desktop, Compose 已内置\n\
            - 否则: sudo apt install docker-compose"
        ));
    }
    
    // Check redis-cli (optional but recommended)
    let redis_cli_check = Command::new("redis-cli").arg("--version").output().await;
    if redis_cli_check.is_err() {
        println!("  ⚠️  redis-cli 未安装 (可选，但建议安装用于测试)");
        println!("     安装: sudo apt install redis-tools 或 brew install redis");
    }
    
    // Check if ports are available
    for port in [6379, 6380, 6381, 6382, 6383, 6384] {
        if is_port_in_use(port).await {
            return Err(anyhow!(
                "❌ 端口 {} 已被占用\n\n\
                请先停止占用该端口的服务:\n\
                - 查看占用进程: lsof -i :{}\n\
                - 或者停止现有集群: aikv-tool cluster stop",
                port, port
            ));
        }
    }
    
    Ok(())
}

/// Check if a port is in use
async fn is_port_in_use(port: u16) -> bool {
    use std::net::TcpListener;
    TcpListener::bind(format!("127.0.0.1:{}", port)).is_err()
}

/// Start the cluster containers
pub async fn start_cluster(deploy_dir: &Path, wait_secs: u64) -> Result<()> {
    println!("▶️  启动 AiKv 集群容器...");

    // Check if docker-compose.yml exists
    let compose_file = deploy_dir.join("docker-compose.yml");
    if !compose_file.exists() {
        return Err(anyhow!(
            "❌ 找不到 docker-compose.yml\n\n\
            路径: {:?}\n\n\
            请先生成部署文件:\n\
            - 一键部署: aikv-tool cluster setup\n\
            - 或仅生成文件: aikv-tool deploy -t cluster -o {:?}",
            deploy_dir, deploy_dir
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
        return Err(anyhow!(
            "❌ 启动容器失败\n\n\
            可能的原因:\n\
            - Docker 镜像不存在，请运行: aikv-tool docker --cluster --tag cluster\n\
            - 端口被占用，请检查 6379-6384 端口\n\
            - Docker 服务未运行\n\n\
            查看详细日志: docker-compose -f {:?}/docker-compose.yml logs",
            deploy_dir
        ));
    }

    println!("   等待 {} 秒让节点就绪...", wait_secs);
    
    // Show progress
    for i in 0..wait_secs {
        sleep(Duration::from_secs(1)).await;
        print!("\r   进度: [{}>{}] {}/{}s", 
            "=".repeat((i + 1) as usize),
            " ".repeat((wait_secs - i - 1) as usize),
            i + 1, wait_secs);
        use std::io::Write;
        std::io::stdout().flush().ok();
    }
    println!();

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
        println!("✅ 所有 6 个节点已启动!");
    } else if running_count > 0 {
        println!("⚠️  只有 {} 个节点在运行，部分节点可能仍在启动...", running_count);
        println!("   查看状态: aikv-tool cluster status");
    } else {
        return Err(anyhow!(
            "❌ 没有节点成功启动\n\n\
            请检查:\n\
            - Docker 镜像是否存在: docker images | grep aikv\n\
            - 容器日志: docker-compose logs\n\
            - 端口占用: lsof -i :6379"
        ));
    }

    Ok(())
}

/// Initialize cluster with MetaRaft membership and slot assignment
pub async fn init_cluster(deploy_dir: &Path) -> Result<()> {
    println!("🔧 初始化 AiKv 集群...\n");

    // Check if init-cluster.sh exists
    let init_script = deploy_dir.join("init-cluster.sh");
    if !init_script.exists() {
        return Err(anyhow!(
            "❌ 找不到 init-cluster.sh\n\n\
            路径: {:?}\n\n\
            请先生成部署文件:\n\
            - 一键部署: aikv-tool cluster setup\n\
            - 或仅生成文件: aikv-tool deploy -t cluster",
            deploy_dir
        ));
    }

    // Run the init script - use just the script name since we set current_dir
    let status = Command::new("bash")
        .arg("init-cluster.sh")
        .current_dir(deploy_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        return Err(anyhow!(
            "❌ 集群初始化失败\n\n\
            可能的原因:\n\
            - 节点未完全启动，请稍后重试: aikv-tool cluster init\n\
            - 网络问题，请检查容器网络\n\
            - redis-cli 未安装\n\n\
            排查步骤:\n\
            1. 检查节点状态: aikv-tool cluster status\n\
            2. 查看容器日志: aikv-tool cluster logs\n\
            3. 手动测试连接: redis-cli -p 6379 PING"
        ));
    }

    Ok(())
}

/// Show cluster status
pub async fn show_cluster_status() -> Result<()> {
    println!("📊 AiKv 集群状态\n");
    println!("{}",  "=".repeat(50));

    // Check CLUSTER INFO
    println!("\n📈 集群信息:");
    println!("{}", "-".repeat(50));
    
    let output = Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "CLUSTER", "INFO"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                let info = String::from_utf8_lossy(&out.stdout);
                // Parse and format key info
                for line in info.lines() {
                    if line.starts_with("cluster_state:") {
                        let state = line.replace("cluster_state:", "");
                        if state.trim() == "ok" {
                            println!("   状态: ✅ {}", state.trim().to_uppercase());
                        } else {
                            println!("   状态: ❌ {}", state.trim().to_uppercase());
                        }
                    } else if line.starts_with("cluster_slots_assigned:") {
                        println!("   已分配槽: {}", line.replace("cluster_slots_assigned:", "").trim());
                    } else if line.starts_with("cluster_known_nodes:") {
                        println!("   已知节点: {}", line.replace("cluster_known_nodes:", "").trim());
                    } else if line.starts_with("cluster_size:") {
                        println!("   集群大小: {} (主节点数)", line.replace("cluster_size:", "").trim());
                    }
                }
            } else {
                println!("   ❌ 无法获取集群信息 (集群可能未运行)");
            }
        }
        Err(e) => {
            println!("   ❌ redis-cli 未安装或连接失败: {}", e);
            println!("   安装 redis-cli: sudo apt install redis-tools");
        }
    }

    // Check CLUSTER NODES
    println!("\n🖥️  节点列表:");
    println!("{}", "-".repeat(50));
    
    let output = Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "CLUSTER", "NODES"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                let nodes = String::from_utf8_lossy(&out.stdout);
                for line in nodes.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 3 {
                        let role = if parts[2].contains("master") {
                            "Master"
                        } else if parts[2].contains("slave") {
                            "Replica"
                        } else {
                            "Unknown"
                        };
                        let addr = parts.get(1).unwrap_or(&"?");
                        let slots = if parts.len() > 8 {
                            parts[8..].join(" ")
                        } else {
                            "-".to_string()
                        };
                        println!("   {} {} | {}", 
                            if role == "Master" { "🔵" } else { "🟢" },
                            addr,
                            if role == "Master" { format!("{} slots: {}", role, slots) } else { format!("{}", role) }
                        );
                    }
                }
            } else {
                println!("   无法获取节点列表");
            }
        }
        Err(_) => {}
    }

    // Check MetaRaft Members
    println!("\n📋 MetaRaft 成员:");
    println!("{}", "-".repeat(50));
    
    let output = Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "CLUSTER", "METARAFT", "MEMBERS"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                println!("{}", String::from_utf8_lossy(&out.stdout));
            } else {
                println!("   无法获取 MetaRaft 成员");
            }
        }
        Err(_) => {}
    }

    // Check Docker status
    println!("\n🐳 容器状态:");
    println!("{}", "-".repeat(50));
    
    // Use docker ps directly to get container status
    let output = Command::new("docker")
        .args(["ps", "--filter", "name=aikv", "--format", "{{.Names}}: {{.Status}}"])
        .output()
        .await;

    match output {
        Ok(out) => {
            if out.status.success() {
                let ps = String::from_utf8_lossy(&out.stdout);
                let lines: Vec<&str> = ps.lines().collect();
                let total = lines.len();
                let running = lines.iter().filter(|l| l.contains("Up")).count();
                
                for line in &lines {
                    let status_icon = if line.contains("(healthy)") {
                        "✅"
                    } else if line.contains("unhealthy") {
                        "⚠️"
                    } else if line.contains("Up") {
                        "🔵"
                    } else {
                        "❌"
                    };
                    println!("   {} {}", status_icon, line);
                }
                
                if total == 0 {
                    println!("   ℹ️  没有运行中的 AiKv 容器");
                } else {
                    println!("\n   总计: {}/{} 容器运行中", running, total);
                }
            }
        }
        Err(e) => {
            println!("   ❌ docker 未找到: {}", e);
        }
    }

    println!("\n{}", "=".repeat(50));

    Ok(())
}

/// Stop the cluster
pub async fn stop_cluster(deploy_dir: &Path, remove_volumes: bool) -> Result<()> {
    println!("⏹️  停止 AiKv 集群...");

    // Check if docker-compose.yml exists
    let compose_file = deploy_dir.join("docker-compose.yml");
    if !compose_file.exists() {
        return Err(anyhow!(
            "❌ 找不到 docker-compose.yml\n\n\
            路径: {:?}\n\n\
            可能的原因:\n\
            - 集群未部署，请先运行: aikv-tool cluster setup\n\
            - 部署目录不正确，请使用 -d 参数指定正确的目录",
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
        println!("  (同时删除数据卷)");
    }

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if status.success() {
        println!("✅ 集群已停止");
        if remove_volumes {
            println!("   数据卷已清理");
        }
    } else {
        return Err(anyhow!("停止集群失败"));
    }

    Ok(())
}

/// Restart the cluster (stop + start + init)
pub async fn restart_cluster(deploy_dir: &Path) -> Result<()> {
    println!("🔄 重启 AiKv 集群...\n");
    
    // Stop first
    stop_cluster(deploy_dir, false).await?;
    println!();
    
    // Wait a moment
    sleep(Duration::from_secs(2)).await;
    
    // Start again
    start_cluster(deploy_dir, 15).await?;
    println!();
    
    // Re-initialize
    init_cluster(deploy_dir).await?;
    
    println!("\n✅ 集群重启完成！");
    println!("   连接: redis-cli -c -h 127.0.0.1 -p 6379");
    
    Ok(())
}

/// Show cluster logs
pub async fn show_logs(deploy_dir: &Path, follow: bool, lines: u32) -> Result<()> {
    let compose_file = deploy_dir.join("docker-compose.yml");
    if !compose_file.exists() {
        return Err(anyhow!(
            "❌ 找不到 docker-compose.yml\n\
            请确保集群已部署: aikv-tool cluster setup"
        ));
    }

    let dc_cmd = get_docker_compose_cmd().await?;
    
    let mut cmd = Command::new(&dc_cmd[0]);
    cmd.args(&dc_cmd[1..]);
    cmd.current_dir(deploy_dir);
    cmd.arg("logs");
    cmd.arg("--tail").arg(lines.to_string());
    
    if follow {
        cmd.arg("-f");
    }
    
    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;
        
    if !status.success() {
        return Err(anyhow!("获取日志失败"));
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
