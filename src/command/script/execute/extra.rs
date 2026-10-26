//! 脚本里补上的 Redis 数据命令. 读写都走 `ScriptTransaction`, 不直接打存储.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use bytes::Bytes;
use rand::seq::SliceRandom;

use crate::command::hash::{format_hash_float, parse_f64_field_bytes};
use crate::command::list::{
    find_lpos, normalize_index, normalize_range, parse_lpos_options, parse_side, remove_direction,
};
use crate::command::router;
use crate::command::scan_util;
use crate::command::script::transaction::{parse_i64_arg, ScriptTransaction};
use crate::command::string::{parse_setrange_offset, string_range_slice};
use crate::command::zset::helpers::{self, format_score};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::memory::glob_match;
use crate::storage::{
    dump_decode, dump_encode, now_ms, zset_subkey, ScriptIo, StoredValue, ValueType,
};

use super::{key, wrong_count};

pub(super) fn exec_mget(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("MGET"));
    }
    let mut items = Vec::with_capacity(args.len());
    for arg in args {
        match txn.get(storage, arg)? {
            Some(v) => items.push(router::bulk(v)),
            None => items.push(router::nil_bulk()),
        }
    }
    Ok(RespValue::Array(Some(items)))
}

pub(super) fn exec_mset(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 || !args.len().is_multiple_of(2) {
        return Err(wrong_count("MSET"));
    }
    for chunk in args.chunks(2) {
        let k = key(&chunk[0]);
        txn.set_string(k.clone(), chunk[1].to_vec());
        txn.set_expire_at(k, None);
    }
    let _ = storage;
    Ok(router::ok())
}

pub(super) fn exec_msetnx(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 || !args.len().is_multiple_of(2) {
        return Err(wrong_count("MSETNX"));
    }
    for chunk in args.chunks(2) {
        if txn.exists(storage, &chunk[0])? {
            return Ok(router::integer(0));
        }
    }
    exec_mset(storage, txn, args)?;
    Ok(router::integer(1))
}

pub(super) fn exec_getrange(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("GETRANGE"));
    }
    let start = parse_i64_arg(&args[1], "start")?;
    let end = parse_i64_arg(&args[2], "end")?;
    match txn.get(storage, &args[0])? {
        None => Ok(router::bulk(Vec::new())),
        Some(value) => Ok(router::bulk(string_range_slice(&value, start, end))),
    }
}

pub(super) fn exec_setrange(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("SETRANGE"));
    }
    let offset = parse_setrange_offset(&args[1])?;
    let patch = &args[2];
    let mut current = txn.get(storage, &args[0])?.unwrap_or_default();
    let end = offset
        .checked_add(patch.len())
        .ok_or_else(|| Error::Command("ERR offset is out of range".into()))?;
    if end > current.len() {
        current.resize(end, 0);
    }
    current[offset..end].copy_from_slice(patch);
    let len = current.len() as i64;
    txn.set_string(key(&args[0]), current);
    Ok(router::integer(len))
}

pub(super) fn exec_setnx(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("SETNX"));
    }
    if txn.get(storage, &args[0])?.is_some() {
        return Ok(router::integer(0));
    }
    txn.set_string(key(&args[0]), args[1].to_vec());
    Ok(router::integer(1))
}

pub(super) fn exec_setex(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    millis: bool,
) -> Result<RespValue> {
    let cmd = if millis { "PSETEX" } else { "SETEX" };
    if args.len() != 3 {
        return Err(wrong_count(cmd));
    }
    let ttl = parse_u64(&args[1])?;
    if ttl == 0 {
        return Err(Error::Command(format!(
            "ERR invalid expire time in '{}' command",
            cmd.to_ascii_lowercase()
        )));
    }
    let expire_at = if millis {
        now_ms().saturating_add(ttl)
    } else {
        now_ms().saturating_add(ttl.saturating_mul(1000))
    };
    let k = key(&args[0]);
    txn.set_string(k.clone(), args[2].to_vec());
    txn.set_expire_at(k, Some(expire_at));
    let _ = storage;
    Ok(router::ok())
}

pub(super) fn exec_getdel(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("GETDEL"));
    }
    match txn.get(storage, &args[0])? {
        None => Ok(router::nil_bulk()),
        Some(v) => {
            txn.delete(key(&args[0]));
            Ok(router::bulk(v))
        }
    }
}

pub(super) fn exec_getex(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("GETEX"));
    }
    let Some(value) = txn.get(storage, &args[0])? else {
        return Ok(router::nil_bulk());
    };
    if args.len() == 1 {
        return Ok(router::bulk(value));
    }
    let opt = String::from_utf8_lossy(&args[1]).to_ascii_uppercase();
    let k = key(&args[0]);
    match opt.as_str() {
        "PERSIST" => txn.set_expire_at(k, None),
        "EX" | "PX" | "EXAT" | "PXAT" => {
            if args.len() < 3 {
                return Err(wrong_count("GETEX"));
            }
            let n = parse_u64(&args[2])?;
            let expire_at = match opt.as_str() {
                "EX" => {
                    if n == 0 {
                        return Err(Error::Command(
                            "ERR invalid expire time in 'getex' command".into(),
                        ));
                    }
                    now_ms().saturating_add(n.saturating_mul(1000))
                }
                "PX" => {
                    if n == 0 {
                        return Err(Error::Command(
                            "ERR invalid expire time in 'getex' command".into(),
                        ));
                    }
                    now_ms().saturating_add(n)
                }
                "EXAT" => n.saturating_mul(1000),
                _ => n,
            };
            txn.set_string(k.clone(), value.clone());
            txn.set_expire_at(k, Some(expire_at));
        }
        _ => return Err(Error::Command("ERR syntax error".into())),
    }
    Ok(router::bulk(value))
}

