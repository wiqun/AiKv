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
RUNNING_COUNT=$(docker-compose ps | grep -c "Up" || true)
if [ "$RUNNING_COUNT" -eq 6 ]; then
    echo "✅ All 6 nodes are running!"
else
    echo "⚠️  Some nodes may not be ready yet. Status:"
    docker-compose ps
fi

echo ""
echo "================================"
echo "Next Steps:"
echo "================================"
echo "1. Initialize the cluster with dynamic MetaRaft membership:"
echo "   ./init-cluster.sh"
echo ""
echo "2. After initialization, connect with:"
echo "   redis-cli -c -h 127.0.0.1 -p 6379"
echo ""
echo "3. Check cluster status:"
echo "   redis-cli -p 6379 CLUSTER INFO"
echo "   redis-cli -p 6379 CLUSTER NODES"
echo "   redis-cli -p 6379 CLUSTER METARAFT MEMBERS"
"#;
    fs::write(output_dir.join("start.sh"), start_script)?;

    let stop_script = r#"#!/bin/bash
# Stop AiKv cluster

echo "Stopping AiKv cluster..."
docker-compose down

echo "✅ AiKv cluster stopped"
"#;
    fs::write(output_dir.join("stop.sh"), stop_script)?;

    let init_script = concat!(
        "#!/bin/bash\n",
        "# Initialize AiKv cluster with dynamic MetaRaft membership\n",
        "# Uses the new learner → voter promotion workflow\n",
        "\n",
        "set -e\n",
        "\n",
        "echo \"================================\"\n",
        "echo \"AiKv Cluster Initialization\"\n",
        "echo \"================================\"\n",
        "echo \"\"\n",
        "\n",
        "# Wait for all nodes to be ready\n",
        "echo \"Step 1: Waiting for all nodes to be ready...\"\n",
        "for i in 1 2 3 4 5 6; do\n",
        "    port=$((6378 + i))\n",
        "    echo \"  Checking node $i (port $port)...\"\n",
        "    for retry in {1..30}; do\n",
        "        if redis-cli -h 127.0.0.1 -p $port PING >/dev/null 2>&1; then\n",
        "            echo \"  ✓ Node $i is ready\"\n",
        "            break\n",
        "        fi\n",
        "        if [ $retry -eq 30 ]; then\n",
        "            echo \"  ✗ Node $i failed to start\"\n",
        "            exit 1\n",
        "        fi\n",
        "        sleep 1\n",
        "    done\n",
        "done\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 2: Getting node IDs from each node...\"\n",
        "NODE1_ID=$(redis-cli -h 127.0.0.1 -p 6379 CLUSTER MYID)\n",
        "NODE2_ID=$(redis-cli -h 127.0.0.1 -p 6380 CLUSTER MYID)\n",
        "NODE3_ID=$(redis-cli -h 127.0.0.1 -p 6381 CLUSTER MYID)\n",
        "echo \"  Node 1 ID: $NODE1_ID\"\n",
        "echo \"  Node 2 ID: $NODE2_ID\"\n",
        "echo \"  Node 3 ID: $NODE3_ID\"\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 3: Adding nodes 2 and 3 as MetaRaft learners...\"\n",
        "echo \"  Adding node 2 (ID: $NODE2_ID)...\"\n",
        "# Convert hex ID to decimal for ADDLEARNER command\n",
        "NODE2_DECIMAL=$(printf \"%d\" 0x${NODE2_ID})\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER $NODE2_DECIMAL aikv2:50052\n",
        "\n",
        "echo \"  Adding node 3 (ID: $NODE3_ID)...\"\n",
        "NODE3_DECIMAL=$(printf \"%d\" 0x${NODE3_ID})\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER $NODE3_DECIMAL aikv3:50053\n",
        "\n",
        "echo \"  Waiting for learners to sync logs...\"\n",
        "sleep 3\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 4: Promoting learners to voters...\"\n",
        "echo \"  Promoting all 3 nodes to voters...\"\n",
        "NODE1_DECIMAL=$(printf \"%d\" 0x${NODE1_ID})\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT PROMOTE $NODE1_DECIMAL $NODE2_DECIMAL $NODE3_DECIMAL\n",
        "\n",
        "echo \"  Waiting for membership change to complete...\"\n",
        "sleep 2\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 5: Verifying MetaRaft membership...\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 6: Adding nodes to cluster metadata...\"\n",
        "echo \"  Meeting node 2...\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6380 $NODE2_ID\n",
        "\n",
        "echo \"  Meeting node 3...\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6381 $NODE3_ID\n",
        "\n",
        "echo \"  Meeting node 4...\"\n",
        "NODE4_ID=$(redis-cli -h 127.0.0.1 -p 6382 CLUSTER MYID)\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6382 $NODE4_ID\n",
        "\n",
        "echo \"  Meeting node 5...\"\n",
        "NODE5_ID=$(redis-cli -h 127.0.0.1 -p 6383 CLUSTER MYID)\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6383 $NODE5_ID\n",
        "\n",
        "echo \"  Meeting node 6...\"\n",
        "NODE6_ID=$(redis-cli -h 127.0.0.1 -p 6384 CLUSTER MYID)\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6384 $NODE6_ID\n",
        "\n",
        "echo \"  Waiting for cluster metadata to sync...\"\n",
        "sleep 2\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 7: Assigning slots to master nodes...\"\n",
        "echo \"  Assigning slots 0-5460 to node 1...\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTS $(seq 0 5460)\n",
        "\n",
        "echo \"  Assigning slots 5461-10922 to node 2...\"\n",
        "redis-cli -h 127.0.0.1 -p 6380 CLUSTER ADDSLOTS $(seq 5461 10922)\n",
        "\n",
        "echo \"  Assigning slots 10923-16383 to node 3...\"\n",
        "redis-cli -h 127.0.0.1 -p 6381 CLUSTER ADDSLOTS $(seq 10923 16383)\n",
        "\n",
        "echo \"\"\n",
        "echo \"Step 8: Setting up replication (nodes 4-6 as replicas)...\"\n",
        "echo \"  Node 4 replicating node 1...\"\n",
        "redis-cli -h 127.0.0.1 -p 6382 CLUSTER REPLICATE $NODE1_ID\n",
        "\n",
        "echo \"  Node 5 replicating node 2...\"\n",
        "redis-cli -h 127.0.0.1 -p 6383 CLUSTER REPLICATE $NODE2_ID\n",
        "\n",
        "echo \"  Node 6 replicating node 3...\"\n",
        "redis-cli -h 127.0.0.1 -p 6384 CLUSTER REPLICATE $NODE3_ID\n",
        "\n",
        "echo \"\"\n",
        "echo \"================================\"\n",
        "echo \"✅ Cluster initialization complete!\"\n",
        "echo \"================================\"\n",
        "echo \"\"\n",
        "echo \"Cluster Status:\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER INFO\n",
        "echo \"\"\n",
        "echo \"Cluster Nodes:\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER NODES\n",
        "echo \"\"\n",
        "echo \"MetaRaft Members:\"\n",
        "redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS\n",
        "echo \"\"\n",
        "echo \"You can now connect with: redis-cli -c -h 127.0.0.1 -p 6379\"\n",
    );
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
    "# AiKv Single Node Deployment

