use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{StorageAdapter, StoredValue};
use bytes::Bytes;
use std::collections::HashMap;

/// Hash command handler
pub struct HashCommands {
    storage: StorageAdapter,
}

impl HashCommands {
    pub fn new(storage: StorageAdapter) -> Self {
        Self {
            storage,
        }
    }

    /// HSET key field value [field value ...]
    /// Sets field in the hash stored at key to value
    pub fn hset(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 || args.len().is_multiple_of(2) {
            return Err(AikvError::WrongArgCount("HSET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let mut hash = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        let mut count = 0;
        for i in (1..args.len()).step_by(2) {
            let field = String::from_utf8_lossy(&args[i]).to_string();
            let value = args[i + 1].clone();
            if hash.insert(field, value).is_none() {
                count += 1;
            }
        }

        self.storage
            .set_value(db_index, key, StoredValue::new_hash(hash))?;
        Ok(RespValue::Integer(count as i64))
    }

    /// HSETNX key field value
    /// Sets field in the hash stored at key to value, only if field does not yet exist
    pub fn hsetnx(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("HSETNX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();
        let value = args[2].clone();

        // Migrated: Logic moved from storage layer to command layer
        let mut hash = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        let set = if let std::collections::hash_map::Entry::Vacant(e) = hash.entry(field) {
            e.insert(value);
            true
        } else {
            false
        };

        if set {
            self.storage
                .set_value(db_index, key, StoredValue::new_hash(hash))?;
        }

        Ok(RespValue::Integer(if set { 1 } else { 0 }))
    }

    /// HGET key field
    /// Returns the value associated with field in the hash stored at key
    pub fn hget(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("HGET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let value = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.get(&field).cloned()
        } else {
            None
        };

        match value {
            Some(value) => Ok(RespValue::bulk_string(value)),
            None => Ok(RespValue::Null),
        }
    }

    /// HMGET key field [field ...]
    /// Returns the values associated with the specified fields in the hash stored at key
    pub fn hmget(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("HMGET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let fields: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        // Migrated: Logic moved from storage layer to command layer
        let values = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let hash = stored.as_hash()?;
            fields.iter().map(|f| hash.get(f).cloned()).collect()
        } else {
            vec![None; fields.len()]
        };

        Ok(RespValue::Array(Some(
            values
                .into_iter()
                .map(|v| match v {
                    Some(val) => RespValue::bulk_string(val),
                    None => RespValue::Null,
                })
                .collect(),
        )))
    }

    /// HDEL key field [field ...]
    /// Removes the specified fields from the hash stored at key
    pub fn hdel(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("HDEL".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let fields: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        // Migrated: Logic moved from storage layer to command layer
        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut hash = stored.as_hash()?.clone();
            let mut deleted = 0;

            for field in fields {
                if hash.remove(&field).is_some() {
                    deleted += 1;
                }
            }

            if hash.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_hash(hash))?;
            }

            deleted
        } else {
            0
        };

        Ok(RespValue::Integer(count as i64))
    }

    /// HEXISTS key field
    /// Returns if field is an existing field in the hash stored at key
    pub fn hexists(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("HEXISTS".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let exists = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.contains_key(&field)
        } else {
            false
        };

        Ok(RespValue::Integer(if exists { 1 } else { 0 }))
    }

    /// HLEN key
    /// Returns the number of fields contained in the hash stored at key
    pub fn hlen(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("HLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let len = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.len()
        } else {
            0
        };

        Ok(RespValue::Integer(len as i64))
    }

    /// HKEYS key
    /// Returns all field names in the hash stored at key
    pub fn hkeys(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("HKEYS".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let keys = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.keys().cloned().collect()
        } else {
            Vec::new()
        };

        Ok(RespValue::Array(Some(
            keys.into_iter()
                .map(|k| RespValue::bulk_string(Bytes::from(k)))
                .collect(),
        )))
    }

    /// HVALS key
    /// Returns all values in the hash stored at key
    pub fn hvals(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("HVALS".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let vals = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.values().cloned().collect()
        } else {
            Vec::new()
        };

        Ok(RespValue::Array(Some(
            vals.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// HGETALL key
    /// Returns all fields and values of the hash stored at key
    pub fn hgetall(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("HGETALL".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let fields = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored
                .as_hash()?
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        } else {
            Vec::new()
        };

        let mut result = Vec::new();
        for (field, value) in fields {
            result.push(RespValue::bulk_string(Bytes::from(field)));
            result.push(RespValue::bulk_string(value));
        }

        Ok(RespValue::Array(Some(result)))
    }

    /// HINCRBY key field increment
    /// Increments the number stored at field in the hash stored at key by increment
    pub fn hincrby(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("HINCRBY".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();
        let increment = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid increment".to_string()))?;

        // Migrated: Logic moved from storage layer to command layer
        let mut hash = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        let current_value = if let Some(val_bytes) = hash.get(&field) {
            String::from_utf8_lossy(val_bytes)
                .parse::<i64>()
                .map_err(|_| {
                    AikvError::InvalidArgument("hash value is not an integer".to_string())
                })?
        } else {
            0
        };

        let new_value = current_value + increment;
        hash.insert(field, Bytes::from(new_value.to_string()));

        self.storage
            .set_value(db_index, key, StoredValue::new_hash(hash))?;
        Ok(RespValue::Integer(new_value))
    }

    /// HINCRBYFLOAT key field increment
    /// Increments the float value stored at field in the hash stored at key by increment
    pub fn hincrbyfloat(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("HINCRBYFLOAT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let field = String::from_utf8_lossy(&args[1]).to_string();
        let increment = String::from_utf8_lossy(&args[2])
            .parse::<f64>()
            .map_err(|_| AikvError::InvalidArgument("invalid increment".to_string()))?;

        // Migrated: Logic moved from storage layer to command layer
        let mut hash = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        let current_value = if let Some(val_bytes) = hash.get(&field) {
            String::from_utf8_lossy(val_bytes)
                .parse::<f64>()
                .map_err(|_| AikvError::InvalidArgument("hash value is not a float".to_string()))?
        } else {
            0.0
        };

        let new_value = current_value + increment;
        hash.insert(field, Bytes::from(new_value.to_string()));

        self.storage
            .set_value(db_index, key, StoredValue::new_hash(hash))?;
        Ok(RespValue::bulk_string(Bytes::from(new_value.to_string())))
    }
}
