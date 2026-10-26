//! String 命令: GET/SET/GETSET/SETNX/SETEX/PSETEX/GETEX/GETDEL/MGET/MSET/MSETNX/STRLEN/GETRANGE/
//! SUBSTR/SETRANGE/SETBIT/GETBIT/BITCOUNT/BITFIELD/BITFIELD_RO/APPEND/INCR/DECR/INCRBY/DECRBY/INCRBYFLOAT,
//! 以及 DEL/UNLINK/EXISTS.
//!
//! # Invariant
//!
//! - 类型分轨: String 一律走 `storage.get`/`set` (非 typed 写路径); 访问非 String key →
//!   `WRONGTYPE` (见 `storage/types.rs`).
//! - MGET wrong-type: 对齐 Redis 7, 对非 String / missing key 返回 `nil`, 整命令不失败.
//! - mutating 写 (SET NX/XX、INCR 等) 先 `key_lock.lock(key)` 再读改写.
//! - DEL/UNLINK/EXISTS 位于本文件 (registry 中归 String 域). UNLINK 与 DEL 同语义.

use std::sync::Arc;

use bytes::Bytes;

use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::{now_ms, KvStorage, WRONGTYPE};

pub struct StringCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetCondition {
    None,
    Nx,
    Xx,
}

impl StringCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self { storage, key_lock }
    }

    pub async fn get(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("GET", args, 1)?;
        match self.storage.get(db, &args[0]).await? {
            None => Ok(router::nil_bulk()),
            Some(v) => Ok(router::bulk(v)),
        }
    }

    pub async fn getset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("GETSET", args, 2)?;
        let with_get = vec![args[0].clone(), args[1].clone(), Bytes::from_static(b"GET")];
        self.set(db, &with_get).await
    }

    pub async fn substr(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SUBSTR", args, 3)?;
        self.getrange(db, args).await
    }

    pub async fn unlink(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("UNLINK", args, 1)?;
        self.del(db, args).await
    }

    pub async fn msetnx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 2 || !args.len().is_multiple_of(2) {
            return Err(router::wrong_args("MSETNX", ""));
        }
        let keys: Vec<&[u8]> = args.chunks(2).map(|chunk| chunk[0].as_ref()).collect();
        let _guard = self.key_lock.lock_keys_sorted(&keys).await;
        for key in &keys {
            if self.storage.exists(db, key).await? {
                return Ok(router::integer(0));
            }
        }
        self.mset(db, args).await?;
        Ok(router::integer(1))
    }

    pub async fn set(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(router::wrong_args("SET", ""));
        }
        let opts = parse_set_options(args)?;

        // GET / KEEPTTL / NX / XX 都需要锁和旧值
        let needs_lock = opts.return_old
            || opts.keep_ttl
            || matches!(opts.condition, SetCondition::Nx | SetCondition::Xx);

        if needs_lock {
            let _lock = self.key_lock.lock(&opts.key).await;
            let old_value = self.storage.get(db, &opts.key).await?;

            // NX: 仅在 key 不存在时设置
            if opts.condition == SetCondition::Nx {
                if let Some(val) = old_value {
                    return Ok(if opts.return_old {
                        router::bulk(val)
                    } else {
                        router::nil_bulk()
                    });
                }
            }
            // XX: 仅在 key 存在时设置
            if opts.condition == SetCondition::Xx && old_value.is_none() {
                return Ok(router::nil_bulk());
            }

            // KEEPTTL: 保留现有过期时间
            let previous_ttl = if opts.keep_ttl {
                self.storage
                    .get_typed(db, &opts.key)
                    .await?
                    .and_then(|s| s.expires_at)
            } else {
                None
            };
            let expire = if opts.keep_ttl {
                previous_ttl
            } else {
                opts.expire_at
            };

            self.apply_set(db, &opts.key, &opts.value, expire).await?;
            if previous_ttl.is_some() {
                self.storage.release_seen_ttl(db).await?;
            }

            return Ok(if opts.return_old {
                match old_value {
                    Some(v) => router::bulk(v),
                    None => router::nil_bulk(),
                }
            } else {
                router::ok()
            });
        }

        self.apply_set(db, &opts.key, &opts.value, opts.expire_at)
            .await
    }

    async fn apply_set(
        &self,
        db: usize,
        key: &[u8],
        value: &[u8],
        expire_at: Option<u64>,
    ) -> Result<RespValue> {
        if let Some(at) = expire_at {
            self.storage.set_with_ttl(db, key, value, at).await?;
        } else {
            self.storage.set(db, key, value).await?;
        }
        Ok(router::ok())
    }

    pub async fn mget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("MGET", args, 1)?;
        let keys: Vec<Vec<u8>> = args.iter().map(|b| b.to_vec()).collect();
        let values = self.storage.mget(db, &keys).await?;
        let items: Vec<RespValue> = values
            .into_iter()
            .map(|v| match v {
                Some(b) => router::bulk(b),
                None => router::nil_bulk(),
            })
            .collect();
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn mset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 2 || !args.len().is_multiple_of(2) {
            return Err(router::wrong_args("MSET", ""));
        }
        let mut pairs = Vec::with_capacity(args.len() / 2);
        for chunk in args.chunks(2) {
            pairs.push((chunk[0].to_vec(), chunk[1].to_vec()));
        }
        self.storage.mset(db, &pairs).await?;
        Ok(router::ok())
    }

    pub async fn del(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("DEL", args, 1)?;
        let mut count = 0i64;
        for key in args {
            if self.storage.delete(db, key).await? {
                count += 1;
            }
        }
        Ok(router::integer(count))
    }

    pub async fn exists(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("EXISTS", args, 1)?;
        let mut count = 0i64;
        for key in args {
            if self.storage.exists(db, key).await? {
                count += 1;
            }
        }
        Ok(router::integer(count))
    }

    pub async fn strlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("STRLEN", args, 1)?;
        match self.storage.get(db, &args[0]).await? {
            None => Ok(router::integer(0)),
            Some(v) => Ok(router::integer(v.len() as i64)),
        }
    }

    pub async fn getrange(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("GETRANGE", args, 3)?;
        let start = parse_i64_arg(&args[1], "GETRANGE")?;
        let end = parse_i64_arg(&args[2], "GETRANGE")?;
        match self.storage.get(db, &args[0]).await? {
            None => Ok(router::bulk(Vec::new())),
            Some(value) => Ok(router::bulk(string_range_slice(&value, start, end))),
        }
    }

    pub async fn setrange(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SETRANGE", args, 3)?;
        let key = &args[0];
        let offset = parse_setrange_offset(&args[1])?;
        let patch = &args[2];
        let _lock = self.key_lock.lock(key).await;
        let mut current = self.storage.get(db, key).await?.unwrap_or_default();
        let end = offset
            .checked_add(patch.len())
            .ok_or_else(|| Error::Command("ERR offset is out of range".into()))?;
        if end > current.len() {
            current.resize(end, 0);
        }
        current[offset..end].copy_from_slice(patch);
        let len = current.len() as i64;
        self.storage.set(db, key, &current).await?;
        Ok(router::integer(len))
    }

    pub async fn setbit(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SETBIT", args, 3)?;
        let key = &args[0];
        let offset = parse_bit_offset(&args[1])?;
        let bit = parse_bit_value(&args[2])?;
        let _lock = self.key_lock.lock(key).await;
        let mut data = self.storage.get(db, key).await?.unwrap_or_default();
        let prev = read_bit(&data, offset);
        write_bit(&mut data, offset, bit);
        self.storage.set(db, key, &data).await?;
        Ok(router::integer(i64::from(prev)))
    }

    pub async fn getbit(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("GETBIT", args, 2)?;
        let data = self.storage.get(db, &args[0]).await?.unwrap_or_default();
        let offset = parse_bit_offset(&args[1])?;
        Ok(router::integer(i64::from(read_bit(&data, offset))))
    }

    pub async fn bitcount(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(router::wrong_args("BITCOUNT", ""));
        }
        let data = self.storage.get(db, &args[0]).await?;
        let n = crate::command::bitfield::bitcount(data.as_deref(), args)?;
        Ok(router::integer(n))
    }

    pub async fn bitfield(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.run_bitfield(db, args, false).await
    }

    pub async fn bitfield_ro(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.run_bitfield(db, args, true).await
    }

    async fn run_bitfield(&self, db: usize, args: &[Bytes], readonly: bool) -> Result<RespValue> {
        let cmd = if readonly { "BITFIELD_RO" } else { "BITFIELD" };
        if args.is_empty() {
            return Err(router::wrong_args(cmd, ""));
        }
        let plan = crate::command::bitfield::parse_bitfield(&args[1..], readonly)?;
        let key = &args[0];
        if !plan.writes {
            let existing = self.storage.get(db, key).await?;
            let result = crate::command::bitfield::execute_bitfield(existing.as_deref(), &plan);
            return Ok(RespValue::Array(Some(result.replies)));
        }
        let _lock = self.key_lock.lock(key).await;
        let existing = self.storage.get(db, key).await?;
        let result = crate::command::bitfield::execute_bitfield(existing.as_deref(), &plan);
        if let Some(bytes) = &result.stored {
            self.storage.set(db, key, bytes).await?;
        }
        Ok(RespValue::Array(Some(result.replies)))
    }

    pub async fn append(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("APPEND", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let mut current: Vec<u8> = self.storage.get(db, key).await?.unwrap_or_default();
        current.extend_from_slice(&args[1]);
        let len = current.len() as i64;
        self.storage.set(db, key, &current).await?;
        Ok(router::integer(len))
    }

    pub async fn incr(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.incrby_delta(db, args, 1, "INCR").await
    }

    pub async fn decr(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.incrby_delta(db, args, -1, "DECR").await
    }

    pub async fn incrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("INCRBY", args, 2)?;
        let delta = parse_i64_arg(&args[1], "INCRBY")?;
        self.incrby_delta(db, &args[0..1], delta, "INCRBY").await
    }

    pub async fn decrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("DECRBY", args, 2)?;
        let delta = parse_i64_arg(&args[1], "DECRBY")?;
        self.incrby_delta(db, &args[0..1], -delta, "DECRBY").await
    }

    async fn incrby_delta(
        &self,
        db: usize,
        args: &[Bytes],
        delta: i64,
        cmd: &str,
    ) -> Result<RespValue> {
        router::require_args(cmd, args, 1)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let stored = self.storage.get(db, key).await?;
        match stored {
            None => {
                self.storage
                    .set(db, key, delta.to_string().as_bytes())
                    .await?;
                Ok(router::integer(delta))
            }
            Some(bytes) => {
                if let Ok(v) = parse_i64_bytes(&bytes) {
                    let result = v.checked_add(delta).ok_or_else(|| {
                        Error::Command("ERR increment or decrement would overflow".into())
                    })?;
                    self.storage
                        .set(db, key, result.to_string().as_bytes())
                        .await?;
                    Ok(router::integer(result))
                } else if let Ok(f) = parse_f64_bytes(&bytes) {
                    let result = f + delta as f64;
                    let s = format_float(result);
                    self.storage.set(db, key, s.as_bytes()).await?;
                    Ok(router::bulk(s.into_bytes()))
                } else {
                    Err(router::wrongtype())
                }
            }
        }
    }

    pub async fn incrbyfloat(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("INCRBYFLOAT", args, 2)?;
        let key = &args[0];
        let delta = parse_f64_arg(&args[1], "INCRBYFLOAT")?;
        if !delta.is_finite() {
            return Err(Error::Command("ERR value is not a valid float".into()));
        }
        let _lock = self.key_lock.lock(key).await;
        let stored = self.storage.get(db, key).await?;
        let result = match stored {
            None => delta,
            Some(bytes) => {
                let v = parse_f64_bytes(&bytes).map_err(|_| router::wrongtype())?;
                let r = v + delta;
                if !r.is_finite() {
                    return Err(Error::Command(
                        "ERR increment would produce NaN or Infinity".into(),
                    ));
                }
                r
            }
        };
        let s = format_float(result);
        self.storage.set(db, key, s.as_bytes()).await?;
        Ok(router::bulk(s.into_bytes()))
    }

    pub async fn setnx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SETNX", args, 2)?;
        let key = &args[0];
        let value = &args[1];
        let _lock = self.key_lock.lock(key).await;
        let exists = self.storage.get(db, key).await?.is_some();
        if exists {
            return Ok(router::integer(0));
        }
        self.storage.set(db, key, value).await?;
        Ok(router::integer(1))
    }

    pub async fn setex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SETEX", args, 3)?;
        let key = &args[0];
        let secs = parse_u64_arg(&args[1], "SETEX")?;
        if secs == 0 {
            return Err(Error::Command(
                "ERR invalid expire time in 'setex' command".into(),
            ));
        }
        let value = &args[2];
        let expire_at = now_ms().saturating_add(secs.saturating_mul(1000));
        self.storage.set_with_ttl(db, key, value, expire_at).await?;
        Ok(router::ok())
    }

    pub async fn psetex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("PSETEX", args, 3)?;
        let key = &args[0];
        let ms = parse_u64_arg(&args[1], "PSETEX")?;
        if ms == 0 {
            return Err(Error::Command(
                "ERR invalid expire time in 'psetex' command".into(),
            ));
        }
        let value = &args[2];
        let expire_at = now_ms().saturating_add(ms);
        self.storage.set_with_ttl(db, key, value, expire_at).await?;
        Ok(router::ok())
    }

    pub async fn getdel(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("GETDEL", args, 1)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        match self.storage.get(db, key).await? {
            None => Ok(router::nil_bulk()),
            Some(v) => {
                self.storage.delete(db, key).await?;
                Ok(router::bulk(v))
            }
        }
    }

    pub async fn getex(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("GETEX", args, 1)?;
        let key = &args[0];
        let value = match self.storage.get(db, key).await? {
            None => return Ok(router::nil_bulk()),
            Some(v) => v,
        };
        if args.len() == 1 {
            return Ok(router::bulk(value));
        }
        let opt = String::from_utf8_lossy(&args[1]).to_ascii_uppercase();
        match opt.as_str() {
            "PERSIST" => {
                self.storage.persist(db, key).await?;
            }
            "EX" => {
                if args.len() < 3 {
                    return Err(router::wrong_args("GETEX", ""));
                }
                let secs = parse_u64_arg(&args[2], "GETEX")?;
                if secs == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'getex' command".into(),
                    ));
                }
                let expire_at = now_ms().saturating_add(secs.saturating_mul(1000));
                self.storage
                    .set_with_ttl(db, key, &value, expire_at)
                    .await?;
            }
            "PX" => {
                if args.len() < 3 {
                    return Err(router::wrong_args("GETEX", ""));
                }
                let ms = parse_u64_arg(&args[2], "GETEX")?;
                if ms == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'getex' command".into(),
                    ));
                }
                let expire_at = now_ms().saturating_add(ms);
                self.storage
                    .set_with_ttl(db, key, &value, expire_at)
                    .await?;
            }
            "EXAT" => {
                if args.len() < 3 {
                    return Err(router::wrong_args("GETEX", ""));
                }
                let ts_secs = parse_u64_arg(&args[2], "GETEX")?;
                let expire_at = ts_secs.saturating_mul(1000);
                self.storage
                    .set_with_ttl(db, key, &value, expire_at)
                    .await?;
            }
            "PXAT" => {
                if args.len() < 3 {
                    return Err(router::wrong_args("GETEX", ""));
                }
                let expire_at = parse_u64_arg(&args[2], "GETEX")?;
                self.storage
                    .set_with_ttl(db, key, &value, expire_at)
                    .await?;
            }
            _ => return Err(Error::Command("ERR syntax error".into())),
        }
        Ok(router::bulk(value))
    }
}

