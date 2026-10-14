//! Lua `redis.call` String 域 `exec_*` (GET/SET/DEL/EXISTS/INCR*/DECR*/APPEND/STRLEN).

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_f64_arg, parse_i64_arg, ScriptTransaction};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::KvStorage;

use super::{key, wrong_count};

pub(super) async fn exec_get(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("GET"));
    }
    match txn.get(storage, &args[0]).await? {
        Some(v) => Ok(router::bulk(v)),
        None => Ok(RespValue::Null),
    }
}

use crate::command::string::{parse_set_options, SetCondition};

pub(super) async fn exec_set(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SET"));
    }
    let opts = parse_set_options(args)?;
    let old_value = if opts.return_old
        || matches!(opts.condition, SetCondition::Nx | SetCondition::Xx)
    {
        txn.get(storage, &args[0]).await?
    } else {
        None
    };

    if opts.condition == SetCondition::Nx && old_value.is_some() {
        return Ok(if opts.return_old {
            old_value.map(router::bulk).unwrap_or(router::nil_bulk())
        } else {
            router::nil_bulk()
        });
    }

    if opts.condition == SetCondition::Xx && old_value.is_none() {
        return Ok(router::nil_bulk());
    }

    txn.set_string(key(&args[0]), opts.value);
    if let Some(exp) = opts.expire_at {
        txn.set_expire_at(key(&args[0]), Some(exp));
    }

    if opts.return_old {
        match old_value {
            Some(v) => Ok(router::bulk(v)),
            None => Ok(router::nil_bulk()),
        }
    } else {
        Ok(RespValue::SimpleString("OK".into()))
    }
}

pub(super) async fn exec_del(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("DEL"));
    }
    let mut count = 0i64;
    for arg in args {
        if txn.exists(storage, arg).await? {
            txn.delete(key(arg));
            count += 1;
        }
    }
    Ok(router::integer(count))
}

pub(super) async fn exec_exists(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("EXISTS"));
    }
    let mut count = 0i64;
    for arg in args {
        if txn.exists(storage, arg).await? {
            count += 1;
        }
    }
    Ok(router::integer(count))
}

pub(super) async fn exec_incr(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    delta: i64,
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("INCR"));
    }
    let current = match txn.get(storage, &args[0]).await? {
        Some(v) => parse_i64_arg(&Bytes::from(v), "value")?,
        None => 0,
    };
    let new_val = current.saturating_add(delta);
    txn.set_string(key(&args[0]), new_val.to_string().into_bytes());
    Ok(router::integer(new_val))
}

pub(super) async fn exec_incrby(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("INCRBY"));
    }
    let inc = parse_i64_arg(&args[1], "increment")?;
    exec_incr(storage, txn, &[args[0].clone()], inc).await
}

pub(super) async fn exec_decrby(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("DECRBY"));
    }
    let dec = parse_i64_arg(&args[1], "decrement")?;
    exec_incr(storage, txn, &[args[0].clone()], -dec).await
}

pub(super) async fn exec_incrbyfloat(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("INCRBYFLOAT"));
    }
    let inc = parse_f64_arg(&args[1], "increment")?;
    let current = match txn.get(storage, &args[0]).await? {
        Some(v) => parse_f64_arg(&Bytes::from(v), "value")?,
        None => 0.0,
    };
    let new_val = current + inc;
    let s = new_val.to_string();
    txn.set_string(key(&args[0]), s.clone().into_bytes());
    Ok(router::bulk(s.into_bytes()))
}

pub(super) async fn exec_append(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("APPEND"));
    }
    let mut cur = txn.get(storage, &args[0]).await?.unwrap_or_default();
    cur.extend_from_slice(&args[1]);
    let len = cur.len() as i64;
    txn.set_string(key(&args[0]), cur);
    Ok(router::integer(len))
}

pub(super) async fn exec_strlen(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("STRLEN"));
    }
    let len = txn
        .get(storage, &args[0])
        .await?
        .map(|v| v.len())
        .unwrap_or(0) as i64;
    Ok(router::integer(len))
}
