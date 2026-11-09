use super::fast_path::{script_fail, with_arg_bytes};
use crate::command::script::call_stats::{CallTimer, ScriptCallOp};
use crate::command::script::execute::{check_key, DeclaredKeys};
use crate::command::script::transaction::ScriptTransaction;
use crate::storage::ScriptIo;

pub(super) fn fast_hget(
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
            return script_fail(lua, throw_error, msg);
        }
        with_arg_bytes(third_arg, |f| {
            let mut prep = CallTimer::start(ScriptCallOp::Prep);
            prep.finish();
            let _timed = CallTimer::start(ScriptCallOp::Hash);
            let is_resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
            let is_subkey = match txn.uses_subkey_hash(storage, k) {
                Ok(b) => b,
                Err(e) => return script_fail(lua, throw_error, e.to_string()),
            };
            let val_opt = if is_subkey {
                match txn.point_hash_field(storage, k, f) {
                    Ok(v) => v,
                    Err(e) => return script_fail(lua, throw_error, e.to_string()),
                }
            } else {
                match txn.with_hash(storage, k, |h| Ok(h.get(f).cloned())) {
                    Ok(opt) => opt.flatten(),
                    Err(e) => return script_fail(lua, throw_error, e.to_string()),
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

pub(super) fn fast_hset(
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
            return script_fail(lua, throw_error, msg);
        }
        with_arg_bytes(third_arg, |f| {
            with_arg_bytes(fourth_arg, |v| {
                let mut prep = CallTimer::start(ScriptCallOp::Prep);
                prep.finish();
                let _timed = CallTimer::start(ScriptCallOp::Hash);
                let is_subkey = match txn.uses_subkey_hash(storage, k) {
                    Ok(b) => b,
                    Err(e) => return script_fail(lua, throw_error, e.to_string()),
                };
                let added = if is_subkey {
                    match txn.subkey_hset_single(storage, k, f, v) {
                        Ok(a) => a,
                        Err(e) => return script_fail(lua, throw_error, e.to_string()),
                    }
                } else {
                    match txn.ensure_hash_mut(storage, k) {
                        Ok(Some(h)) => h.insert(f.to_vec(), v.to_vec()).is_none() as i64,
                        Ok(None) => {
                            let mut map = std::collections::HashMap::new();
                            map.insert(f.to_vec(), v.to_vec());
                            txn.set_hash(k.to_vec(), map);
                            1i64
                        }
                        Err(e) => return script_fail(lua, throw_error, e.to_string()),
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

pub(super) fn fast_hdel(
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
            return script_fail(lua, throw_error, msg);
        }
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::Hash);
        let field_count = args.len() - 2;
        let mut count = 0i64;
        let is_subkey = match txn.uses_subkey_hash(storage, k) {
            Ok(b) => b,
            Err(e) => return script_fail(lua, throw_error, e.to_string()),
        };
        if is_subkey {
            for i in 0..field_count {
                let Some(arg) = args.get(i + 2) else { continue };
                let Some(res) = with_arg_bytes(arg, |f| txn.subkey_hdel_single(storage, k, f))
                else {
                    return script_fail(lua, throw_error, "ERR invalid field argument".into());
                };
                match res {
                    Ok(c) => count += c,
                    Err(e) => return script_fail(lua, throw_error, e.to_string()),
                }
            }
            return Ok(Some(mlua::Value::Integer(count)));
        }
        let delete_res = match txn.ensure_hash_mut(storage, k) {
            Ok(Some(h)) => {
                for i in 0..field_count {
                    let Some(arg) = args.get(i + 2) else { continue };
                    let Some(_) = with_arg_bytes(arg, |f| {
                        if h.remove(f).is_some() {
                            count += 1;
                        }
                    }) else {
                        return script_fail(lua, throw_error, "ERR invalid field argument".into());
                    };
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
            Err(e) => script_fail(lua, throw_error, e.to_string()),
        }
    });
    match res {
        Some(res) => res,
        None => Ok(None),
    }
}

pub(super) fn fast_hmget(
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
            return script_fail(lua, throw_error, msg);
        }
        let mut prep = CallTimer::start(ScriptCallOp::Prep);
        prep.finish();
        let _timed = CallTimer::start(ScriptCallOp::Hash);
        let field_count = args.len() - 2;
        let is_resp3 = matches!(lua.named_registry_value::<i64>("aikv_script_resp"), Ok(3));
        let is_subkey = match txn.uses_subkey_hash(storage, k) {
            Ok(b) => b,
            Err(e) => return script_fail(lua, throw_error, e.to_string()),
        };

        let query_res = if is_subkey {
            let mut vals = Vec::with_capacity(field_count);
            let mut err = None;
            for i in 0..field_count {
                if let Some(arg) = args.get(i + 2) {
                    let v = with_arg_bytes(arg, |f| txn.point_hash_field(storage, k, f));
                    match v {
                        Some(Ok(val)) => vals.push(val),
                        Some(Err(e)) => {
                            err = Some(e);
                            break;
                        }
                        None => vals.push(None),
                    }
                } else {
                    vals.push(None);
                }
            }
            match err {
                Some(e) => Err(e),
                None => Ok(vals),
            }
        } else {
            txn.with_hash(storage, k, |hash| {
                let mut vals = Vec::with_capacity(field_count);
                for i in 0..field_count {
                    let field_val = match args.get(i + 2) {
                        Some(arg) => with_arg_bytes(arg, |f| hash.get(f).cloned()).flatten(),
                        None => None,
                    };
                    vals.push(field_val);
                }
                Ok(vals)
            })
            .map(|opt| opt.unwrap_or_else(|| vec![None; field_count]))
        };
        match query_res {
            Ok(vals) => build_hmget_table(lua, Some(vals), field_count, is_resp3).map(Some),
            Err(e) => script_fail(lua, throw_error, e.to_string()),
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
                        let nil_or_false = if is_resp3 {
                            mlua::Value::Nil
                        } else {
                            mlua::Value::Boolean(false)
                        };
                        table.raw_set(i + 1, nil_or_false)?;
                    }
                }
            }
        }
        None => {
            for i in 0..field_count {
                let nil_or_false = if is_resp3 {
                    mlua::Value::Nil
                } else {
                    mlua::Value::Boolean(false)
                };
                table.raw_set(i + 1, nil_or_false)?;
            }
        }
    }
    Ok(mlua::Value::Table(table))
}
