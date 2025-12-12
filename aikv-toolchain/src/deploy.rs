//! Deployment module - Generate deployment files

use anyhow::{anyhow, Result};
use std::fs;
use std::path::Path;

/// Generate deployment files
pub async fn generate(
    project_dir: &Path,
    deploy_type: &str,
    output_dir: &Path,
    _template: Option<&str>,
) -> Result<()> {
    // Use "aidb" for cluster deployments, "memory" for single node
    let storage_engine = if deploy_type == "cluster" { "aidb" } else { "memory" };
    generate_with_engine(project_dir, deploy_type, output_dir, storage_engine, _template).await
}

/// Generate deployment files with specified storage engine
pub async fn generate_with_engine(
    project_dir: &Path,
    deploy_type: &str,
    output_dir: &Path,
    storage_engine: &str,
    _template: Option<&str>,
) -> Result<()> {
    println!("Generating {} deployment files...", deploy_type);

    // Create output directory
    fs::create_dir_all(output_dir)?;

    match deploy_type {
        "single" => generate_single_deployment_with_engine(project_dir, output_dir, storage_engine)?,
        "cluster" => generate_cluster_deployment_with_engine(project_dir, output_dir, storage_engine)?,
        _ => {
            return Err(anyhow!(
                "Unknown deployment type: {}. Use 'single' or 'cluster'",
                deploy_type
            ))
        }
    }

    println!("\n✅ Deployment files generated successfully!");
    println!("   Output directory: {:?}", output_dir);
    println!("\n   Files created:");

    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        println!("   • {}", entry.file_name().to_string_lossy());
    }

    Ok(())
}

/// Generate single deployment with default "memory" engine.
/// Kept for backward compatibility with existing code.
#[allow(dead_code)]
fn generate_single_deployment(_project_dir: &Path, output_dir: &Path) -> Result<()> {
    generate_single_deployment_with_engine(_project_dir, output_dir, "memory")
}

fn generate_single_deployment_with_engine(_project_dir: &Path, output_dir: &Path, storage_engine: &str) -> Result<()> {
    // Generate docker-compose.yml
    let docker_compose = generate_single_docker_compose();
    fs::write(output_dir.join("docker-compose.yml"), docker_compose)?;

    // Copy or generate configuration
    let config_content = generate_single_config_with_engine(storage_engine);
    fs::write(output_dir.join("aikv.toml"), config_content)?;

    // Generate README
    let readme = generate_single_readme();
    fs::write(output_dir.join("README.md"), readme)?;

    // Generate start/stop scripts
    let start_script = r#"#!/bin/bash
# Start AiKv single node

echo "Starting AiKv..."
docker-compose up -d

echo "Waiting for service to be ready..."
sleep 3

# Health check
if docker-compose ps | grep -q "Up"; then
    echo "✅ AiKv is running!"
    echo "   Connect with: redis-cli -h 127.0.0.1 -p 6379"
else
    echo "❌ Failed to start AiKv"
    docker-compose logs
    exit 1
fi
"#;
    fs::write(output_dir.join("start.sh"), start_script)?;

    let stop_script = r#"#!/bin/bash
# Stop AiKv single node

echo "Stopping AiKv..."
docker-compose down

echo "✅ AiKv stopped"
"#;
    fs::write(output_dir.join("stop.sh"), stop_script)?;

    // Make scripts executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(output_dir.join("start.sh"))?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(output_dir.join("start.sh"), perms.clone())?;
        fs::set_permissions(output_dir.join("stop.sh"), perms)?;
    }

    Ok(())
}

/// Generate cluster deployment with default "aidb" engine.
/// Kept for backward compatibility with existing code.
#[allow(dead_code)]
fn generate_cluster_deployment(_project_dir: &Path, output_dir: &Path) -> Result<()> {
    generate_cluster_deployment_with_engine(_project_dir, output_dir, "aidb")
}

