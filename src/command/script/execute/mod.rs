//! `redis.call` / `redis.pcall` 在脚本线程上同步执行, 存储走 `ScriptIo`.
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

mod extra;
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
use self::set::{
    exec_sadd, exec_scard, exec_sismember, exec_smembers, exec_smismember, exec_srem, exec_sscan,
};
use self::string::{
    exec_append, exec_bitcount, exec_bitfield, exec_decrby, exec_del, exec_exists, exec_get,
    exec_getbit, exec_getset, exec_incr, exec_incrby, exec_incrbyfloat, exec_set, exec_setbit,
    exec_strlen,
};
use self::zset::{
    exec_zadd, exec_zcard, exec_zmscore, exec_zrange, exec_zrangebyscore, exec_zrank, exec_zrem,
    exec_zremrangebylex, exec_zremrangebyrank, exec_zremrangebyscore, exec_zrevrange,
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
    for raw in crate::command::command_keys(cmd, args) {
        check_key(declared, raw)?;
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

/// `redis.call` 允许的命令. 与下面的 `dispatch` 保持同一集合:
/// 在白名单里但没有 match 臂时, 仍返回 "not supported", 脚本回归测试会失败.
pub(crate) const SCRIPT_COMMANDS: &[&str] = &[
    "APPEND",
    "BITCOUNT",
    "BITFIELD",
    "BITFIELD_RO",
    "COPY",
    "DECR",
    "DECRBY",
    "DEL",
    "DUMP",
    "ECHO",
    "EXISTS",
    "EXPIRE",
    "EXPIREAT",
    "EXPIRETIME",
    "GET",
    "GETBIT",
    "GETDEL",
    "GETEX",
    "GETRANGE",
    "GETSET",
    "HDEL",
    "HEXISTS",
    "HGET",
    "HGETALL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HMSET",
    "HSCAN",
    "HSET",
    "HSETNX",
    "HVALS",
    "INCR",
    "INCRBY",
    "INCRBYFLOAT",
    "JSON.ARRAPPEND",
    "JSON.ARRLEN",
    "JSON.DEL",
    "JSON.GET",
    "JSON.MGET",
    "JSON.MSET",
    "JSON.NUMINCRBY",
    "JSON.OBJLEN",
    "JSON.SET",
    "JSON.STRLEN",
    "JSON.TYPE",
    "JSON.UPDATE",
    "LINDEX",
    "LINSERT",
    "LLEN",
    "LMOVE",
    "LPOP",
    "LPOS",
    "LPUSH",
    "LPUSHX",
    "LRANGE",
    "LREM",
    "LSET",
    "LTRIM",
    "MGET",
    "MSET",
    "MSETNX",
    "OBJECT",
    "PERSIST",
    "PEXPIRE",
    "PEXPIREAT",
    "PEXPIRETIME",
    "PING",
    "PSETEX",
    "PTTL",
    "RENAME",
    "RENAMENX",
    "RESTORE",
    "RPOP",
    "RPOPLPUSH",
    "RPUSH",
    "RPUSHX",
    "SADD",
    "SCARD",
    "SDIFF",
    "SDIFFSTORE",
    "SET",
    "SETBIT",
    "SETEX",
    "SETNX",
    "SETRANGE",
    "SINTER",
    "SINTERCARD",
    "SINTERSTORE",
    "SISMEMBER",
    "SMEMBERS",
    "SMISMEMBER",
    "SMOVE",
    "SPOP",
    "SRANDMEMBER",
    "SREM",
    "SSCAN",
    "STRLEN",
    "SUBSTR",
    "SUNION",
    "SUNIONSTORE",
    "TIME",
    "TTL",
    "TYPE",
    "UNLINK",
    "ZADD",
    "ZCARD",
    "ZCOUNT",
    "ZDIFF",
    "ZDIFFSTORE",
    "ZINCRBY",
    "ZINTER",
    "ZINTERSTORE",
    "ZLEXCOUNT",
    "ZMSCORE",
    "ZPOPMAX",
    "ZPOPMIN",
    "ZRANGE",
    "ZRANGEBYLEX",
    "ZRANGEBYSCORE",
    "ZRANK",
    "ZREM",
    "ZREMRANGEBYLEX",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREVRANGE",
    "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE",
    "ZREVRANK",
    "ZSCAN",
    "ZSCORE",
    "ZUNION",
    "ZUNIONSTORE",
];

fn dispatch(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    cmd: &str,
    args: &[Bytes],
) -> Result<RespValue> {
    if SCRIPT_COMMANDS.binary_search(&cmd).is_err() {
        return Err(Error::Command(format!(
            "ERR Command not supported in scripts: {cmd}"
        )));
    }
    match cmd {
        "GET" => exec_get(storage, txn, args),
        "GETBIT" => exec_getbit(storage, txn, args),
        "GETDEL" => extra::exec_getdel(storage, txn, args),
        "GETEX" => extra::exec_getex(storage, txn, args),
        "GETRANGE" => extra::exec_getrange(storage, txn, args),
        "GETSET" => exec_getset(storage, txn, args),
        "SUBSTR" => extra::exec_getrange(storage, txn, args),
        "SETBIT" => exec_setbit(storage, txn, args),
        "SETEX" => extra::exec_setex(storage, txn, args, false),
        "SETNX" => extra::exec_setnx(storage, txn, args),
        "SETRANGE" => extra::exec_setrange(storage, txn, args),
        "SINTER" => extra::exec_sinter(storage, txn, args),
        "SINTERCARD" => extra::exec_sintercard(storage, txn, args),
        "SINTERSTORE" => extra::exec_sstore(storage, txn, args, extra::SetOp::Inter, "SINTERSTORE"),
        "SMOVE" => extra::exec_smove(storage, txn, args),
        "SPOP" => extra::exec_spop(storage, txn, args),
        "SRANDMEMBER" => extra::exec_srandmember(storage, txn, args),
        "SUNION" => extra::exec_sunion(storage, txn, args),
        "SUNIONSTORE" => extra::exec_sstore(storage, txn, args, extra::SetOp::Union, "SUNIONSTORE"),
        "TIME" => extra::exec_time(args),
        "TYPE" => extra::exec_type(storage, txn, args),
        "BITCOUNT" => exec_bitcount(storage, txn, args),
        "BITFIELD" => exec_bitfield(storage, txn, args, false),
        "BITFIELD_RO" => exec_bitfield(storage, txn, args, true),
        "SET" => exec_set(storage, txn, args),
        "DEL" => exec_del(storage, txn, args),
        "UNLINK" => exec_del(storage, txn, args),
        "EXISTS" => exec_exists(storage, txn, args),
        "INCR" => exec_incr(storage, txn, args, 1),
        "DECR" => exec_incr(storage, txn, args, -1),
        "INCRBY" => exec_incrby(storage, txn, args),
        "DECRBY" => exec_decrby(storage, txn, args),
        "INCRBYFLOAT" => exec_incrbyfloat(storage, txn, args),
        "APPEND" => exec_append(storage, txn, args),
        "COPY" => extra::exec_copy(storage, txn, args),
        "DUMP" => extra::exec_dump(storage, txn, args),
        "ECHO" => extra::exec_echo(args),
        "EXPIREAT" => extra::exec_pexpire(storage, txn, args, false, true),
        "EXPIRETIME" => extra::exec_expiretime(storage, txn, args, false),
        "STRLEN" => exec_strlen(storage, txn, args),
        "HGET" => exec_hget(storage, txn, args),
        "HSET" | "HMSET" => exec_hset(storage, txn, args),
        "HSETNX" => exec_hsetnx(storage, txn, args),
        "HDEL" => exec_hdel(storage, txn, args),
        "HGETALL" => exec_hgetall(storage, txn, args),
        "HMGET" => exec_hmget(storage, txn, args),
        "HINCRBY" => exec_hincrby(storage, txn, args),
        "HINCRBYFLOAT" => extra::exec_hincrbyfloat(storage, txn, args),
        "HKEYS" => extra::exec_hkeys(storage, txn, args, false),
        "HSCAN" => extra::exec_hscan(storage, txn, args),
        "HVALS" => extra::exec_hkeys(storage, txn, args, true),
        "HEXISTS" => exec_hexists(storage, txn, args),
        "HLEN" => exec_hlen(storage, txn, args),
        "LPUSH" => exec_lpush(storage, txn, args, true, true),
        "LPUSHX" => exec_lpush(storage, txn, args, true, false),
        "RPUSH" => exec_lpush(storage, txn, args, false, true),
        "RPUSHX" => exec_lpush(storage, txn, args, false, false),
        "LPOP" => exec_lpop(storage, txn, args, true),
        "RPOP" => exec_lpop(storage, txn, args, false),
        "LLEN" => exec_llen(storage, txn, args),
        "LRANGE" => exec_lrange(storage, txn, args),
        "LINDEX" => exec_lindex(storage, txn, args),
        "LINSERT" => extra::exec_linsert(storage, txn, args),
        "LMOVE" => extra::exec_lmove(storage, txn, args),
        "RPOPLPUSH" => extra::exec_rpoplpush(storage, txn, args),
        "LPOS" => extra::exec_lpos(storage, txn, args),
        "LREM" => extra::exec_lrem(storage, txn, args),
        "LSET" => extra::exec_lset(storage, txn, args),
        "LTRIM" => extra::exec_ltrim(storage, txn, args),
        "MGET" => extra::exec_mget(storage, txn, args),
        "MSET" => extra::exec_mset(storage, txn, args),
        "MSETNX" => extra::exec_msetnx(storage, txn, args),
        "OBJECT" => extra::exec_object(storage, txn, args),
        "PERSIST" => extra::exec_persist(storage, txn, args),
        "PEXPIRE" => extra::exec_pexpire(storage, txn, args, true, false),
        "PEXPIREAT" => extra::exec_pexpire(storage, txn, args, true, true),
        "PEXPIRETIME" => extra::exec_expiretime(storage, txn, args, true),
        "PING" => extra::exec_ping(args),
        "PSETEX" => extra::exec_setex(storage, txn, args, true),
        "PTTL" => extra::exec_pttl(storage, txn, args, true),
        "RENAME" => extra::exec_rename(storage, txn, args, false),
        "RENAMENX" => extra::exec_rename(storage, txn, args, true),
        "RESTORE" => extra::exec_restore(storage, txn, args),
        "SADD" => exec_sadd(storage, txn, args),
        "SREM" => exec_srem(storage, txn, args),
        "SMEMBERS" => exec_smembers(storage, txn, args),
        "SISMEMBER" => exec_sismember(storage, txn, args),
        "SMISMEMBER" => exec_smismember(storage, txn, args),
        "SCARD" => exec_scard(storage, txn, args),
        "SDIFF" => extra::exec_sdiff(storage, txn, args),
        "SDIFFSTORE" => extra::exec_sstore(storage, txn, args, extra::SetOp::Diff, "SDIFFSTORE"),
        "SSCAN" => exec_sscan(storage, txn, args),
        "ZADD" => exec_zadd(storage, txn, args),
        "ZREM" => exec_zrem(storage, txn, args),
        "ZREMRANGEBYLEX" => exec_zremrangebylex(storage, txn, args),
        "ZREMRANGEBYRANK" => exec_zremrangebyrank(storage, txn, args),
        "ZREMRANGEBYSCORE" => exec_zremrangebyscore(storage, txn, args),
        "ZSCORE" => exec_zscore(storage, txn, args),
        "ZMSCORE" => exec_zmscore(storage, txn, args),
        "ZRANK" => exec_zrank(storage, txn, args),
        "ZRANGE" => exec_zrange(storage, txn, args),
        "ZREVRANGE" => exec_zrevrange(storage, txn, args),
        "ZRANGEBYSCORE" => exec_zrangebyscore(storage, txn, args),
        "ZREVRANGEBYSCORE" => exec_zrevrangebyscore(storage, txn, args),
        "ZCARD" => exec_zcard(storage, txn, args),
        "ZCOUNT" => extra::exec_zcount(storage, txn, args),
        "ZDIFF" => extra::exec_zdiff(storage, txn, args),
        "ZDIFFSTORE" => extra::exec_zdiffstore(storage, txn, args),
        "ZINCRBY" => extra::exec_zincrby(storage, txn, args),
        "ZINTER" => extra::exec_zinter(storage, txn, args),
        "ZINTERSTORE" => extra::exec_zinterstore(storage, txn, args),
        "ZLEXCOUNT" => extra::exec_zlex(storage, txn, args, false, true),
        "ZPOPMAX" => extra::exec_zpop(storage, txn, args, true),
        "ZPOPMIN" => extra::exec_zpop(storage, txn, args, false),
        "ZRANGEBYLEX" => extra::exec_zlex(storage, txn, args, false, false),
        "ZREVRANGEBYLEX" => extra::exec_zlex(storage, txn, args, true, false),
        "ZREVRANK" => extra::exec_zrevrank(storage, txn, args),
        "ZSCAN" => extra::exec_zscan(storage, txn, args),
        "ZUNION" => extra::exec_zunion(storage, txn, args),
        "ZUNIONSTORE" => extra::exec_zunionstore(storage, txn, args),
        "TTL" => exec_ttl(storage, txn, args),
        "EXPIRE" => exec_expire(storage, txn, args),
        "JSON.GET" => json_exec::exec_json_get(storage, txn, args),
        "JSON.MGET" => json_exec::exec_json_mget(storage, txn, args),
        "JSON.MSET" => json_exec::exec_json_mset(storage, txn, args),
        "JSON.UPDATE" => json_exec::exec_json_update(storage, txn, args),
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
