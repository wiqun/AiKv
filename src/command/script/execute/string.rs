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

pub(super) async fn exec_set(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SET"));
    }
    let opts = crate::command::string::parse_set_options(args)?;
    let old = if opts.return_old {
        txn.get(storage, &opts.key).await?
    } else {
        None
    };
    let exists = if opts.return_old {
        old.is_some()
    } else {
        txn.exists(storage, &opts.key).await?
    };
    let blocked = match opts.condition {
        crate::command::string::SetCondition::Nx if exists => true,
        crate::command::string::SetCondition::Xx if !exists => true,
        _ => false,
    };
    if blocked {
        return Ok(if opts.return_old && exists {
            match old {
                Some(v) => router::bulk(v),
                None => RespValue::Null,
            }
        } else {
            RespValue::Null
        });
    }
    txn.set_string(opts.key.clone(), opts.value);
    if !opts.keep_ttl {
        txn.set_expire_at(opts.key, opts.expire_at);
    }
    if opts.return_old {
        return Ok(match old {
            Some(v) => router::bulk(v),
            None => RespValue::Null,
        });
    }
    Ok(RespValue::SimpleString("OK".into()))
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
