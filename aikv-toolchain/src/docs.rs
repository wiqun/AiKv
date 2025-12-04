//! Documentation module - Display documentation and optimization suggestions

use anyhow::Result;

/// Show optimization suggestions
pub fn show_optimization_suggestions() -> Result<()> {
    println!("{}", get_optimization_text());
    Ok(())
}

/// Show documentation
pub fn show_documentation(topic: Option<&str>) -> Result<()> {
    match topic {
        Some("api") => println!("{}", get_api_docs()),
        Some("deploy") => println!("{}", get_deploy_docs()),
        Some("performance") => println!("{}", get_performance_docs()),
        Some("cluster") => println!("{}", get_cluster_docs()),
        _ => println!("{}", get_documentation_text()),
    }
    Ok(())
}

/// Get optimization suggestions text
pub fn get_optimization_text() -> String {
    r#"🚀 AiKv Optimization Suggestions
=================================

System Level Optimizations
--------------------------

1. File Descriptor Limits
   Increase for high connection counts:
   
   echo "* soft nofile 65535" >> /etc/security/limits.conf
   echo "* hard nofile 65535" >> /etc/security/limits.conf

2. TCP Tuning
   Optimize for high throughput:
   
   sysctl -w net.ipv4.tcp_tw_reuse=1
   sysctl -w net.core.somaxconn=65535
   sysctl -w net.ipv4.tcp_max_syn_backlog=65535
   sysctl -w net.core.netdev_max_backlog=65535

3. Memory Settings
   For persistent storage with aidb:
   
   sysctl -w vm.swappiness=1
   sysctl -w vm.dirty_ratio=40
   sysctl -w vm.dirty_background_ratio=10

Storage Engine Selection
------------------------

Memory Engine (engine = "memory")
  ✅ Best for: Caching, development, testing
  ✅ Performance: Highest (pure memory operations)
  ❌ Persistence: None (data lost on restart)
  ❌ Not recommended for: Production data storage

AiDb Engine (engine = "aidb")
  ✅ Best for: Production, data persistence required
  ✅ Persistence: WAL + SSTable with Bloom Filters
  ✅ Compression: Snappy compression enabled
  ⚠️  Performance: Slightly lower than memory (disk I/O)

Recommendation:
  • Development/Testing: memory
  • Production (caching): memory with backup strategy
  • Production (data): aidb

Connection Optimization
-----------------------

1. Use connection pooling in your clients
2. Enable TCP keepalive for long-lived connections
3. Consider pipelining for batch operations:
   
   redis-cli PING
   # vs
   echo -e "PING\nPING\nPING" | redis-cli --pipe

Command Optimization
--------------------

1. Use batch commands when possible:
   • MSET instead of multiple SET
   • MGET instead of multiple GET
   • Pipeline multiple commands together

2. Use hash tags for cluster mode:
   • Keys with same tag go to same slot
   • Example: {user:1000}:name, {user:1000}:age

3. Avoid KEYS in production:
   • Use SCAN for iterating
   • Use proper data structures

Monitoring Recommendations
--------------------------

1. Enable slowlog:
   CONFIG SET slowlog-log-slower-than 10000
   CONFIG SET slowlog-max-len 128
   SLOWLOG GET 10

2. Monitor with INFO command:
   INFO stats
   INFO memory
   INFO clients

3. Use MONITOR for debugging (not production):
   MONITOR

Cluster Optimization
--------------------

1. Balance slots evenly across masters
2. Use hash tags for related keys
3. Monitor replication lag
4. Configure appropriate failover timeouts

Benchmark Commands
------------------

# Quick benchmark
redis-benchmark -t set,get -n 100000 -q

# With pipelining
redis-benchmark -t set,get -n 100000 -P 16 -q

# Cluster mode
redis-benchmark -c -t set,get -n 100000 -q

Press q/Esc to return"#
        .to_string()
}

