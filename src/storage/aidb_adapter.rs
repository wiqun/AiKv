use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use bytes::Bytes;
use crate::error::{AikvError, Result};

/// Simple in-memory storage adapter
/// This will be replaced with AiDb integration in the future
#[derive(Clone)]
pub struct StorageAdapter {
    data: Arc<RwLock<HashMap<String, Bytes>>>,
}

impl StorageAdapter {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a value by key
    pub fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let data = self.data.read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
        Ok(data.get(key).cloned())
    }

    /// Set a value for a key
    pub fn set(&self, key: String, value: Bytes) -> Result<()> {
        let mut data = self.data.write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
        data.insert(key, value);
        Ok(())
    }

    /// Delete a key
    pub fn delete(&self, key: &str) -> Result<bool> {
        let mut data = self.data.write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
        Ok(data.remove(key).is_some())
    }

    /// Check if a key exists
    pub fn exists(&self, key: &str) -> Result<bool> {
        let data = self.data.read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
        Ok(data.contains_key(key))
    }

    /// Get multiple keys
    pub fn mget(&self, keys: &[String]) -> Result<Vec<Option<Bytes>>> {
        let data = self.data.read()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
        
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            result.push(data.get(key).cloned());
        }
        Ok(result)
    }

    /// Set multiple key-value pairs
    pub fn mset(&self, pairs: Vec<(String, Bytes)>) -> Result<()> {
        let mut data = self.data.write()
            .map_err(|e| AikvError::Storage(format!("Lock error: {}", e)))?;
        
        for (key, value) in pairs {
            data.insert(key, value);
        }
        Ok(())
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
        storage.set("key1".to_string(), Bytes::from("value1")).unwrap();
        
        let value = storage.get("key1").unwrap();
        assert_eq!(value, Some(Bytes::from("value1")));
    }

    #[test]
    fn test_delete() {
        let storage = StorageAdapter::new();
        storage.set("key1".to_string(), Bytes::from("value1")).unwrap();
        
        let deleted = storage.delete("key1").unwrap();
        assert!(deleted);
        
        let value = storage.get("key1").unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_exists() {
        let storage = StorageAdapter::new();
        storage.set("key1".to_string(), Bytes::from("value1")).unwrap();
        
        assert!(storage.exists("key1").unwrap());
        assert!(!storage.exists("key2").unwrap());
    }

    #[test]
    fn test_mget_mset() {
        let storage = StorageAdapter::new();
        
        storage.mset(vec![
            ("key1".to_string(), Bytes::from("value1")),
            ("key2".to_string(), Bytes::from("value2")),
        ]).unwrap();
        
        let values = storage.mget(&["key1".to_string(), "key2".to_string(), "key3".to_string()]).unwrap();
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], Some(Bytes::from("value1")));
        assert_eq!(values[1], Some(Bytes::from("value2")));
        assert_eq!(values[2], None);
    }
}
