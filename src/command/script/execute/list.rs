//! Lua `redis.call` List 域 `exec_*` (LPUSH/RPUSH/LPOP/RPOP/LLEN/LRANGE/LINDEX).

use std::collections::VecDeque;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_i64_arg, ScriptTransaction};
use crate::error::{Error, Result};
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

fn parse_pop_count(arg: &Bytes) -> Result<i64> {
    let s = std::str::from_utf8(arg)
        .map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    let n: i64 = s
        .parse()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    if n < 0 {
        return Err(Error::Command(
            "ERR value is not an integer or out of range".into(),
        ));
    }
    Ok(n)
}

pub(super) fn exec_lpop(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    from_front: bool,
) -> Result<RespValue> {
    if args.is_empty() || args.len() > 2 {
        let cmd = if from_front { "LPOP" } else { "RPOP" };
        return Err(wrong_count(cmd));
    }
    let count = match args.get(1) {
        Some(arg) => Some(parse_pop_count(arg)?),
        None => None,
    };
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(RespValue::Null);
    };
    let mut list = stored.as_list()?.clone();
    if list.is_empty() {
        return Ok(RespValue::Null);
    }
    let is_count_given = count.is_some();
    let limit = count.unwrap_or(1);
    if limit == 0 {
        return Ok(RespValue::Array(Some(vec![])));
    }
    let n = (limit as usize).min(list.len());
    let mut popped = Vec::with_capacity(n);
    for _ in 0..n {
        let v = if from_front {
            list.pop_front()
        } else {
            list.pop_back()
        };
        popped.push(v.expect("len checked"));
    }
    if list.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_list(key(&args[0]), list);
    }
    if !is_count_given {
        return Ok(router::bulk(popped.into_iter().next().unwrap()));
    }
    Ok(RespValue::Array(Some(
        popped
            .into_iter()
            .map(|b| RespValue::BulkString(Some(b.into())))
            .collect(),
    )))
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
