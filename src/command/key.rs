//! Key 管理命令: 过期 (EXPIRE/PEXPIRE/EXPIREAT/PEXPIREAT/EXPIRETIME/PEXPIRETIME/TTL/PTTL/
//! PERSIST)、键空间 (KEYS/SCAN/RANDOMKEY)、重命名 (RENAME/RENAMENX)、COPY、TYPE、
//! DUMP/RESTORE、MIGRATE 编排.
//!
//! # 关键点
//!
//! - 过期: 负 TTL 直接 `delete`; 其余映射为 Unix ms 写入 `expires_at`; `TTL_NO_EXPIRY` (-1)
//!   映射 Redis -1 (见 `map_ttl_seconds`/`map_pttl_ms`).
//! - 键空间: KEYS 直调 `storage.keys` (pattern 走 `glob_match`, 仅 `*`/`?`); SCAN 走
//!   引擎级游标 (`storage.scan`).
//! - 双 key 写: RENAME/RENAMENX/COPY 用 `key_lock.lock_two` (字典序, 同 key 不重入).
//! - DUMP 格式: `[u8 version=1][postcard(StoredValue)]` (非 Redis DUMP); RESTORE 校验
//!   version, 失败 → `ERR DUMP payload version or checksum error`.
//! - MIGRATE: 本文件编排 — `get_expanded` → `dump_encode` → `migrate::send_restore` (TCP) →
//!   非 COPY 时源端 `delete`; 支持 KEYS/AUTH/AUTH2/REPLACE 等选项.

use std::sync::Arc;

use bytes::Bytes;

use crate::command::migrate;
use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::{dump_decode, dump_encode, now_ms, KvStorage, TTL_NO_EXPIRY};

pub struct KeyCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

