use bytes::Bytes;
use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;

/// String command handler
pub struct StringCommands {
    storage: StorageAdapter,
}

impl StringCommands {
    pub fn new(storage: StorageAdapter) -> Self {
        Self { storage }
    }

    /// GET key
    pub fn get(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("GET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        
        match self.storage.get(&key)? {
            Some(value) => Ok(RespValue::bulk_string(value)),
            None => Ok(RespValue::null_bulk_string()),
        }
    }

    /// SET key value [EX seconds] [NX|XX]
    pub fn set(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let value = args[1].clone();

        // Parse options
        let mut i = 2;
        let mut nx = false;
        let mut xx = false;
        // EX option would be handled here in a full implementation

        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "NX" => nx = true,
                "XX" => xx = true,
                "EX" => {
                    // Skip the next argument (seconds)
                    i += 1;
                    // In a full implementation, would set TTL here
                }
                _ => {}
            }
            i += 1;
        }

        // Check conditions
        if nx {
            if self.storage.exists(&key)? {
                return Ok(RespValue::null_bulk_string());
            }
        }

        if xx {
            if !self.storage.exists(&key)? {
                return Ok(RespValue::null_bulk_string());
            }
        }

        self.storage.set(key, value)?;
        Ok(RespValue::ok())
    }

    /// DEL key [key ...]
    pub fn del(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("DEL".to_string()));
        }

        let mut count = 0;
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            if self.storage.delete(&key)? {
                count += 1;
            }
        }

        Ok(RespValue::integer(count))
    }

    /// EXISTS key [key ...]
    pub fn exists(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("EXISTS".to_string()));
        }

        let mut count = 0;
        for arg in args {
            let key = String::from_utf8_lossy(arg).to_string();
            if self.storage.exists(&key)? {
                count += 1;
            }
        }

        Ok(RespValue::integer(count))
    }

    /// MGET key [key ...]
    pub fn mget(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("MGET".to_string()));
        }

        let keys: Vec<String> = args.iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let values = self.storage.mget(&keys)?;
        let resp_values: Vec<RespValue> = values.into_iter()
            .map(|v| match v {
                Some(bytes) => RespValue::bulk_string(bytes),
                None => RespValue::null_bulk_string(),
            })
            .collect();

        Ok(RespValue::array(resp_values))
    }

    /// MSET key value [key value ...]
    pub fn mset(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() || args.len() % 2 != 0 {
            return Err(AikvError::WrongArgCount("MSET".to_string()));
        }

        let mut pairs = Vec::new();
        for chunk in args.chunks(2) {
            let key = String::from_utf8_lossy(&chunk[0]).to_string();
            let value = chunk[1].clone();
            pairs.push((key, value));
        }

        self.storage.mset(pairs)?;
        Ok(RespValue::ok())
    }

    /// STRLEN key
    pub fn strlen(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("STRLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        
        match self.storage.get(&key)? {
            Some(value) => Ok(RespValue::integer(value.len() as i64)),
            None => Ok(RespValue::integer(0)),
        }
    }

    /// APPEND key value
    pub fn append(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("APPEND".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let append_value = &args[1];

        let new_value = match self.storage.get(&key)? {
            Some(existing) => {
                let mut combined = existing.to_vec();
                combined.extend_from_slice(append_value);
                Bytes::from(combined)
            }
            None => append_value.clone(),
        };

        let len = new_value.len() as i64;
        self.storage.set(key, new_value)?;
        
        Ok(RespValue::integer(len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> StringCommands {
        StringCommands::new(StorageAdapter::new())
    }

    #[test]
    fn test_get_set() {
        let cmd = setup();
        
        // SET
        let result = cmd.set(&[Bytes::from("key1"), Bytes::from("value1")]).unwrap();
        assert_eq!(result, RespValue::ok());
        
        // GET
        let result = cmd.get(&[Bytes::from("key1")]).unwrap();
        assert_eq!(result, RespValue::bulk_string("value1"));
    }

    #[test]
    fn test_del() {
        let cmd = setup();
        
        cmd.set(&[Bytes::from("key1"), Bytes::from("value1")]).unwrap();
        cmd.set(&[Bytes::from("key2"), Bytes::from("value2")]).unwrap();
        
        let result = cmd.del(&[Bytes::from("key1"), Bytes::from("key2"), Bytes::from("key3")]).unwrap();
        assert_eq!(result, RespValue::integer(2));
    }

    #[test]
    fn test_exists() {
        let cmd = setup();
        
        cmd.set(&[Bytes::from("key1"), Bytes::from("value1")]).unwrap();
        
        let result = cmd.exists(&[Bytes::from("key1"), Bytes::from("key2")]).unwrap();
        assert_eq!(result, RespValue::integer(1));
    }

    #[test]
    fn test_mget_mset() {
        let cmd = setup();
        
        cmd.mset(&[
            Bytes::from("key1"), Bytes::from("value1"),
            Bytes::from("key2"), Bytes::from("value2"),
        ]).unwrap();
        
        let result = cmd.mget(&[Bytes::from("key1"), Bytes::from("key2"), Bytes::from("key3")]).unwrap();
        
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
        
        cmd.set(&[Bytes::from("key1"), Bytes::from("hello")]).unwrap();
        
        let result = cmd.strlen(&[Bytes::from("key1")]).unwrap();
        assert_eq!(result, RespValue::integer(5));
    }

    #[test]
    fn test_append() {
        let cmd = setup();
        
        let result = cmd.append(&[Bytes::from("key1"), Bytes::from("Hello")]).unwrap();
        assert_eq!(result, RespValue::integer(5));
        
        let result = cmd.append(&[Bytes::from("key1"), Bytes::from(" World")]).unwrap();
        assert_eq!(result, RespValue::integer(11));
        
        let result = cmd.get(&[Bytes::from("key1")]).unwrap();
        assert_eq!(result, RespValue::bulk_string("Hello World"));
    }
}
