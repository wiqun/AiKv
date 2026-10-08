//! 存储抽象层 (Phase 9–10)

pub mod adapter;
pub mod aidb;
pub mod aidb_options;
#[cfg(feature = "cluster")]
pub mod cluster_adapter;
#[cfg(feature = "cluster")]
mod cluster_batcher;
pub mod dump;
pub mod memory;
pub mod observation;
pub mod scan_cursor;
pub mod subkey;
pub mod ttl_filter;
pub mod types;
pub mod watch_registry;
pub mod watch_version;

pub use adapter::{AdapterWriteOp, KvStorageAdapter, StorageAdapter};
pub use aidb::AiDbEngine;
pub use aidb_options::{
    server_db_options, server_db_options_with_preset, testing_db_options, DbPreset,
};
pub use dump::{decode as dump_decode, encode as dump_encode, DUMP_VERSION};
pub use memory::MemoryEngine;
pub use observation::StorageObservation;
pub use types::{
    is_wrongtype, now_ms, CollectionKind, KeyspaceStats, KvStorage, ScanResult, StorageEngineKind,
    StoredValue, ValueType, WriteOp, TTL_NO_EXPIRY, WRONGTYPE,
};

pub use ttl_filter::TtlExpireFilter;
pub use watch_registry::WatchRegistry;
