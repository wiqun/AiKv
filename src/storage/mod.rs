pub mod aidb_adapter;
pub mod memory_adapter;

// Re-export the memory adapter as StorageAdapter for backward compatibility
// In production, you would switch to aidb_adapter::AiDbStorageAdapter
pub use memory_adapter::StorageAdapter;

// Also export the AiDb adapter
pub use aidb_adapter::AiDbStorageAdapter;

// Export the core storage types for command implementations
pub use memory_adapter::{BatchOp, SerializableStoredValue, StoredValue, ValueType};