fn generate_cluster_deployment_with_engine(_project_dir: &Path, output_dir: &Path, storage_engine: &str) -> Result<()> {
    // Generate docker-compose.yml
    let docker_compose = generate_cluster_docker_compose();
    fs::write(output_dir.join("docker-compose.yml"), docker_compose)?;

    // Generate configuration for each node
    for i in 1..=6 {
        let config_content = generate_cluster_node_config_with_engine(i, storage_engine);
        fs::write(
            output_dir.join(format!("aikv-node{}.toml", i)),
            config_content,
        )?;
    }

    // Generate README
    let readme = generate_cluster_readme();
    fs::write(output_dir.join("README.md"), readme)?;

    // Generate start script
    let start_script = r#"#!/bin/bash
# Start AiKv cluster (6 nodes: 3 masters, 3 replicas)

echo "Starting AiKv cluster..."
docker-compose up -d

echo "Waiting for all nodes to be ready..."
sleep 10

# Check if all nodes are up
if docker-compose ps | grep -c "Up" | grep -q "6"; then
    echo "✅ All 6 nodes are running!"
else
    echo "⚠️  Some nodes may not be ready yet. Checking..."
    docker-compose ps
fi

echo ""
echo "To initialize the cluster, run:"
echo "redis-cli --cluster create \\"
echo "  127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 \\"
echo "  127.0.0.1:6382 127.0.0.1:6383 127.0.0.1:6384 \\"
echo "  --cluster-replicas 1"
echo ""
echo "To check cluster status:"
echo "  redis-cli -c -p 6379 CLUSTER INFO"
echo "  redis-cli -c -p 6379 CLUSTER NODES"
"#;
    fs::write(output_dir.join("start.sh"), start_script)?;

    let stop_script = r#"#!/bin/bash
# Stop AiKv cluster

echo "Stopping AiKv cluster..."
docker-compose down

echo "✅ AiKv cluster stopped"
"#;
    fs::write(output_dir.join("stop.sh"), stop_script)?;

    let init_script = r#"#!/bin/bash
# Initialize AiKv cluster

echo "Initializing AiKv cluster..."

redis-cli --cluster create \
  127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 \
  127.0.0.1:6382 127.0.0.1:6383 127.0.0.1:6384 \
  --cluster-replicas 1

echo ""
echo "Cluster initialization complete!"
echo ""
echo "Checking cluster status..."
redis-cli -c -p 6379 CLUSTER INFO
"#;
    fs::write(output_dir.join("init-cluster.sh"), init_script)?;

    // Make scripts executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for script in &["start.sh", "stop.sh", "init-cluster.sh"] {
            let mut perms = fs::metadata(output_dir.join(script))?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(output_dir.join(script), perms)?;
        }
    }

    Ok(())
}

fn generate_single_docker_compose() -> String {
    r#"# AiKv Single Node Deployment
# Generated by aikv-tool

version: '3.8'

services:
  aikv:
    image: aikv:latest
    container_name: aikv
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6379:6379"
    volumes:
      - aikv-data:/app/data
      - aikv-logs:/app/logs
      - ./aikv.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6379 | grep -q PONG"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s
    deploy:
      resources:
        limits:
          cpus: '2'
          memory: 1G
        reservations:
          cpus: '0.5'
          memory: 256M

volumes:
  aikv-data:
    driver: local
  aikv-logs:
    driver: local

networks:
  default:
    name: aikv-network
"#
    .to_string()
}

/// Generate single config with default "memory" engine.
/// Kept for backward compatibility with existing code.
#[allow(dead_code)]
fn generate_single_config() -> String {
    generate_single_config_with_engine("memory")
}

fn generate_single_config_with_engine(storage_engine: &str) -> String {
    format!(
        r#"# AiKv Single Node Configuration
# Generated by aikv-tool

[server]
host = "0.0.0.0"
port = 6379

[storage]
engine = "{}"
data_dir = "./data"
databases = 16

[logging]
level = "info"

[slowlog]
log-slower-than = 10000
max-len = 128
"#,
        storage_engine
    )
}