This directory contains the deployment files for a single-node AiKv instance.

## Prerequisites

- Docker and Docker Compose installed
- AiKv Docker image built (docker build -t aikv:latest . from AiKv project root)

## Files

| File | Description |
|------|-------------|
| docker-compose.yml | Docker Compose configuration |
| aikv.toml | AiKv configuration file |
| start.sh | Start script |
| stop.sh | Stop script |

## Quick Start

Start AiKv: ./start.sh
Or manually: docker-compose up -d

## Connecting

Using redis-cli:
  redis-cli -h 127.0.0.1 -p 6379

Test connection:
  redis-cli PING

## Configuration

Edit aikv.toml to customize:

- Storage Engine: memory (fast) or aidb (persistent)
- Port: Default 6379
- Log Level: trace, debug, info, warn, error

## Monitoring

View logs: docker-compose logs -f
Check status: docker-compose ps

## Stopping

Stop: ./stop.sh
Or manually: docker-compose down
Remove data volumes: docker-compose down -v
"
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
      - "50051:50051"
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
      - "50052:50052"
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
      - "50053:50053"
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
      - "50054:50054"
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
      - "50055:50055"
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
      - "50056:50056"
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
    // Use container hostname for Docker deployments
    let raft_address = format!("aikv{}:{}", node_num, raft_port);
    
    format!(
        r#"# AiKv Cluster Node {} Configuration
# Generated by aikv-tool

[server]
host = "0.0.0.0"
port = {}

[cluster]
enabled = true
raft_address = "{}"
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
        node_num, port, raft_address, is_bootstrap, storage_engine
    )
}

