//! Lua `redis.call` Hash 域 `exec_*` (HGET/HSET/HDEL/HGETALL/HMGET/HINCRBY/HEXISTS/HLEN).
//! 读借用脚本里已经装入的哈希；写在同一份缓冲上原地改。

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_i64_arg, ScriptTransaction};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::KvStorage;

use super::wrong_count;

pub(super) async fn exec_hget(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("HGET"));
    }
    let found = txn
        .with_hash(storage, &args[0], |hash| {
            Ok(hash.get(args[1].as_ref()).cloned())
        })
        .await?;
    Ok(match found.flatten() {
        Some(value) => router::bulk(value),
        None => RespValue::Null,
    })
}

pub(super) async fn exec_hset(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Err(wrong_count("HSET"));
    }
    let added = txn
        .update_hash(storage, &args[0], true, false, |hash| {
            let mut added = 0i64;
            for i in (1..args.len()).step_by(2) {
                if hash
                    .insert(args[i].to_vec(), args[i + 1].to_vec())
                    .is_none()
                {
                    added += 1;
                }
            }
            Ok(added)
        })
        .await?;
    if args.len() > 3 {
        Ok(RespValue::SimpleString("OK".into()))
    } else {
        Ok(router::integer(added.unwrap_or(0)))
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
    let exists = txn
        .with_hash(storage, &args[0], |hash| {
            Ok(hash.contains_key(args[1].as_ref()))
        })
        .await?;
    if exists == Some(true) {
        return Ok(router::integer(0));
    }
    txn.update_hash(storage, &args[0], true, false, |hash| {
        hash.insert(args[1].to_vec(), args[2].to_vec());
        Ok(())
    })
    .await?;
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
    let count = txn
        .update_hash(storage, &args[0], false, true, |hash| {
            let mut count = 0i64;
            for field in &args[1..] {
                if hash.remove(field.as_ref()).is_some() {
                    count += 1;
                }
            }
            Ok(count)
        })
        .await?;
    Ok(router::integer(count.unwrap_or(0)))
}

pub(super) async fn exec_hgetall(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("HGETALL"));
    }
    let rows = txn
        .with_hash(storage, &args[0], |hash| {
            let mut out = Vec::with_capacity(hash.len() * 2);
            for (field, value) in hash {
                out.push(router::bulk(field.clone()));
                out.push(router::bulk(value.clone()));
            }
            Ok(out)
        })
        .await?;
    Ok(RespValue::Array(Some(rows.unwrap_or_default())))
}

pub(super) async fn exec_hmget(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("HMGET"));
    }
    let out = txn
        .with_hash(storage, &args[0], |hash| {
            Ok(args[1..]
                .iter()
                .map(|field| match hash.get(field.as_ref()) {
                    Some(value) => router::bulk(value.clone()),
                    None => RespValue::Null,
                })
                .collect::<Vec<_>>())
        })
        .await?;
    Ok(RespValue::Array(Some(match out {
        Some(rows) => rows,
        None => args[1..].iter().map(|_| RespValue::Null).collect(),
    })))
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
    let new_val = txn
        .update_hash(storage, &args[0], true, false, |hash| {
            let cur = hash
                .get(args[1].as_ref())
                .map(|value| parse_i64_arg(&Bytes::from(value.clone()), "field value"))
                .transpose()?
                .unwrap_or(0);
            let new_val = cur.saturating_add(inc);
            hash.insert(args[1].to_vec(), new_val.to_string().into_bytes());
            Ok(new_val)
        })
        .await?;
    Ok(router::integer(new_val.unwrap_or(0)))
}

pub(super) async fn exec_hexists(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("HEXISTS"));
    }
    let exists = txn
        .with_hash(storage, &args[0], |hash| {
            Ok(hash.contains_key(args[1].as_ref()))
        })
        .await?;
    Ok(router::integer(i64::from(exists == Some(true))))
}

pub(super) async fn exec_hlen(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("HLEN"));
    }
    let len = txn
        .with_hash(storage, &args[0], |hash| Ok(hash.len() as i64))
        .await?;
    Ok(router::integer(len.unwrap_or(0)))
}