pub(crate) struct SetOptions {
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) expire_at: Option<u64>,
    pub(crate) condition: SetCondition,
    pub(crate) return_old: bool,
    pub(crate) keep_ttl: bool,
}

pub(crate) fn parse_set_options(args: &[Bytes]) -> Result<SetOptions> {
    let key = args[0].to_vec();
    let value = args[1].to_vec();
    let mut expire_at: Option<u64> = None;
    let mut condition = SetCondition::None;
    let mut return_old = false;
    let mut keep_ttl = false;
    let mut i = 2;
    while i < args.len() {
        let opt = String::from_utf8_lossy(&args[i]).to_ascii_uppercase();
        match opt.as_str() {
            "EX" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let secs = parse_u64_arg(&args[i], "SET")?;
                if secs == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(now_ms().saturating_add(secs.saturating_mul(1000)));
            }
            "PX" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let ms = parse_u64_arg(&args[i], "SET")?;
                if ms == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(now_ms().saturating_add(ms));
            }
            "EXAT" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let ts_secs = parse_u64_arg(&args[i], "SET")?;
                if ts_secs == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(ts_secs.saturating_mul(1000));
            }
            "PXAT" => {
                if expire_at.is_some() {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                i += 1;
                if i >= args.len() {
                    return Err(router::wrong_args("SET", ""));
                }
                let ts_ms = parse_u64_arg(&args[i], "SET")?;
                if ts_ms == 0 {
                    return Err(Error::Command(
                        "ERR invalid expire time in 'set' command".into(),
                    ));
                }
                expire_at = Some(ts_ms);
            }
            "NX" => {
                if condition != SetCondition::None {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                condition = SetCondition::Nx;
            }
            "XX" => {
                if condition != SetCondition::None {
                    return Err(Error::Command("ERR syntax error".into()));
                }
                condition = SetCondition::Xx;
            }
            "GET" => {
                return_old = true;
            }
            "KEEPTTL" => {
                keep_ttl = true;
            }
            _ => return Err(Error::Command("ERR syntax error".into())),
        }
        i += 1;
    }
    Ok(SetOptions {
        key,
        value,
        expire_at,
        condition,
        return_old,
        keep_ttl,
    })
}

