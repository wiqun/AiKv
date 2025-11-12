use crate::error::{AikvError, Result};
use bytes::Bytes;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Different value types supported by the storage
#[derive(Clone, Debug)]
enum ValueType {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<String, Bytes>),
    Set(HashSet<Vec<u8>>), // Using Vec<u8> instead of Bytes for HashSet compatibility
    ZSet(BTreeMap<Vec<u8>, f64>), // member -> score mapping
}

/// Value with optional expiration time
#[derive(Clone, Debug)]
struct StoredValue {
    value: ValueType,
    /// Expiration time in milliseconds since UNIX epoch
    expires_at: Option<u64>,
}

impl StoredValue {
    fn new_string(data: Bytes) -> Self {
        Self {
            value: ValueType::String(data),
            expires_at: None,
        }
    }

    fn new_list(list: VecDeque<Bytes>) -> Self {
        Self {
            value: ValueType::List(list),
            expires_at: None,
        }
    }

    fn new_hash(hash: HashMap<String, Bytes>) -> Self {
        Self {
            value: ValueType::Hash(hash),
            expires_at: None,
        }
    }

    fn new_set(set: HashSet<Vec<u8>>) -> Self {
        Self {
            value: ValueType::Set(set),
            expires_at: None,
        }
    }

    fn new_zset(zset: BTreeMap<Vec<u8>, f64>) -> Self {
        Self {
            value: ValueType::ZSet(zset),
            expires_at: None,
        }
    }

    fn with_expiration(value: ValueType, expires_at: u64) -> Self {
        Self {
            value,
            expires_at: Some(expires_at),
        }
    }

    fn is_expired(&self) -> bool {
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

    #[allow(dead_code)]
    fn get_type_name(&self) -> &str {
        match &self.value {
            ValueType::String(_) => "string",
            ValueType::List(_) => "list",
            ValueType::Hash(_) => "hash",
            ValueType::Set(_) => "set",
            ValueType::ZSet(_) => "zset",
        }
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
    pub fn mget_from_db(&self, db_index: usize, keys: &[String]) -> Result<Vec<Option<Bytes>>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        let mut result = Vec::with_capacity(keys.len());
        if let Some(db) = databases.get(db_index) {
            for key in keys {
                if let Some(stored) = db.get(key) {
                    if !stored.is_expired() {
                        if let ValueType::String(data) = &stored.value {
                            result.push(Some(data.clone()));
                            continue;
                        }
                    }
                }
                result.push(None);
            }
        } else {
            result.resize(keys.len(), None);
        }
        Ok(result)
    }

    /// Get multiple keys (from default database 0)
    pub fn mget(&self, keys: &[String]) -> Result<Vec<Option<Bytes>>> {
        self.mget_from_db(0, keys)
    }

    /// Set multiple key-value pairs in a specific database
    pub fn mset_in_db(&self, db_index: usize, pairs: Vec<(String, Bytes)>) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            for (key, value) in pairs {
                db.insert(key, StoredValue::new_string(value));
            }
            Ok(())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// Set multiple key-value pairs (in default database 0)
    pub fn mset(&self, pairs: Vec<(String, Bytes)>) -> Result<()> {
        self.mset_in_db(0, pairs)
    }

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

    // ==================== List Operations ====================

    /// LPUSH - Push elements to the head of a list
    pub fn list_lpush_in_db(
        &self,
        db_index: usize,
        key: &str,
        elements: Vec<Bytes>,
    ) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_list(VecDeque::new()));

            if let ValueType::List(list) = &mut stored.value {
                for element in elements.into_iter().rev() {
                    list.push_front(element);
                }
                Ok(list.len())
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// RPUSH - Push elements to the tail of a list
    pub fn list_rpush_in_db(
        &self,
        db_index: usize,
        key: &str,
        elements: Vec<Bytes>,
    ) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_list(VecDeque::new()));

