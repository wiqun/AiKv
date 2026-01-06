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

    let init_script = r#"#!/bin/bash
# Initialize AiKv cluster with dynamic MetaRaft membership
# Uses the new learner → voter promotion workflow

set -e

echo "================================"
echo "AiKv Cluster Initialization"
echo "================================"
echo ""

# Wait for all nodes to be ready
echo "Step 1: Waiting for all nodes to be ready..."
for i in 1 2 3 4 5 6; do
    port=$((6378 + i))
    echo "  Checking node $i (port $port)..."
    for retry in {1..30}; do
        if redis-cli -h 127.0.0.1 -p $port PING >/dev/null 2>&1; then
            echo "  ✓ Node $i is ready"
            break
        fi
        if [ $retry -eq 30 ]; then
            echo "  ✗ Node $i failed to start"
            exit 1
        fi
        sleep 1
    done
done

echo ""
echo "Step 2: Getting node IDs from each node..."
NODE1_ID=$(redis-cli -h 127.0.0.1 -p 6379 CLUSTER MYID)
NODE2_ID=$(redis-cli -h 127.0.0.1 -p 6380 CLUSTER MYID)
NODE3_ID=$(redis-cli -h 127.0.0.1 -p 6381 CLUSTER MYID)
echo "  Node 1 ID: $NODE1_ID"
echo "  Node 2 ID: $NODE2_ID"
echo "  Node 3 ID: $NODE3_ID"

echo ""
echo "Step 3: Adding nodes 2 and 3 as MetaRaft learners..."
echo "  Adding node 2 (ID: $NODE2_ID)..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER 2 aikv2:50052

echo "  Adding node 3 (ID: $NODE3_ID)..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT ADDLEARNER 3 aikv3:50053

echo "  Waiting for learners to sync logs..."
sleep 3

echo ""
echo "Step 4: Promoting learners to voters..."
echo "  Promoting nodes 2 and 3 to voters (node 1 is already a voter)..."
PROMOTE_RETRIES=12
PROMOTE_ATTEMPT=0
while [ $PROMOTE_ATTEMPT -lt $PROMOTE_RETRIES ]; do
    # Only promote nodes 2 and 3, node 1 is already a voter (bootstrap)
    PROMOTE_OUTPUT=$(redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT PROMOTE 2 3 2>&1) || true
    if echo "$PROMOTE_OUTPUT" | grep -qi "ok"; then
        echo "  ✓ Promoted learners to voters"
        break
    fi
    if echo "$PROMOTE_OUTPUT" | grep -qi "InProgress\|Unreachable"; then
        echo "  Promote attempt $((PROMOTE_ATTEMPT+1)) failed (in progress or unreachable). Retrying..."
        PROMOTE_ATTEMPT=$((PROMOTE_ATTEMPT+1))
        sleep 5
        continue
    fi
    if [ -z "$PROMOTE_OUTPUT" ]; then
        echo "  Promote attempt $((PROMOTE_ATTEMPT+1)) produced no immediate response. Retrying..."
        PROMOTE_ATTEMPT=$((PROMOTE_ATTEMPT+1))
        sleep 5
        continue
    fi
    echo "  ✗ Promote failed: $PROMOTE_OUTPUT"
    exit 1
done

if [ $PROMOTE_ATTEMPT -ge $PROMOTE_RETRIES ]; then
    echo "  ✗ Promote failed after retries"
    exit 1
fi

echo "  Waiting for membership change to complete..."
sleep 2

echo ""
echo "Step 5: Verifying MetaRaft membership..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS

echo ""
echo "Step 6: Adding nodes to cluster metadata..."
echo "  Meeting node 2..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6380 $NODE2_ID

echo "  Meeting node 3..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6381 $NODE3_ID

echo "  Meeting node 4..."
NODE4_ID=$(redis-cli -h 127.0.0.1 -p 6382 CLUSTER MYID)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6382 $NODE4_ID

echo "  Meeting node 5..."
NODE5_ID=$(redis-cli -h 127.0.0.1 -p 6383 CLUSTER MYID)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6383 $NODE5_ID

echo "  Meeting node 6..."
NODE6_ID=$(redis-cli -h 127.0.0.1 -p 6384 CLUSTER MYID)
redis-cli -h 127.0.0.1 -p 6379 CLUSTER MEET 127.0.0.1 6384 $NODE6_ID

echo "  Waiting for cluster metadata to sync..."
sleep 2

echo ""
echo "Step 7: Assigning slots to master nodes..."
echo "  Assigning slots 0-5460 to node 1..."
redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 0 5460

echo "  Assigning slots 5461-10922 to node 2..."
# Send to leader (node 1) to assign slots to node 2
redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 5461 10922 $NODE2_ID

echo "  Assigning slots 10923-16383 to node 3..."
# Send to leader (node 1) to assign slots to node 3
redis-cli -h 127.0.0.1 -p 6379 CLUSTER ADDSLOTSRANGE 10923 16383 $NODE3_ID

echo "  Waiting for slot assignment to sync..."
sleep 2

echo ""
echo "Step 8: Setting up replication (nodes 4-6 as replicas)..."
echo "  Node 4 replicating node 1..."
if redis-cli -h 127.0.0.1 -p 6382 CLUSTER REPLICATE $NODE1_ID 2>&1 | grep -qi "ok"; then
    echo "  ✓ Node 4 is now a replica of node 1"
else
    echo "  ⚠ Replication setup for node 4 needs attention (cluster still functional)"
fi

echo "  Node 5 replicating node 2..."
if redis-cli -h 127.0.0.1 -p 6383 CLUSTER REPLICATE $NODE2_ID 2>&1 | grep -qi "ok"; then
    echo "  ✓ Node 5 is now a replica of node 2"
else
    echo "  ⚠ Replication setup for node 5 needs attention (cluster still functional)"
fi

echo "  Node 6 replicating node 3..."
if redis-cli -h 127.0.0.1 -p 6384 CLUSTER REPLICATE $NODE3_ID 2>&1 | grep -qi "ok"; then
    echo "  ✓ Node 6 is now a replica of node 3"
else
    echo "  ⚠ Replication setup for node 6 needs attention (cluster still functional)"
fi

echo ""
echo "================================"
echo "✅ Cluster initialization complete!"
echo "================================"
echo ""
echo "Cluster Status:"
redis-cli -h 127.0.0.1 -p 6379 CLUSTER INFO
echo ""
echo "Cluster Nodes:"
redis-cli -h 127.0.0.1 -p 6379 CLUSTER NODES
echo ""
echo "MetaRaft Members:"
redis-cli -h 127.0.0.1 -p 6379 CLUSTER METARAFT MEMBERS
echo ""
echo "You can now connect with: redis-cli -c -h 127.0.0.1 -p 6379"
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
      test: ["CMD", "redis-cli", "PING"]
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
      test: ["CMD", "redis-cli", "PING"]
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
      test: ["CMD", "redis-cli", "-p", "6380", "PING"]
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
      test: ["CMD", "redis-cli", "-p", "6381", "PING"]
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
      test: ["CMD", "redis-cli", "-p", "6382", "PING"]
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
      test: ["CMD", "redis-cli", "-p", "6383", "PING"]
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
      test: ["CMD", "redis-cli", "-p", "6384", "PING"]
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