/// Redis bit offset: 0 is the leftmost bit of the first byte.
pub(crate) fn read_bit(data: &[u8], offset: u64) -> u8 {
    let byte_idx = (offset / 8) as usize;
    let bit_pos = 7 - (offset % 8);
    let byte = data.get(byte_idx).copied().unwrap_or(0);
    (byte >> bit_pos) & 1
}

pub(crate) fn write_bit(data: &mut Vec<u8>, offset: u64, bit: u8) {
    let byte_idx = (offset / 8) as usize;
    let bit_pos = 7 - (offset % 8);
    if data.len() <= byte_idx {
        data.resize(byte_idx + 1, 0);
    }
    let mask = 1u8 << bit_pos;
    if bit == 1 {
        data[byte_idx] |= mask;
    } else {
        data[byte_idx] &= !mask;
    }
}

/// Redis GETRANGE inclusive byte range (negative indices count from end).
pub(crate) fn string_range_slice(value: &[u8], start: i64, end: i64) -> Vec<u8> {
    let len = value.len() as i64;
    if len == 0 {
        return Vec::new();
    }

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
        Vec::new()
    } else {
        value[start_idx..=end_idx].to_vec()
    }
}

pub(crate) fn parse_setrange_offset(b: &[u8]) -> Result<usize> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR offset is out of range".into()))?;
    if s.starts_with('-') {
        return Err(Error::Command("ERR offset is out of range".into()));
    }
    s.parse::<usize>()
        .map_err(|_| Error::Command("ERR offset is out of range".into()))
}

