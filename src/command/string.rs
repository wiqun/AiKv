use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageEngine;
use bytes::Bytes;

/// String command handler
pub struct StringCommands {
    storage: StorageEngine,
}

impl StringCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
        }
    }

    /// GET key
    pub fn get(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("GET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        match self.storage.get_from_db(current_db, &key)? {
            Some(value) => Ok(RespValue::bulk_string(value)),
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// SET key value \[EX seconds\] \[PX milliseconds\] \[NX|XX\]
    pub fn set(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let value = args[1].clone();

        // Parse options
        let mut i = 2;
        let mut nx = false;
        let mut xx = false;
        let mut expire_ms: Option<u64> = None;

        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "NX" => nx = true,
                "XX" => xx = true,
                "EX" => {
                    // Set expiration in seconds
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    let seconds_str = String::from_utf8_lossy(&args[i]);
                    let seconds = seconds_str.parse::<u64>().map_err(|_| {
                        AikvError::InvalidArgument("ERR value is not an integer".to_string())
                    })?;
                    expire_ms = Some(seconds * 1000);
                }
                "PX" => {
                    // Set expiration in milliseconds
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    let ms_str = String::from_utf8_lossy(&args[i]);
                    let ms = ms_str.parse::<u64>().map_err(|_| {
                        AikvError::InvalidArgument("ERR value is not an integer".to_string())
                    })?;
                    expire_ms = Some(ms);
                }
                _ => {}
            }
            i += 1;
        }

        // Check conditions
        if nx && self.storage.exists_in_db(current_db, &key)? {
            return Ok(RespValue::null_bulk_string());
        }

        if xx && !self.storage.exists_in_db(current_db, &key)? {
            return Ok(RespValue::null_bulk_string());
        }

        // Set with or without expiration
        if let Some(ms) = expire_ms {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let expire_at = now_ms + ms;
            self.storage
                .set_with_expiration_in_db(current_db, key, value, expire_at)?;
        } else {
            self.storage.set_in_db(current_db, key, value)?;
        }

        Ok(RespValue::ok())
    }

    /// DEL key \[key ...\]
    pub fn del(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("DEL".to_string()));
        }

        let mut count = 0;
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            if self.storage.delete_from_db(current_db, &key)? {
                count += 1;
            }
        }

        Ok(RespValue::integer(count))
    }

    /// EXISTS key \[key ...\]
    pub fn exists(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("EXISTS".to_string()));
        }

        let mut count = 0;
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            if self.storage.exists_in_db(current_db, &key)? {
                count += 1;
            }
        }

        Ok(RespValue::integer(count))
    }

    /// MGET key \[key ...\]
    pub fn mget(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("MGET".to_string()));
        }

        // Migrated: Logic moved from storage layer to command layer
        let mut result = Vec::with_capacity(args.len());
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            match self.storage.get_from_db(current_db, &key)? {
                Some(bytes) => result.push(RespValue::bulk_string(bytes)),
                None => result.push(RespValue::null_bulk_string()),
            }
        }

        Ok(RespValue::array(result))
    }

    /// MSET key value \[key value ...\]
    pub fn mset(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.is_empty() || args.len() % 2 != 0 {
            return Err(AikvError::WrongArgCount("MSET".to_string()));
        }

        // Migrated: Logic moved from storage layer to command layer
        for chunk in args.chunks(2) {
            let key = String::from_utf8_lossy(&chunk[0]).to_string();
            let value = chunk[1].clone();
            self.storage.set_in_db(current_db, key, value)?;
        }

        Ok(RespValue::ok())
    }

    /// STRLEN key
    pub fn strlen(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("STRLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        match self.storage.get_from_db(current_db, &key)? {
            Some(value) => Ok(RespValue::integer(value.len() as i64)),
            None => Ok(RespValue::integer(0)),
        }
    }

    /// APPEND key value
    pub fn append(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("APPEND".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let append_value = &args[1];

        let new_value = match self.storage.get_from_db(current_db, &key)? {
            Some(existing) => {
                let mut combined = existing.to_vec();
                combined.extend_from_slice(append_value);
                Bytes::from(combined)
            }
            None => append_value.clone(),
        };

        let len = new_value.len() as i64;
        self.storage.set_in_db(current_db, key, new_value)?;

        Ok(RespValue::integer(len))
    }

    /// INCR key
    /// Increments the number stored at key by one
    pub fn incr(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("INCR".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        self.incr_by_internal(&key, 1, current_db)
    }

    /// DECR key
    /// Decrements the number stored at key by one
    pub fn decr(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("DECR".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        self.incr_by_internal(&key, -1, current_db)
    }

    /// INCRBY key increment
    /// Increments the number stored at key by increment
    pub fn incrby(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("INCRBY".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let increment = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| {
                AikvError::InvalidArgument(
                    "ERR value is not an integer or out of range".to_string(),
                )
            })?;

        self.incr_by_internal(&key, increment, current_db)
    }

    /// DECRBY key decrement
    /// Decrements the number stored at key by decrement
    pub fn decrby(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("DECRBY".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let decrement = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| {
                AikvError::InvalidArgument(
                    "ERR value is not an integer or out of range".to_string(),
                )
            })?;

        self.incr_by_internal(&key, -decrement, current_db)
    }

    /// Internal helper for INCR/DECR/INCRBY/DECRBY
    fn incr_by_internal(&self, key: &str, increment: i64, current_db: usize) -> Result<RespValue> {
        let current_value = match self.storage.get_from_db(current_db, key)? {
            Some(value) => {
                let value_str = String::from_utf8_lossy(&value);
                value_str.parse::<i64>().map_err(|_| {
                    AikvError::InvalidArgument(
                        "ERR value is not an integer or out of range".to_string(),
                    )
                })?
            }
            None => 0,
        };

        let new_value = current_value.checked_add(increment).ok_or_else(|| {
            AikvError::InvalidArgument("ERR increment or decrement would overflow".to_string())
        })?;

        self.storage.set_in_db(
            current_db,
            key.to_string(),
            Bytes::from(new_value.to_string()),
        )?;
        Ok(RespValue::integer(new_value))
    }

    /// INCRBYFLOAT key increment
    /// Increments the string representing a floating point number by the specified increment
    pub fn incrbyfloat(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("INCRBYFLOAT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let increment = String::from_utf8_lossy(&args[1])
            .parse::<f64>()
            .map_err(|_| {
                AikvError::InvalidArgument("ERR value is not a valid float".to_string())
            })?;

        let current_value = match self.storage.get_from_db(current_db, &key)? {
            Some(value) => {
                let value_str = String::from_utf8_lossy(&value);
                value_str.parse::<f64>().map_err(|_| {
                    AikvError::InvalidArgument("ERR value is not a valid float".to_string())
                })?
            }
            None => 0.0,
        };

        let new_value = current_value + increment;

        // Check for infinity or NaN
        if new_value.is_infinite() || new_value.is_nan() {
            return Err(AikvError::InvalidArgument(
                "ERR increment would produce NaN or Infinity".to_string(),
            ));
        }

        // Format the float as Redis does (remove trailing zeros)
        let formatted = format!("{}", new_value);
        self.storage
            .set_in_db(current_db, key, Bytes::from(formatted.clone()))?;
        Ok(RespValue::bulk_string(Bytes::from(formatted)))
    }

    /// GETRANGE key start end
    /// Returns the substring of the string value stored at key
    pub fn getrange(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("GETRANGE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| {
                AikvError::InvalidArgument(
                    "ERR value is not an integer or out of range".to_string(),
                )
            })?;
        let end = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| {
                AikvError::InvalidArgument(
                    "ERR value is not an integer or out of range".to_string(),
                )
            })?;

        match self.storage.get_from_db(current_db, &key)? {
            Some(value) => {
                let len = value.len() as i64;
                if len == 0 {
                    return Ok(RespValue::bulk_string(Bytes::from("")));
                }

                // Normalize negative indices
                let start_idx = if start < 0 {
                    (len + start).max(0)
                } else {
                    start.min(len)
                } as usize;
                let end_idx = if end < 0 {
                    (len + end).max(0)
                } else {
                    end.min(len - 1)
                } as usize;

                if start_idx > end_idx || start_idx >= value.len() {
                    Ok(RespValue::bulk_string(Bytes::from("")))
                } else {
                    Ok(RespValue::bulk_string(Bytes::from(
                        value[start_idx..=end_idx].to_vec(),
                    )))
                }
            }
            None => Ok(RespValue::bulk_string(Bytes::from(""))),
        }
    }

    /// SETRANGE key offset value
    /// Overwrites part of the string stored at key, starting at the specified offset
    pub fn setrange(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("SETRANGE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let offset = String::from_utf8_lossy(&args[1])
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR offset is out of range".to_string()))?;
        let value = &args[2];

        let mut current = match self.storage.get_from_db(current_db, &key)? {
            Some(v) => v.to_vec(),
            None => Vec::new(),
        };

        // Extend with null bytes if necessary
        let required_len = offset + value.len();
        if required_len > current.len() {
            current.resize(required_len, 0);
        }

        // Overwrite at offset
        current[offset..offset + value.len()].copy_from_slice(value);

        let len = current.len() as i64;
        self.storage
            .set_in_db(current_db, key, Bytes::from(current))?;
        Ok(RespValue::integer(len))
    }

    /// GETEX key [EX seconds | PX milliseconds | EXAT unix-time | PXAT unix-time-milliseconds | PERSIST]
    /// Get the value of key and optionally set its expiration
    pub fn getex(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("GETEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Get the value first
        let value = match self.storage.get_from_db(current_db, &key)? {
            Some(v) => v,
            None => return Ok(RespValue::null_bulk_string()),
        };

        // Parse options
        if args.len() > 1 {
            let option = String::from_utf8_lossy(&args[1]).to_uppercase();
            match option.as_str() {
                "EX" => {
                    if args.len() < 3 {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    let seconds =
                        String::from_utf8_lossy(&args[2])
                            .parse::<u64>()
                            .map_err(|_| {
                                AikvError::InvalidArgument(
                                    "ERR value is not an integer or out of range".to_string(),
                                )
                            })?;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    let expire_at = now_ms + seconds * 1000;
                    self.storage.set_with_expiration_in_db(
                        current_db,
                        key,
                        value.clone(),
                        expire_at,
                    )?;
                }
                "PX" => {
                    if args.len() < 3 {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    let ms = String::from_utf8_lossy(&args[2])
                        .parse::<u64>()
                        .map_err(|_| {
                            AikvError::InvalidArgument(
                                "ERR value is not an integer or out of range".to_string(),
                            )
                        })?;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    let expire_at = now_ms + ms;
                    self.storage.set_with_expiration_in_db(
                        current_db,
                        key,
                        value.clone(),
                        expire_at,
                    )?;
                }
                "EXAT" => {
                    if args.len() < 3 {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    let unix_time =
                        String::from_utf8_lossy(&args[2])
                            .parse::<u64>()
                            .map_err(|_| {
                                AikvError::InvalidArgument(
                                    "ERR value is not an integer or out of range".to_string(),
                                )
                            })?;
                    let expire_at = unix_time * 1000;
                    self.storage.set_with_expiration_in_db(
                        current_db,
                        key,
                        value.clone(),
                        expire_at,
                    )?;
                }
                "PXAT" => {
                    if args.len() < 3 {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    let expire_at =
                        String::from_utf8_lossy(&args[2])
                            .parse::<u64>()
                            .map_err(|_| {
                                AikvError::InvalidArgument(
                                    "ERR value is not an integer or out of range".to_string(),
                                )
                            })?;
                    self.storage.set_with_expiration_in_db(
                        current_db,
                        key,
                        value.clone(),
                        expire_at,
                    )?;
                }
                "PERSIST" => {
                    // Re-set without expiration to remove TTL
                    self.storage.set_in_db(current_db, key, value.clone())?;
                }
                _ => {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
            }
        }

        Ok(RespValue::bulk_string(value))
    }

    /// GETDEL key
    /// Get the value of key and delete the key
    pub fn getdel(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("GETDEL".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        match self.storage.get_from_db(current_db, &key)? {
            Some(value) => {
                self.storage.delete_from_db(current_db, &key)?;
                Ok(RespValue::bulk_string(value))
            }
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// SETNX key value
    /// Set key to hold string value if key does not exist
    pub fn setnx(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("SETNX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let value = args[1].clone();

        if self.storage.exists_in_db(current_db, &key)? {
            Ok(RespValue::integer(0))
        } else {
            self.storage.set_in_db(current_db, key, value)?;
            Ok(RespValue::integer(1))
        }
    }

    /// SETEX key seconds value
    /// Set key to hold the string value and set key to timeout after a given number of seconds
    pub fn setex(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("SETEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let seconds = String::from_utf8_lossy(&args[1])
            .parse::<u64>()
            .map_err(|_| {
                AikvError::InvalidArgument(
                    "ERR value is not an integer or out of range".to_string(),
                )
            })?;
        let value = args[2].clone();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let expire_at = now_ms + seconds * 1000;

        self.storage
            .set_with_expiration_in_db(current_db, key, value, expire_at)?;
        Ok(RespValue::ok())
    }

    /// PSETEX key milliseconds value
    /// Set key to hold the string value and set key to timeout after a given number of milliseconds
    pub fn psetex(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("PSETEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let milliseconds = String::from_utf8_lossy(&args[1])
            .parse::<u64>()
            .map_err(|_| {
                AikvError::InvalidArgument(
                    "ERR value is not an integer or out of range".to_string(),
                )
            })?;
        let value = args[2].clone();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let expire_at = now_ms + milliseconds;

        self.storage
            .set_with_expiration_in_db(current_db, key, value, expire_at)?;
        Ok(RespValue::ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageEngine;

    fn setup() -> StringCommands {
        StringCommands::new(StorageEngine::new_memory(16))
    }

    #[test]
    fn test_get_set() {
        let cmd = setup();

        // SET
        let result = cmd
            .set(&[Bytes::from("key1"), Bytes::from("value1")], 0)
            .unwrap();
        assert_eq!(result, RespValue::ok());

        // GET
        let result = cmd.get(&[Bytes::from("key1")], 0).unwrap();
        assert_eq!(result, RespValue::bulk_string("value1"));
    }

    #[test]
    fn test_del() {
        let cmd = setup();

        cmd.set(&[Bytes::from("key1"), Bytes::from("value1")], 0)
            .unwrap();
        cmd.set(&[Bytes::from("key2"), Bytes::from("value2")], 0)
            .unwrap();

        let result = cmd
            .del(
                &[
                    Bytes::from("key1"),
                    Bytes::from("key2"),
                    Bytes::from("key3"),
                ],
                0,
            )
            .unwrap();
        assert_eq!(result, RespValue::integer(2));
    }

    #[test]
    fn test_exists() {
        let cmd = setup();

        cmd.set(&[Bytes::from("key1"), Bytes::from("value1")], 0)
            .unwrap();

        let result = cmd
            .exists(&[Bytes::from("key1"), Bytes::from("key2")], 0)
            .unwrap();
        assert_eq!(result, RespValue::integer(1));
    }

    #[test]
    fn test_mget_mset() {
        let cmd = setup();

        cmd.mset(
            &[
                Bytes::from("key1"),
                Bytes::from("value1"),
                Bytes::from("key2"),
                Bytes::from("value2"),
            ],
            0,
        )
        .unwrap();

        let result = cmd
            .mget(
                &[
                    Bytes::from("key1"),
                    Bytes::from("key2"),
                    Bytes::from("key3"),
                ],
                0,
            )
            .unwrap();

        if let RespValue::Array(Some(arr)) = result {
            assert_eq!(arr.len(), 3);
            assert_eq!(arr[0], RespValue::bulk_string("value1"));
            assert_eq!(arr[1], RespValue::bulk_string("value2"));
            assert_eq!(arr[2], RespValue::null_bulk_string());
        } else {
            panic!("Expected array response");
        }
    }

    #[test]
    fn test_strlen() {
        let cmd = setup();

        cmd.set(&[Bytes::from("key1"), Bytes::from("hello")], 0)
            .unwrap();

        let result = cmd.strlen(&[Bytes::from("key1")], 0).unwrap();
        assert_eq!(result, RespValue::integer(5));
    }

    #[test]
    fn test_append() {
        let cmd = setup();

        let result = cmd
            .append(&[Bytes::from("key1"), Bytes::from("Hello")], 0)
            .unwrap();
        assert_eq!(result, RespValue::integer(5));

        let result = cmd
            .append(&[Bytes::from("key1"), Bytes::from(" World")], 0)
            .unwrap();
        assert_eq!(result, RespValue::integer(11));

        let result = cmd.get(&[Bytes::from("key1")], 0).unwrap();
        assert_eq!(result, RespValue::bulk_string("Hello World"));
    }
}
