//! AiDb Storage Adapter - Persistent storage backend for AiKv
//!
//! This module provides a persistent storage adapter using AiDb as the underlying
//! storage engine. It implements a minimal, orthogonal interface that separates
//! storage concerns from command logic.
//!
//! # Architecture
//!
//! The storage layer follows these principles:
//! - **Minimal Interface**: Only provides basic CRUD operations, not command-specific logic
//! - **Type Agnostic**: All data types (String, List, Hash, Set, ZSet) use the same interface
//! - **Separation of Concerns**: Storage handles persistence, commands handle business logic
//! - **Efficiency**: Uses bincode for fast binary serialization/deserialization
//!
//! # Core Methods
//!
//! - `get_value()` - Retrieve any data type by key
//! - `set_value()` - Store any data type with a key
//! - `update_value()` - Atomically modify a value in-place
//! - `delete_and_get()` - Atomically delete and return a value
//!
//! # Example
//!
//! ```ignore
//! use aikv::storage::AiDbStorageAdapter;
//! use aikv::storage::StoredValue;
//! use bytes::Bytes;
//!
//! let storage = AiDbStorageAdapter::new("/tmp/aikv", 16)?;
//!
//! // Store a string
//! let value = StoredValue::new_string(Bytes::from("hello"));
//! storage.set_value(0, "key1".to_string(), value)?;
//!
//! // Retrieve it
//! if let Some(v) = storage.get_value(0, "key1")? {
//!     println!("Value: {:?}", v.as_string());
//! }
//! ```

use crate::error::{AikvError, Result};
use crate::storage::{SerializableStoredValue, StoredValue};
use aidb::{Options, WriteBatch, DB};
use bytes::Bytes;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

// Re-export BatchOp from memory_adapter for consistency
pub use crate::storage::memory_adapter::BatchOp;

/// AiDb-based storage adapter providing persistent storage for AiKv.
///
/// This adapter uses AiDb as the underlying storage engine and supports
/// multiple databases (default 16, like Redis). Each database is a separate
/// AiDb instance with its own directory for data isolation.
///
/// # Features
///
/// - **Persistent Storage**: All data is persisted to disk via AiDb
/// - **Multi-Database**: Supports multiple logical databases (0-15 by default)
/// - **All Data Types**: Supports String, List, Hash, Set, and ZSet through serialization
/// - **Expiration**: Built-in support for key expiration with automatic cleanup
/// - **Thread-Safe**: Uses Arc for safe sharing across threads
#[derive(Clone)]
pub struct AiDbStorageAdapter {
    /// Multiple databases (default: 16 databases like Redis)
    /// Each database is a separate AiDb instance with its own directory
    databases: Arc<Vec<Arc<DB>>>,
}

impl AiDbStorageAdapter {
    /// Create a new AiDb storage adapter with the given path and database count.
    ///
    /// # Arguments
    /// * `path` - Base directory for storage (subdirectories created for each DB)
    /// * `db_count` - Number of logical databases to create (typically 16)
    ///
    /// # Returns
    /// * `Ok(AiDbStorageAdapter)` - If all databases were successfully opened
    /// * `Err(AikvError)` - If directory creation or database opening fails
    pub fn new<P: AsRef<Path>>(path: P, db_count: usize) -> Result<Self> {
        let base_path = path.as_ref();

        // Create the base directory if it doesn't exist
        if !base_path.exists() {
            std::fs::create_dir_all(base_path)
                .map_err(|e| AikvError::Storage(format!("Failed to create directory: {}", e)))?;
        }

        let mut databases = Vec::with_capacity(db_count);
        for i in 0..db_count {
            let db_path = base_path.join(format!("db{}", i));
            // Use sync_wal(false) for better write performance.
            // The default Options::default() has sync_wal: true, which causes
            // synchronous disk writes for every put operation, resulting in
            // very low write throughput (~155 rps). Setting sync_wal to false
            // trades some durability for significantly better performance.
            // Data is still written to WAL, but fsync is not called on every write.
            let options = Options::default().sync_wal(false);
            let db = DB::open(&db_path, options)
                .map_err(|e| AikvError::Storage(format!("Failed to open database {}: {}", i, e)))?;
            databases.push(Arc::new(db));
        }

        Ok(Self {
            databases: Arc::new(databases),
        })
    }