            if let ValueType::List(list) = &mut stored.value {
                for element in elements {
                    list.push_back(element);
                }
                Ok(list.len())
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LPOP - Pop elements from the head of a list
    pub fn list_lpop_in_db(&self, db_index: usize, key: &str, count: usize) -> Result<Vec<Bytes>> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(Vec::new());
                }
                if let ValueType::List(list) = &mut stored.value {
                    let mut result = Vec::new();
                    for _ in 0..count.min(list.len()) {
                        if let Some(val) = list.pop_front() {
                            result.push(val);
                        }
                    }
                    if list.is_empty() {
                        db.remove(key);
                    }
                    Ok(result)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// RPOP - Pop elements from the tail of a list
    pub fn list_rpop_in_db(&self, db_index: usize, key: &str, count: usize) -> Result<Vec<Bytes>> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(Vec::new());
                }
                if let ValueType::List(list) = &mut stored.value {
                    let mut result = Vec::new();
                    for _ in 0..count.min(list.len()) {
                        if let Some(val) = list.pop_back() {
                            result.push(val);
                        }
                    }
                    if list.is_empty() {
                        db.remove(key);
                    }
                    Ok(result)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LLEN - Get the length of a list
    pub fn list_len_in_db(&self, db_index: usize, key: &str) -> Result<usize> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(0);
                }
                if let ValueType::List(list) = &stored.value {
                    Ok(list.len())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LRANGE - Get a range of elements from a list
    pub fn list_range_in_db(
        &self,
        db_index: usize,
        key: &str,
        start: i64,
        stop: i64,
    ) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::List(list) = &stored.value {
                    let len = list.len() as i64;
                    let start_idx = if start < 0 {
                        (len + start).max(0)
                    } else {
                        start.min(len)
                    } as usize;
                    let stop_idx = if stop < 0 {
                        (len + stop).max(-1) + 1
                    } else {
                        (stop + 1).min(len)
                    } as usize;

                    if start_idx >= stop_idx {
                        return Ok(Vec::new());
                    }

                    Ok(list
                        .iter()
                        .skip(start_idx)
                        .take(stop_idx - start_idx)
                        .cloned()
                        .collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LINDEX - Get an element from a list by index
    pub fn list_index_in_db(
        &self,
        db_index: usize,
        key: &str,
        index: i64,
    ) -> Result<Option<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(None);
                }
                if let ValueType::List(list) = &stored.value {
                    let len = list.len() as i64;
                    let idx = if index < 0 { len + index } else { index };

                    if idx >= 0 && idx < len {
                        Ok(list.get(idx as usize).cloned())
                    } else {
                        Ok(None)
                    }
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(None)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LSET - Set the value of an element in a list by index
    pub fn list_set_in_db(
        &self,
        db_index: usize,
        key: &str,
        index: i64,
        element: Bytes,
    ) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    return Err(AikvError::KeyNotFound);
                }
                if let ValueType::List(list) = &mut stored.value {
                    let len = list.len() as i64;
                    let idx = if index < 0 { len + index } else { index };

                    if idx >= 0 && idx < len {
                        if let Some(elem) = list.get_mut(idx as usize) {
                            *elem = element;
                        }
                        Ok(())
                    } else {
                        Err(AikvError::InvalidArgument("index out of range".to_string()))
                    }
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Err(AikvError::KeyNotFound)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LREM - Remove elements from a list
    pub fn list_rem_in_db(
        &self,
        db_index: usize,
        key: &str,
        count: i64,
        element: Bytes,
    ) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(0);
                }
                if let ValueType::List(list) = &mut stored.value {
                    let mut removed = 0;

                    if count == 0 {
                        // Remove all occurrences
                        list.retain(|e| e != &element);
                        removed = list.len();
                    } else if count > 0 {
                        // Remove first count occurrences
                        let mut i = 0;
                        while i < list.len() && removed < count as usize {
                            if list[i] == element {
                                list.remove(i);
                                removed += 1;
                            } else {
                                i += 1;
                            }
                        }
                    } else {
                        // Remove last -count occurrences (from tail)
                        let target = (-count) as usize;
                        let mut i = list.len();
                        while i > 0 && removed < target {
                            i -= 1;
                            if list[i] == element {
                                list.remove(i);
                                removed += 1;
                            }
                        }
                    }

                    if list.is_empty() {
                        db.remove(key);
                    }
                    Ok(removed)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// LTRIM - Trim a list to the specified range
    pub fn list_trim_in_db(&self, db_index: usize, key: &str, start: i64, stop: i64) -> Result<()> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(());
                }
                if let ValueType::List(list) = &mut stored.value {
                    let len = list.len() as i64;
                    let start_idx = if start < 0 {
                        (len + start).max(0)
                    } else {
                        start.min(len)
                    } as usize;
                    let stop_idx = if stop < 0 {
                        (len + stop).max(-1) + 1
                    } else {
                        (stop + 1).min(len)
                    } as usize;

                    if start_idx >= stop_idx || start_idx >= list.len() {
                        db.remove(key);
                    } else {
                        let new_list: VecDeque<Bytes> = list
                            .iter()
                            .skip(start_idx)
                            .take(stop_idx - start_idx)
                            .cloned()
                            .collect();
                        *list = new_list;
                    }
                    Ok(())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    // ==================== Hash Operations ====================

    /// HSET - Set field in hash
    pub fn hash_set_in_db(
        &self,
        db_index: usize,
        key: &str,
        fields: Vec<(String, Bytes)>,
    ) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_hash(HashMap::new()));

            if let ValueType::Hash(hash) = &mut stored.value {
                let mut count = 0;
                for (field, value) in fields {
                    if hash.insert(field, value).is_none() {
                        count += 1;
                    }
                }
                Ok(count)
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HSETNX - Set field in hash only if it doesn't exist
    pub fn hash_setnx_in_db(
        &self,
        db_index: usize,
        key: &str,
        field: String,
        value: Bytes,
    ) -> Result<bool> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_hash(HashMap::new()));

            if let ValueType::Hash(hash) = &mut stored.value {
                if let std::collections::hash_map::Entry::Vacant(e) = hash.entry(field) {
                    e.insert(value);
                    Ok(true)
                } else {
                    Ok(false)
                }
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HGET - Get field value from hash
    pub fn hash_get_in_db(&self, db_index: usize, key: &str, field: &str) -> Result<Option<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(None);
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(hash.get(field).cloned())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(None)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HMGET - Get multiple field values from hash
    pub fn hash_mget_in_db(
        &self,
        db_index: usize,
        key: &str,
        fields: &[String],
    ) -> Result<Vec<Option<Bytes>>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(vec![None; fields.len()]);
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(fields.iter().map(|f| hash.get(f).cloned()).collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(vec![None; fields.len()])
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HDEL - Delete fields from hash
    pub fn hash_del_in_db(&self, db_index: usize, key: &str, fields: &[String]) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(0);
                }
                if let ValueType::Hash(hash) = &mut stored.value {
                    let mut count = 0;
                    for field in fields {
                        if hash.remove(field).is_some() {
                            count += 1;
                        }
                    }
                    if hash.is_empty() {
                        db.remove(key);
                    }
                    Ok(count)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HEXISTS - Check if field exists in hash
    pub fn hash_exists_in_db(&self, db_index: usize, key: &str, field: &str) -> Result<bool> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(false);
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(hash.contains_key(field))
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(false)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HLEN - Get number of fields in hash
    pub fn hash_len_in_db(&self, db_index: usize, key: &str) -> Result<usize> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(0);
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(hash.len())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HKEYS - Get all field names in hash
    pub fn hash_keys_in_db(&self, db_index: usize, key: &str) -> Result<Vec<String>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(hash.keys().cloned().collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HVALS - Get all values in hash
    pub fn hash_vals_in_db(&self, db_index: usize, key: &str) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(hash.values().cloned().collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HGETALL - Get all fields and values in hash
    pub fn hash_getall_in_db(&self, db_index: usize, key: &str) -> Result<Vec<(String, Bytes)>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::Hash(hash) = &stored.value {
                    Ok(hash.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HINCRBY - Increment integer value of hash field
    pub fn hash_incrby_in_db(
        &self,
        db_index: usize,
        key: &str,
        field: &str,
        increment: i64,
    ) -> Result<i64> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_hash(HashMap::new()));

            if let ValueType::Hash(hash) = &mut stored.value {
                let current = if let Some(val) = hash.get(field) {
                    String::from_utf8_lossy(val).parse::<i64>().map_err(|_| {
                        AikvError::InvalidArgument("hash value is not an integer".to_string())
                    })?
                } else {
                    0
                };
                let new_val = current + increment;
                hash.insert(field.to_string(), Bytes::from(new_val.to_string()));
                Ok(new_val)
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// HINCRBYFLOAT - Increment float value of hash field
    pub fn hash_incrbyfloat_in_db(
        &self,
        db_index: usize,
        key: &str,
        field: &str,
        increment: f64,
    ) -> Result<f64> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_hash(HashMap::new()));

            if let ValueType::Hash(hash) = &mut stored.value {
                let current = if let Some(val) = hash.get(field) {
                    String::from_utf8_lossy(val).parse::<f64>().map_err(|_| {
                        AikvError::InvalidArgument("hash value is not a float".to_string())
                    })?
                } else {
                    0.0
                };
                let new_val = current + increment;
                hash.insert(field.to_string(), Bytes::from(new_val.to_string()));
                Ok(new_val)
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    // ==================== Set Operations ====================

    /// SADD - Add members to a set
    pub fn set_add_in_db(&self, db_index: usize, key: &str, members: Vec<Bytes>) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_set(HashSet::new()));

            if let ValueType::Set(set) = &mut stored.value {
                let mut count = 0;
                for member in members {
                    if set.insert(member.to_vec()) {
                        count += 1;
                    }
                }
                Ok(count)
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SREM - Remove members from a set
    pub fn set_rem_in_db(&self, db_index: usize, key: &str, members: Vec<Bytes>) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(0);
                }
                if let ValueType::Set(set) = &mut stored.value {
                    let mut count = 0;
                    for member in members {
                        if set.remove(&member.to_vec()) {
                            count += 1;
                        }
                    }
                    if set.is_empty() {
                        db.remove(key);
                    }
                    Ok(count)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SISMEMBER - Check if member exists in set
    pub fn set_ismember_in_db(&self, db_index: usize, key: &str, member: &Bytes) -> Result<bool> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(false);
                }
                if let ValueType::Set(set) = &stored.value {
                    Ok(set.contains(&member.to_vec()))
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(false)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SMEMBERS - Get all members of a set
    pub fn set_members_in_db(&self, db_index: usize, key: &str) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::Set(set) = &stored.value {
                    Ok(set.iter().map(|v| Bytes::from(v.clone())).collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SCARD - Get the number of members in a set
    pub fn set_card_in_db(&self, db_index: usize, key: &str) -> Result<usize> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(0);
                }
                if let ValueType::Set(set) = &stored.value {
                    Ok(set.len())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SPOP - Remove and return random members from a set
    pub fn set_pop_in_db(&self, db_index: usize, key: &str, count: usize) -> Result<Vec<Bytes>> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(Vec::new());
                }
                if let ValueType::Set(set) = &mut stored.value {
                    let mut result = Vec::new();
                    let members: Vec<Vec<u8>> = set.iter().take(count).cloned().collect();
                    for member in members {
                        set.remove(&member);
                        result.push(Bytes::from(member));
                    }
                    if set.is_empty() {
                        db.remove(key);
                    }
                    Ok(result)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SRANDMEMBER - Get random members from a set
    pub fn set_randmember_in_db(
        &self,
        db_index: usize,
        key: &str,
        count: i64,
    ) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::Set(set) = &stored.value {
                    let members: Vec<Bytes> = set
                        .iter()
                        .take(count.unsigned_abs() as usize)
                        .map(|v| Bytes::from(v.clone()))
                        .collect();
                    Ok(members)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SUNION - Union of multiple sets
    pub fn set_union_in_db(&self, db_index: usize, keys: &[String]) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            let mut result = HashSet::new();
            for key in keys {
                if let Some(stored) = db.get(key) {
                    if !stored.is_expired() {
                        if let ValueType::Set(set) = &stored.value {
                            result.extend(set.iter().cloned());
                        } else {
                            return Err(AikvError::WrongType(
                                "Operation against a key holding the wrong kind of value"
                                    .to_string(),
                            ));
                        }
                    }
                }
            }
            Ok(result.into_iter().map(Bytes::from).collect())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SINTER - Intersection of multiple sets
    pub fn set_inter_in_db(&self, db_index: usize, keys: &[String]) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if keys.is_empty() {
                return Ok(Vec::new());
            }

            // Start with the first set
            let mut result: Option<HashSet<Vec<u8>>> = None;

            for key in keys {
                if let Some(stored) = db.get(key) {
                    if !stored.is_expired() {
                        if let ValueType::Set(set) = &stored.value {
                            if let Some(res) = &mut result {
                                *res = res.intersection(set).cloned().collect();
                            } else {
                                result = Some(set.clone());
                            }
                        } else {
                            return Err(AikvError::WrongType(
                                "Operation against a key holding the wrong kind of value"
                                    .to_string(),
                            ));
                        }
                    } else {
                        return Ok(Vec::new());
                    }
                } else {
                    return Ok(Vec::new());
                }
            }

            Ok(result
                .unwrap_or_default()
                .into_iter()
                .map(Bytes::from)
                .collect())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SDIFF - Difference of multiple sets
    pub fn set_diff_in_db(&self, db_index: usize, keys: &[String]) -> Result<Vec<Bytes>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if keys.is_empty() {
                return Ok(Vec::new());
            }

            // Start with the first set
            let mut result: HashSet<Vec<u8>> = HashSet::new();

            if let Some(stored) = db.get(&keys[0]) {
                if !stored.is_expired() {
                    if let ValueType::Set(set) = &stored.value {
                        result = set.clone();
                    } else {
                        return Err(AikvError::WrongType(
                            "Operation against a key holding the wrong kind of value".to_string(),
                        ));
                    }
                }
            }

            // Subtract all other sets
            for key in &keys[1..] {
                if let Some(stored) = db.get(key) {
                    if !stored.is_expired() {
                        if let ValueType::Set(set) = &stored.value {
                            result = result.difference(set).cloned().collect();
                        } else {
                            return Err(AikvError::WrongType(
                                "Operation against a key holding the wrong kind of value"
                                    .to_string(),
                            ));
                        }
                    }
                }
            }

            Ok(result.into_iter().map(Bytes::from).collect())
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SUNIONSTORE - Union of multiple sets and store result
    pub fn set_unionstore_in_db(
        &self,
        db_index: usize,
        dest: &str,
        keys: &[String],
    ) -> Result<usize> {
        let members = self.set_union_in_db(db_index, keys)?;
        let count = members.len();

        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let set: HashSet<Vec<u8>> = members.into_iter().map(|b| b.to_vec()).collect();
            db.insert(dest.to_string(), StoredValue::new_set(set));
            Ok(count)
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SINTERSTORE - Intersection of multiple sets and store result
    pub fn set_interstore_in_db(
        &self,
        db_index: usize,
        dest: &str,
        keys: &[String],
    ) -> Result<usize> {
        let members = self.set_inter_in_db(db_index, keys)?;
        let count = members.len();

        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let set: HashSet<Vec<u8>> = members.into_iter().map(|b| b.to_vec()).collect();
            db.insert(dest.to_string(), StoredValue::new_set(set));
            Ok(count)
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// SDIFFSTORE - Difference of multiple sets and store result
    pub fn set_diffstore_in_db(
        &self,
        db_index: usize,
        dest: &str,
        keys: &[String],
    ) -> Result<usize> {
        let members = self.set_diff_in_db(db_index, keys)?;
        let count = members.len();

        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let set: HashSet<Vec<u8>> = members.into_iter().map(|b| b.to_vec()).collect();
            db.insert(dest.to_string(), StoredValue::new_set(set));
            Ok(count)
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    // ==================== Sorted Set (ZSet) Operations ====================

    /// ZADD - Add members with scores to a sorted set
    pub fn zset_add_in_db(
        &self,
        db_index: usize,
        key: &str,
        members: Vec<(f64, Bytes)>,
    ) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_zset(BTreeMap::new()));

            if let ValueType::ZSet(zset) = &mut stored.value {
                let mut count = 0;
                for (score, member) in members {
                    if zset.insert(member.to_vec(), score).is_none() {
                        count += 1;
                    }
                }
                Ok(count)
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZREM - Remove members from a sorted set
    pub fn zset_rem_in_db(&self, db_index: usize, key: &str, members: Vec<Bytes>) -> Result<usize> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            if let Some(stored) = db.get_mut(key) {
                if stored.is_expired() {
                    db.remove(key);
                    return Ok(0);
                }
                if let ValueType::ZSet(zset) = &mut stored.value {
                    let mut count = 0;
                    for member in members {
                        if zset.remove(&member.to_vec()).is_some() {
                            count += 1;
                        }
                    }
                    if zset.is_empty() {
                        db.remove(key);
                    }
                    Ok(count)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZSCORE - Get the score of a member in a sorted set
    pub fn zset_score_in_db(
        &self,
        db_index: usize,
        key: &str,
        member: &Bytes,
    ) -> Result<Option<f64>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(None);
                }
                if let ValueType::ZSet(zset) = &stored.value {
                    Ok(zset.get(&member.to_vec()).copied())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(None)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZRANK/ZREVRANK - Get the rank of a member in a sorted set
    pub fn zset_rank_in_db(
        &self,
        db_index: usize,
        key: &str,
        member: &Bytes,
        reverse: bool,
    ) -> Result<Option<usize>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(None);
                }
                if let ValueType::ZSet(zset) = &stored.value {
                    let member_vec = member.to_vec();
                    if !zset.contains_key(&member_vec) {
                        return Ok(None);
                    }

                    // Create sorted vec by score
                    let mut sorted: Vec<_> = zset.iter().collect();
                    sorted.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap());

                    if reverse {
                        sorted.reverse();
                    }

                    for (idx, (m, _)) in sorted.iter().enumerate() {
                        if *m == &member_vec {
                            return Ok(Some(idx));
                        }
                    }
                    Ok(None)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(None)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZRANGE/ZREVRANGE - Get a range of members from a sorted set
    pub fn zset_range_in_db(
        &self,
        db_index: usize,
        key: &str,
        start: i64,
        stop: i64,
        reverse: bool,
    ) -> Result<Vec<(Bytes, f64)>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::ZSet(zset) = &stored.value {
                    let mut sorted: Vec<_> = zset.iter().collect();
                    sorted.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap());

                    if reverse {
                        sorted.reverse();
                    }

                    let len = sorted.len() as i64;
                    let start_idx = if start < 0 {
                        (len + start).max(0)
                    } else {
                        start.min(len)
                    } as usize;
                    let stop_idx = if stop < 0 {
                        (len + stop).max(-1) + 1
                    } else {
                        (stop + 1).min(len)
                    } as usize;

                    if start_idx >= stop_idx {
                        return Ok(Vec::new());
                    }

                    Ok(sorted
                        .iter()
                        .skip(start_idx)
                        .take(stop_idx - start_idx)
                        .map(|(m, s)| (Bytes::from(m.to_vec()), **s))
                        .collect())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZRANGEBYSCORE/ZREVRANGEBYSCORE - Get members by score range
    pub fn zset_rangebyscore_in_db(
        &self,
        db_index: usize,
        key: &str,
        min: f64,
        max: f64,
        reverse: bool,
    ) -> Result<Vec<(Bytes, f64)>> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(Vec::new());
                }
                if let ValueType::ZSet(zset) = &stored.value {
                    let mut result: Vec<_> = zset
                        .iter()
                        .filter(|(_, s)| **s >= min && **s <= max)
                        .map(|(m, s)| (Bytes::from(m.to_vec()), *s))
                        .collect();

                    result.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

                    if reverse {
                        result.reverse();
                    }

                    Ok(result)
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(Vec::new())
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZCARD - Get the number of members in a sorted set
    pub fn zset_card_in_db(&self, db_index: usize, key: &str) -> Result<usize> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(0);
                }
                if let ValueType::ZSet(zset) = &stored.value {
                    Ok(zset.len())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZCOUNT - Count members with scores in a range
    pub fn zset_count_in_db(
        &self,
        db_index: usize,
        key: &str,
        min: f64,
        max: f64,
    ) -> Result<usize> {
        let databases = self
            .databases
            .read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get(db_index) {
            if let Some(stored) = db.get(key) {
                if stored.is_expired() {
                    return Ok(0);
                }
                if let ValueType::ZSet(zset) = &stored.value {
                    Ok(zset.values().filter(|s| **s >= min && **s <= max).count())
                } else {
                    Err(AikvError::WrongType(
                        "Operation against a key holding the wrong kind of value".to_string(),
                    ))
                }
            } else {
                Ok(0)
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
        }
    }

    /// ZINCRBY - Increment the score of a member in a sorted set
    pub fn zset_incrby_in_db(
        &self,
        db_index: usize,
        key: &str,
        increment: f64,
        member: Bytes,
    ) -> Result<f64> {
        let mut databases = self
            .databases
            .write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;

        if let Some(db) = databases.get_mut(db_index) {
            let stored = db
                .entry(key.to_string())
                .or_insert_with(|| StoredValue::new_zset(BTreeMap::new()));

            if let ValueType::ZSet(zset) = &mut stored.value {
                let member_vec = member.to_vec();
                let current = zset.get(&member_vec).copied().unwrap_or(0.0);
                let new_score = current + increment;
                zset.insert(member_vec, new_score);
                Ok(new_score)
            } else {
                Err(AikvError::WrongType(
                    "Operation against a key holding the wrong kind of value".to_string(),
                ))
            }
        } else {
            Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )))
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

        storage
            .mset(vec![
                ("key1".to_string(), Bytes::from("value1")),
                ("key2".to_string(), Bytes::from("value2")),
            ])
            .unwrap();

        let values = storage
            .mget(&["key1".to_string(), "key2".to_string(), "key3".to_string()])
            .unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], Some(Bytes::from("value1")));
        assert_eq!(values[1], Some(Bytes::from("value2")));
        assert_eq!(values[2], None);
    }
}
