use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageAdapter;
use bytes::Bytes;

/// List command handler
pub struct ListCommands {
    storage: StorageAdapter,
}

impl ListCommands {
    pub fn new(storage: StorageAdapter) -> Self {
        Self {
            storage,
        }
    }

    /// LPUSH key element [element ...]
    /// Insert all the specified values at the head of the list stored at key
    pub fn lpush(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("LPUSH".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let elements: Vec<Bytes> = args[1..].to_vec();

        let len = self.storage.list_lpush_in_db(db_index, &key, elements)?;
        Ok(RespValue::Integer(len as i64))
    }

    /// RPUSH key element [element ...]
    /// Insert all the specified values at the tail of the list stored at key
    pub fn rpush(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("RPUSH".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let elements: Vec<Bytes> = args[1..].to_vec();

        let len = self.storage.list_rpush_in_db(db_index, &key, elements)?;
        Ok(RespValue::Integer(len as i64))
    }

    /// LPOP key [count]
    /// Remove and return the first elements of the list stored at key
    pub fn lpop(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("LPOP".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?
        } else {
            1
        };

        let values = self.storage.list_lpop_in_db(db_index, &key, count)?;

        if values.is_empty() {
            Ok(RespValue::Null)
        } else if count == 1 {
            Ok(RespValue::bulk_string(values[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                values.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// RPOP key [count]
    /// Remove and return the last elements of the list stored at key
    pub fn rpop(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("RPOP".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?
        } else {
            1
        };

        let values = self.storage.list_rpop_in_db(db_index, &key, count)?;

        if values.is_empty() {
            Ok(RespValue::Null)
        } else if count == 1 {
            Ok(RespValue::bulk_string(values[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                values.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// LLEN key
    /// Returns the length of the list stored at key
    pub fn llen(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("LLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let len = self.storage.list_len_in_db(db_index, &key)?;
        Ok(RespValue::Integer(len as i64))
    }

    /// LRANGE key start stop
    /// Returns the specified elements of the list stored at key
    pub fn lrange(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LRANGE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid start index".to_string()))?;
        let stop = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid stop index".to_string()))?;

        let values = self.storage.list_range_in_db(db_index, &key, start, stop)?;
        Ok(RespValue::Array(Some(
            values.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// LINDEX key index
    /// Returns the element at index in the list stored at key
    pub fn lindex(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("LINDEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let index = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid index".to_string()))?;

        match self.storage.list_index_in_db(db_index, &key, index)? {
            Some(value) => Ok(RespValue::bulk_string(value)),
            None => Ok(RespValue::Null),
        }
    }

    /// LSET key index element
    /// Sets the list element at index to element
    pub fn lset(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LSET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let index = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid index".to_string()))?;
        let element = args[2].clone();

        self.storage
            .list_set_in_db(db_index, &key, index, element)?;
        Ok(RespValue::simple_string("OK"))
    }

    /// LREM key count element
    /// Removes the first count occurrences of elements equal to element from the list
    pub fn lrem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?;
        let element = args[2].clone();

        let removed = self
            .storage
            .list_rem_in_db(db_index, &key, count, element)?;
        Ok(RespValue::Integer(removed as i64))
    }

    /// LTRIM key start stop
    /// Trim the list to the specified range
    pub fn ltrim(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LTRIM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid start index".to_string()))?;
        let stop = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid stop index".to_string()))?;

        self.storage.list_trim_in_db(db_index, &key, start, stop)?;
        Ok(RespValue::simple_string("OK"))
    }
}
