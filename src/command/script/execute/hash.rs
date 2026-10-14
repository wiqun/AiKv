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
    let res = txn
        .with_hash(storage, &args[0], |hash| {
            Ok(match hash.get(args[1].as_ref()) {
                Some(v) => router::bulk(v.clone()),
                None => RespValue::Null,
            })
        })
        .await?;
    Ok(res.unwrap_or(RespValue::Null))
}

pub(super) async fn exec_hset(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Err(wrong_count("HSET"));
    }
    let k = key(&args[0]);
    let mut added = 0i64;
    let hash = match txn.ensure_hash_mut(storage, &k).await? {
        Some(h) => h,
        None => {
            txn.set_hash(k.clone(), HashMap::new());
            txn.ensure_hash_mut(storage, &k).await?.unwrap()
        }
    };
    for i in (1..args.len()).step_by(2) {
        if hash
            .insert(args[i].to_vec(), args[i + 1].to_vec())
            .is_none()
        {
            added += 1;
        }
    }
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
    let k = key(&args[0]);
    let hash = match txn.ensure_hash_mut(storage, &k).await? {
        Some(h) => h,
        None => {
            txn.set_hash(k.clone(), HashMap::new());
            txn.ensure_hash_mut(storage, &k).await?.unwrap()
        }
    };
    if hash.contains_key(args[1].as_ref()) {
        return Ok(router::integer(0));
    }
    hash.insert(args[1].to_vec(), args[2].to_vec());
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
    let k = key(&args[0]);
    let mut count = 0i64;
    let should_delete = if let Some(hash) = txn.ensure_hash_mut(storage, &k).await? {
        for f in &args[1..] {
            if hash.remove(f.as_ref()).is_some() {
                count += 1;
            }
        }
        hash.is_empty()
    } else {
        false
    };
    if should_delete {
        txn.delete(k);
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
    let res = txn
        .with_hash(storage, &args[0], |hash| {
            let mut out = Vec::with_capacity(hash.len() * 2);
            for (f, v) in hash {
                out.push(router::bulk(f.clone()));
                out.push(router::bulk(v.clone()));
            }
            Ok(RespValue::Array(Some(out)))
        })
        .await?;
    Ok(res.unwrap_or_else(|| RespValue::Array(Some(Vec::new()))))
}

pub(super) async fn exec_hmget(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("HMGET"));
    }
    let fields = &args[1..];
    let res = txn
        .with_hash(storage, &args[0], |hash| {
            let mut out = Vec::with_capacity(fields.len());
            for f in fields {
                out.push(match hash.get(f.as_ref()) {
                    Some(v) => router::bulk(v.clone()),
                    None => RespValue::Null,
                });
            }
            Ok(RespValue::Array(Some(out)))
        })
        .await?;
    Ok(res.unwrap_or_else(|| {
        RespValue::Array(Some(vec![RespValue::Null; fields.len()]))
    }))
}

pub(super) async fn exec_hincrby(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("HINCRBY"));
    }
    let k = key(&args[0]);
    let inc = parse_i64_arg(&args[2], "increment")?;
    let hash = match txn.ensure_hash_mut(storage, &k).await? {
        Some(h) => h,
        None => {
            txn.set_hash(k.clone(), HashMap::new());
            txn.ensure_hash_mut(storage, &k).await?.unwrap()
        }
    };
    let cur = hash
        .get(args[1].as_ref())
        .map(|v| parse_i64_arg(&Bytes::from(v.clone()), "field value"))
        .transpose()?
        .unwrap_or(0);
    let new_val = cur.saturating_add(inc);
    hash.insert(args[1].to_vec(), new_val.to_string().into_bytes());
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
    let res = txn
        .with_hash(storage, &args[0], |hash| {
            Ok(hash.contains_key(args[1].as_ref()))
        })
        .await?;
    let exists = res.unwrap_or(false);
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
    let res = txn
        .with_hash(storage, &args[0], |hash| Ok(hash.len() as i64))
        .await?;
    let len = res.unwrap_or(0);
    Ok(router::integer(len))
}
