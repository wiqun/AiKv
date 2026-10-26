//! Lua `redis.call` List 域 `exec_*` (LPUSH/RPUSH/LPOP/RPOP/LLEN/LRANGE/LINDEX).

use std::collections::VecDeque;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_i64_arg, ScriptTransaction};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::ScriptIo;

use super::{key, wrong_count};

pub(super) fn exec_lpush(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    front: bool,
    create: bool,
) -> Result<RespValue> {
    let name = match (front, create) {
        (true, true) => "LPUSH",
        (false, true) => "RPUSH",
        (true, false) => "LPUSHX",
        (false, false) => "RPUSHX",
    };
    if args.len() < 2 {
        return Err(wrong_count(name));
    }
    let mut list = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored.as_list()?.clone()
    } else if create {
        VecDeque::new()
    } else {
        return Ok(router::integer(0));
    };
    if front {
        for i in (1..args.len()).rev() {
            list.push_front(args[i].to_vec());
        }
    } else {
        for arg in &args[1..] {
            list.push_back(arg.to_vec());
        }
    }
    let len = list.len() as i64;
    txn.set_list(key(&args[0]), list);
    Ok(router::integer(len))
}

pub(super) fn exec_lpop(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    from_front: bool,
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("LPOP"));
    }
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(RespValue::Null);
    };
    let mut list = stored.as_list()?.clone();
    let val = if from_front {
        list.pop_front()
    } else {
        list.pop_back()
    };
    let Some(v) = val else {
        return Ok(RespValue::Null);
    };
    if list.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_list(key(&args[0]), list);
    }
    Ok(router::bulk(v))
}

pub(super) fn exec_llen(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("LLEN"));
    }
    let len = if let Some(stored) = txn.get_value(storage, &args[0])? {
        stored.as_list()?.len()
    } else {
        0
    };
    Ok(router::integer(len as i64))
}

pub(super) fn exec_lrange(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("LRANGE"));
    }
    let start = parse_i64_arg(&args[1], "start")?;
    let stop = parse_i64_arg(&args[2], "stop")?;
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(RespValue::Array(Some(Vec::new())));
    };
    let list = stored.as_list()?;
    let len = list.len() as i64;
    let start_idx = (if start < 0 { len + start } else { start }).clamp(0, len) as usize;
    let stop_idx = (if stop < 0 { len + stop + 1 } else { stop + 1 }).clamp(0, len) as usize;
    if start_idx >= stop_idx {
        return Ok(RespValue::Array(Some(Vec::new())));
    }
    let out: Vec<RespValue> = list
        .iter()
        .skip(start_idx)
        .take(stop_idx - start_idx)
        .map(|v| router::bulk(v.clone()))
        .collect();
    Ok(RespValue::Array(Some(out)))
}

pub(super) fn exec_lindex(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("LINDEX"));
    }
    let idx = parse_i64_arg(&args[1], "index")?;
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(RespValue::Null);
    };
    let list = stored.as_list()?;
    let len = list.len() as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        return Ok(RespValue::Null);
    }
    Ok(router::bulk(list[actual as usize].clone()))
}