/// Get general documentation text
pub fn get_documentation_text() -> String {
    r#"📖 AiKv Documentation
=====================

Overview
--------
AiKv is a Redis protocol compatible key-value store built on AiDb.

Key Features:
• 100+ Redis commands supported
• RESP2 and RESP3 protocol support
• Memory and persistent storage engines
• Cluster mode support (90% complete)
• Lua scripting with transaction rollback
• JSON data type support

Quick Start
-----------
# Build
cargo build --release

# Run
./target/release/aikv

# Connect
redis-cli -h 127.0.0.1 -p 6379

Supported Commands
------------------

Protocol:
  PING, HELLO, ECHO

String:
  GET, SET, DEL, EXISTS, MGET, MSET, STRLEN, APPEND

JSON (7 commands):
  JSON.GET, JSON.SET, JSON.DEL, JSON.TYPE, JSON.STRLEN,
  JSON.ARRLEN, JSON.OBJLEN

List (10 commands):
  LPUSH, RPUSH, LPOP, RPOP, LLEN, LRANGE, LINDEX,
  LSET, LREM, LTRIM

Hash (12 commands):
  HSET, HSETNX, HGET, HMGET, HDEL, HEXISTS, HLEN,
  HKEYS, HVALS, HGETALL, HINCRBY, HINCRBYFLOAT

Set (13 commands):
  SADD, SREM, SISMEMBER, SMEMBERS, SCARD, SPOP,
  SRANDMEMBER, SUNION, SINTER, SDIFF,
  SUNIONSTORE, SINTERSTORE, SDIFFSTORE

Sorted Set (12 commands):
  ZADD, ZREM, ZSCORE, ZRANK, ZREVRANK, ZRANGE,
  ZREVRANGE, ZRANGEBYSCORE, ZREVRANGEBYSCORE,
  ZCARD, ZCOUNT, ZINCRBY

Database:
  SELECT, DBSIZE, FLUSHDB, FLUSHALL, SWAPDB, MOVE

Key Management (17 commands):
  KEYS, SCAN, RANDOMKEY, RENAME, RENAMENX, TYPE, COPY,
  EXPIRE, EXPIREAT, PEXPIRE, PEXPIREAT, TTL, PTTL,
  PERSIST, EXPIRETIME, PEXPIRETIME

Server:
  INFO, TIME, CONFIG GET/SET, CLIENT LIST/SETNAME/GETNAME,
  MONITOR

Lua Scripting:
  EVAL, EVALSHA, SCRIPT LOAD/EXISTS/FLUSH/KILL

Cluster (17 commands):
  CLUSTER INFO/NODES/SLOTS/MYID/KEYSLOT
  CLUSTER MEET/FORGET
  CLUSTER ADDSLOTS/DELSLOTS/SETSLOT
  CLUSTER GETKEYSINSLOT/COUNTKEYSINSLOT
  CLUSTER REPLICATE/FAILOVER/REPLICAS
  READONLY/READWRITE

Documentation Topics
--------------------
Run with --topic for specific documentation:

  aikv-tool docs --topic api         # API reference
  aikv-tool docs --topic deploy      # Deployment guide
  aikv-tool docs --topic performance # Performance tuning
  aikv-tool docs --topic cluster     # Cluster operations

Press q/Esc to return, ↑/↓ to scroll"#
        .to_string()
}

fn get_api_docs() -> String {
    r#"📖 AiKv API Documentation
=========================

Connection
----------
PING [message]
  Returns PONG or the message if provided

HELLO [protover [AUTH username password] [SETNAME clientname]]
  Switch protocol version (2 or 3)
  Returns server information

ECHO message
  Returns the message

String Commands
---------------
SET key value [EX seconds] [PX ms] [NX|XX]
  Set key to value
  EX: expiration in seconds
  PX: expiration in milliseconds
  NX: only set if key doesn't exist
  XX: only set if key exists

GET key
  Get value of key

DEL key [key ...]
  Delete one or more keys

EXISTS key [key ...]
  Check if keys exist

MGET key [key ...]
  Get multiple values

MSET key value [key value ...]
  Set multiple key-value pairs

STRLEN key
  Get string length

APPEND key value
  Append value to existing string

JSON Commands
-------------
JSON.SET key path value
  Set JSON value at path

JSON.GET key [path ...]
  Get JSON value(s) at path(s)

JSON.DEL key [path]
  Delete JSON value at path

JSON.TYPE key [path]
  Get JSON type at path

JSON.STRLEN key [path]
  Get string length at path

JSON.ARRLEN key [path]
  Get array length at path

JSON.OBJLEN key [path]
  Get object key count at path

Hash Commands
-------------
HSET key field value [field value ...]
  Set field(s) in hash

HGET key field
  Get field value

HMGET key field [field ...]
  Get multiple field values

HDEL key field [field ...]
  Delete field(s)

HGETALL key
  Get all fields and values

HKEYS key
  Get all field names

HVALS key
  Get all values

... (see full API.md for complete reference)
"#
    .to_string()
}

