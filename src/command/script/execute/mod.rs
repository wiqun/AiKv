//! Lua 脚本内 `redis.call` / `redis.pcall` 的异步执行: 参数转换 → 集群同 slot 校验 →
//! 按命令名 dispatch 到各域 exec_* → 写操作进 `ScriptTransaction` 缓冲, 读操作优先读缓冲.
//!
//! 子模块: `string` / `hash` / `list` / `set` / `zset` (各域 `exec_*`); JSON 仍转发 `json_exec`.
//!
//! # 执行流程
//!
//! ```text
//! redis.call("GET", key, …) ── mlua 闭包 ──> redis_call_async(lua, storage, txn, declared, args, throw_error)
//!   │ 1. Lua 参数 → Bytes: String/Integer/Number/Boolean; 其它类型 throw_error → RuntimeError, 否则 Nil
//!   ├─ 2. check_key_slots: 集群下命令 key 必须与 KEYS[1] 同 slot
//!   │       无声明 key 时以第一次访问的 key 为锚; 跨 slot → "same slot"
//!   ├─ 3. dispatch(cmd): match 到各域 exec_* (String/Hash/List/Set/ZSet/JSON/EXPIRE)
//!   │       读: txn.get / txn.get_value (写缓冲优先; subkey 布局经 `get_expanded` 展开为完整集合)
//!   │       写: txn.set_* / txn.delete / txn.set_expire_at 缓冲
//!   ├─ 4a. 成功 → resp_to_lua (RESP → LuaValue)
//!   └─ 4b. 失败 → throw_error ? mlua RuntimeError : pcall_error_table ({err="…"})
//! ```
//!
//! # Invariant
//!
//! - 集群 slot: 脚本可以访问未出现在 KEYS 里的 key, 但必须与锚点同 slot
//!   (redis.call 抛错 / redis.pcall 返回 `{err}` 表). 非集群构建不检查.
//! - 写操作全部进入 `ScriptTransaction.write_buffer`, 由 `script.rs::execute_script` 结束后
//!   单次 `commit` 落盘; 脚本失败时缓冲随 txn drop 丢弃, 保证原子性.
//! - `pcall` 错误返回 `{err="…"}` 表 (非 nil), 与 `convert.rs::pcall_error_table` 对齐.

#[cfg(feature = "cluster")]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "cluster")]
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::convert::{pcall_error_table, resp_to_lua};
use crate::command::script::json_exec;
use crate::command::script::transaction::{expire_seconds_to_at, parse_i64_arg, ScriptTransaction};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::KvStorage;
use crate::storage::TTL_NO_EXPIRY;

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

pub(super) fn wrong_count(cmd: &str) -> Error {
    router::wrong_args(cmd, "")
}

pub(super) fn key(b: &Bytes) -> Vec<u8> {
    b.to_vec()
}

/// 一次 EVAL 内 `redis.call` / `redis.pcall` 共享的槽位锚点.
#[derive(Clone)]
pub(super) struct ScriptAccess {
    #[cfg(feature = "cluster")]
    anchor: Option<u16>,
    /// `u32::MAX` 表示还没有脚本内访问过的 key。slot 本身是 0..16384。
    #[cfg(feature = "cluster")]
    seen: Arc<AtomicU32>,
}

pub(super) fn script_access(keys: &[Bytes]) -> ScriptAccess {
    #[cfg(feature = "cluster")]
    {
        ScriptAccess {
            anchor: keys
                .first()
                .map(|k| aidb::cluster::router::key_to_slot(k.as_ref())),
            seen: Arc::new(AtomicU32::new(u32::MAX)),
        }
    }
    #[cfg(not(feature = "cluster"))]
    {
        let _ = keys;
        ScriptAccess {}
    }
}

fn check_key_slots(access: &ScriptAccess, cmd: &str, args: &[Bytes]) -> Result<()> {
    #[cfg(feature = "cluster")]
    {
        for idx in command_key_indices(cmd, args.len()) {
            if idx >= args.len() {
                continue;
            }
            let slot = aidb::cluster::router::key_to_slot(&args[idx]);
            let seen = access.seen.load(Ordering::Relaxed);
            let expected = access.anchor.or(if seen == u32::MAX {
                None
            } else {
                Some(seen as u16)
            });
            if let Some(expected) = expected {
                if slot != expected {
                    return Err(Error::Command(
                        "ERR Script attempted to access keys that do not hash to the same slot"
                            .into(),
                    ));
                }
            } else {
                access.seen.store(slot as u32, Ordering::Relaxed);
            }
        }
    }
    #[cfg(not(feature = "cluster"))]
    {
        let _ = (access, cmd, args);
    }
    Ok(())
}

