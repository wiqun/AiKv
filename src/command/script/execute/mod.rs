//! `redis.call` / `redis.pcall` 在调用任务上同步执行, 存储走 `ScriptIo`.
//! 按命令名 dispatch 到各域 exec_* → 写操作进 `ScriptTransaction` 缓冲, 读操作优先读缓冲.
//!
//! 子模块: `string` / `hash` / `list` / `set` / `zset` (各域 `exec_*`); JSON 仍转发 `json_exec`.
//!
//! # 执行流程
//!
//! ```text
//! redis.call("GET", key, …) ── mlua 闭包 ──> redis_call_async(lua, storage, txn, declared, args, throw_error)
//!   │ 1. Lua 参数 → Bytes: String/Integer/Number/Boolean; 其它类型 throw_error → RuntimeError, 否则 Nil
//!   ├─ 2. validate_keys: 用 command_key_indices 定位命令 key 参数, 须在 EVAL 声明的 KEYS 内
//!   │       未声明 → "ERR Script attempted to access undeclared key"
//!   ├─ 3. dispatch(cmd): match 到各域 exec_* (String/Hash/List/Set/ZSet/JSON/EXPIRE)
//!   │       读: txn.get / txn.get_value (写缓冲优先; CollectionHeader 透明展开为完整集合)
//!   │       写: txn.set_* / txn.delete / txn.set_expire_at 缓冲
//!   ├─ 4a. 成功 → resp_to_lua (RESP → LuaValue)
//!   └─ 4b. 失败 → throw_error ? mlua RuntimeError : pcall_error_table ({err="…"})
//! ```
//!
//! # Invariant
//!
//! - KEYS 校验: 脚本访问未声明的 key 报错 (redis.call 抛错 / redis.pcall 返回 `{err}` 表).
//! - 写操作全部进入 `ScriptTransaction.write_buffer`, 由 `script.rs::execute_script` 结束后
//!   单次 `commit` 落盘; 脚本失败时缓冲随 txn drop 丢弃, 保证原子性.
//! - `pcall` 错误返回 `{err="…"}` 表 (非 nil), 与 `convert.rs::pcall_error_table` 对齐.

use std::collections::HashSet;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::convert::{pcall_error_table, resp_to_lua};
use crate::command::script::json_exec;
use crate::command::script::transaction::{expire_seconds_to_at, parse_i64_arg, ScriptTransaction};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::ScriptIo;

mod hash;
mod list;
mod set;
mod string;
mod zset;

use self::hash::{
    exec_hdel, exec_hexists, exec_hget, exec_hgetall, exec_hincrby, exec_hlen, exec_hmget,
    exec_hset, exec_hsetnx,
};
use self::list::{exec_lindex, exec_llen, exec_lpop, exec_lpush, exec_lrange};
use self::set::{exec_sadd, exec_scard, exec_sismember, exec_smembers, exec_srem, exec_sscan};
use self::string::{
    exec_append, exec_decrby, exec_del, exec_exists, exec_get, exec_incr, exec_incrby,
    exec_incrbyfloat, exec_set, exec_strlen,
};
use self::zset::{
    exec_zadd, exec_zcard, exec_zrange, exec_zrangebyscore, exec_zrank, exec_zrem, exec_zrevrange,
    exec_zrevrangebyscore, exec_zscore,
};

use crate::storage::watch_version::extract_hash_tag;

pub(super) fn wrong_count(cmd: &str) -> Error {
    router::wrong_args(cmd, "")
}

pub(super) fn key(b: &Bytes) -> Vec<u8> {
    b.to_vec()
}

#[derive(Debug, Clone)]
pub struct DeclaredKeys {
    exact: HashSet<Vec<u8>>,
    tags: HashSet<Vec<u8>>,
}

impl DeclaredKeys {
    pub fn new(keys: &[Bytes]) -> Self {
        let mut exact = HashSet::with_capacity(keys.len());
        let mut tags = HashSet::with_capacity(keys.len());
        for k in keys {
            exact.insert(k.to_vec());
            let tag = extract_hash_tag(k);
            if tag != k.as_ref() {
                tags.insert(tag.to_vec());
            }
        }
        Self { exact, tags }
    }

    pub fn is_declared(&self, k: &[u8]) -> bool {
        if self.exact.contains(k) {
            return true;
        }
        if !self.tags.is_empty() {
            let tag = extract_hash_tag(k);
            if tag != k && self.tags.contains(tag) {
                return true;
            }
        }
        false
    }
}