impl KeyCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self { storage, key_lock }
    }

    pub fn storage(&self) -> &Arc<dyn KvStorage> {
        &self.storage
    }

    pub async fn expire(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("EXPIRE", args, 2)?;
        let secs = parse_i64(&args[1])?;
        if secs < 0 {
            let deleted = self.storage.delete(db, &args[0]).await?;
            return Ok(router::integer(i64::from(deleted)));
        }
        let ttl_ms = (secs as u64).saturating_mul(1000);
        let ok = self.storage.expire(db, &args[0], ttl_ms).await?;
        Ok(router::integer(i64::from(ok)))
    }

    pub async fn expireat(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("EXPIREAT", args, 2)?;
        let ts_secs = parse_i64(&args[1])?;
        let ts_ms = (ts_secs as u64).saturating_mul(1000);
        let ok = self.storage.expire_at(db, &args[0], ts_ms as u64).await?;
        Ok(router::integer(i64::from(ok)))
    }

    pub async fn pexpire(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("PEXPIRE", args, 2)?;
        let ms = parse_i64(&args[1])?;
        if ms < 0 {
            let deleted = self.storage.delete(db, &args[0]).await?;
            return Ok(router::integer(i64::from(deleted)));
        }
        let ok = self.storage.expire(db, &args[0], ms as u64).await?;
        Ok(router::integer(i64::from(ok)))
    }

    pub async fn pexpireat(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("PEXPIREAT", args, 2)?;
        let ts_ms = parse_i64(&args[1])?;
        if ts_ms < 0 {
            let deleted = self.storage.delete(db, &args[0]).await?;
            return Ok(router::integer(i64::from(deleted)));
        }
        let ok = self.storage.expire_at(db, &args[0], ts_ms as u64).await?;
        Ok(router::integer(i64::from(ok)))
    }

    pub async fn ttl(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("TTL", args, 1)?;
        Ok(router::integer(map_ttl_seconds(
            self.storage.ttl(db, &args[0]).await?,
        )))
    }

    pub async fn pttl(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("PTTL", args, 1)?;
        Ok(router::integer(map_pttl_ms(
            self.storage.ttl(db, &args[0]).await?,
        )))
    }

    pub async fn persist(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("PERSIST", args, 1)?;
        let ok = self.storage.persist(db, &args[0]).await?;
        Ok(router::integer(i64::from(ok)))
    }

    pub async fn keys(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("KEYS", args, 1)?;
        let matched = self.storage.keys(db, &args[0]).await?;
        Ok(array_of_bulk(matched))
    }

    pub async fn scan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SCAN", args, 1)?;
        let cursor = parse_u64(&args[0])?;
        let mut pattern = Vec::new();
        let mut count = 10usize;
        let mut i = 1;
        while i < args.len() {
            if eq_ignore_case(&args[i], b"MATCH") {
                if i + 1 >= args.len() {
                    return Err(router::wrong_args("SCAN", ""));
                }
                pattern = args[i + 1].to_vec();
                i += 2;
            } else if eq_ignore_case(&args[i], b"COUNT") {
                if i + 1 >= args.len() {
                    return Err(router::wrong_args("SCAN", ""));
                }
                count = parse_i64(&args[i + 1])? as usize;
                i += 2;
            } else {
                return Err(router::wrong_args("SCAN", ""));
            }
        }
        let result = self.storage.scan(db, cursor, &pattern, count).await?;
        Ok(RespValue::Array(Some(vec![
            RespValue::BulkString(Some(Bytes::from(result.cursor.to_string()))),
            array_of_bulk(result.keys),
        ])))
    }

    pub async fn randomkey(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("RANDOMKEY", args, 0)?;
        match self.storage.random_key(db).await? {
            None => Ok(router::nil_bulk()),
            Some(k) => Ok(router::bulk(k)),
        }
    }

    pub async fn rename(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("RENAME", args, 2)?;
        let key = &args[0];
        let newkey = &args[1];
        let (_lock_a, _lock_b) = self.key_lock.lock_two(key, newkey).await;
        if key == newkey {
            if self.storage.get_typed(db, key).await?.is_none() {
                return Err(Error::Command("ERR no such key".into()));
            }
            return Ok(router::ok());
        }
        if self.storage.get_typed(db, key).await?.is_none() {
            return Err(Error::Command("ERR no such key".into()));
        }
        self.storage.rename_key(db, key, newkey).await?;
        Ok(router::ok())
    }

    pub async fn renamenx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("RENAMENX", args, 2)?;
        let key = &args[0];
        let newkey = &args[1];
        let (_lock_a, _lock_b) = self.key_lock.lock_two(key, newkey).await;
        if self.storage.get_typed(db, key).await?.is_none() {
            return Err(Error::Command("ERR no such key".into()));
        }
        let ok = self.storage.rename_key_nx(db, key, newkey).await?;
        Ok(router::integer(i64::from(ok)))
    }

    pub async fn type_cmd(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("TYPE", args, 1)?;
        match self.storage.get_typed(db, &args[0]).await? {
            None => Ok(RespValue::SimpleString("none".into())),
            Some(stored) => Ok(RespValue::SimpleString(stored.type_name().into())),
        }
    }

    pub async fn copy(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("COPY", args, 2)?;
        let src_key = &args[0];
        let dst_key = &args[1];
        let mut dest_db = db;
        let mut replace = false;
        let mut i = 2;
        while i < args.len() {
            if eq_ignore_case(&args[i], b"DB") {
                if i + 1 >= args.len() {
                    return Err(router::wrong_args("COPY", ""));
                }
                dest_db = parse_i64(&args[i + 1])? as usize;
                i += 2;
            } else if eq_ignore_case(&args[i], b"REPLACE") {
                replace = true;
                i += 1;
            } else {
                return Err(router::wrong_args("COPY", ""));
            }
        }
        let copied = self
            .storage
            .copy_key(db, dest_db, src_key, dst_key, replace)
            .await?;
        Ok(router::integer(i64::from(copied)))
    }

    pub async fn expiretime(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("EXPIRETIME", args, 1)?;
        Ok(router::integer(map_expiretime(
            self.storage.get_typed(db, &args[0]).await?,
        )))
    }

    pub async fn pexpiretime(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("PEXPIRETIME", args, 1)?;
        Ok(router::integer(map_pexpiretime(
            self.storage.get_typed(db, &args[0]).await?,
        )))
    }

    /// DUMP — AiKv 内部格式 `[version: u8=1][postcard(StoredValue)]`, 非 Redis 兼容.
    pub async fn dump(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("DUMP", args, 1)?;
        match self.storage.get_expanded(db, &args[0]).await? {
            None => Ok(router::nil_bulk()),
            Some(stored) => {
                let payload = dump_encode(&stored)?;
                Ok(router::bulk(payload))
            }
        }
    }

    pub async fn restore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("RESTORE", args, 3)?;
        let key = &args[0];
        let ttl_ms = parse_i64(&args[1])?;
        if ttl_ms < 0 {
            return Err(Error::Command("ERR Invalid TTL value".into()));
        }
        let payload = &args[2];
        let mut replace = false;
        let mut absttl = false;
        let mut i = 3;
        while i < args.len() {
            if eq_ignore_case(&args[i], b"REPLACE") {
                replace = true;
                i += 1;
            } else if eq_ignore_case(&args[i], b"ABSTTL") {
                absttl = true;
                i += 1;
            } else {
                return Err(router::wrong_args("RESTORE", ""));
            }
        }

        let mut stored = dump_decode(payload)?;
        apply_restore_ttl(&mut stored, ttl_ms, absttl);

        let _lock = self.key_lock.lock(key).await;
        let get_result = self.storage.get_typed(db, key).await;
        if let Err(e) = &get_result {
            tracing::error!(key = %String::from_utf8_lossy(key), error = %e, "RESTORE get_typed failed");
        }
        if get_result?.is_some() {
            if replace {
                tracing::error!(key = %String::from_utf8_lossy(key), "RESTORE deleting existing key");
                self.storage.delete(db, key).await?;
            } else {
                return Err(Error::Command(
                    "BUSYKEY Target key name already exists.".into(),
                ));
            }
        }
        let set_result = self.storage.put_value(db, key, stored).await;
        if let Err(e) = &set_result {
            tracing::error!(key = %String::from_utf8_lossy(key), error = %e, "RESTORE put_value failed");
        }
        set_result?;
        Ok(router::ok())
    }

    pub async fn migrate(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("MIGRATE", args, 5)?;
        let host =
            std::str::from_utf8(&args[0]).map_err(|_| Error::Command("ERR invalid host".into()))?;
        let port = parse_u16(&args[1])?;
        let key = &args[2];
        let dest_db = parse_i64(&args[3])? as usize;
        let timeout_ms = parse_i64(&args[4])? as u64;

        let mut copy = false;
        let mut replace = false;
        let mut auth: Option<migrate::RestoreAuth<'_>> = None;
        let mut batch_keys: Vec<&[u8]> = Vec::new();
        let mut i = 5;
        while i < args.len() {
            if eq_ignore_case(&args[i], b"COPY") {
                copy = true;
                i += 1;
            } else if eq_ignore_case(&args[i], b"REPLACE") {
                replace = true;
                i += 1;
            } else if eq_ignore_case(&args[i], b"AUTH2") {
                if i + 2 >= args.len() {
                    return Err(router::wrong_args("MIGRATE", ""));
                }
                auth = Some(migrate::RestoreAuth::Acl {
                    username: &args[i + 1],
                    password: &args[i + 2],
                });
                i += 3;
            } else if eq_ignore_case(&args[i], b"AUTH") {
                if i + 1 >= args.len() {
                    return Err(router::wrong_args("MIGRATE", ""));
                }
                auth = Some(migrate::RestoreAuth::LegacyPassword(&args[i + 1]));
                i += 2;
            } else if eq_ignore_case(&args[i], b"KEYS") {
                if i + 1 >= args.len() {
                    return Err(router::wrong_args("MIGRATE", ""));
                }
                i += 1;
                while i < args.len() {
                    if eq_ignore_case(&args[i], b"COPY")
                        || eq_ignore_case(&args[i], b"REPLACE")
                        || eq_ignore_case(&args[i], b"AUTH")
                        || eq_ignore_case(&args[i], b"AUTH2")
                    {
                        break;
                    }
                    batch_keys.push(&args[i]);
                    i += 1;
                }
                continue;
            } else {
                return Err(router::wrong_args("MIGRATE", ""));
            }
        }

        if !batch_keys.is_empty() {
            for key in batch_keys {
                let Some(stored) = self.storage.get_expanded(db, key).await? else {
                    continue;
                };
                let payload = dump_encode(&stored)?;
                let ttl_ms = migrate_ttl_ms(&stored);
                migrate::send_restore(migrate::RestoreTarget {
                    host,
                    port,
                    timeout_ms,
                    dest_db,
                    key,
                    ttl_ms,
                    payload: &payload,
                    replace,
                    auth,
                })
                .await?;
                if !copy {
                    self.storage.delete(db, key).await?;
                }
            }
            return Ok(router::ok());
        }

        let _lock = self.key_lock.lock(key).await;
        let Some(stored) = self.storage.get_expanded(db, key).await? else {
            return Ok(router::ok());
        };

        let payload = dump_encode(&stored)?;
        let ttl_ms = migrate_ttl_ms(&stored);

        migrate::send_restore(migrate::RestoreTarget {
            host,
            port,
            timeout_ms,
            dest_db,
            key,
            ttl_ms,
            payload: &payload,
            replace,
            auth,
        })
        .await?;

        if !copy {
            self.storage.delete(db, key).await?;
        }
        Ok(router::ok())
    }
}

