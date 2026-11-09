//! Lua 极速直通路径 (ADR-008).
//! 对 GET/SET/ZCARD/HMGET/ZREM 等高频命令进行参数零分配直通.

use crate::command::script::call_stats::{CallTimer, ScriptCallOp};
use crate::command::script::convert::pcall_error_table;
use crate::command::script::execute::check_key;
use crate::command::script::execute::fast_path_hash::{
    fast_hdel, fast_hget, fast_hmget, fast_hset,
};
use crate::command::script::execute::DeclaredKeys;
use crate::command::script::transaction::ScriptTransaction;
use crate::storage::ScriptIo;

pub(super) fn with_arg_bytes<R>(val: &mlua::Value, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    match val {
        mlua::Value::String(s) => Some(f(s.as_bytes().as_ref())),
        mlua::Value::Integer(i) => {
            let mut buf = [0u8; 32];
            let mut cur = std::io::Cursor::new(&mut buf[..]);
            use std::io::Write;
            let _ = write!(cur, "{i}");
            let len = cur.position() as usize;
            Some(f(&buf[..len]))
        }
        mlua::Value::Number(n) => {
            let mut buf = [0u8; 32];
            let mut cur = std::io::Cursor::new(&mut buf[..]);
            use std::io::Write;
            let _ = write!(cur, "{n}");
            let len = cur.position() as usize;
            Some(f(&buf[..len]))
        }
        mlua::Value::Boolean(b) => Some(f(if *b { b"1" } else { b"0" })),
        _ => None,
    }
}

