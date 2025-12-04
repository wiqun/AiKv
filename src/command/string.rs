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