fn map_expiretime(stored: Option<crate::storage::StoredValue>) -> i64 {
    match stored {
        None => -2,
        Some(s) => match s.expires_at {
            None => -1,
            Some(ms) => (ms / 1000) as i64,
        },
    }
}

fn map_pexpiretime(stored: Option<crate::storage::StoredValue>) -> i64 {
    match stored {
        None => -2,
        Some(s) => match s.expires_at {
            None => -1,
            Some(ms) => ms as i64,
        },
    }
}

fn apply_restore_ttl(stored: &mut crate::storage::StoredValue, ttl_ms: i64, absttl: bool) {
    if ttl_ms == 0 {
        stored.expires_at = None;
        return;
    }
    if absttl {
        stored.expires_at = Some(ttl_ms as u64);
    } else {
        stored.expires_at = Some(now_ms().saturating_add(ttl_ms as u64));
    }
}

fn migrate_ttl_ms(stored: &crate::storage::StoredValue) -> i64 {
    match stored.expires_at {
        None => 0,
        Some(exp) => {
            let now = now_ms();
            if now >= exp {
                0
            } else {
                (exp - now) as i64
            }
        }
    }
}

fn parse_u16(b: &Bytes) -> Result<u16> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<u16>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}

fn map_ttl_seconds(ttl: Option<i64>) -> i64 {
    match ttl {
        None => -2,
        Some(TTL_NO_EXPIRY) => -1,
        Some(ms) => ms / 1000,
    }
}

fn map_pttl_ms(ttl: Option<i64>) -> i64 {
    match ttl {
        None => -2,
        Some(TTL_NO_EXPIRY) => -1,
        Some(ms) => ms,
    }
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn parse_i64(b: &Bytes) -> Result<i64> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}

fn parse_u64(b: &Bytes) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command("ERR invalid cursor".into()))?;
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR invalid cursor".into()))
}

fn eq_ignore_case(a: &Bytes, b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}
