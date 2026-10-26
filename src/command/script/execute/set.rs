//! Lua `redis.call` Set 域 `exec_*` (SADD/SREM/SMEMBERS/SISMEMBER/SCARD/SSCAN).

use std::collections::HashSet;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::ScriptTransaction;
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::ScriptIo;

use super::{key, wrong_count};

pub(super) fn exec_sadd(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SADD"));
    }
    let mut set = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored.as_set()?.clone()
    } else {
        HashSet::new()
    };
    let mut added = 0i64;
    for m in &args[1..] {
        if set.insert(m.to_vec()) {
            added += 1;
        }
    }
    txn.set_set(key(&args[0]), set);
    Ok(router::integer(added))
}

pub(super) fn exec_srem(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SREM"));
    }
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(router::integer(0));
    };
    let mut set = stored.as_set()?.clone();
    let mut count = 0i64;
    for m in &args[1..] {
        if set.remove(&m.to_vec()) {
            count += 1;
        }
    }
    if set.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_set(key(&args[0]), set);
    }
    Ok(router::integer(count))
}

pub(super) fn exec_smembers(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("SMEMBERS"));
    }
    let out = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored
            .as_set()?
            .iter()
            .map(|m| router::bulk(m.clone()))
            .collect()
    } else {
        Vec::new()
    };
    Ok(RespValue::Array(Some(out)))
}

pub(super) fn exec_smismember(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SMISMEMBER"));
    }
    let mut items = Vec::with_capacity(args.len() - 1);
    for member in &args[1..] {
        let pair = [args[0].clone(), member.clone()];
        items.push(exec_sismember(storage, txn, &pair)?);
    }
    Ok(RespValue::Array(Some(items)))
}

pub(super) fn exec_sismember(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("SISMEMBER"));
    }
    let member = args[1].to_vec();
    let ok = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored.as_set()?.contains(&member)
    } else {
        false
    };
    Ok(router::integer(i64::from(ok)))
}

pub(super) fn exec_scard(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("SCARD"));
    }
    let n = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored.as_set()?.len()
    } else {
        0
    };
    Ok(router::integer(n as i64))
}

pub(super) fn exec_sscan(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SSCAN"));
    }
    let cursor: u64 = std::str::from_utf8(&args[1])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let members = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored
            .as_set()?
            .iter()
            .map(|m| router::bulk(m.clone()))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let (new_cursor, items) = if cursor == 0 {
        (1, members)
    } else {
        (0, Vec::new())
    };
    Ok(RespValue::Array(Some(vec![
        router::bulk(new_cursor.to_string().into_bytes()),
        RespValue::Array(Some(items)),
    ])))
}
