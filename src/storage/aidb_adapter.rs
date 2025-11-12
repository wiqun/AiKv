use crate::error::{AikvError, Result};
use aidb::{Options, DB};
use bytes::Bytes;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// AiDb-based storage adapter
/// This adapter uses AiDb as the underlying storage engine
#[derive(Clone)]
pub struct AiDbStorageAdapter {
    /// Multiple databases (default: 16 databases like Redis)
    /// Each database is a separate AiDb instance with its own directory
    databases: Arc<Vec<Arc<DB>>>,
}

impl AiDbStorageAdapter {
    /// Create a new AiDb storage adapter with the given path and database count
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
            let options = Options::default();
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

    /// Get a value by key from a specific database
    pub fn get_from_db(&self, db_index: usize, key: &str) -> Result<Option<Bytes>> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Check if key is expired
        if self.is_expired(db, key_bytes)? {
            // Clean up expired key
            db.delete(key_bytes)
                .map_err(|e| AikvError::Storage(format!("Failed to delete expired key: {}", e)))?;
            let expire_key = Self::expiration_key(key_bytes);
            db.delete(&expire_key)
                .map_err(|e| AikvError::Storage(format!("Failed to delete expiration: {}", e)))?;
            return Ok(None);
        }

        // Get the actual value
        match db
            .get(key_bytes)
            .map_err(|e| AikvError::Storage(format!("Failed to get value: {}", e)))?
        {
            Some(value) => Ok(Some(Bytes::from(value))),
            None => Ok(None),
        }
    }

    /// Get a value by key (from default database 0)
    pub fn get(&self, key: &str) -> Result<Option<Bytes>> {
        self.get_from_db(0, key)
    }

    /// Set a value for a key in a specific database
    pub fn set_in_db(&self, db_index: usize, key: String, value: Bytes) -> Result<()> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        db.put(key.as_bytes(), &value)
            .map_err(|e| AikvError::Storage(format!("Failed to put value: {}", e)))?;
        Ok(())
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
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let db = &self.databases[db_index];
        let key_bytes = key.as_bytes();

        // Set the value
        db.put(key_bytes, &value)
            .map_err(|e| AikvError::Storage(format!("Failed to put value: {}", e)))?;

        // Set the expiration
        let expire_key = Self::expiration_key(key_bytes);
        db.put(&expire_key, &expires_at.to_le_bytes())
            .map_err(|e| AikvError::Storage(format!("Failed to set expiration: {}", e)))?;

        Ok(())
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

    /// Get multiple keys from a specific database
    pub fn mget_from_db(&self, db_index: usize, keys: &[String]) -> Result<Vec<Option<Bytes>>> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            result.push(self.get_from_db(db_index, key)?);
        }
        Ok(result)
    }

    /// Get multiple keys (from default database 0)
    pub fn mget(&self, keys: &[String]) -> Result<Vec<Option<Bytes>>> {
        self.mget_from_db(0, keys)
    }

    /// Set multiple key-value pairs in a specific database
    pub fn mset_in_db(&self, db_index: usize, pairs: Vec<(String, Bytes)>) -> Result<()> {
        if db_index >= self.databases.len() {
            return Err(AikvError::Storage(format!(
                "Invalid database index: {}",
                db_index
            )));
        }

        for (key, value) in pairs {
            self.set_in_db(db_index, key, value)?;
        }
        Ok(())
    }

    /// Set multiple key-value pairs (in default database 0)
    pub fn mset(&self, pairs: Vec<(String, Bytes)>) -> Result<()> {
        self.mset_in_db(0, pairs)
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
                if now_ns.is_multiple_of(5) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