fn validate_keys(declared: &DeclaredKeys, cmd: &str, args: &[Bytes]) -> Result<()> {
    match cmd {
        "DEL" | "EXISTS" => {
            for a in args {
                check_key(declared, a.as_ref())?;
            }
        }
        "JSON.MGET" => {
            for a in args.iter().take(args.len().saturating_sub(1)) {
                check_key(declared, a.as_ref())?;
            }
        }
        _ => {
            if let Some(first) = args.first() {
                check_key(declared, first.as_ref())?;
            }
        }
    }
    Ok(())
}

fn check_key(declared: &DeclaredKeys, k: &[u8]) -> Result<()> {
    if declared.is_declared(k) {
        Ok(())
    } else {
        Err(Error::Command(format!(
            "ERR Script attempted to access undeclared key '{:?}'",
            String::from_utf8_lossy(k)
        )))
    }
}

pub fn redis_call(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<mlua::Value> {
    let mut prep = super::call_stats::CallTimer::start(super::call_stats::ScriptCallOp::Prep);
    let Some(cmd_args) = lua_args_to_bytes(args, throw_error)? else {
        return Ok(mlua::Value::Nil);
    };
    let mut cmd_buf = [0u8; 32];
    let mut cmd_owned = String::new();
    let command = ascii_command(cmd_args[0].as_ref(), &mut cmd_buf, &mut cmd_owned);
    let command_args = &cmd_args[1..];
    if let Err(e) = validate_keys(declared, command, command_args) {
        if throw_error {
            return Err(mlua::Error::RuntimeError(e.to_string()));
        }
        return pcall_error_table(lua, &e.to_string());
    }
    prep.finish();
    let family = super::call_stats::script_call_family(command);
    let _timed = super::call_stats::CallTimer::start(family);
    reply_call(lua, storage, txn, command, command_args, throw_error)
}

fn lua_args_to_bytes(
    args: mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<Vec<Bytes>>> {
    let mut cmd_args = Vec::with_capacity(args.len());
    for arg in args {
        match arg {
            mlua::Value::String(s) => cmd_args.push(Bytes::copy_from_slice(s.as_bytes().as_ref())),
            mlua::Value::Integer(i) => cmd_args.push(Bytes::from(i.to_string())),
            mlua::Value::Number(n) => cmd_args.push(Bytes::from(n.to_string())),
            mlua::Value::Boolean(b) => {
                cmd_args.push(Bytes::from_static(if b { b"1" } else { b"0" }))
            }
            _ => {
                if throw_error {
                    return Err(mlua::Error::RuntimeError(
                        "Invalid argument type".to_string(),
                    ));
                }
                return Ok(None);
            }
        }
    }
    if cmd_args.is_empty() {
        if throw_error {
            return Err(mlua::Error::RuntimeError(
                "No command specified".to_string(),
            ));
        }
        return Ok(None);
    }
    Ok(Some(cmd_args))
}

fn ascii_command<'a>(raw: &[u8], buf: &'a mut [u8; 32], owned: &'a mut String) -> &'a str {
    if raw.len() <= 32 {
        for (i, &b) in raw.iter().enumerate() {
            buf[i] = b.to_ascii_uppercase();
        }
        return std::str::from_utf8(&buf[..raw.len()]).unwrap_or("");
    }
    *owned = String::from_utf8_lossy(raw).to_ascii_uppercase();
    owned.as_str()
}

fn reply_call(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    command: &str,
    command_args: &[Bytes],
    throw_error: bool,
) -> mlua::Result<mlua::Value> {
    match dispatch(storage, txn, command, command_args) {
        Ok(resp) => resp_to_lua(lua, resp),
        Err(e) => {
            let msg = match &e {
                Error::Command(s) => s.clone(),
                _ => e.to_string(),
            };
            if throw_error {
                Err(mlua::Error::RuntimeError(msg))
            } else {
                pcall_error_table(lua, &msg)
            }
        }
    }
}

fn dispatch(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    cmd: &str,
    args: &[Bytes],
) -> Result<RespValue> {
    match cmd {
        "GET" => exec_get(storage, txn, args),
        "SET" => exec_set(storage, txn, args),
        "DEL" => exec_del(storage, txn, args),
        "EXISTS" => exec_exists(storage, txn, args),
        "INCR" => exec_incr(storage, txn, args, 1),
        "DECR" => exec_incr(storage, txn, args, -1),
        "INCRBY" => exec_incrby(storage, txn, args),
        "DECRBY" => exec_decrby(storage, txn, args),
        "INCRBYFLOAT" => exec_incrbyfloat(storage, txn, args),
        "APPEND" => exec_append(storage, txn, args),
        "STRLEN" => exec_strlen(storage, txn, args),
        "HGET" => exec_hget(storage, txn, args),
        "HSET" | "HMSET" => exec_hset(storage, txn, args),
        "HSETNX" => exec_hsetnx(storage, txn, args),
        "HDEL" => exec_hdel(storage, txn, args),
        "HGETALL" => exec_hgetall(storage, txn, args),
        "HMGET" => exec_hmget(storage, txn, args),
        "HINCRBY" => exec_hincrby(storage, txn, args),
        "HEXISTS" => exec_hexists(storage, txn, args),
        "HLEN" => exec_hlen(storage, txn, args),
        "LPUSH" => exec_lpush(storage, txn, args, true),
        "RPUSH" => exec_lpush(storage, txn, args, false),
        "LPOP" => exec_lpop(storage, txn, args, true),
        "RPOP" => exec_lpop(storage, txn, args, false),
        "LLEN" => exec_llen(storage, txn, args),
        "LRANGE" => exec_lrange(storage, txn, args),
        "LINDEX" => exec_lindex(storage, txn, args),
        "SADD" => exec_sadd(storage, txn, args),
        "SREM" => exec_srem(storage, txn, args),
        "SMEMBERS" => exec_smembers(storage, txn, args),
        "SISMEMBER" => exec_sismember(storage, txn, args),
        "SCARD" => exec_scard(storage, txn, args),
        "SSCAN" => exec_sscan(storage, txn, args),
        "ZADD" => exec_zadd(storage, txn, args),
        "ZREM" => exec_zrem(storage, txn, args),
        "ZSCORE" => exec_zscore(storage, txn, args),
        "ZRANK" => exec_zrank(storage, txn, args),
        "ZRANGE" => exec_zrange(storage, txn, args),
        "ZREVRANGE" => exec_zrevrange(storage, txn, args),
        "ZRANGEBYSCORE" => exec_zrangebyscore(storage, txn, args),
        "ZREVRANGEBYSCORE" => exec_zrevrangebyscore(storage, txn, args),
        "ZCARD" => exec_zcard(storage, txn, args),
        "TTL" => exec_ttl(storage, txn, args),
        "EXPIRE" => exec_expire(storage, txn, args),
        "JSON.GET" => json_exec::exec_json_get(storage, txn, args),
        "JSON.MGET" => json_exec::exec_json_mget(storage, txn, args),
        "JSON.SET" => json_exec::exec_json_set(storage, txn, args),
        "JSON.DEL" => json_exec::exec_json_del(storage, txn, args),
        "JSON.TYPE" => json_exec::exec_json_type(storage, txn, args),
        "JSON.STRLEN" => json_exec::exec_json_strlen(storage, txn, args),
        "JSON.ARRLEN" => json_exec::exec_json_arrlen(storage, txn, args),
        "JSON.OBJLEN" => json_exec::exec_json_objlen(storage, txn, args),
        "JSON.NUMINCRBY" => json_exec::exec_json_numincrby(storage, txn, args),
        "JSON.ARRAPPEND" => json_exec::exec_json_arrappend(storage, txn, args),
        _ => Err(Error::Command(format!(
            "ERR Command not supported in scripts: {cmd}"
        ))),
    }
}

fn exec_ttl(storage: &dyn ScriptIo, txn: &ScriptTransaction, args: &[Bytes]) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("TTL"));
    }
    let k = &args[0];
    if !txn.exists(storage, k)? {
        return Ok(router::integer(-2));
    }
    let ttl_res = storage.ttl(txn.db_index, k)?;
    let res = match ttl_res {
        None => -2,
        Some(crate::storage::TTL_NO_EXPIRY) => -1,
        Some(ms) => (ms / 1000).max(0),
    };
    Ok(router::integer(res))
}

fn exec_expire(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("EXPIRE"));
    }
    let seconds = parse_i64_arg(&args[1], "seconds")?;
    if !txn.exists(storage, &args[0])? {
        return Ok(router::integer(0));
    }
    if seconds <= 0 {
        txn.delete(key(&args[0]));
    } else {
        txn.set_expire_at(key(&args[0]), expire_seconds_to_at(seconds));
    }
    Ok(router::integer(1))
}
