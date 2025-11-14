//! Memory Storage Adapter - In-memory storage backend for AiKv
//!
//! This module provides a high-performance in-memory storage adapter with support
//! for all Redis data types. It implements the same minimal interface as the
//! persistent AiDb adapter, allowing seamless switching between storage backends.
//!
//! # Architecture
//!
//! The memory adapter follows the same architectural principles as AiDbStorageAdapter:
//! - **Minimal Interface**: Only provides basic CRUD operations
//! - **Type Agnostic**: All data types use the same interface
//! - **Separation of Concerns**: Storage handles data, commands handle logic
//! - **High Performance**: Direct in-memory operations without serialization overhead
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
//! use aikv::storage::StorageAdapter;
//! use aikv::storage::StoredValue;
//! use bytes::Bytes;
//!
//! let storage = StorageAdapter::new();
//!
//! // Store a list
//! let mut list = VecDeque::new();
//! list.push_back(Bytes::from("item1"));
//! let value = StoredValue::new_list(list);
//! storage.set_value(0, "mylist".to_string(), value)?;
//! ```

use crate::error::{AikvError, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Different value types supported by the storage.
///
/// These types correspond to Redis data types and are used by the storage layer
/// to represent values generically. The command layer operates on these types
/// directly through the StoredValue wrapper.
#[derive(Clone, Debug)]
pub enum ValueType {
    /// String type - stores bytes
    String(Bytes),
    /// List type - ordered collection of bytes (Redis LIST)
    List(VecDeque<Bytes>),
    /// Hash type - key-value map (Redis HASH)
    Hash(HashMap<String, Bytes>),
    /// Set type - unordered collection of unique bytes (Redis SET)
    Set(HashSet<Vec<u8>>), // Using Vec<u8> instead of Bytes for HashSet compatibility
    /// Sorted Set type - ordered collection with scores (Redis ZSET)
    ZSet(BTreeMap<Vec<u8>, f64>), // member -> score mapping
}

/// Value with optional expiration time.
///
/// This is the primary value container used throughout the storage layer.
/// It wraps a ValueType and includes optional expiration metadata.
#[derive(Clone, Debug)]
pub struct StoredValue {
    pub(crate) value: ValueType,
    /// Expiration time in milliseconds since UNIX epoch
    pub(crate) expires_at: Option<u64>,
}

// Serializable versions for storage (optimized for bincode)
#[derive(Serialize, Deserialize)]
enum SerializableValueType {
    String(Vec<u8>),
    List(Vec<Vec<u8>>),
    Hash(Vec<(String, Vec<u8>)>),
    Set(Vec<Vec<u8>>),
    ZSet(Vec<(Vec<u8>, f64)>),
}

/// Serializable representation of StoredValue for persistence.
///
/// This struct is used by AiDbStorageAdapter to serialize values to disk
/// efficiently using bincode.
#[derive(Serialize, Deserialize)]
pub struct SerializableStoredValue {
    value: SerializableValueType,
    expires_at: Option<u64>,
}

impl StoredValue {
    /// Convert to serializable format for storage.
    ///
    /// Used by AiDbStorageAdapter to persist values to disk.
    pub fn to_serializable(&self) -> SerializableStoredValue {
        let value = match &self.value {
            ValueType::String(bytes) => SerializableValueType::String(bytes.to_vec()),
            ValueType::List(list) => {
                SerializableValueType::List(list.iter().map(|b| b.to_vec()).collect())
            }
            ValueType::Hash(hash) => SerializableValueType::Hash(
                hash.iter().map(|(k, v)| (k.clone(), v.to_vec())).collect(),
            ),
            ValueType::Set(set) => SerializableValueType::Set(set.iter().cloned().collect()),
            ValueType::ZSet(zset) => {
                SerializableValueType::ZSet(zset.iter().map(|(k, v)| (k.clone(), *v)).collect())
            }
        };
        SerializableStoredValue {
            value,
            expires_at: self.expires_at,
        }
    }

    /// Create from serializable format
    pub fn from_serializable(serializable: SerializableStoredValue) -> Self {
        let value = match serializable.value {
            SerializableValueType::String(vec) => ValueType::String(Bytes::from(vec)),
            SerializableValueType::List(vec_list) => {
                ValueType::List(vec_list.into_iter().map(Bytes::from).collect())
            }
            SerializableValueType::Hash(vec_hash) => ValueType::Hash(
                vec_hash
                    .into_iter()
                    .map(|(k, v)| (k, Bytes::from(v)))
                    .collect(),
            ),
            SerializableValueType::Set(vec_set) => ValueType::Set(vec_set.into_iter().collect()),
            SerializableValueType::ZSet(vec_zset) => {
                ValueType::ZSet(vec_zset.into_iter().collect())
            }
        };
        Self {
            value,
            expires_at: serializable.expires_at,
        }
    }
}

impl StoredValue {
    pub fn new_string(data: Bytes) -> Self {
        Self {
            value: ValueType::String(data),
            expires_at: None,
        }
    }

    pub fn new_list(list: VecDeque<Bytes>) -> Self {
        Self {
            value: ValueType::List(list),
            expires_at: None,
        }
    }

    pub fn new_hash(hash: HashMap<String, Bytes>) -> Self {
        Self {
            value: ValueType::Hash(hash),
            expires_at: None,
        }
    }

    pub fn new_set(set: HashSet<Vec<u8>>) -> Self {
        Self {
            value: ValueType::Set(set),
            expires_at: None,
        }
    }

    pub fn new_zset(zset: BTreeMap<Vec<u8>, f64>) -> Self {
        Self {
            value: ValueType::ZSet(zset),
            expires_at: None,
        }
    }

    pub fn with_expiration(value: ValueType, expires_at: u64) -> Self {
        Self {
            value,
            expires_at: Some(expires_at),
        }
    }

    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            now >= expires_at
        } else {
            false
        }
    }

    pub fn get_type_name(&self) -> &str {
        match &self.value {
            ValueType::String(_) => "string",
            ValueType::List(_) => "list",
            ValueType::Hash(_) => "hash",
            ValueType::Set(_) => "set",
            ValueType::ZSet(_) => "zset",
        }
    }

    /// Get reference to the underlying value
    pub fn value(&self) -> &ValueType {
        &self.value
    }

    /// Get mutable reference to the underlying value
    pub fn value_mut(&mut self) -> &mut ValueType {
        &mut self.value
    }

    /// Check if value is of String type and return reference to it
    pub fn as_string(&self) -> Result<&Bytes> {
        match &self.value {
            ValueType::String(data) => Ok(data),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of List type and return reference to it
    pub fn as_list(&self) -> Result<&VecDeque<Bytes>> {
        match &self.value {
            ValueType::List(list) => Ok(list),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of List type and return mutable reference to it
    pub fn as_list_mut(&mut self) -> Result<&mut VecDeque<Bytes>> {
        match &mut self.value {
            ValueType::List(list) => Ok(list),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of Hash type and return reference to it
    pub fn as_hash(&self) -> Result<&HashMap<String, Bytes>> {
        match &self.value {
            ValueType::Hash(hash) => Ok(hash),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of Hash type and return mutable reference to it
    pub fn as_hash_mut(&mut self) -> Result<&mut HashMap<String, Bytes>> {
        match &mut self.value {
            ValueType::Hash(hash) => Ok(hash),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of Set type and return reference to it
    pub fn as_set(&self) -> Result<&HashSet<Vec<u8>>> {
        match &self.value {
            ValueType::Set(set) => Ok(set),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of Set type and return mutable reference to it
    pub fn as_set_mut(&mut self) -> Result<&mut HashSet<Vec<u8>>> {
        match &mut self.value {
            ValueType::Set(set) => Ok(set),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of ZSet type and return reference to it
    pub fn as_zset(&self) -> Result<&BTreeMap<Vec<u8>, f64>> {
        match &self.value {
            ValueType::ZSet(zset) => Ok(zset),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Check if value is of ZSet type and return mutable reference to it
    pub fn as_zset_mut(&mut self) -> Result<&mut BTreeMap<Vec<u8>, f64>> {
        match &mut self.value {
            ValueType::ZSet(zset) => Ok(zset),
            _ => Err(AikvError::WrongType(
                "Operation against a key holding the wrong kind of value".to_string(),
            )),
        }
    }

    /// Get expiration time in milliseconds since UNIX epoch
    pub fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }

    /// Set expiration time in milliseconds since UNIX epoch
    pub fn set_expiration(&mut self, expires_at: Option<u64>) {
        self.expires_at = expires_at;
    }
}

/// Database containing key-value pairs
type Database = HashMap<String, StoredValue>;

/// Simple in-memory storage adapter
/// This will be replaced with AiDb integration in the future
#[derive(Clone)]
pub struct StorageAdapter {
    /// Multiple databases (default: 16 databases like Redis)
    databases: Arc<RwLock<Vec<Database>>>,
}

impl StorageAdapter {
    pub fn new() -> Self {
        Self::with_db_count(16) // Default to 16 databases like Redis
    }

    pub fn with_db_count(count: usize) -> Self {
        let mut databases = Vec::with_capacity(count);
        for _ in 0..count {
            databases.push(HashMap::new());
        }
        Self {
            databases: Arc::new(RwLock::new(databases)),
        }
    }

    /// Get current time in milliseconds
    fn current_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Clean up expired keys in a database
    /// Reserved for future background cleanup task
    #[allow(dead_code)]
    fn cleanup_expired(&self, db_index: usize) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            db.retain(|_, v| !v.is_expired());
        }
        Ok(())
    }

    // ========================================================================
    // CORE STORAGE METHODS (Minimal Interface Post-Refactoring)
    // ========================================================================
    // These methods provide the minimal, orthogonal interface for storage operations.
    // All command-specific logic should be implemented in the command layer.

    /// Get a stored value by key from a specific database.
    ///
    /// This method supports all data types (String, List, Hash, Set, ZSet) without
    /// serialization overhead. Expired keys are automatically filtered and return `None`.
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to retrieve
    ///
    /// # Returns
    /// * `Ok(Some(StoredValue))` - The value if found and not expired
    /// * `Ok(None)` - If the key doesn't exist or has expired
    /// * `Err(AikvError)` - If lock acquisition fails
    ///
    /// # Example
    /// ```ignore
    /// let value = storage.get_value(0, "mykey")?;
    /// if let Some(v) = value {
    ///     if let Some(list) = v.as_list() {
    ///         println!("List has {} items", list.len());
    ///     }
    /// }
    /// ```
    pub fn get_value(&self, db_index: usize, key: &str) -> Result<Option<StoredValue>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(None);
                }
                return Ok(Some(stored.clone()));
            }
        }
        Ok(None)
    }

    /// Set a value for a key in a specific database.
    ///
    /// This method supports all data types (String, List, Hash, Set, ZSet) with
    /// direct in-memory storage (no serialization needed). The value's expiration
    /// metadata is preserved.
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to set
    /// * `value` - The StoredValue to store (can be any supported data type)
    ///
    /// # Returns
    /// * `Ok(())` - If the value was successfully stored
    /// * `Err(AikvError)` - If the database index is invalid or lock acquisition fails
    ///
    /// # Example
    /// ```ignore
    /// let mut hash = HashMap::new();
    /// hash.insert("field1".to_string(), Bytes::from("value1"));
    /// let value = StoredValue::new_hash(hash);
    /// storage.set_value(0, "myhash".to_string(), value)?;
    /// ```
    pub fn set_value(&self, db_index: usize, key: String, value: StoredValue) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            db.insert(key, value);
            Ok(())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
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
    /// * `Ok(Some(StoredValue))` - If the key existed and wasn't expired
    /// * `Ok(None)` - If the key doesn't exist or has expired
    /// * `Err(AikvError)` - If lock acquisition fails
    ///
    /// # Example
    /// ```ignore
    /// // Pop an item from a list
    /// if let Some(mut value) = storage.delete_and_get(0, "mylist")? {
    ///     if let Some(list) = value.as_list_mut() {
    ///         if let Some(item) = list.pop_front() {
    ///             println!("Popped: {:?}", item);
    ///         }
    ///     }
    /// }
    /// ```
    pub fn delete_and_get(&self, db_index: usize, key: &str) -> Result<Option<StoredValue>> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.remove(key) {
                if !stored.is_expired() {
                    return Ok(Some(stored));
                }
            }
        }
        Ok(None)
    }

    /// Atomically update a value using a closure.
    ///
    /// This method provides atomic read-modify-write semantics for updating values
    /// in-place. It's useful for implementing commands that need to modify data
    /// structures atomically (e.g., LPUSH, HSET, SADD, ZINCRBY).
    ///
    /// # Arguments
    /// * `db_index` - The database index (0-15 by default)
    /// * `key` - The key to update
    /// * `f` - A closure that modifies the StoredValue
    ///
    /// # Returns
    /// * `Ok(true)` - If the key existed and was successfully updated
    /// * `Ok(false)` - If the key doesn't exist or has expired
    /// * `Err(AikvError)` - If lock acquisition fails or the closure returns an error
    ///
    /// # Example
    /// ```ignore
    /// // Add a field to a hash
    /// let updated = storage.update_value(0, "myhash", |v| {
    ///     let hash = v.as_hash_mut()?;
    ///     hash.insert("field2".to_string(), Bytes::from("value2"));
    ///     Ok(())
    /// })?;
    /// ```
    pub fn update_value<F>(&self, db_index: usize, key: &str, f: F) -> Result<bool>
    where
        F: FnOnce(&mut StoredValue) -> Result<()>,
    {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(false);
                }
                f(stored)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Get a value by key from a specific database
    pub fn get_from_db(&self, db_index: usize, key: &str) -> Result<Option<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(None);
                }
                // Only return value if it's a String type
                if let ValueType::String(data) = &stored.value {
                    return Ok(Some(data.clone()));
                } else {
                    return Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ));
                }
            }
        }
        Ok(None)
    }

    /// Get a value by key (from default database 0)
    pub fn get(&self, key: &str) -> Result<Option<Bytes>> {
        self.get_from_db(0, key)
    }

    /// Set a value for a key in a specific database
    pub fn set_in_db(&self, db_index: usize, key: String, value: Bytes) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            db.insert(key, StoredValue::new_string(value));
            Ok(())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// Set a value for a key (in default database 0)
    pub fn set(&self, key: String, value: Bytes) -> Result<()> {
        self.set_in_db(0, key, value)
    }

    /// Set a value with expiration time in milliseconds
    pub fn set_with_expiration_in_db(
        &self,
        db_index: usize,
        key: String,
        value: Bytes,
        expires_at: u64,
    ) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            db.insert(
                key,
                StoredValue::with_expiration(ValueType::String(value), expires_at),
            );
            Ok(())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// Set expiration for a key in milliseconds
    pub fn set_expire_in_db(&self, db_index: usize, key: &str, expire_ms: u64) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(false);
                }
                stored.expires_at = Some(Self::current_time_ms() + expire_ms);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Set expiration at absolute timestamp in milliseconds
    pub fn set_expire_at_in_db(
        &self,
        db_index: usize,
        key: &str,
        timestamp_ms: u64,
    ) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(false);
                }
                stored.expires_at = Some(timestamp_ms);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Get TTL in milliseconds
    pub fn get_ttl_in_db(&self, db_index: usize, key: &str) -> Result<i64> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(-2); // Key doesn't exist (expired)
                }
                if let Some(expires_at) = stored.expires_at {
                    let now = Self::current_time_ms();
                    if expires_at > now {
                        return Ok((expires_at - now) as i64);
                    } else {
                        return Ok(-2); // Already expired
                    }
                } else {
                    return Ok(-1); // No expiration set
                }
            }
        }
        Ok(-2) // Key doesn't exist
    }

    /// Get expiration timestamp in milliseconds
    pub fn get_expire_time_in_db(&self, db_index: usize, key: &str) -> Result<i64> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(-2); // Key doesn't exist (expired)
                }
                if let Some(expires_at) = stored.expires_at {
                    return Ok(expires_at as i64);
                } else {
                    return Ok(-1); // No expiration set
                }
            }
        }
        Ok(-2) // Key doesn't exist
    }

    /// Remove expiration from a key
    pub fn persist_in_db(&self, db_index: usize, key: &str) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(false);
                }
                if stored.expires_at.is_some() {
                    stored.expires_at = None;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Delete a key from a specific database
    pub fn delete_from_db(&self, db_index: usize, key: &str) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            Ok(db.remove(key).is_some())
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
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                return Ok(!stored.is_expired());
            }
        }
        Ok(false)
    }

    /// Check if a key exists (in default database 0)
    pub fn exists(&self, key: &str) -> Result<bool> {
        self.exists_in_db(0, key)
    }

    /// Get all keys in a database
    pub fn get_all_keys_in_db(&self, db_index: usize) -> Result<Vec<String>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            let keys: Vec<String> = db
                .iter()
                .filter(|(_, v)| !v.is_expired())
                .map(|(k, _)| k.clone())
                .collect();
            Ok(keys)
        } else {
            Ok(Vec::new())
        }
    }

    /// Get database size (number of keys)
    pub fn dbsize_in_db(&self, db_index: usize) -> Result<usize> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            let count = db.iter().filter(|(_, v)| !v.is_expired()).count();
            Ok(count)
        } else {
            Ok(0)
        }
    }

    /// Clear a specific database
    pub fn flush_db(&self, db_index: usize) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            db.clear();
        }
        Ok(())
    }

    /// Clear all databases
    pub fn flush_all(&self) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        for db in databases.iter_mut() {
            db.clear();
        }
        Ok(())
    }

    /// Swap two databases
    pub fn swap_db(&self, db1: usize, db2: usize) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if db1 >= databases.len() || db2 >= databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {} or {}",
                db1, db2
            )));
        }

        databases.swap(db1, db2);
        Ok(())
    }

    /// Move a key from one database to another
    pub fn move_key(&self, src_db: usize, dst_db: usize, key: &str) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if src_db >= databases.len() || dst_db >= databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {} or {}",
                src_db, dst_db
            )));
        }

        // Check if key exists in source and not expired
        let value = if let Some(src) = databases.get(src_db) {
            if let Some(stored) = src.get(key) {
                if stored.is_expired() {
                    return Ok(false);
                }
                Some(stored.clone())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(stored_value) = value {
            // Check if key already exists in destination
            if let Some(dst) = databases.get(dst_db) {
                if dst.contains_key(key) {
                    return Ok(false);
                }
            }

            // Remove from source and add to destination
            if let Some(src) = databases.get_mut(src_db) {
                src.remove(key);
            }
            if let Some(dst) = databases.get_mut(dst_db) {
                dst.insert(key.to_string(), stored_value);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Rename a key
    pub fn rename_in_db(&self, db_index: usize, old_key: &str, new_key: &str) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(value) = db.remove(old_key) {
                if value.is_expired() {
                    return Ok(false);
                }
                db.insert(new_key.to_string(), value);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Rename a key only if new key doesn't exist
    pub fn rename_nx_in_db(&self, db_index: usize, old_key: &str, new_key: &str) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if db.contains_key(new_key) {
                return Ok(false);
            }
            if let Some(value) = db.remove(old_key) {
                if value.is_expired() {
                    return Ok(false);
                }
                db.insert(new_key.to_string(), value);
                return Ok(true);
            }
        }
        Ok(false)
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
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if src_db >= databases.len() || dst_db >= databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {} or {}",
                src_db, dst_db
            )));
        }

        // Get value from source
        let value = if let Some(src) = databases.get(src_db) {
            if let Some(stored) = src.get(src_key) {
                if stored.is_expired() {
                    return Ok(false);
                }
                Some(stored.clone())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(stored_value) = value {
            // Check if destination key exists
            if let Some(dst) = databases.get(dst_db) {
                if dst.contains_key(dst_key) && !replace {
                    return Ok(false);
                }
            }

            // Copy to destination
            if let Some(dst) = databases.get_mut(dst_db) {
                dst.insert(dst_key.to_string(), stored_value);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get multiple keys from a specific database
    /// Get a random key from a database
    pub fn random_key_in_db(&self, db_index: usize) -> Result<Option<String>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            let valid_keys: Vec<String> = db
                .iter()
                .filter(|(_, v)| !v.is_expired())
                .map(|(k, _)| k.clone())
                .collect();

            if valid_keys.is_empty() {
                return Ok(None);
            }

            // Simple random selection using current time
            let idx = (Self::current_time_ms() as usize) % valid_keys.len();
            Ok(Some(valid_keys[idx].clone()))
        } else {
            Ok(None)
        }
    }
}

impl Default for StorageAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_get() {
        let storage = StorageAdapter::new();
        storage
            .set("key1".to_string(), Bytes::from("value1"))
            .unwrap();

        let value = storage.get("key1").unwrap();
        assert_eq!(value, Some(Bytes::from("value1")));
    }

    #[test]
    fn test_delete() {
        let storage = StorageAdapter::new();
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
        let storage = StorageAdapter::new();
        storage
            .set("key1".to_string(), Bytes::from("value1"))
            .unwrap();

        assert!(storage.exists("key1").unwrap());
        assert!(!storage.exists("key2").unwrap());
    }

    #[test]
    fn test_mget_mset() {
        let storage = StorageAdapter::new();

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
}
