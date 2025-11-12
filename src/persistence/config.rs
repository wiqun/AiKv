use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistence configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceConfig {
    /// Enable RDB persistence
    pub enable_rdb: bool,
    /// RDB file path
    pub rdb_path: PathBuf,
    /// RDB save interval in seconds (0 to disable automatic saves)
    pub rdb_save_interval: u64,

    /// Enable AOF persistence
    pub enable_aof: bool,
    /// AOF file path
    pub aof_path: PathBuf,
    /// AOF sync policy
    pub aof_sync_policy: AofSyncPolicy,
}

/// AOF sync policy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AofSyncPolicy {
    /// Sync every write (safest, slowest)
    Always,
    /// Sync every second (balanced)
    EverySecond,
    /// Let OS decide when to sync (fastest, least safe)
    No,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enable_rdb: true,
            rdb_path: PathBuf::from("dump.rdb"),
            rdb_save_interval: 300, // 5 minutes

            enable_aof: false,
            aof_path: PathBuf::from("appendonly.aof"),
            aof_sync_policy: AofSyncPolicy::EverySecond,
        }
    }
}