    /// Get current time in milliseconds
    fn current_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Check if a key is expired based on its stored expiration metadata
    fn is_expired(&self, db: &DB, key: &[u8]) -> Result<bool> {
        let expire_key = Self::expiration_key(key);
        if let Some(expire_bytes) = db
            .get(&expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
        {
            if expire_bytes.len() == 8 {
                let expire_at = u64::from_le_bytes([
                    expire_bytes[0],
                    expire_bytes[1],
                    expire_bytes[2],
                    expire_bytes[3],
                    expire_bytes[4],
                    expire_bytes[5],
                    expire_bytes[6],
                    expire_bytes[7],
                ]);
                let now = Self::current_time_ms();
                return Ok(now >= expire_at);
            }
        }
        Ok(false)
    }

    /// Generate expiration metadata key for a given key
    fn expiration_key(key: &[u8]) -> Vec<u8> {
        let mut expire_key = Vec::with_capacity(key.len() + 8);
        expire_key.extend_from_slice(b"__exp__:");
        expire_key.extend_from_slice(key);
        expire_key
    }

    // ========================================================================
    // CORE STORAGE METHODS (Minimal Interface Post-Refactoring)
    // ========================================================================
    // These methods provide the minimal, orthogonal interface for storage operations.
    // All command-specific logic should be implemented in the command layer.
    // The storage layer only handles:
    // - Basic CRUD operations (get, set, delete)
    // - Database management (flush, swap, size)
    // - Expiration management (as a storage concern)

    /// Get a stored value by key from a specific database.
    ///
    /// This method supports all data types (String, List, Hash, Set, ZSet) through
    /// automatic deserialization. Expired keys are automatically cleaned up and return `None`.
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to retrieve
    ///
    /// # Returns
    /// * `Ok(Some(StoredValue))` - The value if found and not expired
    /// * `Ok(None)` - If the key doesn't exist or has expired
    /// * `Err(AikvError)` - If the database index is invalid or I/O error occurs
    ///
    /// # Example
    /// ```ignore
    /// let value = storage.get_value(0, "mykey")?;
    /// if let Some(v) = value {
    ///     if let Some(s) = v.as_string() {
    ///         println!("String value: {}", String::from_utf8_lossy(s));
    ///     }
    /// }
    /// ```
    pub fn get_value(&self, db_index: usize, key: &str) -> Result<Option<StoredValue>> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // 先尝试读取主键，只有主键存在时才检查过期时间
        // 这样可以避免对不存在的键进行额外的过期检查数据库读取
        match db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to get value: {}", e)))?
        {
            Some(serialized) => {
                // 主键存在，检查是否过期
                if self.is_expired(db, key_bytes)? {
                    // 清理过期键
                    db.delete(key_bytes)
                        .map_err(|e| AikvError::Storage(format!("Failed to delete expired key: {}", e)))?;
                    let expire_key = Self::expiration_key(key_bytes);
                    db.delete(&expire_key)
                        .map_err(|e| AikvError::Storage(format!("Failed to delete expiration: {}", e)))?;
                    return Ok(None);
                }
                // 反序列化并返回
                let serializable: SerializableStoredValue = bincode::deserialize(&serialized)
                    .map_err(|e| {
                        AikvError::Storage(format!("Failed to deserialize value: {}", e))
                    })?;
                Ok(Some(StoredValue::from_serializable(serializable)))
            }
            None => Ok(None),
        }
    }

    /// Set a value for a key in a specific database.
    ///
    /// This method supports all data types (String, List, Hash, Set, ZSet) through
    /// automatic serialization using bincode for efficiency. The value's expiration
    /// metadata (if set) is automatically stored as well.
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to set
    /// * `value` - The StoredValue to store (can be any supported data type)
    ///
    /// # Returns
    /// * `Ok(())` - If the value was successfully stored
    /// * `Err(AikvError)` - If the database index is invalid, serialization fails, or I/O error occurs
    ///
    /// # Example
    /// ```ignore
    /// let value = StoredValue::new_string(Bytes::from("hello"));
    /// storage.set_value(0, "mykey".to_string(), value)?;
    /// ```
    pub fn set_value(&self, db_index: usize, key: String, value: StoredValue) -> Result<()> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Serialize using bincode
        let serializable = value.to_serializable();
        let serialized = bincode::serialize(&serializable)
            .map_err(|e| AikvError::Storage(format!("Failed to serialize value: {}", e)))?;

        // Store the serialized value
        db.put(key_bytes, &serialized)
            .map_err(|e| AikvError::Storage(format!("Failed to put value: {}", e)))?;

        // Handle expiration if set
        if let Some(expires_at) = value.expires_at() {
            let expire_key = Self::expiration_key(key_bytes);
            db.put(&expire_key, &expires_at.to_le_bytes())
                .map_err(|e| AikvError::Storage(format!("Failed to set expiration: {}", e)))?;
        }