pub fn try_fast_path(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
    read_only: bool,
) -> mlua::Result<Option<mlua::Value>> {
    let Some(first_arg) = args.front() else {
        return Ok(None);
    };
    let mlua::Value::String(cmd_str) = first_arg else {
        return Ok(None);
    };
    let cmd_bytes = cmd_str.as_bytes();
    let cmd = cmd_bytes.as_ref();

    if cmd.eq_ignore_ascii_case(b"GET") {
        fast_get(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"SET") {
        if read_only {
            return write_rejected(lua, throw_error);
        }
        fast_set(lua, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZCARD") {
        fast_zcard(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HMGET") {
        fast_hmget(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HGET") {
        fast_hget(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HSET") {
        if read_only {
            return write_rejected(lua, throw_error);
        }
        fast_hset(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HDEL") {
        if read_only {
            return write_rejected(lua, throw_error);
        }
        fast_hdel(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZSCORE") {
        fast_zscore(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZREM") {
        if read_only {
            return write_rejected(lua, throw_error);
        }
        fast_zrem(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZRANGEBYSCORE") {
        fast_zrangebyscore(lua, storage, txn, declared, args, throw_error)
    } else {
        Ok(None)
    }
}

fn write_rejected(lua: &mlua::Lua, throw_error: bool) -> mlua::Result<Option<mlua::Value>> {
    let msg = "ERR Write commands not allowed in read-only script";
    if throw_error {
        Err(mlua::Error::RuntimeError(msg.to_string()))
    } else {
        pcall_error_table(lua, msg).map(Some)
    }
}

fn fast_get(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() != 2 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let get_res = with_arg_bytes(second_arg, |k| {
        if let Err(e) = check_key(declared, k) {
            let msg = e.to_string();
            if throw_error {
                return Err(mlua::Error::RuntimeError(msg));
            }
            return pcall_error_table(lua, &msg).map(Some);
        }
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::String);
        match txn.get(storage, k) {
            Ok(Some(v)) => Ok(Some(mlua::Value::String(lua.create_string(&v)?))),
            Ok(None) => {
                let resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
                if resp3 {
                    Ok(Some(mlua::Value::Nil))
                } else {
                    Ok(Some(mlua::Value::Boolean(false)))
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if throw_error {
                    Err(mlua::Error::RuntimeError(msg))
                } else {
                    pcall_error_table(lua, &msg).map(Some)
                }
            }
        }
    });
    match get_res {
        Some(res) => res,
        None => Ok(None),
    }
}

fn fast_set(
    lua: &mlua::Lua,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() != 3 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let Some(third_arg) = args.get(2) else {
        return Ok(None);
    };
    let set_res = with_arg_bytes(second_arg, |k| {
        if let Err(e) = check_key(declared, k) {
            let msg = e.to_string();
            if throw_error {
                return Err(mlua::Error::RuntimeError(msg));
            }
            return pcall_error_table(lua, &msg).map(Some);
        }
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::String);
        let inner_res = with_arg_bytes(third_arg, |v| {
            txn.set_string(k.to_vec(), v.to_vec());
            Ok(Some(mlua::Value::String(lua.create_string(b"OK")?)))
        });
        match inner_res {
            Some(res) => res,
            None => Ok(None),
        }
    });
    match set_res {
        Some(res) => res,
        None => Ok(None),
    }
}

fn fast_zcard(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() != 2 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let zcard_res = with_arg_bytes(second_arg, |k| {
        if let Err(e) = check_key(declared, k) {
            let msg = e.to_string();
            if throw_error {
                return Err(mlua::Error::RuntimeError(msg));
            }
            return pcall_error_table(lua, &msg).map(Some);
        }
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::Zother);
        if let Some(count) = txn.zset_delta_count(k) {
            return Ok(Some(mlua::Value::Integer(count as i64)));
        }
        let stored = match txn.get_value(storage, k) {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                if throw_error {
                    return Err(mlua::Error::RuntimeError(msg));
                }
                return pcall_error_table(lua, &msg).map(Some);
            }
        };
        if let Some(count) = stored
            .as_ref()
            .and_then(crate::storage::zset_subkey::is_zset_header)
        {
            return Ok(Some(mlua::Value::Integer(count as i64)));
        }
        let len = txn
            .with_zset(storage, k, |zset| Ok(zset.len() as i64))
            .unwrap_or(Some(0))
            .unwrap_or(0);
        Ok(Some(mlua::Value::Integer(len)))
    });
    match zcard_res {
        Some(res) => res,
        None => Ok(None),
    }
}

fn do_inline_zrem(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    k: &[u8],
    m: &[u8],
) -> crate::error::Result<i64> {
    let mut count = 0i64;
    let should_delete = match txn.ensure_zset_mut(storage, k)? {
        Some(zset) => {
            if zset.remove(m).is_some() {
                count += 1;
            }
            zset.is_empty()
        }
        None => false,
    };
    if should_delete {
        txn.delete(k.to_vec());
    }
    Ok(count)
}

fn fast_zrem(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() != 3 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let Some(third_arg) = args.get(2) else {
        return Ok(None);
    };
    let zrem_res = with_arg_bytes(second_arg, |k| {
        if let Err(e) = check_key(declared, k) {
            let msg = e.to_string();
            if throw_error {
                return Err(mlua::Error::RuntimeError(msg));
            }
            return pcall_error_table(lua, &msg).map(Some);
        }
        with_arg_bytes(third_arg, |m| {
            let mut prep = CallTimer::start(ScriptCallOp::Prep);
            prep.finish();
            let _timed = CallTimer::start(ScriptCallOp::Zother);
            let is_subkey = match txn.uses_subkey_zset(storage, k) {
                Ok(b) => b,
                Err(e) => {
                    let msg = e.to_string();
                    if throw_error {
                        return Err(mlua::Error::RuntimeError(msg));
                    }
                    return pcall_error_table(lua, &msg).map(Some);
                }
            };
            let count_res = if is_subkey {
                txn.subkey_zrem_single(storage, k, m)
            } else {
                do_inline_zrem(storage, txn, k, m)
            };
            match count_res {
                Ok(count) => Ok(Some(mlua::Value::Integer(count))),
                Err(e) => {
                    let msg = e.to_string();
                    if throw_error {
                        Err(mlua::Error::RuntimeError(msg))
                    } else {
                        pcall_error_table(lua, &msg).map(Some)
                    }
                }
            }
        })
        .unwrap_or(Ok(None))
    });
    match zrem_res {
        Some(res) => res,
        None => Ok(None),
    }
}

struct ScoreLimit {
    key: Vec<u8>,
    min: String,
    max: String,
    limit: Option<(usize, Option<usize>)>,
}

fn lua_text(val: &mlua::Value) -> Option<String> {
    with_arg_bytes(val, |raw| String::from_utf8(raw.to_vec()).ok()).flatten()
}

fn lua_i64(val: &mlua::Value) -> Option<i64> {
    lua_text(val)?.parse().ok()
}

/// 只接 `key min max` 和 `key min max LIMIT offset count`. 其他形态交回普通路径.
fn parse_score_limit(args: &mlua::MultiValue) -> Option<ScoreLimit> {
    let n = args.len();
    if n != 4 && n != 7 {
        return None;
    }
    let key = with_arg_bytes(args.get(1)?, |raw| raw.to_vec())?;
    let min = lua_text(args.get(2)?)?;
    let max = lua_text(args.get(3)?)?;
    let limit = if n == 4 {
        None
    } else {
        let flag = lua_text(args.get(4)?)?;
        if !flag.eq_ignore_ascii_case("LIMIT") {
            return None;
        }
        let offset = lua_i64(args.get(5)?)?;
        let count = lua_i64(args.get(6)?)?;
        if offset < 0 {
            return None;
        }
        let count = (count >= 0).then_some(count as usize);
        Some((offset as usize, count))
    };
    Some(ScoreLimit {
        key,
        min,
        max,
        limit,
    })
}

pub(super) fn script_fail(
    lua: &mlua::Lua,
    throw_error: bool,
    msg: String,
) -> mlua::Result<Option<mlua::Value>> {
    if throw_error {
        Err(mlua::Error::RuntimeError(msg))
    } else {
        pcall_error_table(lua, &msg).map(Some)
    }
}

fn fast_zrangebyscore(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    let Some(parsed) = parse_score_limit(args) else {
        return Ok(None);
    };
    if let Err(e) = check_key(declared, &parsed.key) {
        return script_fail(lua, throw_error, e.to_string());
    }
    let Ok(min) = crate::command::zset::helpers::parse_score_bound(&parsed.min, true) else {
        return Ok(None);
    };
    let Ok(max) = crate::command::zset::helpers::parse_score_bound(&parsed.max, false) else {
        return Ok(None);
    };
    let (offset, count) = parsed.limit.unwrap_or((0, None));
    let window = crate::command::zset::range_scan::ScoreWindow {
        min,
        max,
        reverse: false,
        offset,
        count,
    };
    let rows = match super::zset_scan::by_score(storage, txn, &parsed.key, &window) {
        Ok(Some(rows)) => rows,
        Ok(None) => return Ok(None),
        Err(e) => return script_fail(lua, throw_error, e.to_string()),
    };
    crate::command::script::convert::member_list_to_lua(lua, rows).map(Some)
}

fn fast_zscore(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() != 3 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let Some(third_arg) = args.get(2) else {
        return Ok(None);
    };
    let res = with_arg_bytes(second_arg, |k| {
        if let Err(e) = check_key(declared, k) {
            let msg = e.to_string();
            if throw_error {
                return Err(mlua::Error::RuntimeError(msg));
            }
            return pcall_error_table(lua, &msg).map(Some);
        }
        with_arg_bytes(third_arg, |m| {
            let mut prep = CallTimer::start(ScriptCallOp::Prep);
            prep.finish();
            let _timed = CallTimer::start(ScriptCallOp::Zother);
            let is_resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
            let score_opt = if match txn.uses_subkey_zset(storage, k) {
                Ok(b) => b,
                Err(e) => {
                    let msg = e.to_string();
                    if throw_error {
                        return Err(mlua::Error::RuntimeError(msg));
                    }
                    return pcall_error_table(lua, &msg).map(Some);
                }
            } {
                match txn.point_member_score(storage, k, m) {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        if throw_error {
                            return Err(mlua::Error::RuntimeError(msg));
                        }
                        return pcall_error_table(lua, &msg).map(Some);
                    }
                }
            } else {
                txn.with_zset(storage, k, |zset| Ok(zset.get(m).copied()))
                    .unwrap_or(None)
                    .flatten()
            };
            match score_opt {
                Some(score) => {
                    let s = format!("{score}");
                    Ok(Some(mlua::Value::String(lua.create_string(&s)?)))
                }
                None => {
                    if is_resp3 {
                        Ok(Some(mlua::Value::Nil))
                    } else {
                        Ok(Some(mlua::Value::Boolean(false)))
                    }
                }
            }
        })
        .unwrap_or(Ok(None))
    });
    match res {
        Some(res) => res,
        None => Ok(None),
    }
}