#[cfg(feature = "cluster")]
fn command_key_indices(cmd: &str, argc: usize) -> Vec<usize> {
    match cmd {
        "DEL" | "EXISTS" => (0..argc).collect(),
        "HMGET" | "HDEL" | "HSET" | "HMSET" | "HINCRBY" | "HEXISTS" => {
            if argc > 0 {
                vec![0]
            } else {
                vec![]
            }
        }
        "LPUSH" | "RPUSH" | "LPOP" | "RPOP" | "LLEN" | "LRANGE" | "LINDEX" => {
            if argc > 0 {
                vec![0]
            } else {
                vec![]
            }
        }
        "SADD" | "SREM" | "SMEMBERS" | "SISMEMBER" | "SCARD" | "SSCAN" => {
            if argc > 0 {
                vec![0]
            } else {
                vec![]
            }
        }
        "JSON.MGET" => {
            if argc >= 2 {
                (0..argc - 1).collect()
            } else {
                vec![]
            }
        }
        "JSON.SET" | "JSON.GET" | "JSON.DEL" | "JSON.TYPE" | "JSON.STRLEN" | "JSON.ARRAPPEND"
        | "JSON.NUMINCRBY" | "JSON.OBJLEN" | "JSON.ARRLEN" => {
            if argc > 0 {
                vec![0]
            } else {
                vec![]
            }
        }
        "ZADD" | "ZREM" | "ZSCORE" | "ZRANK" | "ZRANGE" | "ZCARD" => {
            if argc > 0 {
                vec![0]
            } else {
                vec![]
            }
        }
        _ => {
            if argc > 0 {
                vec![0]
            } else {
                vec![]
            }
        }
    }
}

