use crate::error::{AikvError, Result};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Value with optional expiration time
#[derive(Clone, Debug)]
struct StoredValue {
    data: Bytes,
    /// Expiration time in milliseconds since UNIX epoch
    expires_at: Option<u64>,
}

impl StoredValue {
    fn new(data: Bytes) -> Self {
        Self {
            data,
            expires_at: None,
        }
    }

    fn with_expiration(data: Bytes, expires_at: u64) -> Self {
        Self {
            data,
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
                return Ok(Some(stored.data.clone()));
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
            db.insert(key, StoredValue::new(value));
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
            db.insert(key, StoredValue::with_expiration(value, expires_at));
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
    pub fn set_expire_at_in_db(&self, db_index: usize, key: &str, timestamp_ms: u64) -> Result<bool> {
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
                        result.push(Some(stored.data.clone()));
                        continue;
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
                db.insert(key, StoredValue::new(value));
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