pub(super) fn exec_hkeys(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
    values: bool,
) -> Result<RespValue> {
    let cmd = if values { "HVALS" } else { "HKEYS" };
    if args.len() != 1 {
        return Err(wrong_count(cmd));
    }
    let items = txn
        .with_hash(storage, &args[0], |hash| {
            let mut items: Vec<Vec<u8>> = if values {
                hash.values().cloned().collect()
            } else {
                hash.keys().cloned().collect()
            };
            items.sort();
            Ok(items)
        })?
        .unwrap_or_default();
    Ok(array_of_bulk(items))
}

pub(super) fn exec_hscan(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("HSCAN"));
    }
    let cursor = scan_util::parse_u64(&args[1])?;
    let opts = scan_util::parse_scan_options("HSCAN", args, 2)?;
    let Some(pairs) = txn.with_hash(storage, &args[0], |hash| {
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = hash
            .iter()
            .filter(|(field, _)| {
                opts.pattern
                    .as_ref()
                    .is_none_or(|p| glob_match(p, field.as_slice()))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pairs)
    })?
    else {
        return Ok(field_scan_response(0, &[]));
    };
    let (next, page) = scan_util::paginate_slice(&pairs, cursor, opts.count);
    Ok(field_scan_response(next, page))
}

pub(super) fn exec_hincrbyfloat(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("HINCRBYFLOAT"));
    }
    let delta = parse_f64_field_bytes(&args[2])?;
    if !delta.is_finite() {
        return Err(Error::Command("ERR value is not a valid float".into()));
    }
    let k = key(&args[0]);
    let hash = match txn.ensure_hash_mut(storage, &k)? {
        Some(h) => h,
        None => {
            txn.set_hash(k.clone(), HashMap::new());
            txn.ensure_hash_mut(storage, &k)?.unwrap()
        }
    };
    let current = match hash.get(args[1].as_ref()) {
        None => 0.0,
        Some(existing) => parse_f64_field_bytes(existing)?,
    };
    let new_val = current + delta;
    if !new_val.is_finite() {
        return Err(Error::Command(
            "ERR increment would produce NaN or Infinity".into(),
        ));
    }
    let text = format_hash_float(new_val);
    hash.insert(args[1].to_vec(), text.as_bytes().to_vec());
    Ok(router::bulk(text.into_bytes()))
}

pub(super) fn exec_lset(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("LSET"));
    }
    let index = parse_i64_arg(&args[1], "index")?;
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Err(Error::Command("ERR no such key".into()));
    };
    let mut list = stored.as_list()?.clone();
    let idx = normalize_index(list.len(), index);
    let Some(slot) = list.get_mut(idx) else {
        return Err(Error::Command("ERR index out of range".into()));
    };
    *slot = args[2].to_vec();
    txn.set_list(key(&args[0]), list);
    Ok(router::ok())
}

pub(super) fn exec_lrem(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("LREM"));
    }
    let count = parse_i64_arg(&args[1], "count")?;
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(router::integer(0));
    };
    let mut list = stored.as_list()?.clone();
    let element = args[2].as_ref();
    let removed = if count == 0 {
        let before = list.len();
        list.retain(|v| v.as_slice() != element);
        before - list.len()
    } else if count > 0 {
        remove_direction(&mut list, element, count as usize, true)
    } else {
        remove_direction(&mut list, element, (-count) as usize, false)
    };
    if list.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_list(key(&args[0]), list);
    }
    Ok(router::integer(removed as i64))
}

pub(super) fn exec_linsert(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 4 {
        return Err(wrong_count("LINSERT"));
    }
    let before = if eq_ignore(&args[1], b"BEFORE") {
        true
    } else if eq_ignore(&args[1], b"AFTER") {
        false
    } else {
        return Err(Error::Command("ERR syntax error".into()));
    };
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(router::integer(0));
    };
    let mut list = stored.as_list()?.clone();
    let Some(idx) = list.iter().position(|v| v.as_slice() == args[2].as_ref()) else {
        return Ok(router::integer(-1));
    };
    let insert_at = if before { idx } else { idx + 1 };
    list.insert(insert_at, args[3].to_vec());
    let len = list.len() as i64;
    txn.set_list(key(&args[0]), list);
    Ok(router::integer(len))
}

pub(super) fn exec_lmove(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 4 {
        return Err(wrong_count("LMOVE"));
    }
    let from_left = parse_side(&args[2])?;
    let to_left = parse_side(&args[3])?;
    let same = args[0].as_ref() == args[1].as_ref();
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(router::nil_bulk());
    };
    let mut src = stored.as_list()?.clone();
    if src.is_empty() {
        return Ok(router::nil_bulk());
    }
    if !same {
        if let Some(dest) = txn.get_value(storage, &args[1])? {
            dest.as_list()?;
        }
    }
    let element = if from_left {
        src.pop_front().expect("non-empty")
    } else {
        src.pop_back().expect("non-empty")
    };
    if same {
        if to_left {
            src.push_front(element.clone());
        } else {
            src.push_back(element.clone());
        }
        txn.set_list(key(&args[0]), src);
        return Ok(router::bulk(element));
    }
    if src.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_list(key(&args[0]), src);
    }
    let mut dest = match txn.get_value(storage, &args[1])? {
        Some(stored) => stored.as_list()?.clone(),
        None => VecDeque::new(),
    };
    if to_left {
        dest.push_front(element.clone());
    } else {
        dest.push_back(element.clone());
    }
    txn.set_list(key(&args[1]), dest);
    Ok(router::bulk(element))
}