fn get_deploy_docs() -> String {
    r#"📖 AiKv Deployment Guide
========================

Requirements
------------
• Rust 1.70+ (for building)
• Docker (for containerized deployment)
• 512MB+ RAM (recommended: 2GB+)
• SSD recommended for aidb storage engine

Building
--------
# Debug build
cargo build

# Release build
cargo build --release

# Cluster feature
cargo build --release --features cluster

Docker
------
# Build image
docker build -t aikv:latest .

# Build cluster image
docker build -t aikv:cluster --build-arg FEATURES=cluster .

# Run container
docker run -d -p 6379:6379 aikv:latest

# Run with persistent data
docker run -d -p 6379:6379 \
  -v $(pwd)/data:/app/data \
  aikv:latest

Docker Compose
--------------
# Single node
docker-compose up -d

# Cluster (6 nodes)
docker-compose -f docker-compose.cluster.yml up -d

Systemd Service
---------------
Create /etc/systemd/system/aikv.service:

[Unit]
Description=AiKv Server
After=network.target

[Service]
Type=simple
User=aikv
WorkingDirectory=/opt/aikv
ExecStart=/opt/aikv/aikv --config /opt/aikv/config.toml
Restart=on-failure
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target

Commands:
  systemctl start aikv
  systemctl enable aikv
  systemctl status aikv

Configuration
-------------
See: aikv-tool config
Or: config/aikv.toml for template

Security
--------
• Run as non-root user
• Use firewall to restrict access
• Enable TLS (future feature)
• Set authentication (future feature)
"#
    .to_string()
}

fn get_performance_docs() -> String {
    r#"📖 AiKv Performance Guide
=========================

Benchmarks
----------
Current performance targets:
  SET: ~80,000 ops/s
  GET: ~100,000 ops/s
  LPUSH: ~75,000 ops/s
  HSET: ~70,000 ops/s

Latency targets:
  P50: < 1ms
  P99: < 5ms
  P99.9: < 10ms

Running Benchmarks
------------------
# Quick benchmark
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -q

# With pipelining (higher throughput)
redis-benchmark -h 127.0.0.1 -p 6379 -t set,get -n 100000 -P 16 -q

# All commands
redis-benchmark -h 127.0.0.1 -p 6379 -n 100000 -q

# Cargo benchmarks
cargo bench

Tuning Tips
-----------
1. Use release builds (cargo build --release)
2. Choose appropriate storage engine:
   - memory: best performance, no persistence
   - aidb: good performance with persistence
3. Enable pipelining in clients
4. Use MGET/MSET for batch operations
5. Tune system limits (see optimize command)

Memory Usage
------------
Base memory: ~50MB
Per connection: ~1KB
Per key (estimate): key_size + value_size + ~50 bytes

Monitoring
----------
INFO memory         # Memory stats
INFO stats          # Command stats
INFO clients        # Client info
SLOWLOG GET 10      # Slow queries
"#
    .to_string()
}

fn get_cluster_docs() -> String {
    r#"📖 AiKv Cluster Guide
=====================

Overview
--------
AiKv supports Redis Cluster protocol with:
• 16384 hash slots
• CRC16 slot calculation (Redis compatible)
• -MOVED/-ASK redirections
• Online slot migration
• Replica support

Cluster Status: 90% complete

Building for Cluster
--------------------
cargo build --release --features cluster

Deployment (6 nodes)
--------------------
docker-compose -f docker-compose.cluster.yml up -d

Initialize Cluster
------------------
redis-cli --cluster create \
  127.0.0.1:6379 127.0.0.1:6380 127.0.0.1:6381 \
  127.0.0.1:6382 127.0.0.1:6383 127.0.0.1:6384 \
  --cluster-replicas 1

Cluster Commands
----------------
# Information
CLUSTER INFO          # Cluster state
CLUSTER NODES         # All nodes
CLUSTER SLOTS         # Slot distribution
CLUSTER MYID          # Current node ID

# Key operations
CLUSTER KEYSLOT key   # Get slot for key

# Node management
CLUSTER MEET ip port  # Add node
CLUSTER FORGET id     # Remove node

# Slot management
CLUSTER ADDSLOTS slot [slot ...]
CLUSTER DELSLOTS slot [slot ...]
CLUSTER SETSLOT slot MIGRATING|IMPORTING|STABLE|NODE

# Replication
CLUSTER REPLICATE node-id
CLUSTER FAILOVER [FORCE]
CLUSTER REPLICAS node-id

# Read mode
READONLY              # Enable reads on replica
READWRITE             # Disable reads on replica

Hash Tags
---------
Use hash tags to ensure related keys go to same slot:
  SET {user:1000}:name "John"
  SET {user:1000}:age "30"
  MGET {user:1000}:name {user:1000}:age

Slot Migration
--------------
# Get keys in slot
CLUSTER GETKEYSINSLOT 5000 10

# Start migration
CLUSTER SETSLOT 5000 MIGRATING target-id
CLUSTER SETSLOT 5000 IMPORTING source-id

# Finish migration
CLUSTER SETSLOT 5000 NODE target-id

Failover
--------
# On replica node
CLUSTER FAILOVER

# Force (when master is down)
CLUSTER FAILOVER FORCE
"#
    .to_string()
}
