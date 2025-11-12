use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;

/// Key command handler
pub struct KeyCommands {
    storage: StorageAdapter,
}

impl KeyCommands {
    pub fn new(storage: StorageAdapter) -> Self {
        Self { storage }
    }

    /// KEYS pattern - Find all keys matching pattern
    /// Note: Simplified implementation, supports only * wildcard
    pub fn keys(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("KEYS".to_string()));
        }

        let pattern = String::from_utf8_lossy(&args[0]).to_string();
        let all_keys = self.storage.get_all_keys_in_db(current_db)?;

        // Simple pattern matching: * matches everything, otherwise exact match
        let matched_keys: Vec<RespValue> = if pattern == "*" {
            all_keys
                .into_iter()
                .map(RespValue::bulk_string)
                .collect()
        } else {
            all_keys
                .into_iter()
                .filter(|k| self.match_pattern(k, &pattern))
                .map(RespValue::bulk_string)
                .collect()
        };

        Ok(RespValue::array(matched_keys))
    }

    /// Simple pattern matching helper (supports * and ? wildcards)
    fn match_pattern(&self, key: &str, pattern: &str) -> bool {
        // Simple implementation: exact match or * wildcard
        if pattern == "*" {
            return true;
        }

        // Support basic wildcards
        let pattern_chars: Vec<char> = pattern.chars().collect();
        let key_chars: Vec<char> = key.chars().collect();

        Self::match_pattern_recursive(&key_chars, 0, &pattern_chars, 0)
    }

    fn match_pattern_recursive(
        key: &[char],
        ki: usize,
        pattern: &[char],
        pi: usize,
    ) -> bool {
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

    /// SCAN cursor [MATCH pattern] [COUNT count]
    /// Iterate keys using cursor-based iteration
    pub fn scan(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SCAN".to_string()));
        }

        // Parse cursor
        let cursor_str = String::from_utf8_lossy(&args[0]);
        let cursor = cursor_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid cursor".to_string()))?;

        // Parse optional arguments
        let mut pattern = String::from("*");
        let mut count = 10_usize; // Default count

        let mut i = 1;
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

        // Get all keys and filter by pattern
        let all_keys = self.storage.get_all_keys_in_db(current_db)?;
        let matched_keys: Vec<String> = if pattern == "*" {
            all_keys
        } else {
            all_keys
                .into_iter()
                .filter(|k| self.match_pattern(k, &pattern))
                .collect()
        };

        // Calculate the range to return
        let total_keys = matched_keys.len();
        let start = cursor;
        let end = std::cmp::min(start + count, total_keys);

        // Determine next cursor (0 means iteration complete)
        let next_cursor = if end >= total_keys { 0 } else { end };

        // Collect keys for this iteration
        let keys_to_return: Vec<RespValue> = matched_keys[start..end]
            .iter()
            .map(|k| RespValue::bulk_string(k.clone()))
            .collect();

        // Return [cursor, [keys]]
        Ok(RespValue::array(vec![
            RespValue::bulk_string(next_cursor.to_string()),
            RespValue::array(keys_to_return),
        ]))
    }

    /// RANDOMKEY - Return a random key
    pub fn randomkey(&self, _args: &[Bytes], current_db: usize) -> Result<RespValue> {
        match self.storage.random_key_in_db(current_db)? {
            Some(key) => Ok(RespValue::bulk_string(key)),
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// RENAME key newkey - Rename a key
    pub fn rename(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("RENAME".to_string()));
        }

        let old_key = String::from_utf8_lossy(&args[0]).to_string();
        let new_key = String::from_utf8_lossy(&args[1]).to_string();

        if !self.storage.exists_in_db(current_db, &old_key)? {
            return Err(AikvError::KeyNotFound);
        }

        self.storage.rename_in_db(current_db, &old_key, &new_key)?;
        Ok(RespValue::ok())
    }

    /// RENAMENX key newkey - Rename key only if newkey doesn't exist
    pub fn renamenx(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("RENAMENX".to_string()));
        }

        let old_key = String::from_utf8_lossy(&args[0]).to_string();
        let new_key = String::from_utf8_lossy(&args[1]).to_string();

        if !self.storage.exists_in_db(current_db, &old_key)? {
            return Err(AikvError::KeyNotFound);
        }

        let renamed = self.storage.rename_nx_in_db(current_db, &old_key, &new_key)?;
        Ok(RespValue::integer(if renamed { 1 } else { 0 }))
    }

    /// TYPE key - Return the type of the value stored at key
    pub fn get_type(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("TYPE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        if !self.storage.exists_in_db(current_db, &key)? {
            return Ok(RespValue::simple_string("none"));
        }

        // For now, we only support string type
        // In future, we'll need to track the actual type
        Ok(RespValue::simple_string("string"))
    }

    /// COPY source destination [DB destination-db] [REPLACE]
    pub fn copy(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("COPY".to_string()));
        }

        let src_key = String::from_utf8_lossy(&args[0]).to_string();
        let dst_key = String::from_utf8_lossy(&args[1]).to_string();

        let mut dest_db = current_db;
        let mut replace = false;

        // Parse options
        let mut i = 2;
        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "DB" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument(
                            "ERR syntax error".to_string(),
                        ));
                    }
                    i += 1;
                    let db_str = String::from_utf8_lossy(&args[i]);
                    dest_db = db_str.parse::<usize>().map_err(|_| {
                        AikvError::InvalidArgument("ERR invalid DB index".to_string())
                    })?;
                }
                "REPLACE" => {
                    replace = true;
                }
                _ => {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
            }
            i += 1;
        }

        let copied = self
            .storage
            .copy_in_db(current_db, dest_db, &src_key, &dst_key, replace)?;
        Ok(RespValue::integer(if copied { 1 } else { 0 }))
    }

    /// EXPIRE key seconds - Set a key's time to live in seconds
    pub fn expire(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("EXPIRE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let seconds_str = String::from_utf8_lossy(&args[1]);
        let seconds = seconds_str
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("ERR value is not an integer".to_string()))?;

        if seconds <= 0 {
            // Delete the key immediately if seconds <= 0
            let deleted = self.storage.delete_from_db(current_db, &key)?;
            return Ok(RespValue::integer(if deleted { 1 } else { 0 }));
        }

        let expire_ms = (seconds as u64) * 1000;
        let set = self.storage.set_expire_in_db(current_db, &key, expire_ms)?;
        Ok(RespValue::integer(if set { 1 } else { 0 }))
    }

    /// EXPIREAT key timestamp - Set expiration as UNIX timestamp in seconds
    pub fn expireat(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("EXPIREAT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let timestamp_str = String::from_utf8_lossy(&args[1]);
        let timestamp = timestamp_str
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("ERR value is not an integer".to_string()))?;

        if timestamp <= 0 {
            let deleted = self.storage.delete_from_db(current_db, &key)?;
            return Ok(RespValue::integer(if deleted { 1 } else { 0 }));
        }

        let timestamp_ms = (timestamp as u64) * 1000;
        let set = self
            .storage
            .set_expire_at_in_db(current_db, &key, timestamp_ms)?;
        Ok(RespValue::integer(if set { 1 } else { 0 }))
    }

    /// PEXPIRE key milliseconds - Set expiration in milliseconds
    pub fn pexpire(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("PEXPIRE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let ms_str = String::from_utf8_lossy(&args[1]);
        let milliseconds = ms_str
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("ERR value is not an integer".to_string()))?;

        if milliseconds <= 0 {
            let deleted = self.storage.delete_from_db(current_db, &key)?;
            return Ok(RespValue::integer(if deleted { 1 } else { 0 }));
        }

        let set = self
            .storage
            .set_expire_in_db(current_db, &key, milliseconds as u64)?;
        Ok(RespValue::integer(if set { 1 } else { 0 }))
    }

    /// PEXPIREAT key milliseconds-timestamp - Set expiration as UNIX timestamp in milliseconds
    pub fn pexpireat(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("PEXPIREAT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let timestamp_str = String::from_utf8_lossy(&args[1]);
        let timestamp_ms = timestamp_str
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("ERR value is not an integer".to_string()))?;

        if timestamp_ms <= 0 {
            let deleted = self.storage.delete_from_db(current_db, &key)?;
            return Ok(RespValue::integer(if deleted { 1 } else { 0 }));
        }

        let set = self
            .storage
            .set_expire_at_in_db(current_db, &key, timestamp_ms as u64)?;
        Ok(RespValue::integer(if set { 1 } else { 0 }))
    }

    /// TTL key - Get the time to live for a key in seconds
    pub fn ttl(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("TTL".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let ttl_ms = self.storage.get_ttl_in_db(current_db, &key)?;

        let ttl_seconds = if ttl_ms > 0 {
            ttl_ms / 1000
        } else {
            ttl_ms
        };

        Ok(RespValue::integer(ttl_seconds))
    }

    /// PTTL key - Get the time to live for a key in milliseconds
    pub fn pttl(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("PTTL".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let ttl_ms = self.storage.get_ttl_in_db(current_db, &key)?;

        Ok(RespValue::integer(ttl_ms))
    }

    /// PERSIST key - Remove the expiration from a key
    pub fn persist(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("PERSIST".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let persisted = self.storage.persist_in_db(current_db, &key)?;

        Ok(RespValue::integer(if persisted { 1 } else { 0 }))
    }

    /// EXPIRETIME key - Get the expiration Unix timestamp in seconds
    pub fn expiretime(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("EXPIRETIME".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let expire_time_ms = self.storage.get_expire_time_in_db(current_db, &key)?;

        let expire_time_seconds = if expire_time_ms > 0 {
            expire_time_ms / 1000
        } else {
            expire_time_ms
        };

        Ok(RespValue::integer(expire_time_seconds))
    }

    /// PEXPIRETIME key - Get the expiration Unix timestamp in milliseconds
    pub fn pexpiretime(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("PEXPIRETIME".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let expire_time_ms = self.storage.get_expire_time_in_db(current_db, &key)?;

        Ok(RespValue::integer(expire_time_ms))
    }
}
