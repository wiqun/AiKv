//! Configuration module - Display configuration documentation

use anyhow::Result;

/// Show configuration documentation
pub fn show_config(cluster: bool) -> Result<()> {
    if cluster {
        println!("{}", get_cluster_config_docs());
    } else {
        println!("{}", get_single_config_docs());
    }
    Ok(())
}

/// Get single node configuration documentation
pub fn get_single_config_docs() -> String {
    r#"AiKv Single Node Configuration Guide
=====================================

Configuration file: config/aikv.toml or custom path via --config

[server] Section
----------------
✅ host = "127.0.0.1"
   Description: Server bind address
   Values: IP address or hostname
   Default: 127.0.0.1
   Example: 0.0.0.0 for all interfaces

✅ port = 6379
   Description: Server port
   Values: 1-65535
   Default: 6379

🚧 max_connections = 10000
   Description: Maximum concurrent connections
   Status: Planned

🚧 connection_timeout = 300
   Description: Connection timeout in seconds
   Status: Planned

[storage] Section
-----------------
✅ engine = "memory"
   Description: Storage engine type
   Values: "memory" or "aidb"
   - memory: High performance, no persistence
   - aidb: LSM-Tree persistent storage with WAL

✅ data_dir = "./data"
   Description: Data storage directory
   Required: Only for aidb engine

✅ databases = 16
   Description: Number of databases (0-15)
   Default: 16

🚧 max_memory = "1GB"
   Description: Maximum memory usage
   Status: Planned

[logging] Section
-----------------
✅ level = "info"
   Description: Log level
   Values: trace, debug, info, warn, error
   Can be changed dynamically via:
   CONFIG SET loglevel <level>

🚧 file = "./logs/aikv.log"
   Description: Log file path
   Status: Planned

🚧 format = "text"
   Description: Log format (text or json)
   Status: Planned

[slowlog] Section
-----------------
✅ log-slower-than = 10000
   Description: Slow query threshold in microseconds
   Can be changed via: CONFIG SET slowlog-log-slower-than <us>

✅ max-len = 128
   Description: Maximum slow log entries
   Can be changed via: CONFIG SET slowlog-max-len <len>

Example Configuration
---------------------
[server]
host = "0.0.0.0"
port = 6379

[storage]
engine = "memory"
data_dir = "./data"
databases = 16

[logging]
level = "info"

[slowlog]
log-slower-than = 10000
max-len = 128

Usage
-----
# Start with configuration file
./aikv --config config.toml

# Start with command line options
./aikv --host 0.0.0.0 --port 6379

Press q/Esc to return, c to toggle cluster mode"#
        .to_string()
}

/// Get cluster configuration documentation
pub fn get_cluster_config_docs() -> String {
    r#"AiKv Cluster Configuration Guide
=================================

Configuration file: config/aikv-cluster.toml

Build with: cargo build --release --features cluster

[server] Section
----------------
✅ host = "0.0.0.0"
   Description: Server bind address
   Recommendation: Use 0.0.0.0 for cluster mode

✅ port = 6379
   Description: Data port

[cluster] Section
-----------------
✅ enabled = true
   Description: Enable cluster mode
   Status: Implemented

✅ raft_address = "127.0.0.1:50051"
   Description: Raft RPC address (gRPC) for cluster communication
   Note: Each node must use a unique port
   Status: Implemented

✅ is_bootstrap = false
   Description: Whether this is the bootstrap node (first node in cluster)
   Note: Set to true for the first node only
   Status: Implemented

[storage] Section
-----------------
✅ engine = "aidb"
   Description: Storage engine
   Recommendation: Use aidb for cluster mode

✅ data_dir = "./data"
   Description: Data directory

[raft] Section (Future)
-----------------------
🚧 heartbeat_interval = 100
   Description: Heartbeat interval in ms
   Status: Planned (uses openraft defaults)

🚧 election_timeout_min = 300
🚧 election_timeout_max = 500
   Description: Election timeout range in ms

🚧 snapshot_interval = 10000
   Description: Snapshot interval (log entries)

[migration] Section (Planned)
-----------------------------
🚧 batch_size = 100
   Description: Migration batch size (keys per batch)

🚧 concurrency = 4
   Description: Migration concurrency

🚧 timeout = 300
   Description: Migration timeout in seconds

[failover] Section (Planned)
----------------------------
🚧 node_timeout = 15000
   Description: Node timeout in ms

🚧 failover_auth_timeout = 5000
   Description: Failover auth timeout in ms

🚧 require_majority = true
   Description: Require majority for failover

Docker Compose Cluster Deployment
---------------------------------
# Start 6-node cluster (3 masters, 3 replicas)
docker-compose -f docker-compose.cluster.yml up -d

# Initialize cluster
redis-cli --cluster create \
  127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 \
  127.0.0.1:6382 127.0.0.1:6383 127.0.0.1:6384 \
  --cluster-replicas 1

# Check cluster status
redis-cli -c -p 6379 CLUSTER INFO
redis-cli -c -p 6379 CLUSTER NODES

Cluster Commands
----------------
• CLUSTER INFO - Cluster information
• CLUSTER NODES - List all nodes
• CLUSTER SLOTS - Slot distribution
• CLUSTER KEYSLOT <key> - Get key slot
• CLUSTER MEET <ip> <port> - Add node
• CLUSTER REPLICATE <node-id> - Set as replica
• CLUSTER FAILOVER - Manual failover
• READONLY / READWRITE - Read mode

Press q/Esc to return, c to toggle single mode"#
        .to_string()
}
