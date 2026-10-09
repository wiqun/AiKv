//! Lua `redis.call` Hash 域 `exec_*` (HGET/HSET/HDEL/HGETALL/HMGET/HINCRBY/HEXISTS/HLEN).

use std::collections::HashMap;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_i64_arg, ScriptTransaction};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::KvStorage;

use super::{key, wrong_count};

pub(super) async fn exec_hget(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("HGET"));
    }
    if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        let hash = stored.as_hash()?;
        Ok(match hash.get(args[1].as_ref()) {
            Some(v) => router::bulk(v.clone()),
            None => RespValue::Null,
        })
    } else {
        Ok(RespValue::Null)
    }
}

pub(super) async fn exec_hset(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Err(wrong_count("HSET"));
    }
    let mut hash = if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        stored.as_hash()?.clone()
    } else {
        HashMap::new()
    };
    let mut added = 0i64;
    for i in (1..args.len()).step_by(2) {
        if hash
            .insert(args[i].to_vec(), args[i + 1].to_vec())
            .is_none()
        {
            added += 1;
        }
    }
    txn.set_hash(key(&args[0]), hash);
    if args.len() > 3 {
        Ok(RespValue::SimpleString("OK".into()))
    } else {
        Ok(router::integer(added))
    }
}

pub(super) async fn exec_hsetnx(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("HSETNX"));
    }
    let mut hash = if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        stored.as_hash()?.clone()
    } else {
        HashMap::new()
    };
    if hash.contains_key(args[1].as_ref()) {
        return Ok(router::integer(0));
    }
    hash.insert(args[1].to_vec(), args[2].to_vec());
    txn.set_hash(key(&args[0]), hash);
    Ok(router::integer(1))
}

pub(super) async fn exec_hdel(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("HDEL"));
    }
    let Some(stored) = txn.get_value(storage, &args[0]).await? else {
        return Ok(router::integer(0));
    };
    let mut hash = stored.as_hash()?.clone();
    let mut count = 0i64;
    for f in &args[1..] {
        if hash.remove(f.as_ref()).is_some() {
            count += 1;
        }
    }
    if hash.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_hash(key(&args[0]), hash);
    }
    Ok(router::integer(count))
}

pub(super) async fn exec_hgetall(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("HGETALL"));
    }
    let Some(stored) = txn.get_value(storage, &args[0]).await? else {
        return Ok(RespValue::Array(Some(Vec::new())));
    };
    let hash = stored.as_hash()?;
    let mut out = Vec::new();
    for (f, v) in hash {
        out.push(router::bulk(f.clone()));
        out.push(router::bulk(v.clone()));
    }
    Ok(RespValue::Array(Some(out)))
}

pub(super) async fn exec_hmget(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("HMGET"));
    }
    let hash = if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        Some(stored.as_hash()?.clone())
    } else {
        None
    };
    let out: Vec<RespValue> = args[1..]
        .iter()
        .map(|f| {
            if let Some(h) = &hash {
                if let Some(v) = h.get(f.as_ref()) {
                    return router::bulk(v.clone());
                }
            }
            RespValue::Null
        })
        .collect();
    Ok(RespValue::Array(Some(out)))
}

pub(super) async fn exec_hincrby(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("HINCRBY"));
    }
    let inc = parse_i64_arg(&args[2], "increment")?;
    let mut hash = if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        stored.as_hash()?.clone()
    } else {
        HashMap::new()
    };
    let cur = hash
        .get(args[1].as_ref())
        .map(|v| parse_i64_arg(&Bytes::from(v.clone()), "field value"))
        .transpose()?
        .unwrap_or(0);
    let new_val = cur.saturating_add(inc);
    hash.insert(args[1].to_vec(), new_val.to_string().into_bytes());
    txn.set_hash(key(&args[0]), hash);
    Ok(router::integer(new_val))
}

pub(super) async fn exec_hexists(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("HEXISTS"));
    }
    let exists = if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        stored.as_hash()?.contains_key(args[1].as_ref())
    } else {
        false
    };
    Ok(router::integer(i64::from(exists)))
}

pub(super) async fn exec_hlen(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("HLEN"));
    }
    let len = if let Some(stored) = txn.get_value(storage, &args[0]).await? {
        stored.as_hash()?.len()
    } else {
        0
    };
    Ok(router::integer(len as i64))
}