        Ok(())
    }

    /// Atomically update a value using a closure.
    ///
    /// This method provides atomic read-modify-write semantics for updating values.
    /// It's useful for implementing commands that need to modify data structures
    /// in-place (e.g., LPUSH, HSET, SADD).
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to update
    /// * `f` - A closure that modifies the StoredValue
    ///
    /// # Returns
    /// * `Ok(true)` - If the key existed and was successfully updated
    /// * `Ok(false)` - If the key doesn't exist
    /// * `Err(AikvError)` - If the database index is invalid, the closure fails, or I/O error occurs
    ///
    /// # Example
    /// ```ignore
    /// // Add an item to a list
    /// let updated = storage.update_value(0, "mylist", |v| {
    ///     v.as_list_mut()?.push_back(Bytes::from("item"));
    ///     Ok(())
    /// })?;
    /// ```
    pub fn update_value<F>(&self, db_index: usize, key: &str, f: F) -> Result<bool>
    where
        F: FnOnce(&mut StoredValue) -> Result<()>,
    {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        // Get the current value
        let mut value = match self.get_value(db_index, key)? {
            Some(v) => v,
            None => return Ok(false),
        };

        // Apply the update function
        f(&mut value)?;

        // Store the updated value
        self.set_value(db_index, key.to_string(), value)?;

        Ok(true)
    }

    /// Atomically delete a key and return its value.
    ///
    /// This method provides atomic delete-and-get semantics, useful for implementing
    /// commands like LPOP, RPOP, SPOP that need to retrieve and remove values atomically.
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to delete
    ///
    /// # Returns
    /// * `Ok(Some(StoredValue))` - If the key existed, returns its value before deletion
    /// * `Ok(None)` - If the key doesn't exist
    /// * `Err(AikvError)` - If the database index is invalid or I/O error occurs
    ///
    /// # Example
    /// ```ignore
    /// // Pop from a list
    /// if let Some(value) = storage.delete_and_get(0, "mykey")? {
    ///     println!("Deleted value: {:?}", value);
    /// }
    /// ```
    pub fn delete_and_get(&self, db_index: usize, key: &str) -> Result<Option<StoredValue>> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Get the value before deleting
        let value = self.get_value(db_index, key)?;

        if value.is_some() {
            // Delete the key
            db.delete(key_bytes)
                .map_err(|e| AikvError::Storage(format!("Failed to delete key: {}", e)))?;

            // Delete expiration metadata if exists
            let expire_key = Self::expiration_key(key_bytes);
            let _ = db.delete(&expire_key);
        }

        Ok(value)
    }

    /// Write a batch of operations atomically using AiDb's WriteBatch.
    ///
    /// This method provides atomic batch writes with durability guarantees:
    /// - All operations are written to the WAL first
    /// - Single fsync for the entire batch
    /// - On failure, the entire batch is rolled back
    /// - On crash recovery, all batch operations are replayed together
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `operations` - Vector of (key, operation) pairs where operation is either Set(value) or Delete
    ///
    /// # Returns
    /// * `Ok(())` - If all operations succeeded
    /// * `Err(AikvError)` - If any operation failed (entire batch is rolled back)
    ///
    /// # Example
    /// ```ignore
    /// use crate::storage::aidb_adapter::BatchOp;
    ///
    /// let ops = vec![
    ///     ("key1".to_string(), BatchOp::Set(Bytes::from("value1"))),
    ///     ("key2".to_string(), BatchOp::Set(Bytes::from("value2"))),
    ///     ("key3".to_string(), BatchOp::Delete),
    /// ];
    /// storage.write_batch(0, ops)?;
    /// ```
    pub fn write_batch(&self, db_index: usize, operations: Vec<(String, BatchOp)>) -> Result<()> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        if operations.is_empty() {
            return Ok(());
        }

        let db = &self.databases[db_index];
        let mut batch = WriteBatch::new();

        for (key, op) in operations {
            let key_bytes = key.as_bytes();
            match op {
                BatchOp::Set(value) => {
                    // Serialize StoredValue into bincode format before putting into AiDb
                    let stored = StoredValue::new_string(value);
                    let serializable = stored.to_serializable();
                    let serialized = bincode::serialize(&serializable).map_err(|e| {
                        AikvError::Storage(format!("Failed to serialize value: {}", e))
                    })?;
                    batch.put(key_bytes, &serialized);
                }
                BatchOp::Delete => {
                    batch.delete(key_bytes);
                    // Also delete expiration metadata
                    let expire_key = Self::expiration_key(key_bytes);
                    batch.delete(&expire_key);
                }
            }
        }

        // Write the batch atomically
        db.write(batch)
            .map_err(|e| AikvError::Storage(format!("Failed to write batch: {}", e)))?;

        Ok(())
    }

    // ========================================================================
    // LEGACY METHODS (For backward compatibility with existing code)
    // ========================================================================

    /// Get a value by key from a specific database
    ///
    /// Uses get_value internally and extracts string bytes if the stored value is a string.
    pub fn get_from_db(&self, db_index: usize, key: &str) -> Result<Option<Bytes>> {
        // Use get_value which properly deserializes bincode data
        match self.get_value(db_index, key)? {
            Some(stored_value) => {
                // Extract string bytes from StoredValue
                match stored_value.as_string() {
                    Ok(bytes) => Ok(Some(bytes.clone())),
                    Err(_) => Ok(None), // Non-string types return None for legacy compatibility
                }
            }
            None => Ok(None),
        }
    }

    /// Get a value by key (from default database 0)
    pub fn get(&self, key: &str) -> Result<Option<Bytes>> {
        self.get_from_db(0, key)
    }

    /// Set a value for a key in a specific database
    ///
    /// Uses set_value internally to properly serialize with bincode.
    pub fn set_in_db(&self, db_index: usize, key: String, value: Bytes) -> Result<()> {
        let stored_value = StoredValue::new_string(value);
        self.set_value(db_index, key, stored_value)
    }

    /// Set a value for a key (in default database 0)
    pub fn set(&self, key: String, value: Bytes) -> Result<()> {
        self.set_in_db(0, key, value)
    }

    /// Set a value with expiration time in milliseconds
    ///
    /// Uses set_value internally to properly serialize with bincode, then sets expiration.
    pub fn set_with_expiration_in_db(
        &self,
        db_index: usize,
        key: String,
        value: Bytes,
        expires_at: u64,
    ) -> Result<()> {
        // Create a StoredValue with expiration
        let mut stored_value = StoredValue::new_string(value);
        stored_value.set_expiration(Some(expires_at));
        self.set_value(db_index, key, stored_value)
    }

    /// Set expiration for a key in milliseconds
    pub fn set_expire_in_db(&self, db_index: usize, key: &str, expire_ms: u64) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key exists and is not expired
        if self.is_expired(db, key_bytes)? {
            return Ok(false);
        }

        if db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_none()
        {
            return Ok(false);
        }

        // Set expiration
        let expire_at = Self::current_time_ms() + expire_ms;
        let expire_key = Self::expiration_key(key_bytes);
        db.put(&expire_key, &expire_at.to_le_bytes())
            .map_err(|e| AikvError::Storage(format!("Failed to set expiration: {}", e)))?;

        Ok(true)
    }

    /// Set expiration at absolute timestamp in milliseconds
    pub fn set_expire_at_in_db(
        &self,
        db_index: usize,
        key: &str,
        timestamp_ms: u64,
    ) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key exists and is not expired
        if self.is_expired(db, key_bytes)? {
            return Ok(false);
        }

        if db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_none()
        {
            return Ok(false);
        }

        // Set expiration
        let expire_key = Self::expiration_key(key_bytes);
        db.put(&expire_key, &timestamp_ms.to_le_bytes())
            .map_err(|e| AikvError::Storage(format!("Failed to set expiration: {}", e)))?;

        Ok(true)
    }

    /// Get TTL in milliseconds
    pub fn get_ttl_in_db(&self, db_index: usize, key: &str) -> Result<i64> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key exists
        if db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_none()
        {
            return Ok(-2); // Key doesn't exist
        }

        // Check if expired
        if self.is_expired(db, key_bytes)? {
            return Ok(-2);
        }

        // Get expiration
        let expire_key = Self::expiration_key(key_bytes);
        if let Some(expire_bytes) = db
            .get(&expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
        {
            if expire_bytes.len() == 8 {
                let expire_at = u64::from_le_bytes([
                    expire_bytes[0],
                    expire_bytes[1],
                    expire_bytes[2],
                    expire_bytes[3],
                    expire_bytes[4],
                    expire_bytes[5],
                    expire_bytes[6],
                    expire_bytes[7],
                ]);
                let now = Self::current_time_ms();
                if expire_at > now {
                    return Ok((expire_at - now) as i64);
                } else {
                    return Ok(-2);
                }
            }
        }

        Ok(-1) // No expiration set
    }

    /// Get expiration timestamp in milliseconds
    pub fn get_expire_time_in_db(&self, db_index: usize, key: &str) -> Result<i64> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key exists
        if db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_none()
        {
            return Ok(-2); // Key doesn't exist
        }

        // Check if expired
        if self.is_expired(db, key_bytes)? {
            return Ok(-2);
        }

        // Get expiration
        let expire_key = Self::expiration_key(key_bytes);
        if let Some(expire_bytes) = db
            .get(&expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
        {
            if expire_bytes.len() == 8 {
                let expire_at = u64::from_le_bytes([
                    expire_bytes[0],
                    expire_bytes[1],
                    expire_bytes[2],
                    expire_bytes[3],
                    expire_bytes[4],
                    expire_bytes[5],
                    expire_bytes[6],
                    expire_bytes[7],
                ]);
                return Ok(expire_at as i64);
            }
        }

        Ok(-1) // No expiration set
    }

    /// Remove expiration from a key
    pub fn persist_in_db(&self, db_index: usize, key: &str) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key exists
        if db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_none()
        {
            return Ok(false);
        }

        // Check if expired
        if self.is_expired(db, key_bytes)? {
            return Ok(false);
        }

        // Check if expiration exists
        let expire_key = Self::expiration_key(key_bytes);
        if db
            .get(&expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
            .is_some()
        {
            // Remove expiration
            db.delete(&expire_key)
                .map_err(|e| AikvError::Storage(format!("Failed to delete expiration: {}", e)))?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Delete a key from a specific database
    pub fn delete_from_db(&self, db_index: usize, key: &str) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key exists
        let exists = db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_some();

        if exists {
            // Delete the key
            db.delete(key_bytes)
                .map_err(|e| AikvError::Storage(format!("Failed to delete key: {}", e)))?;

            // Delete expiration metadata if exists
            let expire_key = Self::expiration_key(key_bytes);
            let _ = db.delete(&expire_key);

            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Delete a key (from default database 0)
    pub fn delete(&self, key: &str) -> Result<bool> {
        self.delete_from_db(0, key)
    }

    /// Check if a key exists in a specific database
    pub fn exists_in_db(&self, db_index: usize, key: &str) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if expired
        if self.is_expired(db, key_bytes)? {
            return Ok(false);
        }

        // Check if key exists
        Ok(db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check key existence: {}", e)))?
            .is_some())
    }

    /// Check if a key exists (in default database 0)
    pub fn exists(&self, key: &str) -> Result<bool> {
        self.exists_in_db(0, key)
    }

    /// Get all keys in a database
    /// Note: This is an expensive operation for large databases
    pub fn get_all_keys_in_db(&self, db_index: usize) -> Result<Vec<String>> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let mut keys = Vec::new();

        // Create an iterator to scan all keys
        // Note: AiDb v0.6.2+ iterator automatically skips tombstones (deleted keys)
        let mut iter = db.iter();

        while iter.valid() {
            let key = iter.key();

            // Skip expiration metadata keys
            if key.starts_with(b"__exp__:") {
                iter.next();
                continue;
            }

            // Check if expired
            if self.is_expired(db, key)? {
                iter.next();
                continue;
            }

            if let Ok(key_str) = String::from_utf8(key.to_vec()) {
                keys.push(key_str);
            }

            iter.next();
        }

        Ok(keys)
    }

    /// Get database size (number of keys)
    pub fn dbsize_in_db(&self, db_index: usize) -> Result<usize> {
        Ok(self.get_all_keys_in_db(db_index)?.len())
    }

    /// Clear a specific database
    pub fn flush_db(&self, db_index: usize) -> Result<()> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];

        // Get all keys and delete them
        let mut iter = db.iter();

        while iter.valid() {
            let key = iter.key().to_vec();
            iter.next();

            db.delete(&key)
                .map_err(|e| AikvError::Storage(format!("Failed to delete key: {}", e)))?;
        }

        Ok(())
    }

    /// Clear all databases
    pub fn flush_all(&self) -> Result<()> {
        for i in 0..self.databases.len() {
            self.flush_db(i)?;
        }
        Ok(())
    }

    /// Swap two databases
    /// Note: This is not efficiently implementable with AiDb, so we return an error
    pub fn swap_db(&self, _db1: usize, _db2: usize) -> Result<()> {
        Err(AikvError::Storage(
            "SWAPDB is not supported with AiDb storage backend".to_string(),
        ))
    }

    /// Move a key from one database to another
    pub fn move_key(&self, src_db: usize, dst_db: usize, key: &str) -> Result<bool> {
        if src_db >= self.databases.len() || dst_db >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {} or {}",
                src_db, dst_db
            )));
        }

        let src = &self.databases[src_db];
        let dst = &self.databases[dst_db];
        let key_bytes = key.as_bytes();

        // Check if key exists in source and is not expired
        if self.is_expired(src, key_bytes)? {
            return Ok(false);
        }

        let value = match src
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to get value: {}", e)))?
        {
            Some(v) => v,
            None => return Ok(false),
        };

        // Check if key already exists in destination
        if dst
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check destination: {}", e)))?
            .is_some()
        {
            return Ok(false);
        }

        // Copy to destination
        dst.put(key_bytes, &value)
            .map_err(|e| AikvError::Storage(format!("Failed to put value: {}", e)))?;

        // Copy expiration if exists
        let expire_key = Self::expiration_key(key_bytes);
        if let Some(expire_bytes) = src
            .get(&expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
        {
            dst.put(&expire_key, &expire_bytes)
                .map_err(|e| AikvError::Storage(format!("Failed to put expiration: {}", e)))?;
        }

        // Delete from source
        src.delete(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to delete from source: {}", e)))?;
        let _ = src.delete(&expire_key);

        Ok(true)
    }

    /// Rename a key
    pub fn rename_in_db(&self, db_index: usize, old_key: &str, new_key: &str) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let old_key_bytes = old_key.as_bytes();
        let new_key_bytes = new_key.as_bytes();

        // Check if old key exists and is not expired
        if self.is_expired(db, old_key_bytes)? {
            return Ok(false);
        }

        let value = match db
            .get(old_key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to get value: {}", e)))?
        {
            Some(v) => v,
            None => return Ok(false),
        };

        // Set new key
        db.put(new_key_bytes, &value)
            .map_err(|e| AikvError::Storage(format!("Failed to put value: {}", e)))?;

        // Copy expiration if exists
        let old_expire_key = Self::expiration_key(old_key_bytes);
        let new_expire_key = Self::expiration_key(new_key_bytes);
        if let Some(expire_bytes) = db
            .get(&old_expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
        {
            db.put(&new_expire_key, &expire_bytes)
                .map_err(|e| AikvError::Storage(format!("Failed to put expiration: {}", e)))?;
        }

        // Delete old key
        db.delete(old_key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to delete old key: {}", e)))?;
        let _ = db.delete(&old_expire_key);

        Ok(true)
    }

    /// Rename a key only if new key doesn't exist
    pub fn rename_nx_in_db(&self, db_index: usize, old_key: &str, new_key: &str) -> Result<bool> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let new_key_bytes = new_key.as_bytes();

        // Check if new key exists
        if db
            .get(new_key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check new key: {}", e)))?
            .is_some()
        {
            return Ok(false);
        }

        self.rename_in_db(db_index, old_key, new_key)
    }

    /// Copy a key
    pub fn copy_in_db(
        &self,
        src_db: usize,
        dst_db: usize,
        src_key: &str,
        dst_key: &str,
        replace: bool,
    ) -> Result<bool> {
        if src_db >= self.databases.len() || dst_db >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {} or {}",
                src_db, dst_db
            )));
        }

        let src = &self.databases[src_db];
        let dst = &self.databases[dst_db];
        let src_key_bytes = src_key.as_bytes();
        let dst_key_bytes = dst_key.as_bytes();

        // Check if source key exists and is not expired
        if self.is_expired(src, src_key_bytes)? {
            return Ok(false);
        }

        let value = match src
            .get(src_key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to get value: {}", e)))?
        {
            Some(v) => v,
            None => return Ok(false),
        };

        // Check if destination key exists
        let dst_exists = dst
            .get(dst_key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to check destination: {}", e)))?
            .is_some();

        if dst_exists && !replace {
            return Ok(false);
        }

        // Copy to destination
        dst.put(dst_key_bytes, &value)
            .map_err(|e| AikvError::Storage(format!("Failed to put value: {}", e)))?;

        // Copy expiration if exists
        let src_expire_key = Self::expiration_key(src_key_bytes);
        let dst_expire_key = Self::expiration_key(dst_key_bytes);
        if let Some(expire_bytes) = src
            .get(&src_expire_key)
            .map_err(|e| AikvError::Storage(format!("Failed to get expiration: {}", e)))?
        {
            dst.put(&dst_expire_key, &expire_bytes)
                .map_err(|e| AikvError::Storage(format!("Failed to put expiration: {}", e)))?;
        }

        Ok(true)
    }

    /// Get a random key from a database
    pub fn random_key_in_db(&self, db_index: usize) -> Result<Option<String>> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];

        // Create an iterator and get the first valid key
        let mut iter = db.iter();

        while iter.valid() {
            let key = iter.key();

            // Skip expiration metadata keys
            if key.starts_with(b"__exp__:") {
                iter.next();
                continue;
            }

            // Check if expired
            if self.is_expired(db, key)? {
                iter.next();
                continue;
            }

            if let Ok(key_str) = String::from_utf8(key.to_vec()) {
                // Use current time as a simple random selection mechanism
                // In a production system, this would use a proper random number generator
                let now_ns = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                if now_ns % 5 == 0 {
                    return Ok(Some(key_str));
                }
            }

            iter.next();
        }

        // If we didn't find a key through random selection, return the first valid key
        let mut iter = db.iter();

        while iter.valid() {
            let key = iter.key();

            if key.starts_with(b"__exp__:") {
                iter.next();
                continue;
            }

            if self.is_expired(db, key)? {
                iter.next();
                continue;
            }

            if let Ok(key_str) = String::from_utf8(key.to_vec()) {
                return Ok(Some(key_str));
            }

            iter.next();
        }

        Ok(None)
    }

    /// Export all databases as StoredValue maps (for persistence)
    pub fn export_all_databases(&self) -> Result<Vec<HashMap<String, StoredValue>>> {
        let mut result = Vec::with_capacity(self.databases.len());

        for db_index in 0..self.databases.len() {
            let mut db_map = HashMap::new();
            let db = &self.databases[db_index];

            let mut iter = db.iter();
            while iter.valid() {
                let key = iter.key();

                // Skip expiration metadata keys
                if key.starts_with(b"__exp__:") {
                    iter.next();
                    continue;
                }

                // Check if expired
                if self.is_expired(db, key)? {
                    iter.next();
                    continue;
                }

                // Get the value
                if let Some(serialized) = db
                    .get(key)
                    .map_err(|e| AikvError::Storage(format!("Failed to get value: {}", e)))?
                {
                    // Deserialize using bincode
                    let serializable: SerializableStoredValue = bincode::deserialize(&serialized)
                        .map_err(|e| {
                        AikvError::Storage(format!("Failed to deserialize value: {}", e))
                    })?;
                    let mut stored_value = StoredValue::from_serializable(serializable);

                    // Get expiration if exists
                    let expire_key = Self::expiration_key(key);
                    if let Some(expire_bytes) = db.get(&expire_key).map_err(|e| {
                        AikvError::Storage(format!("Failed to get expiration: {}", e))
                    })? {
                        if expire_bytes.len() == 8 {
                            let expire_at = u64::from_le_bytes([
                                expire_bytes[0],
                                expire_bytes[1],
                                expire_bytes[2],
                                expire_bytes[3],
                                expire_bytes[4],
                                expire_bytes[5],
                                expire_bytes[6],
                                expire_bytes[7],
                            ]);
                            stored_value.set_expiration(Some(expire_at));
                        }
                    }

                    if let Ok(key_str) = String::from_utf8(key.to_vec()) {
                        db_map.insert(key_str, stored_value);
                    }
                }

                iter.next();
            }

            result.push(db_map);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
    use tempfile::TempDir;

    fn create_temp_storage() -> (TempDir, AiDbStorageAdapter) {
        let temp_dir = TempDir::new().unwrap();
        let storage = AiDbStorageAdapter::new(temp_dir.path(), 2).unwrap();
        (temp_dir, storage)
    }

    #[test]
    fn test_set_get() {
        let (_dir, storage) = create_temp_storage();
        storage
            .set("key1".to_string(), Bytes::from("value1"))
            .unwrap();

        let value = storage.get("key1").unwrap();
        assert_eq!(value, Some(Bytes::from("value1")));
    }

    #[test]
    fn test_delete() {
        let (_dir, storage) = create_temp_storage();
        storage
            .set("key1".to_string(), Bytes::from("value1"))
            .unwrap();

        let deleted = storage.delete("key1").unwrap();
        assert!(deleted);

        let value = storage.get("key1").unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_exists() {
        let (_dir, storage) = create_temp_storage();
        storage
            .set("key1".to_string(), Bytes::from("value1"))
            .unwrap();

        assert!(storage.exists("key1").unwrap());
        assert!(!storage.exists("key2").unwrap());
    }

    #[test]
    fn test_mget_mset() {
        let (_dir, storage) = create_temp_storage();

        // Migrated: Use set_value instead of mset
        storage
            .set_value(
                0,
                "key1".to_string(),
                StoredValue::new_string(Bytes::from("value1")),
            )
            .unwrap();
        storage
            .set_value(
                0,
                "key2".to_string(),
                StoredValue::new_string(Bytes::from("value2")),
            )
            .unwrap();

        // Migrated: Use get_value instead of mget
        let value1 = storage.get_value(0, "key1").unwrap();
        let value2 = storage.get_value(0, "key2").unwrap();
        let value3 = storage.get_value(0, "key3").unwrap();

        assert!(value1.is_some());
        assert_eq!(value1.unwrap().as_string().unwrap(), &Bytes::from("value1"));
        assert!(value2.is_some());
        assert_eq!(value2.unwrap().as_string().unwrap(), &Bytes::from("value2"));
        assert!(value3.is_none());
    }

    #[test]
    fn test_expiration() {
        let (_dir, storage) = create_temp_storage();
        storage
            .set("key1".to_string(), Bytes::from("value1"))
            .unwrap();

        // Set expiration to 1 second from now
        let expire_ms = 1000;
        storage.set_expire_in_db(0, "key1", expire_ms).unwrap();

        // Key should still exist
        assert!(storage.exists("key1").unwrap());

        // Wait for expiration
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Key should be expired
        assert!(!storage.exists("key1").unwrap());
        assert_eq!(storage.get("key1").unwrap(), None);
    }

    // ========================================================================
    // Tests for new serialization-based storage (all data types)
    // ========================================================================

    #[test]
    fn test_stored_value_string() {
        let (_dir, storage) = create_temp_storage();
        let value = StoredValue::new_string(Bytes::from("hello world"));

        storage
            .set_value(0, "key1".to_string(), value.clone())
            .unwrap();

        let retrieved = storage.get_value(0, "key1").unwrap().unwrap();
        assert_eq!(retrieved.as_string().unwrap(), &Bytes::from("hello world"));
    }

    #[test]
    fn test_stored_value_list() {
        let (_dir, storage) = create_temp_storage();
        let mut list = VecDeque::new();
        list.push_back(Bytes::from("item1"));
        list.push_back(Bytes::from("item2"));
        list.push_back(Bytes::from("item3"));

        let value = StoredValue::new_list(list.clone());
        storage.set_value(0, "mylist".to_string(), value).unwrap();

        let retrieved = storage.get_value(0, "mylist").unwrap().unwrap();
        let retrieved_list = retrieved.as_list().unwrap();
        assert_eq!(retrieved_list.len(), 3);
        assert_eq!(retrieved_list[0], Bytes::from("item1"));
        assert_eq!(retrieved_list[1], Bytes::from("item2"));
        assert_eq!(retrieved_list[2], Bytes::from("item3"));
    }

    #[test]
    fn test_stored_value_hash() {
        let (_dir, storage) = create_temp_storage();
        let mut hash = HashMap::new();
        hash.insert("field1".to_string(), Bytes::from("value1"));
        hash.insert("field2".to_string(), Bytes::from("value2"));

        let value = StoredValue::new_hash(hash);
        storage.set_value(0, "myhash".to_string(), value).unwrap();

        let retrieved = storage.get_value(0, "myhash").unwrap().unwrap();
        let retrieved_hash = retrieved.as_hash().unwrap();
        assert_eq!(retrieved_hash.len(), 2);
        assert_eq!(
            retrieved_hash.get("field1").unwrap(),
            &Bytes::from("value1")
        );
        assert_eq!(
            retrieved_hash.get("field2").unwrap(),
            &Bytes::from("value2")
        );
    }

    #[test]
    fn test_stored_value_set() {
        let (_dir, storage) = create_temp_storage();
        let mut set = HashSet::new();
        set.insert(b"member1".to_vec());
        set.insert(b"member2".to_vec());
        set.insert(b"member3".to_vec());

        let value = StoredValue::new_set(set);
        storage.set_value(0, "myset".to_string(), value).unwrap();

        let retrieved = storage.get_value(0, "myset").unwrap().unwrap();
        let retrieved_set = retrieved.as_set().unwrap();
        assert_eq!(retrieved_set.len(), 3);
        assert!(retrieved_set.contains(&b"member1".to_vec()));
        assert!(retrieved_set.contains(&b"member2".to_vec()));
        assert!(retrieved_set.contains(&b"member3".to_vec()));
    }

    #[test]
    fn test_stored_value_zset() {
        let (_dir, storage) = create_temp_storage();
        let mut zset = BTreeMap::new();
        zset.insert(b"member1".to_vec(), 1.0);
        zset.insert(b"member2".to_vec(), 2.5);
        zset.insert(b"member3".to_vec(), 3.7);

        let value = StoredValue::new_zset(zset);
        storage.set_value(0, "myzset".to_string(), value).unwrap();

        let retrieved = storage.get_value(0, "myzset").unwrap().unwrap();
        let retrieved_zset = retrieved.as_zset().unwrap();
        assert_eq!(retrieved_zset.len(), 3);
        assert_eq!(retrieved_zset.get(&b"member1".to_vec()).unwrap(), &1.0);
        assert_eq!(retrieved_zset.get(&b"member2".to_vec()).unwrap(), &2.5);
        assert_eq!(retrieved_zset.get(&b"member3".to_vec()).unwrap(), &3.7);
    }

    #[test]
    fn test_update_value() {
        let (_dir, storage) = create_temp_storage();
        let mut list = VecDeque::new();
        list.push_back(Bytes::from("item1"));

        let value = StoredValue::new_list(list);
        storage.set_value(0, "mylist".to_string(), value).unwrap();

        // Update the list by adding an item
        let updated = storage
            .update_value(0, "mylist", |v| {
                v.as_list_mut()?.push_back(Bytes::from("item2"));
                Ok(())
            })
            .unwrap();

        assert!(updated);

        let retrieved = storage.get_value(0, "mylist").unwrap().unwrap();
        let retrieved_list = retrieved.as_list().unwrap();
        assert_eq!(retrieved_list.len(), 2);
        assert_eq!(retrieved_list[0], Bytes::from("item1"));
        assert_eq!(retrieved_list[1], Bytes::from("item2"));
    }

    #[test]
    fn test_delete_and_get() {
        let (_dir, storage) = create_temp_storage();
        let value = StoredValue::new_string(Bytes::from("test value"));
        storage.set_value(0, "key1".to_string(), value).unwrap();

        let deleted_value = storage.delete_and_get(0, "key1").unwrap();
        assert!(deleted_value.is_some());
        assert_eq!(
            deleted_value.unwrap().as_string().unwrap(),
            &Bytes::from("test value")
        );

        // Key should no longer exist
        assert!(!storage.exists_in_db(0, "key1").unwrap());
    }

    #[test]
    fn test_expiration_with_serialized_value() {
        let (_dir, storage) = create_temp_storage();
        let mut hash = HashMap::new();
        hash.insert("field1".to_string(), Bytes::from("value1"));

        let mut value = StoredValue::new_hash(hash);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        value.set_expiration(Some(now + 1000)); // Expire in 1 second

        storage.set_value(0, "myhash".to_string(), value).unwrap();

        // Key should exist
        assert!(storage.exists_in_db(0, "myhash").unwrap());

        // Wait for expiration
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Key should be expired
        assert!(!storage.exists_in_db(0, "myhash").unwrap());
        assert!(storage.get_value(0, "myhash").unwrap().is_none());
    }

    #[test]
    fn test_cross_database_operations() {
        let (_dir, storage) = create_temp_storage();

        // Set values in different databases
        let value1 = StoredValue::new_string(Bytes::from("db0 value"));
        let value2 = StoredValue::new_string(Bytes::from("db1 value"));

        storage.set_value(0, "key".to_string(), value1).unwrap();
        storage.set_value(1, "key".to_string(), value2).unwrap();

        // Verify they're separate
        let v0 = storage.get_value(0, "key").unwrap().unwrap();
        let v1 = storage.get_value(1, "key").unwrap().unwrap();

        assert_eq!(v0.as_string().unwrap(), &Bytes::from("db0 value"));
        assert_eq!(v1.as_string().unwrap(), &Bytes::from("db1 value"));
    }

    #[test]
    fn test_complex_list_operations() {
        let (_dir, storage) = create_temp_storage();
        let mut list = VecDeque::new();
        list.push_back(Bytes::from("a"));
        list.push_back(Bytes::from("b"));
        list.push_back(Bytes::from("c"));

        let value = StoredValue::new_list(list);
        storage.set_value(0, "mylist".to_string(), value).unwrap();

        // Modify the list
        storage
            .update_value(0, "mylist", |v| {
                let list = v.as_list_mut()?;
                list.pop_front();
                list.push_back(Bytes::from("d"));
                Ok(())
            })
            .unwrap();

        let retrieved = storage.get_value(0, "mylist").unwrap().unwrap();
        let retrieved_list = retrieved.as_list().unwrap();
        assert_eq!(retrieved_list.len(), 3);
        assert_eq!(retrieved_list[0], Bytes::from("b"));
        assert_eq!(retrieved_list[1], Bytes::from("c"));
        assert_eq!(retrieved_list[2], Bytes::from("d"));
    }

    #[test]
    fn test_complex_hash_operations() {
        let (_dir, storage) = create_temp_storage();
        let mut hash = HashMap::new();
        hash.insert("name".to_string(), Bytes::from("Alice"));
        hash.insert("age".to_string(), Bytes::from("30"));

        let value = StoredValue::new_hash(hash);
        storage.set_value(0, "user:1".to_string(), value).unwrap();

        // Update hash
        storage
            .update_value(0, "user:1", |v| {
                let hash = v.as_hash_mut()?;
                hash.insert("age".to_string(), Bytes::from("31"));
                hash.insert("city".to_string(), Bytes::from("New York"));
                Ok(())
            })
            .unwrap();

        let retrieved = storage.get_value(0, "user:1").unwrap().unwrap();
        let retrieved_hash = retrieved.as_hash().unwrap();
        assert_eq!(retrieved_hash.len(), 3);
        assert_eq!(retrieved_hash.get("name").unwrap(), &Bytes::from("Alice"));
        assert_eq!(retrieved_hash.get("age").unwrap(), &Bytes::from("31"));
        assert_eq!(
            retrieved_hash.get("city").unwrap(),
            &Bytes::from("New York")
        );
    }
}
