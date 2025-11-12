# Persistence Module

The persistence module provides RDB (Redis Database) snapshot and AOF (Append-Only File) log persistence for AiKv.

## Features

- **RDB Snapshots**: Point-in-time snapshots of the database
- **AOF Logs**: Command logging for durability
- **Configurable**: Flexible persistence configuration options

## RDB Persistence

RDB creates point-in-time snapshots of your database.

### Example: Saving to RDB

```rust
use aikv::persistence::{save_rdb, DatabaseData};
use bytes::Bytes;
use std::collections::HashMap;

// Create database data
let mut db0 = HashMap::new();
db0.insert("key1".to_string(), (Bytes::from("value1"), None));
db0.insert("key2".to_string(), (Bytes::from("value2"), Some(9999999999999)));

let databases = vec![db0];

// Save to RDB file
save_rdb("dump.rdb", &databases)?;
```

### Example: Loading from RDB

```rust
use aikv::persistence::load_rdb;

// Load from RDB file
let databases = load_rdb("dump.rdb")?;

for (db_index, db_data) in databases.iter().enumerate() {
    println!("Database {}: {} keys", db_index, db_data.len());
}
```

## AOF Persistence

AOF logs every write command, providing better durability.

### Example: Writing Commands to AOF

```rust
use aikv::persistence::{AofWriter, AofSyncPolicy};

// Create AOF writer
let writer = AofWriter::new("appendonly.aof", AofSyncPolicy::EverySecond)?;

// Log commands
writer.log_command(&["SET".to_string(), "key1".to_string(), "value1".to_string()])?;
writer.log_command(&["SET".to_string(), "key2".to_string(), "value2".to_string()])?;
writer.log_command(&["DEL".to_string(), "key1".to_string()])?;
```

### Example: Reading Commands from AOF

```rust
use aikv::persistence::load_aof;

// Load all commands from AOF
let commands = load_aof("appendonly.aof")?;

for command in commands {
    println!("Command: {:?}", command);
}
```

## Configuration

### Persistence Configuration

```rust
use aikv::persistence::{PersistenceConfig, AofSyncPolicy};
use std::path::PathBuf;

let config = PersistenceConfig {
    enable_rdb: true,
    rdb_path: PathBuf::from("dump.rdb"),
    rdb_save_interval: 300, // Save every 5 minutes
    
    enable_aof: true,
    aof_path: PathBuf::from("appendonly.aof"),
    aof_sync_policy: AofSyncPolicy::EverySecond,
};
```

### AOF Sync Policies

- **Always**: Sync on every write (safest, slowest)
- **EverySecond**: Sync every second (balanced)
- **No**: Let OS decide (fastest, least safe)

## RDB Format

The RDB format is compatible with Redis RDB format (simplified version):

- Magic string: "REDIS"
- Version: "0001"
- Metadata (auxiliary fields)
- Database sections with key-value pairs
- EOF marker with checksum

## AOF Format

The AOF format uses RESP (Redis Serialization Protocol):

```
*3\r\n
$3\r\n
SET\r\n
$4\r\n
key1\r\n
$6\r\n
value1\r\n
```

## Error Handling

All persistence operations return `Result<T>` with `AikvError::Persistence` for errors:

```rust
match save_rdb("dump.rdb", &databases) {
    Ok(()) => println!("Saved successfully"),
    Err(e) => eprintln!("Save failed: {}", e),
}
```

## Testing

The module includes comprehensive unit tests:

```bash
cargo test persistence
```