fn generate_cluster_readme() -> String {
    "# AiKv Cluster Deployment

This directory contains deployment files for a 6-node AiKv cluster (3 masters, 3 replicas).

## Architecture

- **Node 1-3**: Master nodes with MetaRaft voters
- **Node 4-6**: Replica nodes
- **MetaRaft**: Distributed consensus for cluster metadata
- **Slot Distribution**: 16384 slots evenly distributed across 3 masters

## Prerequisites

- Docker and Docker Compose installed
- AiKv cluster Docker image built

## Files

| File | Description |
|------|-------------|
| docker-compose.yml | Cluster Docker Compose configuration |
| aikv-node[1-6].toml | Per-node configuration files |
| start.sh | Start cluster script |
| stop.sh | Stop cluster script |
| init-cluster.sh | Initialize cluster with dynamic MetaRaft membership |

## Quick Start

1. Start all nodes:
   ./start.sh

2. Wait for nodes to be ready (~10 seconds)

3. Initialize cluster with dynamic MetaRaft membership:
   ./init-cluster.sh

## Initialization Process

The init-cluster.sh script uses the new dynamic MetaRaft membership approach:

1. **Wait for all nodes**: Ensures all 6 nodes are running and healthy
2. **Bootstrap verification**: Confirms node 1 is initialized as single-node MetaRaft cluster
3. **Add learners**: Adds nodes 2 and 3 as MetaRaft learners
4. **Promote to voters**: Promotes learners to voting members using Joint Consensus
5. **Cluster metadata**: Uses CLUSTER MEET to add all nodes to cluster metadata
6. **Slot assignment**: Distributes 16384 slots evenly across 3 masters
7. **Replication setup**: Configures nodes 4-6 as replicas

### Why Dynamic Membership?

This cluster uses **dynamic MetaRaft membership** instead of pre-configured peer lists:

- **No simultaneous startup required**: Nodes can join incrementally
- **Zero-downtime changes**: Uses OpenRaft Joint Consensus
- **Flexible scaling**: Easy to add/remove MetaRaft voters at runtime

See docs/METARAFT_DYNAMIC_MEMBERSHIP.md for details.

## Cluster Operations

### Check cluster status
redis-cli -c -h 127.0.0.1 -p 6379 CLUSTER INFO
redis-cli -c -h 127.0.0.1 -p 6379 CLUSTER NODES

### Check MetaRaft membership
redis-cli -c -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS

### Connect to cluster
redis-cli -c -h 127.0.0.1 -p 6379

## Port Mapping

| Node | Redis Port | Raft Port |
|------|------------|-----------|
| aikv1 | 6379 | 50051 |
| aikv2 | 6380 | 50052 |
| aikv3 | 6381 | 50053 |
| aikv4 | 6382 | 50054 |
| aikv5 | 6383 | 50055 |
| aikv6 | 6384 | 50056 |

## Troubleshooting

If init-cluster.sh fails:

1. Check all nodes are running: docker-compose ps
2. Check node logs: docker-compose logs aikv1
3. Reset and try again:
   ./stop.sh
   docker-compose down -v
   ./start.sh
   sleep 10
   ./init-cluster.sh

## Additional Resources

- AiKv Documentation: ../../README.md
- Cluster Architecture: ../../docs/ARCHITECTURE.md
- MetaRaft Dynamic Membership: ../../docs/METARAFT_DYNAMIC_MEMBERSHIP.md
- Cluster API Reference: ../../docs/AIDB_CLUSTER_API_REFERENCE.md
".to_string()
}