pub(super) fn exec_rpoplpush(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("RPOPLPUSH"));
    }
    let moved = [
        args[0].clone(),
        args[1].clone(),
        Bytes::from_static(b"RIGHT"),
        Bytes::from_static(b"LEFT"),
    ];
    exec_lmove(storage, txn, &moved)
}

pub(super) fn exec_lpos(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("LPOS"));
    }
    let opts = parse_lpos_options(&args[2..])?;
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return if opts.count.is_some() {
            Ok(RespValue::Array(Some(vec![])))
        } else {
            Ok(router::nil_bulk())
        };
    };
    let list = stored.as_list()?.clone();
    find_lpos(&list, args[1].as_ref(), &opts)
}

pub(super) fn exec_ltrim(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("LTRIM"));
    }
    let start = parse_i64_arg(&args[1], "start")?;
    let stop = parse_i64_arg(&args[2], "stop")?;
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(router::ok());
    };
    let list = stored.as_list()?.clone();
    let (start_idx, stop_idx) = normalize_range(list.len(), start, stop);
    if start_idx > stop_idx || list.is_empty() {
        txn.delete(key(&args[0]));
        return Ok(router::ok());
    }
    let trimmed: VecDeque<Vec<u8>> = list
        .iter()
        .skip(start_idx)
        .take(stop_idx - start_idx + 1)
        .cloned()
        .collect();
    if trimmed.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_list(key(&args[0]), trimmed);
    }
    Ok(router::ok())
}

pub(super) fn exec_spop(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("SPOP"));
    }
    let count = if args.len() > 1 {
        Some(parse_i64_arg(&args[1], "count")?)
    } else {
        None
    };
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return if count.is_some() {
            Ok(RespValue::Array(Some(vec![])))
        } else {
            Ok(router::nil_bulk())
        };
    };
    let mut set = stored.as_set()?.clone();
    let count = count.unwrap_or(1);
    if count < 0 {
        return Err(Error::Command(
            "ERR value is not an integer or out of range".into(),
        ));
    }
    if count == 0 {
        return Ok(RespValue::Array(Some(vec![])));
    }
    let mut members: Vec<Vec<u8>> = set.iter().cloned().collect();
    members.shuffle(&mut rand::thread_rng());
    let n = (count as usize).min(members.len());
    let picked: Vec<Vec<u8>> = members.into_iter().take(n).collect();
    for member in &picked {
        set.remove(member);
    }
    if set.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_set(key(&args[0]), set);
    }
    if args.len() == 1 {
        Ok(picked
            .into_iter()
            .next()
            .map(router::bulk)
            .unwrap_or(router::nil_bulk()))
    } else {
        Ok(array_of_bulk(picked))
    }
}

pub(super) fn exec_srandmember(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("SRANDMEMBER"));
    }
    let count = if args.len() > 1 {
        Some(parse_i64_arg(&args[1], "count")?)
    } else {
        None
    };
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return if count.is_some() {
            Ok(RespValue::Array(Some(vec![])))
        } else {
            Ok(router::nil_bulk())
        };
    };
    let set = stored.as_set()?;
    if set.is_empty() {
        return if count.is_some() {
            Ok(RespValue::Array(Some(vec![])))
        } else {
            Ok(router::nil_bulk())
        };
    }
    let mut members: Vec<Vec<u8>> = set.iter().cloned().collect();
    let count = count.unwrap_or(1);
    if count == 0 {
        return Ok(RespValue::Array(Some(vec![])));
    }
    let mut rng = rand::thread_rng();
    if count > 0 {
        members.shuffle(&mut rng);
        let n = (count as usize).min(members.len());
        let picked: Vec<Vec<u8>> = members.into_iter().take(n).collect();
        if args.len() == 1 {
            return Ok(router::bulk(picked.into_iter().next().unwrap()));
        }
        return Ok(array_of_bulk(picked));
    }
    let picked: Vec<Vec<u8>> = (0..(-count) as usize)
        .map(|_| members.choose(&mut rng).cloned().unwrap())
        .collect();
    Ok(array_of_bulk(picked))
}

pub(super) fn exec_sunion(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("SUNION"));
    }
    Ok(array_of_bulk(
        set_combine(storage, txn, args, SetOp::Union)?
            .into_iter()
            .collect(),
    ))
}

pub(super) fn exec_sinter(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("SINTER"));
    }
    Ok(array_of_bulk(
        set_combine(storage, txn, args, SetOp::Inter)?
            .into_iter()
            .collect(),
    ))
}