pub(crate) fn parse_bit_offset(b: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR bit offset is not an integer or out of range".into()))?;
    if s.starts_with('-') {
        return Err(Error::Command(
            "ERR bit offset is not an integer or out of range".into(),
        ));
    }
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR bit offset is not an integer or out of range".into()))
}

pub(crate) fn parse_bit_value(b: &[u8]) -> Result<u8> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR bit is not an integer or out of range".into()))?;
    match s {
        "0" => Ok(0),
        "1" => Ok(1),
        _ => Err(Error::Command(
            "ERR bit is not an integer or out of range".into(),
        )),
    }
}

fn parse_i64_arg(b: &[u8], _cmd: &str) -> Result<i64> {
    parse_i64_bytes(b)
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))
}

fn parse_u64_arg(b: &[u8], _cmd: &str) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command("ERR syntax error".into()))?;
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR syntax error".into()))
}

fn parse_f64_arg(b: &[u8], _cmd: &str) -> Result<f64> {
    parse_f64_bytes(b)
}

fn parse_i64_bytes(b: &[u8]) -> Result<i64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command(WRONGTYPE.into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command(WRONGTYPE.into()))
}

fn parse_f64_bytes(b: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command(WRONGTYPE.into()))?;
    s.parse::<f64>()
        .map_err(|_| Error::Command(WRONGTYPE.into()))
}

fn format_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
