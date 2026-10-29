//! Lua 极速直通路径 (ADR-008).
//! 对 GET/SET/ZCARD/HMGET/ZREM 等高频命令进行参数零分配直通.

use crate::command::script::call_stats::{CallTimer, ScriptCallOp};
use crate::command::script::convert::pcall_error_table;
use crate::command::script::execute::check_key;
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
        fast_set(lua, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZCARD") {
        fast_zcard(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HMGET") {
        fast_hmget(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HGET") {
        fast_hget(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HSET") {
        fast_hset(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"HDEL") {
        fast_hdel(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZSCORE") {
        fast_zscore(lua, storage, txn, declared, args, throw_error)
    } else if cmd.eq_ignore_ascii_case(b"ZREM") {
        fast_zrem(lua, storage, txn, declared, args, throw_error)
    } else {
        Ok(None)
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

fn fast_hmget(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() < 3 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let hmget_res = with_arg_bytes(second_arg, |k| {
        if let Err(e) = check_key(declared, k) {
            let msg = e.to_string();
            if throw_error {
                return Err(mlua::Error::RuntimeError(msg));
            }
            return pcall_error_table(lua, &msg).map(Some);
        }
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::Hash);
        let field_count = args.len() - 2;
        let is_resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));

        let query_res = txn.with_hash(storage, k, |hash| {
            let mut vals = Vec::with_capacity(field_count);
            for i in 0..field_count {
                let field_val = match args.get(i + 2) {
                    Some(arg) => with_arg_bytes(arg, |f| hash.get(f).cloned()).flatten(),
                    None => None,
                };
                vals.push(field_val);
            }
            Ok(vals)
        });
        match query_res {
            Ok(vals) => build_hmget_table(lua, vals, field_count, is_resp3).map(Some),
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
    match hmget_res {
        Some(res) => res,
        None => Ok(None),
    }
}

fn build_hmget_table(
    lua: &mlua::Lua,
    vals: Option<Vec<Option<Vec<u8>>>>,
    field_count: usize,
    is_resp3: bool,
) -> mlua::Result<mlua::Value> {
    let table = lua.create_table_with_capacity(field_count, 0)?;
    match vals {
        Some(vals) => {
            for (i, val) in vals.into_iter().enumerate() {
                match val {
                    Some(v) => table.raw_set(i + 1, mlua::Value::String(lua.create_string(&v)?))?,
                    None => {
                        let nil_or_false = if is_resp3 { mlua::Value::Nil } else { mlua::Value::Boolean(false) };
                        table.raw_set(i + 1, nil_or_false)?;
                    }
                }
            }
        }
        None => {
            for i in 0..field_count {
                let nil_or_false = if is_resp3 { mlua::Value::Nil } else { mlua::Value::Boolean(false) };
                table.raw_set(i + 1, nil_or_false)?;
            }
        }
    }
    Ok(mlua::Value::Table(table))
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

fn fast_hget(
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
        with_arg_bytes(third_arg, |f| {
            let mut prep = CallTimer::start(ScriptCallOp::Prep);
            prep.finish();
            let _timed = CallTimer::start(ScriptCallOp::Hash);
            let is_resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
            let val_opt = match txn.with_hash(storage, k, |h| Ok(h.get(f).cloned())) {
                Ok(opt) => opt.flatten(),
                Err(e) => {
                    let msg = e.to_string();
                    if throw_error {
                        return Err(mlua::Error::RuntimeError(msg));
                    }
                    return pcall_error_table(lua, &msg).map(Some);
                }
            };
            match val_opt {
                Some(v) => Ok(Some(mlua::Value::String(lua.create_string(&v)?))),
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

fn fast_hset(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() != 4 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
        return Ok(None);
    };
    let Some(third_arg) = args.get(2) else {
        return Ok(None);
    };
    let Some(fourth_arg) = args.get(3) else {
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
        with_arg_bytes(third_arg, |f| {
            with_arg_bytes(fourth_arg, |v| {
                let mut prep = CallTimer::start(ScriptCallOp::Prep);
                prep.finish();
                let _timed = CallTimer::start(ScriptCallOp::Hash);
                let added = match txn.ensure_hash_mut(storage, k) {
                    Ok(Some(h)) => h.insert(f.to_vec(), v.to_vec()).is_none() as i64,
                    Ok(None) => {
                        let mut map = std::collections::HashMap::new();
                        map.insert(f.to_vec(), v.to_vec());
                        txn.set_hash(k.to_vec(), map);
                        1i64
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if throw_error {
                            return Err(mlua::Error::RuntimeError(msg));
                        }
                        return pcall_error_table(lua, &msg).map(Some);
                    }
                };
                Ok(Some(mlua::Value::Integer(added)))
            })
            .unwrap_or(Ok(None))
        })
        .unwrap_or(Ok(None))
    });
    match res {
        Some(res) => res,
        None => Ok(None),
    }
}

fn fast_hdel(
    lua: &mlua::Lua,
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    declared: &DeclaredKeys,
    args: &mlua::MultiValue,
    throw_error: bool,
) -> mlua::Result<Option<mlua::Value>> {
    if args.len() < 3 {
        return Ok(None);
    }
    let Some(second_arg) = args.get(1) else {
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
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::Hash);
        let field_count = args.len() - 2;
        let mut count = 0i64;
        let delete_res = match txn.ensure_hash_mut(storage, k) {
            Ok(Some(h)) => {
                for i in 0..field_count {
                    if let Some(arg) = args.get(i + 2) {
                        with_arg_bytes(arg, |f| {
                            if h.remove(f).is_some() {
                                count += 1;
                            }
                        });
                    }
                }
                let should_delete = h.is_empty();
                Ok(should_delete)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(e),
        };
        match delete_res {
            Ok(should_delete) => {
                if should_delete {
                    txn.delete(k.to_vec());
                }
                Ok(Some(mlua::Value::Integer(count)))
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
    match res {
        Some(res) => res,
        None => Ok(None),
    }
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