pub(super) fn exec_sintercard(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("SINTERCARD"));
    }
    let numkeys: usize = bytes_to_str(&args[0])?
        .parse()
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))?;
    if numkeys < 1 || args.len() < 1 + numkeys {
        return Err(Error::Command(
            "ERR at least 1 input key is needed for the 'SINTERCARD' command".into(),
        ));
    }
    let mut index = 1 + numkeys;
    let mut limit = None;
    if index < args.len() {
        if !eq_ignore(&args[index], b"LIMIT") {
            return Err(Error::Command("ERR syntax error".into()));
        }
        let Some(raw_limit) = args.get(index + 1) else {
            return Err(wrong_count("SINTERCARD"));
        };
        let n = parse_i64_arg(raw_limit, "limit")?;
        if n < 0 {
            return Err(Error::Command("ERR LIMIT can't be negative".into()));
        }
        if n > 0 {
            limit = Some(n as usize);
        }
        index += 2;
        if index != args.len() {
            return Err(Error::Command("ERR syntax error".into()));
        }
    }
    let set = set_combine(storage, txn, &args[1..1 + numkeys], SetOp::Inter)?;
    let count = match limit {
        Some(limit) => set.len().min(limit),
        None => set.len(),
    };
    Ok(router::integer(count as i64))
}

pub(super) fn exec_sdiff(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("SDIFF"));
    }
    Ok(array_of_bulk(
        set_combine(storage, txn, args, SetOp::Diff)?
            .into_iter()
            .collect(),
    ))
}

pub(super) fn exec_sstore(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    op: SetOp,
    cmd: &str,
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count(cmd));
    }
    let result = set_combine(storage, txn, &args[1..], op)?;
    let len = result.len() as i64;
    if result.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_set(key(&args[0]), result);
    }
    Ok(router::integer(len))
}

pub(super) fn exec_smove(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("SMOVE"));
    }
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(router::integer(0));
    };
    let mut source = stored.as_set()?.clone();
    let member = args[2].to_vec();
    if !source.remove(&member) {
        return Ok(router::integer(0));
    }
    if args[0].as_ref() == args[1].as_ref() {
        return Ok(router::integer(1));
    }
    if source.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_set(key(&args[0]), source);
    }
    let mut dest = match txn.get_value(storage, &args[1])? {
        Some(stored) => stored.as_set()?.clone(),
        None => HashSet::new(),
    };
    dest.insert(member);
    txn.set_set(key(&args[1]), dest);
    Ok(router::integer(1))
}

#[derive(Clone, Copy)]
pub(super) enum SetOp {
    Union,
    Inter,
    Diff,
}

fn set_combine(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
    op: SetOp,
) -> Result<HashSet<Vec<u8>>> {
    let mut sets = Vec::with_capacity(args.len());
    for arg in args {
        match txn.get_value(storage, arg)? {
            None => sets.push(HashSet::new()),
            Some(stored) => sets.push(stored.as_set()?.clone()),
        }
    }
    Ok(match op {
        SetOp::Union => {
            let mut out = HashSet::new();
            for set in sets {
                out.extend(set);
            }
            out
        }
        SetOp::Inter => {
            let mut out = sets.first().cloned().unwrap_or_default();
            for set in sets.iter().skip(1) {
                out.retain(|member| set.contains(member));
            }
            out
        }
        SetOp::Diff => {
            let mut out = sets.first().cloned().unwrap_or_default();
            for set in sets.iter().skip(1) {
                for member in set {
                    out.remove(member);
                }
            }
            out
        }
    })
}

pub(super) fn exec_zcount(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("ZCOUNT"));
    }
    let min = helpers::parse_score_bound(bytes_to_str(&args[1])?, true)?;
    let max = helpers::parse_score_bound(bytes_to_str(&args[2])?, false)?;
    let count = txn
        .with_zset(storage, &args[0], |zset| {
            Ok(zset
                .values()
                .filter(|score| helpers::score_in_range(**score, &min, &max))
                .count() as i64)
        })?
        .unwrap_or(0);
    Ok(router::integer(count))
}

pub(super) fn exec_zincrby(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 3 {
        return Err(wrong_count("ZINCRBY"));
    }
    let increment = helpers::parse_score(&args[1])?;
    if txn.uses_subkey_zset(storage, &args[0])? {
        return txn.subkey_zincrby(storage, &args[0], increment, args[2].as_ref());
    }
    let k = key(&args[0]);
    let zset = match txn.ensure_zset_mut(storage, &k)? {
        Some(z) => z,
        None => {
            txn.set_zset(k.clone(), Default::default());
            txn.ensure_zset_mut(storage, &k)?.unwrap()
        }
    };
    let new_score = zset.get(args[2].as_ref()).copied().unwrap_or(0.0) + increment;
    if !new_score.is_finite() {
        return Err(Error::Command(
            "ERR increment would produce NaN or Infinity".into(),
        ));
    }
    zset.insert(args[2].to_vec(), new_score);
    Ok(router::bulk(format_score(new_score).into_bytes()))
}

pub(super) fn exec_zrevrank(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("ZREVRANK"));
    }
    let Some(found) = txn.with_zset(storage, &args[0], |zset| {
        if !zset.contains_key(args[1].as_ref()) {
            return Ok(None);
        }
        let sorted = helpers::sorted_by_score(zset, true);
        let pos = sorted
            .iter()
            .position(|(member, _)| member.as_slice() == args[1].as_ref())
            .unwrap();
        Ok(Some(pos as i64))
    })?
    else {
        return Ok(router::nil_bulk());
    };
    match found {
        Some(pos) => Ok(router::integer(pos)),
        None => Ok(router::nil_bulk()),
    }
}

