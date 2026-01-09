use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{SerializableStoredValue, StorageEngine, StoredValue};
use bytes::Bytes;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default number of databases (matching Redis default)
const DEFAULT_DB_COUNT: usize = 16;

/// Key command handler
pub struct KeyCommands {
    storage: StorageEngine,
}

impl KeyCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
        }
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
            all_keys.into_iter().map(RespValue::bulk_string).collect()
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

    /// SCAN cursor \[MATCH pattern\] \[COUNT count\]
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

        let renamed = self
            .storage
            .rename_nx_in_db(current_db, &old_key, &new_key)?;
        Ok(RespValue::integer(if renamed { 1 } else { 0 }))
    }

    /// TYPE key - Return the type of the value stored at key
    pub fn get_type(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("TYPE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        match self.storage.get_value(current_db, &key)? {
            Some(stored_value) => {
                let type_name = stored_value.get_type_name();
                Ok(RespValue::simple_string(type_name))
            }
            None => Ok(RespValue::simple_string("none")),
        }
    }

    /// COPY source destination \[DB destination-db\] \[REPLACE\]
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
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
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

        let ttl_seconds = if ttl_ms > 0 { ttl_ms / 1000 } else { ttl_ms };

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

    /// DUMP key - Serialize the value stored at key in a Redis-specific format
    ///
    /// Returns a serialized representation of the value that can be restored
    /// using the RESTORE command. The serialization format is compatible with
    /// Redis's RDB format structure (simplified version).
    ///
    /// Format:
    /// - 1 byte: type
    /// - variable: serialized value (bincode)
    /// - 2 bytes: RDB version (0x0009)
    /// - 8 bytes: CRC64 checksum
    pub fn dump(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("DUMP".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Get the value
        match self.storage.get_value(current_db, &key)? {
            Some(stored_value) => {
                // Serialize the value
                let serializable = stored_value.to_serializable();
                let serialized = bincode::serialize(&serializable)
                    .map_err(|e| AikvError::Storage(format!("Failed to serialize value: {}", e)))?;

                // Build the dump format:
                // - serialized value
                // - 2 bytes RDB version (0x0009 = 9)
                // - 8 bytes checksum (simplified additive checksum)
                let mut dump_data = serialized;
                dump_data.extend_from_slice(&[0x00, 0x09]); // RDB version 9

                // Calculate a simple 64-bit additive checksum for data integrity
                let checksum = Self::calculate_checksum(&dump_data);
                dump_data.extend_from_slice(&checksum.to_le_bytes());

                Ok(RespValue::bulk_string(Bytes::from(dump_data)))
            }
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// Calculate a simple 64-bit additive checksum for the data
    fn calculate_checksum(data: &[u8]) -> u64 {
        let mut checksum: u64 = 0;
        for (i, byte) in data.iter().enumerate() {
            checksum =
                checksum.wrapping_add((*byte as u64).wrapping_mul((i as u64).wrapping_add(1)));
        }
        checksum
    }

    /// Verify the checksum in the dump data
    fn verify_checksum(data: &[u8]) -> bool {
        if data.len() < 10 {
            return false;
        }

        // Extract checksum (last 8 bytes)
        let stored_checksum = u64::from_le_bytes([
            data[data.len() - 8],
            data[data.len() - 7],
            data[data.len() - 6],
            data[data.len() - 5],
            data[data.len() - 4],
            data[data.len() - 3],
            data[data.len() - 2],
            data[data.len() - 1],
        ]);

        // Calculate checksum on data without the checksum itself
        let calculated_checksum = Self::calculate_checksum(&data[..data.len() - 8]);

        stored_checksum == calculated_checksum
    }

    /// RESTORE key ttl serialized-value \[REPLACE\] \[ABSTTL\] \[IDLETIME seconds\] \[FREQ frequency\]
    ///
    /// Create a key using the provided serialized value, previously obtained using DUMP.
    ///
    /// Arguments:
    /// - key: The key name to create
    /// - ttl: Time to live in milliseconds (0 means no expiration)
    /// - serialized-value: The serialized value from DUMP command
    /// - REPLACE: Replace existing key if present
    /// - ABSTTL: TTL is an absolute Unix timestamp in milliseconds
    pub fn restore(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() < 3 {
            return Err(AikvError::WrongArgCount("RESTORE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let ttl_str = String::from_utf8_lossy(&args[1]);
        let ttl = ttl_str
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid TTL value".to_string()))?;

        let serialized_value = &args[2];

        // Parse options
        let mut replace = false;
        let mut absttl = false;

        let mut i = 3;
        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "REPLACE" => {
                    replace = true;
                }
                "ABSTTL" => {
                    absttl = true;
                }
                "IDLETIME" | "FREQ" => {
                    // These options are accepted but ignored (for Redis compatibility)
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1; // Skip the value
                }
                _ => {
                    return Err(AikvError::InvalidArgument(format!(
                        "ERR syntax error, unknown option: {}",
                        option
                    )));
                }
            }
            i += 1;
        }

        // Check if key already exists
        if !replace && self.storage.exists_in_db(current_db, &key)? {
            return Err(AikvError::InvalidArgument(
                "BUSYKEY Target key name already exists".to_string(),
            ));
        }

        // Verify the serialized value format
        if serialized_value.len() < 10 {
            return Err(AikvError::InvalidArgument(
                "ERR DUMP payload version or checksum are wrong".to_string(),
            ));
        }

        // Verify checksum
        if !Self::verify_checksum(serialized_value) {
            return Err(AikvError::InvalidArgument(
                "ERR DUMP payload version or checksum are wrong".to_string(),
            ));
        }

        // Extract the serialized data (without RDB version and checksum)
        let data_len = serialized_value.len() - 10; // -2 for version, -8 for checksum
        let data = &serialized_value[..data_len];

        // Deserialize the value
        let serializable: SerializableStoredValue = bincode::deserialize(data).map_err(|e| {
            AikvError::InvalidArgument(format!(
                "ERR DUMP payload version or checksum are wrong: {}",
                e
            ))
        })?;

        let mut stored_value = StoredValue::from_serializable(serializable);

        // Set expiration if TTL is provided
        if ttl > 0 {
            let expires_at = if absttl {
                // TTL is an absolute timestamp
                ttl as u64
            } else {
                // TTL is relative (milliseconds from now)
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;
                now + (ttl as u64)
            };
            stored_value.set_expiration(Some(expires_at));
        } else if ttl == 0 {
            // No expiration
            stored_value.set_expiration(None);
        } else {
            // Negative TTL is an error
            return Err(AikvError::InvalidArgument(
                "ERR invalid TTL value, must be >= 0".to_string(),
            ));
        }

        // Store the value
        self.storage.set_value(current_db, key, stored_value)?;

        Ok(RespValue::ok())
    }

    /// MIGRATE host port key|"" destination-db timeout \[COPY\] \[REPLACE\] \[AUTH password\] \[AUTH2 username password\] \[KEYS key \[key ...\]\]
    ///
    /// Atomically transfer a key from a source Redis instance to a destination Redis instance.
    ///
    /// Note: This is a simplified implementation that works within a single AiKv instance.
    /// It simulates migration by moving/copying keys between databases.
    ///
    /// For true cross-instance migration, a network client would need to be implemented.
    pub fn migrate(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() < 5 {
            return Err(AikvError::WrongArgCount("MIGRATE".to_string()));
        }

        let _host = String::from_utf8_lossy(&args[0]).to_string();
        let _port = String::from_utf8_lossy(&args[1]);
        let key_arg = String::from_utf8_lossy(&args[2]).to_string();
        let dest_db_str = String::from_utf8_lossy(&args[3]);
        let dest_db = dest_db_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid DB index".to_string()))?;
        let _timeout_str = String::from_utf8_lossy(&args[4]);
        let _timeout = _timeout_str
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("ERR timeout is not an integer".to_string()))?;

        // Parse options
        let mut copy = false;
        let mut replace = false;
        let mut keys: Vec<String> = Vec::new();

        let mut i = 5;
        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "COPY" => {
                    copy = true;
                }
                "REPLACE" => {
                    replace = true;
                }
                "AUTH" => {
                    // Skip AUTH argument (password)
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                }
                "AUTH2" => {
                    // Skip AUTH2 arguments (username, password)
                    if i + 2 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 2;
                }
                "KEYS" => {
                    // Collect all remaining arguments as keys
                    i += 1;
                    while i < args.len() {
                        keys.push(String::from_utf8_lossy(&args[i]).to_string());
                        i += 1;
                    }
                    break;
                }
                _ => {
                    return Err(AikvError::InvalidArgument(format!(
                        "ERR syntax error, unknown option: {}",
                        option
                    )));
                }
            }
            i += 1;
        }

        // If no KEYS argument, use the single key
        if keys.is_empty() {
            if key_arg.is_empty() {
                return Err(AikvError::InvalidArgument(
                    "ERR empty key specified".to_string(),
                ));
            }
            keys.push(key_arg);
        }

        // Validate destination database
        if dest_db >= DEFAULT_DB_COUNT {
            return Err(AikvError::InvalidArgument(
                "ERR invalid DB index".to_string(),
            ));
        }

        // Process each key
        let mut migrated_count = 0;
        for key in &keys {
            // Check if source key exists
            if !self.storage.exists_in_db(current_db, key)? {
                continue;
            }

            // Check if destination key exists and REPLACE is not set
            if self.storage.exists_in_db(dest_db, key)? && !replace {
                return Err(AikvError::InvalidArgument(
                    "BUSYKEY Target key name already exists".to_string(),
                ));
            }

            // Get the source value
            if let Some(stored_value) = self.storage.get_value(current_db, key)? {
                // Remember if destination had a value for rollback
                let dest_had_value = self.storage.exists_in_db(dest_db, key)?;
                let dest_old_value = if dest_had_value && replace {
                    self.storage.get_value(dest_db, key)?
                } else {
                    None
                };

                // Copy to destination
                self.storage
                    .set_value(dest_db, key.clone(), stored_value.clone())?;

                // Delete from source if not COPY mode
                if !copy {
                    if let Err(e) = self.storage.delete_from_db(current_db, key) {
                        // Rollback: restore destination to previous state
                        if let Some(old_val) = dest_old_value {
                            let _ = self.storage.set_value(dest_db, key.clone(), old_val);
                        } else if !dest_had_value {
                            let _ = self.storage.delete_from_db(dest_db, key);
                        }
                        return Err(e);
                    }
                }

                migrated_count += 1;
            }
        }

        if migrated_count == 0 {
            Ok(RespValue::simple_string("NOKEY"))
        } else {
            Ok(RespValue::ok())
        }
    }
}
