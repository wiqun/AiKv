use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{StorageEngine, StoredValue};
use bytes::Bytes;
use std::collections::HashMap;

/// Hash command handler
pub struct HashCommands {
    storage: StorageEngine,
}

impl HashCommands {
    pub fn new(storage: StorageEngine) -> Self {
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

    /// HMSET key field value [field value ...]
    /// Sets multiple field-value pairs in the hash stored at key
    /// This command is deprecated in favor of HSET, but still supported for compatibility
    pub fn hmset(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
            return Err(AikvError::WrongArgCount("HMSET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Get existing hash or create new one
        let mut hash = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        // Set all field-value pairs
        for i in (1..args.len()).step_by(2) {
            let field = String::from_utf8_lossy(&args[i]).to_string();
            let value = args[i + 1].clone();
            hash.insert(field, value);
        }

        self.storage
            .set_value(db_index, key, StoredValue::new_hash(hash))?;

        // HMSET returns OK, unlike HSET which returns the number of new fields
        Ok(RespValue::ok())
    }

    /// HSCAN key cursor [MATCH pattern] [COUNT count]
    /// Iterates fields of a hash stored at key using cursor-based iteration
    pub fn hscan(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("HSCAN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Parse cursor
        let cursor_str = String::from_utf8_lossy(&args[1]);
        let cursor = cursor_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid cursor".to_string()))?;

        // Parse optional arguments
        let mut pattern = String::from("*");
        let mut count = 10_usize; // Default count

        let mut i = 2;
        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "MATCH" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    pattern = String::from_utf8_lossy(&args[i]).to_string();
                }
                "COUNT" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    let count_str = String::from_utf8_lossy(&args[i]);
                    count = count_str.parse::<usize>().map_err(|_| {
                        AikvError::InvalidArgument("ERR value is not an integer".to_string())
                    })?;
                    if count == 0 {
                        count = 1; // Minimum count is 1
                    }
                }
                _ => {
                    return Err(AikvError::InvalidArgument(format!(
                        "ERR unknown option '{}'",
                        option
                    )));
                }
            }
            i += 1;
        }

        // Get hash fields
        let hash = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_hash()?.clone()
        } else {
            HashMap::new()
        };

        // Convert hash to sorted list of (field, value) pairs for consistent iteration
        let mut fields: Vec<(String, Bytes)> = hash.into_iter().collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));

        // Filter by pattern if not "*"
        let matched_fields: Vec<(String, Bytes)> = if pattern == "*" {
            fields
        } else {
            fields
                .into_iter()
                .filter(|(field, _)| Self::match_pattern(field, &pattern))
                .collect()
        };

        // Calculate the range to return
        let total_fields = matched_fields.len();
        let start = cursor;
        let end = std::cmp::min(start + count, total_fields);

        // Determine next cursor (0 means iteration complete)
        let next_cursor = if end >= total_fields { 0 } else { end };

        // Collect field-value pairs for this iteration
        let mut result_items = Vec::new();
        for (field, value) in matched_fields.into_iter().skip(start).take(count) {
            result_items.push(RespValue::bulk_string(Bytes::from(field)));
            result_items.push(RespValue::bulk_string(value));
        }

        // Return [cursor, [field, value, field, value, ...]]
        Ok(RespValue::array(vec![
            RespValue::bulk_string(next_cursor.to_string()),
            RespValue::array(result_items),
        ]))
    }

    /// Simple pattern matching helper (supports * and ? wildcards)
    fn match_pattern(key: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }

        let pattern_chars: Vec<char> = pattern.chars().collect();
        let key_chars: Vec<char> = key.chars().collect();

        Self::match_pattern_recursive(&key_chars, 0, &pattern_chars, 0)
    }

    fn match_pattern_recursive(key: &[char], ki: usize, pattern: &[char], pi: usize) -> bool {
        if pi == pattern.len() {
            return ki == key.len();
        }

        if pattern[pi] == '*' {
            // Try matching zero or more characters
            for i in ki..=key.len() {
                if Self::match_pattern_recursive(key, i, pattern, pi + 1) {
                    return true;
                }
            }
            false
        } else if pattern[pi] == '?' {
            // Match exactly one character
            if ki < key.len() {
                Self::match_pattern_recursive(key, ki + 1, pattern, pi + 1)
            } else {
                false
            }
        } else {
            // Exact character match
            if ki < key.len() && key[ki] == pattern[pi] {
                Self::match_pattern_recursive(key, ki + 1, pattern, pi + 1)
            } else {
                false
            }
        }
    }
}