pub(super) fn exec_zscan(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("ZSCAN"));
    }
    let cursor = scan_util::parse_u64(&args[1])?;
    let opts = scan_util::parse_scan_options("ZSCAN", args, 2)?;
    let Some(entries) = txn.with_zset(storage, &args[0], |zset| {
        let entries: Vec<(Vec<u8>, f64)> = zset
            .iter()
            .filter(|(member, _)| {
                opts.pattern
                    .as_ref()
                    .is_none_or(|p| glob_match(p, member.as_slice()))
            })
            .map(|(m, s)| (m.clone(), *s))
            .collect();
        Ok(entries)
    })?
    else {
        return Ok(helpers::scan_response(0, &[]));
    };
    let (next, page) = scan_util::paginate_slice(&entries, cursor, opts.count);
    Ok(helpers::scan_response(next, page))
}

pub(super) fn exec_zpop(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    max: bool,
) -> Result<RespValue> {
    let cmd = if max { "ZPOPMAX" } else { "ZPOPMIN" };
    if args.is_empty() {
        return Err(wrong_count(cmd));
    }
    let count = if args.len() > 1 {
        let n = parse_i64_arg(&args[1], "count")?;
        if n < 0 {
            return Err(Error::Command(
                "ERR value is not an integer or out of range".into(),
            ));
        }
        n as usize
    } else {
        1
    };
    if txn.uses_subkey_zset(storage, &args[0])? {
        return txn.subkey_zpop(storage, &args[0], count, max);
    }
    let Some(stored) = txn.get_value(storage, &args[0])? else {
        return Ok(RespValue::Array(Some(vec![])));
    };
    let mut zset = stored.as_zset()?.clone();
    let sorted = helpers::sorted_by_score(&zset, max);
    let n = count.min(sorted.len());
    let mut items = Vec::with_capacity(n * 2);
    for (member, score) in sorted.into_iter().take(n) {
        zset.remove(&member);
        items.push(router::bulk(member));
        items.push(router::bulk(format_score(score).into_bytes()));
    }
    if zset.is_empty() {
        txn.delete(key(&args[0]));
    } else {
        txn.set_zset(key(&args[0]), zset);
    }
    Ok(RespValue::Array(Some(items)))
}

pub(super) fn exec_zlex(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
    reverse: bool,
    count_only: bool,
) -> Result<RespValue> {
    let cmd = if count_only {
        "ZLEXCOUNT"
    } else if reverse {
        "ZREVRANGEBYLEX"
    } else {
        "ZRANGEBYLEX"
    };
    if args.len() < 3 {
        return Err(wrong_count(cmd));
    }
    let (min, max) = if reverse && !count_only {
        (
            helpers::parse_lex_bound(bytes_to_str(&args[2])?)?,
            helpers::parse_lex_bound(bytes_to_str(&args[1])?)?,
        )
    } else {
        (
            helpers::parse_lex_bound(bytes_to_str(&args[1])?)?,
            helpers::parse_lex_bound(bytes_to_str(&args[2])?)?,
        )
    };
    let mut limit = None;
    if !count_only {
        let mut i = 3;
        while i < args.len() {
            if eq_ignore(&args[i], b"LIMIT") {
                if i + 2 >= args.len() {
                    return Err(wrong_count(cmd));
                }
                limit = Some(helpers::parse_limit_args(cmd, &args[i + 1], &args[i + 2])?);
                i += 3;
            } else {
                return Err(wrong_count(cmd));
            }
        }
    }
    let Some(mut members) = txn.with_zset(storage, &args[0], |zset| {
        let members: Vec<Vec<u8>> = zset
            .keys()
            .filter(|member| helpers::lex_in_range(member.as_slice(), &min, &max))
            .cloned()
            .collect();
        Ok(members)
    })?
    else {
        return if count_only {
            Ok(router::integer(0))
        } else {
            Ok(RespValue::Array(Some(vec![])))
        };
    };
    if count_only {
        return Ok(router::integer(members.len() as i64));
    }
    if reverse {
        members.reverse();
    }
    if let Some((offset, count)) = limit {
        helpers::apply_limit(&mut members, offset, count);
    }
    Ok(array_of_bulk(members))
}

pub(super) fn exec_zinter(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    let opts = helpers::parse_zset_combine_args("ZINTER", args)?;
    let zsets = load_zsets(storage, txn, &opts.keys)?;
    if zsets.is_empty() {
        return Ok(RespValue::Array(Some(vec![])));
    }
    let smallest = zsets
        .iter()
        .enumerate()
        .min_by_key(|(_, z)| z.len())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut results = Vec::new();
    for member in zsets[smallest].keys() {
        if zsets.iter().all(|z| z.contains_key(member)) {
            let score = helpers::aggregate_score(&zsets, member, &opts.weights, &opts.aggregate);
            results.push((member.clone(), score));
        }
    }
    helpers::sort_by_score_then_member(&mut results);
    Ok(helpers::members_to_resp(&results, opts.withscores))
}

pub(super) fn exec_zunion(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    let opts = helpers::parse_zset_combine_args("ZUNION", args)?;
    let zsets = load_zsets(storage, txn, &opts.keys)?;
    let mut seen = Vec::new();
    for zset in &zsets {
        for member in zset.keys() {
            if !seen.contains(member) {
                seen.push(member.clone());
            }
        }
    }
    let mut results = Vec::with_capacity(seen.len());
    for member in &seen {
        let score = helpers::aggregate_score(&zsets, member, &opts.weights, &opts.aggregate);
        results.push((member.clone(), score));
    }
    helpers::sort_by_score_then_member(&mut results);
    Ok(helpers::members_to_resp(&results, opts.withscores))
}

