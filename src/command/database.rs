//! Database 命令

use std::sync::Arc;

use bytes::Bytes;

use crate::command::router;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::types::DB_COUNT;
use crate::storage::KvStorage;

pub struct DatabaseCommands {
    storage: Arc<dyn KvStorage>,
}

impl DatabaseCommands {
    pub fn new(storage: Arc<dyn KvStorage>) -> Self {
        Self { storage }
    }

    pub async fn select(&self, args: &[Bytes], db: &mut usize) -> Result<RespValue> {
        router::require_args("SELECT", args, 1)?;
        let index = parse_db_index(&args[0])?;
        if index >= DB_COUNT {
            return Err(Error::Command("ERR DB index is out of range".into()));
        }
        // 对齐 Kvrocks: 兼容客户端连接时的 SELECT 指令, 始终保持单库 (db 0)
        *db = 0;
        Ok(router::ok())
    }

    pub async fn dbsize(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("DBSIZE", args, 0)?;
        let n = self.storage.len(db).await?;
        Ok(router::integer(n as i64))
    }

    pub async fn flushdb(&self, _db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("FLUSHDB", args, 0)?;
        self.storage.clear_all().await?;
        Ok(router::ok())
    }

    pub async fn flushall(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("FLUSHALL", args, 0)?;
        self.storage.clear_all().await?;
        Ok(router::ok())
    }

    pub async fn swapdb(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SWAPDB", args, 2)?;
        let a = parse_db_index(&args[0])?;
        let b = parse_db_index(&args[1])?;
        if a >= DB_COUNT || b >= DB_COUNT {
            return Err(Error::Command("ERR DB index is out of range".into()));
        }
        Err(Error::Command(
            "ERR SWAPDB is not supported in single-db mode".into(),
        ))
    }

    pub async fn move_key(&self, _src_db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("MOVE", args, 2)?;
        let target = parse_db_index(&args[1])?;
        if target >= DB_COUNT {
            return Err(Error::Command("ERR DB index is out of range".into()));
        }
        // 单库模式下不支持跨库移动
        Ok(router::integer(0))
    }
}

fn parse_db_index(b: &Bytes) -> Result<usize> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<usize>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}