fn generate_single_readme() -> String {
    r#"# AiKv Single Node Deployment

This directory contains the deployment files for a single-node AiKv instance.

## Prerequisites

- Docker and Docker Compose installed
- AiKv Docker image built (`docker build -t aikv:latest .` from AiKv project root)

## Files

| File | Description |
|------|-------------|
| docker-compose.yml | Docker Compose configuration |
| aikv.toml | AiKv configuration file |
| start.sh | Start script |
| stop.sh | Stop script |

## Quick Start

```bash
# Start AiKv
./start.sh

# Or manually
docker-compose up -d
```

## Connecting

```bash
# Using redis-cli
redis-cli -h 127.0.0.1 -p 6379

# Test connection
redis-cli PING
```

## Configuration

Edit `aikv.toml` to customize:

- **Storage Engine**: `memory` (fast) or `aidb` (persistent)
- **Port**: Default 6379
- **Log Level**: trace, debug, info, warn, error

## Monitoring

```bash
# View logs
docker-compose logs -f

# Check status
docker-compose ps
```

## Stopping

```bash
./stop.sh

# Or manually
docker-compose down

# Remove data volumes
docker-compose down -v
```
"#
    .to_string()
}

fn generate_cluster_docker_compose() -> String {
    r#"# AiKv Cluster Deployment (6 nodes: 3 masters, 3 replicas)
# Generated by aikv-tool

version: '3.8'

services:
  aikv1:
    image: aikv:cluster
    container_name: aikv1
    hostname: aikv1
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6379:6379"
      - "16379:16379"
    volumes:
      - aikv1-data:/app/data
      - aikv1-logs:/app/logs
      - ./aikv-node1.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
      - AIKV_NODE_ID=n1
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6379 | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    networks:
      - aikv-cluster

  aikv2:
    image: aikv:cluster
    container_name: aikv2
    hostname: aikv2
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6380:6380"
      - "16380:16380"
    volumes:
      - aikv2-data:/app/data
      - aikv2-logs:/app/logs
      - ./aikv-node2.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
      - AIKV_NODE_ID=n2
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6380 | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    networks:
      - aikv-cluster

  aikv3:
    image: aikv:cluster
    container_name: aikv3
    hostname: aikv3
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6381:6381"
      - "16381:16381"
    volumes:
      - aikv3-data:/app/data
      - aikv3-logs:/app/logs
      - ./aikv-node3.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
      - AIKV_NODE_ID=n3
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6381 | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    networks:
      - aikv-cluster

  aikv4:
    image: aikv:cluster
    container_name: aikv4
    hostname: aikv4
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6382:6382"
      - "16382:16382"
    volumes:
      - aikv4-data:/app/data
      - aikv4-logs:/app/logs
      - ./aikv-node4.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
      - AIKV_NODE_ID=n4
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6382 | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    networks:
      - aikv-cluster

  aikv5:
    image: aikv:cluster
    container_name: aikv5
    hostname: aikv5
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6383:6383"
      - "16383:16383"
    volumes:
      - aikv5-data:/app/data
      - aikv5-logs:/app/logs
      - ./aikv-node5.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
      - AIKV_NODE_ID=n5
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6383 | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    networks:
      - aikv-cluster

  aikv6:
    image: aikv:cluster
    container_name: aikv6
    hostname: aikv6
    command: ["--config", "/app/config/aikv.toml"]
    ports:
      - "6384:6384"
      - "16384:16384"
    volumes:
      - aikv6-data:/app/data
      - aikv6-logs:/app/logs
      - ./aikv-node6.toml:/app/config/aikv.toml:ro
    environment:
      - RUST_LOG=info
      - AIKV_NODE_ID=n6
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "sh", "-c", "echo PING | nc -w 1 localhost 6384 | grep -q PONG"]
      interval: 10s
      timeout: 5s
      retries: 5
    networks:
      - aikv-cluster

volumes:
  aikv1-data:
  aikv1-logs:
  aikv2-data:
  aikv2-logs:
  aikv3-data:
  aikv3-logs:
  aikv4-data:
  aikv4-logs:
  aikv5-data:
  aikv5-logs:
  aikv6-data:
  aikv6-logs:

networks:
  aikv-cluster:
    name: aikv-cluster-network
    driver: bridge
"#
    .to_string()
}

/// Generate cluster node config with default "aidb" engine.
/// Kept for backward compatibility with existing code.
#[allow(dead_code)]
fn generate_cluster_node_config(node_num: u8) -> String {
    generate_cluster_node_config_with_engine(node_num, "aidb")
}

fn generate_cluster_node_config_with_engine(node_num: u8, storage_engine: &str) -> String {
    let port = 6378 + node_num as u16;
    let raft_port = 50050 + node_num as u16;
    let is_bootstrap = if node_num == 1 { "true" } else { "false" };
    format!(
        r#"# AiKv Cluster Node {} Configuration
# Generated by aikv-tool

[server]
host = "0.0.0.0"
port = {}

[cluster]
enabled = true
raft_address = "127.0.0.1:{}"
is_bootstrap = {}

[storage]
engine = "{}"
data_dir = "./data"
databases = 16

[logging]
level = "info"

[slowlog]
log-slower-than = 10000
max-len = 128
"#,
        node_num, port, raft_port, is_bootstrap, storage_engine
    )
}

fn generate_cluster_readme() -> String {
    r#"# AiKv Cluster Deployment

This directory contains deployment files for a 6-node AiKv cluster (3 masters, 3 replicas).

## Prerequisites

- Docker and Docker Compose installed
- AiKv cluster Docker image built:
  ```bash
  docker build -t aikv:cluster --build-arg FEATURES=cluster .
  ```

## Files

| File | Description |
|------|-------------|
| docker-compose.yml | Cluster Docker Compose configuration |
| aikv-node[1-6].toml | Per-node configuration files |
| start.sh | Start cluster script |
| stop.sh | Stop cluster script |
| init-cluster.sh | Initialize cluster script |

## Quick Start

```bash
# 1. Start all nodes
./start.sh

# 2. Wait for nodes to be ready, then initialize cluster
./init-cluster.sh
```

## Manual Steps

```bash
# Start cluster
docker-compose up -d

# Initialize cluster (after all nodes are up)
redis-cli --cluster create \
  127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 \
  127.0.0.1:6382 127.0.0.1:6383 127.0.0.1:6384 \
  --cluster-replicas 1

# Check cluster status
redis-cli -c -p 6379 CLUSTER INFO
redis-cli -c -p 6379 CLUSTER NODES
```

## Connecting

```bash
# Connect with cluster mode
redis-cli -c -p 6379

# Test with hash tags (ensures keys go to same slot)
redis-cli -c -p 6379 SET {user:1000}:name "John"
redis-cli -c -p 6379 SET {user:1000}:age "30"
```

## Node Ports

| Node | Data Port | Cluster Port | Role |
|------|-----------|--------------|------|
| aikv1 | 6379 | 16379 | Master |
| aikv2 | 6380 | 16380 | Master |
| aikv3 | 6381 | 16381 | Master |
| aikv4 | 6382 | 16382 | Replica |
| aikv5 | 6383 | 16383 | Replica |
| aikv6 | 6384 | 16384 | Replica |

## Cluster Operations

```bash
# Check cluster info
redis-cli -c -p 6379 CLUSTER INFO

# View nodes
redis-cli -c -p 6379 CLUSTER NODES

# View slot distribution
redis-cli -c -p 6379 CLUSTER SLOTS

# Get key slot
redis-cli -c -p 6379 CLUSTER KEYSLOT mykey

# Manual failover (on replica node)
redis-cli -p 6382 CLUSTER FAILOVER
```

## Monitoring

```bash
# View all logs
docker-compose logs -f

# View specific node logs
docker-compose logs -f aikv1

# Check node status
docker-compose ps
```

## Stopping

```bash
./stop.sh

# Or manually
docker-compose down

# Remove all data
docker-compose down -v
```
"#
    .to_string()
}