pub(super) fn exec_zunionstore(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    store_zset_combine(storage, txn, args, "ZUNIONSTORE", StoreCombine::Union)
}

pub(super) fn exec_zinterstore(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    store_zset_combine(storage, txn, args, "ZINTERSTORE", StoreCombine::Inter)
}

pub(super) fn exec_zdiffstore(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count("ZDIFFSTORE"));
    }
    let results = zdiff_results(storage, txn, &args[1..], "ZDIFFSTORE")?;
    write_script_zset(txn, &args[0], results)
}

enum StoreCombine {
    Union,
    Inter,
}

fn store_zset_combine(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    cmd: &str,
    op: StoreCombine,
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count(cmd));
    }
    let opts = helpers::parse_zset_combine_args(cmd, &args[1..])?;
    if opts.withscores {
        return Err(Error::Command("ERR syntax error".into()));
    }
    let zsets = load_zsets(storage, txn, &opts.keys)?;
    let mut results = match op {
        StoreCombine::Union => union_pairs(&zsets, &opts),
        StoreCombine::Inter => inter_pairs(&zsets, &opts),
    };
    helpers::sort_by_score_then_member(&mut results);
    write_script_zset(txn, &args[0], results)
}

fn union_pairs(
    zsets: &[BTreeMap<Vec<u8>, f64>],
    opts: &helpers::ZSetCombineArgs,
) -> Vec<(Vec<u8>, f64)> {
    let mut seen = Vec::new();
    for zset in zsets {
        for member in zset.keys() {
            if !seen.contains(member) {
                seen.push(member.clone());
            }
        }
    }
    seen.into_iter()
        .map(|member| {
            let score = helpers::aggregate_score(zsets, &member, &opts.weights, &opts.aggregate);
            (member, score)
        })
        .collect()
}

fn inter_pairs(
    zsets: &[BTreeMap<Vec<u8>, f64>],
    opts: &helpers::ZSetCombineArgs,
) -> Vec<(Vec<u8>, f64)> {
    if zsets.is_empty() {
        return Vec::new();
    }
    let smallest = zsets
        .iter()
        .enumerate()
        .min_by_key(|(_, z)| z.len())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut results = Vec::new();
    for member in zsets[smallest].keys() {
        if zsets.iter().all(|z| z.contains_key(member)) {
            let score = helpers::aggregate_score(zsets, member, &opts.weights, &opts.aggregate);
            results.push((member.clone(), score));
        }
    }
    results
}

fn zdiff_results(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
    cmd: &str,
) -> Result<Vec<(Vec<u8>, f64)>> {
    if args.len() < 2 {
        return Err(wrong_count(cmd));
    }
    let numkeys: usize = bytes_to_str(&args[0])?
        .parse()
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))?;
    if numkeys < 1 || args.len() < 1 + numkeys {
        return Err(wrong_count(cmd));
    }
    if args.len() != 1 + numkeys {
        return Err(Error::Command("ERR syntax error".into()));
    }
    let zsets = load_zsets(storage, txn, &args[1..1 + numkeys])?;
    let mut results = Vec::new();
    if !zsets.is_empty() {
        for (member, score) in &zsets[0] {
            if zsets[1..].iter().any(|z| z.contains_key(member)) {
                continue;
            }
            results.push((member.clone(), *score));
        }
    }
    helpers::sort_by_score_then_member(&mut results);
    Ok(results)
}

fn write_script_zset(
    txn: &mut ScriptTransaction,
    dest: &Bytes,
    results: Vec<(Vec<u8>, f64)>,
) -> Result<RespValue> {
    let len = results.len() as i64;
    if results.is_empty() {
        txn.delete(key(dest));
        return Ok(router::integer(0));
    }
    txn.set_zset(key(dest), results.into_iter().collect());
    Ok(router::integer(len))
}

pub(super) fn exec_zdiff(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("ZDIFF"));
    }
    let numkeys: usize = bytes_to_str(&args[0])?
        .parse()
        .map_err(|_| Error::Command("ERR value is not an integer or out of range".into()))?;
    if numkeys < 1 || args.len() < 1 + numkeys {
        return Err(wrong_count("ZDIFF"));
    }
    let withscores = args.iter().any(|a| eq_ignore(a, b"WITHSCORES"));
    let zsets = load_zsets(storage, txn, &args[1..1 + numkeys])?;
    let mut results = Vec::new();
    for (member, score) in &zsets[0] {
        if zsets[1..].iter().any(|z| z.contains_key(member)) {
            continue;
        }
        results.push((member.clone(), *score));
    }
    helpers::sort_by_score_then_member(&mut results);
    Ok(helpers::members_to_resp(&results, withscores))
}

fn load_zsets(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    keys: &[Bytes],
) -> Result<Vec<std::collections::BTreeMap<Vec<u8>, f64>>> {
    let mut zsets = Vec::with_capacity(keys.len());
    for key in keys {
        let map = txn
            .with_zset(storage, key, |zset| Ok(zset.clone()))?
            .unwrap_or_default();
        zsets.push(map);
    }
    Ok(zsets)
}