pub async fn redis_call_async(
    lua: &mlua::Lua,
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    access: &ScriptAccess,
    args: mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<mlua::Value> {
    let mut cmd_args: Vec<Bytes> = Vec::new();
    for arg in args {
        match arg {
            mlua::Value::String(s) => cmd_args.push(Bytes::from(s.as_bytes().to_vec())),
            mlua::Value::Integer(i) => cmd_args.push(Bytes::from(i.to_string())),
            mlua::Value::Number(n) => cmd_args.push(Bytes::from(n.to_string())),
            mlua::Value::Boolean(b) => cmd_args.push(Bytes::from(if b { "1" } else { "0" })),
            _ => {
                if throw_error {
                    return Err(mlua::Error::RuntimeError(
                        "Invalid argument type".to_string(),
                    ));
                }
                return Ok(mlua::Value::Nil);
            }
        }
    }

    if cmd_args.is_empty() {
        if throw_error {
            return Err(mlua::Error::RuntimeError(
                "No command specified".to_string(),
            ));
        }
        return Ok(mlua::Value::Nil);
    }

    let command = String::from_utf8_lossy(&cmd_args[0]).to_ascii_uppercase();
    let command_args = &cmd_args[1..];

    if let Err(e) = check_key_slots(access, &command, command_args) {
        if throw_error {
            return Err(mlua::Error::RuntimeError(e.to_string()));
        }
        return pcall_error_table(lua, &e.to_string());
    }

    async fn run(
        lua: &mlua::Lua,
        storage: &dyn KvStorage,
        txn: &mut ScriptTransaction,
        command: &str,
        command_args: &[Bytes],
        throw_error: bool,
    ) -> mlua::Result<mlua::Value> {
        let result = dispatch(storage, txn, command, command_args).await;
        match result {
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

    run(lua, storage, txn, &command, command_args, throw_error).await
}

async fn dispatch(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    cmd: &str,
    args: &[Bytes],
) -> Result<RespValue> {
    match cmd {
        "GET" => exec_get(storage, txn, args).await,
        "SET" => exec_set(storage, txn, args).await,
        "TTL" => exec_ttl(storage, txn, args).await,
        "DEL" => exec_del(storage, txn, args).await,
        "EXISTS" => exec_exists(storage, txn, args).await,
        "INCR" => exec_incr(storage, txn, args, 1).await,
        "DECR" => exec_incr(storage, txn, args, -1).await,
        "INCRBY" => exec_incrby(storage, txn, args).await,
        "DECRBY" => exec_decrby(storage, txn, args).await,
        "INCRBYFLOAT" => exec_incrbyfloat(storage, txn, args).await,
        "APPEND" => exec_append(storage, txn, args).await,
        "STRLEN" => exec_strlen(storage, txn, args).await,
        "HGET" => exec_hget(storage, txn, args).await,
        "HSET" | "HMSET" => exec_hset(storage, txn, args).await,
        "HSETNX" => exec_hsetnx(storage, txn, args).await,
        "HDEL" => exec_hdel(storage, txn, args).await,
        "HGETALL" => exec_hgetall(storage, txn, args).await,
        "HMGET" => exec_hmget(storage, txn, args).await,
        "HINCRBY" => exec_hincrby(storage, txn, args).await,
        "HEXISTS" => exec_hexists(storage, txn, args).await,
        "HLEN" => exec_hlen(storage, txn, args).await,
        "LPUSH" => exec_lpush(storage, txn, args, true).await,
        "RPUSH" => exec_lpush(storage, txn, args, false).await,
        "LPOP" => exec_lpop(storage, txn, args, true).await,
        "RPOP" => exec_lpop(storage, txn, args, false).await,
        "LLEN" => exec_llen(storage, txn, args).await,
        "LRANGE" => exec_lrange(storage, txn, args).await,
        "LINDEX" => exec_lindex(storage, txn, args).await,
        "SADD" => exec_sadd(storage, txn, args).await,
        "SREM" => exec_srem(storage, txn, args).await,
        "SMEMBERS" => exec_smembers(storage, txn, args).await,
        "SISMEMBER" => exec_sismember(storage, txn, args).await,
        "SCARD" => exec_scard(storage, txn, args).await,
        "SSCAN" => exec_sscan(storage, txn, args).await,
        "ZADD" => exec_zadd(storage, txn, args).await,
        "ZREM" => exec_zrem(storage, txn, args).await,
        "ZSCORE" => exec_zscore(storage, txn, args).await,
        "ZRANK" => exec_zrank(storage, txn, args).await,
        "ZRANGE" => exec_zrange(storage, txn, args).await,
        "ZREVRANGE" => exec_zrevrange(storage, txn, args).await,
        "ZRANGEBYSCORE" => exec_zrangebyscore(storage, txn, args).await,
        "ZREVRANGEBYSCORE" => exec_zrevrangebyscore(storage, txn, args).await,
        "ZCARD" => exec_zcard(storage, txn, args).await,
        "EXPIRE" => exec_expire(storage, txn, args).await,
        "JSON.GET" => json_exec::exec_json_get(storage, txn, args).await,
        "JSON.MGET" => json_exec::exec_json_mget(storage, txn, args).await,
        "JSON.SET" => json_exec::exec_json_set(storage, txn, args).await,
        "JSON.DEL" => json_exec::exec_json_del(storage, txn, args).await,
        "JSON.TYPE" => json_exec::exec_json_type(storage, txn, args).await,
        "JSON.STRLEN" => json_exec::exec_json_strlen(storage, txn, args).await,
        "JSON.ARRLEN" => json_exec::exec_json_arrlen(storage, txn, args).await,
        "JSON.OBJLEN" => json_exec::exec_json_objlen(storage, txn, args).await,
        "JSON.NUMINCRBY" => json_exec::exec_json_numincrby(storage, txn, args).await,
        "JSON.ARRAPPEND" => json_exec::exec_json_arrappend(storage, txn, args).await,
        _ => Err(Error::Command(format!(
            "ERR Command not supported in scripts: {cmd}"
        ))),
    }
}

async fn exec_ttl(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("TTL"));
    }
    let ttl = txn.ttl_ms(storage, &args[0]).await?;
    Ok(router::integer(match ttl {
        None => -2,
        Some(TTL_NO_EXPIRY) => -1,
        Some(ms) => ms / 1000,
    }))
}

async fn exec_expire(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("EXPIRE"));
    }
    let seconds = parse_i64_arg(&args[1], "seconds")?;
    if !txn.exists(storage, &args[0]).await? {
        return Ok(router::integer(0));
    }
    if seconds <= 0 {
        txn.delete(key(&args[0]));
    } else {
        txn.set_expire_at(key(&args[0]), expire_seconds_to_at(seconds));
    }
    Ok(router::integer(1))
}
