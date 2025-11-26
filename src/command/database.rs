use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::StorageEngine;
use bytes::Bytes;

/// Database command handler
pub struct DatabaseCommands {
    storage: StorageEngine,
}

impl DatabaseCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
        }
    }

    /// SELECT index - Select database by index
    pub fn select(&self, args: &[Bytes], current_db: &mut usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SELECT".to_string()));
        }

        let index_str = String::from_utf8_lossy(&args[0]);
        let index = index_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid DB index".to_string()))?;

        if index >= 16 {
            // Redis default is 16 databases
            return Err(AikvError::InvalidArgument(
                "ERR DB index is out of range".to_string(),
            ));
        }

        *current_db = index;
        Ok(RespValue::ok())
    }

    /// DBSIZE - Get the number of keys in current database
    pub fn dbsize(&self, _args: &[Bytes], current_db: usize) -> Result<RespValue> {
        let size = self.storage.dbsize_in_db(current_db)?;
        Ok(RespValue::integer(size as i64))
    }

    /// FLUSHDB - Clear current database
    pub fn flushdb(&self, _args: &[Bytes], current_db: usize) -> Result<RespValue> {
        self.storage.flush_db(current_db)?;
        Ok(RespValue::ok())
    }

    /// FLUSHALL - Clear all databases
    pub fn flushall(&self, _args: &[Bytes]) -> Result<RespValue> {
        self.storage.flush_all()?;
        Ok(RespValue::ok())
    }

    /// SWAPDB db1 db2 - Swap two databases
    pub fn swapdb(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("SWAPDB".to_string()));
        }

        let db1_str = String::from_utf8_lossy(&args[0]);
        let db2_str = String::from_utf8_lossy(&args[1]);

        let db1 = db1_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid first DB index".to_string()))?;
        let db2 = db2_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid second DB index".to_string()))?;

        if db1 >= 16 || db2 >= 16 {
            return Err(AikvError::InvalidArgument(
                "ERR DB index is out of range".to_string(),
            ));
        }

        self.storage.swap_db(db1, db2)?;
        Ok(RespValue::ok())
    }

    /// MOVE key db - Move key to another database
    pub fn move_key(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("MOVE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let db_str = String::from_utf8_lossy(&args[1]);

        let dest_db = db_str
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid DB index".to_string()))?;

        if dest_db >= 16 {
            return Err(AikvError::InvalidArgument(
                "ERR DB index is out of range".to_string(),
            ));
        }

        let moved = self.storage.move_key(current_db, dest_db, &key)?;
        Ok(RespValue::integer(if moved { 1 } else { 0 }))
    }
}