pub(super) fn exec_pexpire(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    millis: bool,
    absolute: bool,
) -> Result<RespValue> {
    let cmd = match (millis, absolute) {
        (false, false) => "EXPIRE",
        (true, false) => "PEXPIRE",
        (false, true) => "EXPIREAT",
        (true, true) => "PEXPIREAT",
    };
    if args.len() != 2 {
        return Err(wrong_count(cmd));
    }
    if !txn.exists(storage, &args[0])? {
        return Ok(router::integer(0));
    }
    let n = parse_i64_arg(&args[1], "ttl")?;
    if !absolute && n < 0 {
        txn.delete(key(&args[0]));
        return Ok(router::integer(1));
    }
    let at = if absolute {
        if millis {
            n
        } else {
            n.saturating_mul(1000)
        }
    } else if millis {
        now_ms() as i64 + n
    } else {
        now_ms() as i64 + n.saturating_mul(1000)
    };
    if at <= now_ms() as i64 {
        txn.delete(key(&args[0]));
    } else {
        txn.set_expire_at(key(&args[0]), Some(at as u64));
    }
    Ok(router::integer(1))
}

pub(super) fn exec_pttl(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
    millis: bool,
) -> Result<RespValue> {
    let cmd = if millis { "PTTL" } else { "TTL" };
    if args.len() != 1 {
        return Err(wrong_count(cmd));
    }
    if !txn.exists(storage, &args[0])? {
        return Ok(router::integer(-2));
    }
    let Some(exp) = txn.resolve_expires_at(storage, &args[0])? else {
        return Ok(router::integer(-1));
    };
    let left = exp as i64 - now_ms() as i64;
    if left <= 0 {
        return Ok(router::integer(-2));
    }
    if millis {
        Ok(router::integer(left))
    } else {
        Ok(router::integer((left / 1000).max(0)))
    }
}

pub(super) fn exec_expiretime(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
    millis: bool,
) -> Result<RespValue> {
    let cmd = if millis { "PEXPIRETIME" } else { "EXPIRETIME" };
    if args.len() != 1 {
        return Err(wrong_count(cmd));
    }
    if !txn.exists(storage, &args[0])? {
        return Ok(router::integer(-2));
    }
    match txn.resolve_expires_at(storage, &args[0])? {
        None => Ok(router::integer(-1)),
        Some(ms) if millis => Ok(router::integer(ms as i64)),
        Some(ms) => Ok(router::integer((ms / 1000) as i64)),
    }
}

pub(super) fn exec_persist(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("PERSIST"));
    }
    if !txn.exists(storage, &args[0])? {
        return Ok(router::integer(0));
    }
    if txn.resolve_expires_at(storage, &args[0])?.is_none() {
        return Ok(router::integer(0));
    }
    txn.set_expire_at(key(&args[0]), None);
    Ok(router::integer(1))
}

pub(super) fn exec_type(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("TYPE"));
    }
    match txn.get_value(storage, &args[0])? {
        None => Ok(RespValue::SimpleString("none".into())),
        Some(stored) => Ok(RespValue::SimpleString(stored.type_name().into())),
    }
}

pub(super) fn exec_rename(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
    nx: bool,
) -> Result<RespValue> {
    let cmd = if nx { "RENAMENX" } else { "RENAME" };
    if args.len() != 2 {
        return Err(wrong_count(cmd));
    }
    if args[0].as_ref() == args[1].as_ref() {
        if txn.get_value(storage, &args[0])?.is_none() {
            return Err(Error::Command("ERR no such key".into()));
        }
        return if nx {
            Ok(router::integer(0))
        } else {
            Ok(router::ok())
        };
    }
    let Some(stored) = read_movable(storage, txn, &args[0])? else {
        return Err(Error::Command("ERR no such key".into()));
    };
    if nx && txn.exists(storage, &args[1])? {
        return Ok(router::integer(0));
    }
    txn.replace_stored(key(&args[1]), stored)?;
    txn.delete(key(&args[0]));
    if nx {
        Ok(router::integer(1))
    } else {
        Ok(router::ok())
    }
}

pub(super) fn exec_copy(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("COPY"));
    }
    let mut replace = false;
    let mut i = 2;
    while i < args.len() {
        if eq_ignore(&args[i], b"REPLACE") {
            replace = true;
            i += 1;
        } else if eq_ignore(&args[i], b"DB") {
            if i + 1 >= args.len() {
                return Err(wrong_count("COPY"));
            }
            let dest_db = parse_i64_arg(&args[i + 1], "db")?;
            if dest_db != txn.db_index as i64 {
                return Err(Error::Command(
                    "ERR COPY across databases is not supported in scripts".into(),
                ));
            }
            i += 2;
        } else {
            return Err(wrong_count("COPY"));
        }
    }
    if args[0].as_ref() == args[1].as_ref() {
        return Ok(router::integer(i64::from(
            replace && txn.exists(storage, &args[0])?,
        )));
    }
    let Some(stored) = read_movable(storage, txn, &args[0])? else {
        return Ok(router::integer(0));
    };
    if txn.exists(storage, &args[1])? && !replace {
        return Ok(router::integer(0));
    }
    txn.replace_stored(key(&args[1]), stored)?;
    Ok(router::integer(1))
}

pub(super) fn exec_dump(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("DUMP"));
    }
    match read_movable(storage, txn, &args[0])? {
        None => Ok(router::nil_bulk()),
        Some(stored) => Ok(router::bulk(dump_encode(&stored)?)),
    }
}

pub(super) fn exec_restore(
    storage: &dyn ScriptIo,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count("RESTORE"));
    }
    let ttl_ms = parse_i64_arg(&args[1], "ttl")?;
    if ttl_ms < 0 {
        return Err(Error::Command("ERR Invalid TTL value".into()));
    }
    let mut replace = false;
    let mut absttl = false;
    let mut i = 3;
    while i < args.len() {
        if eq_ignore(&args[i], b"REPLACE") {
            replace = true;
            i += 1;
        } else if eq_ignore(&args[i], b"ABSTTL") {
            absttl = true;
            i += 1;
        } else {
            return Err(wrong_count("RESTORE"));
        }
    }
    if txn.exists(storage, &args[0])? && !replace {
        return Err(Error::Command(
            "BUSYKEY Target key name already exists.".into(),
        ));
    }
    let mut stored = dump_decode(&args[2])?;
    if ttl_ms == 0 {
        stored.expires_at = None;
    } else if absttl {
        stored.expires_at = Some(ttl_ms as u64);
    } else {
        stored.expires_at = Some(now_ms().saturating_add(ttl_ms as u64));
    }
    txn.replace_stored(key(&args[0]), stored)?;
    Ok(router::ok())
}

pub(super) fn exec_object(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.is_empty() {
        return Err(wrong_count("OBJECT"));
    }
    let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    if sub == "HELP" {
        return Ok(RespValue::Array(Some(vec![
            router::bulk(b"OBJECT ENCODING key - Return encoding of the object".to_vec()),
            router::bulk(b"OBJECT REFCOUNT key - Return reference count (stub: 1)".to_vec()),
            router::bulk(b"OBJECT IDLETIME key - Return idle time in seconds (stub: 0)".to_vec()),
            router::bulk(b"OBJECT FREQ key - Return access frequency (stub: 0)".to_vec()),
        ])));
    }
    if args.len() < 2 {
        return Err(wrong_count("OBJECT"));
    }
    let stored = txn.get_value(storage, &args[1])?;
    if stored.is_none() {
        return Err(Error::Command("ERR no such key".into()));
    }
    match sub.as_str() {
        "ENCODING" => {
            let enc = match &stored.unwrap().value {
                ValueType::String(v) if v.len() <= 44 => "embstr",
                ValueType::String(_) => "raw",
                ValueType::Hash(_)
                | ValueType::List(_)
                | ValueType::Set(_)
                | ValueType::ZSet(_)
                | ValueType::CollectionHeader { .. } => "listpack",
            };
            Ok(router::bulk(enc.as_bytes().to_vec()))
        }
        "REFCOUNT" => Ok(router::integer(1)),
        "IDLETIME" | "FREQ" => Ok(router::integer(0)),
        _ => Err(Error::Command(format!(
            "ERR unknown OBJECT subcommand '{sub}'"
        ))),
    }
}

pub(super) fn exec_ping(args: &[Bytes]) -> Result<RespValue> {
    if args.is_empty() {
        Ok(RespValue::SimpleString("PONG".into()))
    } else if args.len() == 1 {
        Ok(router::bulk(args[0].to_vec()))
    } else {
        Err(wrong_count("PING"))
    }
}

pub(super) fn exec_echo(args: &[Bytes]) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("ECHO"));
    }
    Ok(router::bulk(args[0].to_vec()))
}

pub(super) fn exec_time(args: &[Bytes]) -> Result<RespValue> {
    if !args.is_empty() {
        return Err(wrong_count("TIME"));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(RespValue::Array(Some(vec![
        router::bulk(now.as_secs().to_string().into_bytes()),
        router::bulk(now.subsec_micros().to_string().into_bytes()),
    ])))
}

fn read_movable(
    storage: &dyn ScriptIo,
    txn: &ScriptTransaction,
    raw_key: &[u8],
) -> Result<Option<StoredValue>> {
    let Some(stored) = txn.get_value(storage, raw_key)? else {
        return Ok(None);
    };
    let expires_at = txn.resolve_expires_at(storage, raw_key)?;
    if zset_subkey::is_zset_header(&stored).is_some() {
        let map = txn
            .with_zset(storage, raw_key, |zset| Ok(zset.clone()))?
            .unwrap_or_default();
        return Ok(Some(StoredValue {
            value: ValueType::ZSet(map),
            expires_at,
        }));
    }
    let mut stored = stored;
    stored.expires_at = expires_at;
    Ok(Some(stored))
}

fn field_scan_response(cursor: u64, page: &[(Vec<u8>, Vec<u8>)]) -> RespValue {
    let mut pairs = Vec::with_capacity(page.len() * 2);
    for (field, value) in page {
        pairs.push(router::bulk(field.clone()));
        pairs.push(router::bulk(value.clone()));
    }
    RespValue::Array(Some(vec![
        router::bulk(cursor.to_string().into_bytes()),
        RespValue::Array(Some(pairs)),
    ]))
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn eq_ignore(a: &Bytes, b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

fn parse_u64(b: &Bytes) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command("ERR syntax error".into()))?;
    s.parse::<u64>()
        .map_err(|_| Error::Command("ERR syntax error".into()))
}

fn bytes_to_str(b: &Bytes) -> Result<&str> {
    std::str::from_utf8(b).map_err(|_| Error::Command("ERR syntax error".into()))
}
