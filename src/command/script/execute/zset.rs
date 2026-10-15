//! Lua `redis.call` ZSet 域 `exec_*` (ZADD/ZREM/ZSCORE/ZRANK/ZRANGE/ZCARD).

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_f64_arg, parse_i64_arg, ScriptTransaction};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::KvStorage;

use super::{key, wrong_count};

pub(super) async fn exec_zadd(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
        return Err(wrong_count("ZADD"));
    }
    let k = key(&args[0]);
    if txn.uses_subkey_zset(storage, &k).await? {
        return txn.subkey_zadd(storage, args).await;
    }
    let mut added = 0i64;
    let zset = match txn.ensure_zset_mut(storage, &k).await? {
        Some(z) => z,
        None => {
            txn.set_zset(k.clone(), BTreeMap::new());
            txn.ensure_zset_mut(storage, &k).await?.unwrap()
        }
    };
    for i in (1..args.len()).step_by(2) {
        let score = parse_f64_arg(&args[i], "score")?;
        if zset.insert(args[i + 1].to_vec(), score).is_none() {
            added += 1;
        }
    }
    Ok(router::integer(added))
}

pub(super) async fn exec_zrem(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("ZREM"));
    }
    let k = key(&args[0]);
    if txn.uses_subkey_zset(storage, &k).await? {
        return txn.subkey_zrem(storage, args).await;
    }
    let mut count = 0i64;
    let should_delete = if let Some(zset) = txn.ensure_zset_mut(storage, &k).await? {
        for m in &args[1..] {
            if zset.remove(m.as_ref()).is_some() {
                count += 1;
            }
        }
        zset.is_empty()
    } else {
        false
    };
    if should_delete {
        txn.delete(k);
    }
    Ok(router::integer(count))
}

pub(super) async fn exec_zscore(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("ZSCORE"));
    }
    let key = key(&args[0]);
    if txn.uses_subkey_zset(storage, &key).await? {
        return Ok(score_bulk(
            txn.point_member_score(storage, &key, &args[1]).await?,
        ));
    }
    let res = txn
        .with_zset(storage, &key, |zset| {
            Ok(match zset.get(args[1].as_ref()) {
                Some(s) => router::bulk(s.to_string().into_bytes()),
                None => RespValue::Null,
            })
        })
        .await?;
    Ok(res.unwrap_or(RespValue::Null))
}

fn score_bulk(score: Option<f64>) -> RespValue {
    match score {
        Some(score) => router::bulk(score.to_string().into_bytes()),
        None => RespValue::Null,
    }
}

pub(super) async fn exec_zrank(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("ZRANK"));
    }
    let res = txn
        .with_zset(storage, &args[0], |zset| {
            if !zset.contains_key(args[1].as_ref()) {
                return Ok(RespValue::Null);
            }
            let mut sorted: Vec<_> = zset.iter().collect();
            sorted.sort_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(b.0))
            });
            for (rank, (m, _)) in sorted.iter().enumerate() {
                if *m == args[1].as_ref() {
                    return Ok(router::integer(rank as i64));
                }
            }
            Ok(RespValue::Null)
        })
        .await?;
    Ok(res.unwrap_or(RespValue::Null))
}

use crate::command::zset::helpers::{
    bytes_to_str, eq_ignore_case, members_to_resp, normalize_range, parse_limit_args,
    parse_score_bound, score_in_range, sorted_by_score, ScoreBound,
};
use crate::storage::zset_subkey::{self, score_index_entry, score_prefix};
use crate::storage::AiDbEngine;

pub(super) async fn exec_zrange(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count("ZRANGE"));
    }
    range_by_index(storage, txn, args, false).await
}

pub(super) async fn exec_zrevrange(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count("ZREVRANGE"));
    }
    range_by_index(storage, txn, args, true).await
}

async fn range_by_index(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
    reverse: bool,
) -> Result<RespValue> {
    let start = parse_i64_arg(&args[1], "start")?;
    let stop = parse_i64_arg(&args[2], "stop")?;
    let with_scores = args.iter().any(|a| eq_ignore_case(a, b"WITHSCORES"));
    if reverse && start == 0 && stop == 0 {
        if let Some(resp) = revrange_top(storage, txn, &args[0], with_scores).await? {
            return Ok(resp);
        }
    }
    let res = txn
        .with_zset(storage, &args[0], |zset| {
            let sorted = sorted_by_score(zset, reverse);
            let len = sorted.len();
            let (start_idx, stop_idx) = normalize_range(len, start, stop);
            if start_idx > stop_idx {
                return Ok(RespValue::Array(Some(Vec::new())));
            }
            let slice = &sorted[start_idx..=stop_idx];
            Ok(members_to_resp(slice, with_scores))
        })
        .await?;
    Ok(res.unwrap_or_else(|| RespValue::Array(Some(Vec::new()))))
}

pub(super) async fn exec_zrangebyscore(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count("ZRANGEBYSCORE"));
    }
    range_by_score(storage, txn, args, false).await
}

pub(super) async fn exec_zrevrangebyscore(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 {
        return Err(wrong_count("ZREVRANGEBYSCORE"));
    }
    range_by_score(storage, txn, args, true).await
}

async fn range_by_score(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
    reverse: bool,
) -> Result<RespValue> {
    let (min, max) = if reverse {
        (
            parse_score_bound(bytes_to_str(&args[2])?, true)?,
            parse_score_bound(bytes_to_str(&args[1])?, false)?,
        )
    } else {
        (
            parse_score_bound(bytes_to_str(&args[1])?, true)?,
            parse_score_bound(bytes_to_str(&args[2])?, false)?,
        )
    };
    let mut withscores = false;
    let mut limit: Option<(usize, Option<usize>)> = None;
    let mut i = 3;
    let cmd_name = if reverse {
        "ZREVRANGEBYSCORE"
    } else {
        "ZRANGEBYSCORE"
    };
    while i < args.len() {
        if eq_ignore_case(&args[i], b"WITHSCORES") {
            withscores = true;
            i += 1;
        } else if eq_ignore_case(&args[i], b"LIMIT") {
            if i + 2 >= args.len() {
                return Err(wrong_count(cmd_name));
            }
            limit = Some(parse_limit_args(
                cmd_name,
                &args[i + 1],
                &args[i + 2],
            )?);
            i += 3;
        } else {
            return Err(wrong_count(cmd_name));
        }
    }
    let res = txn
        .with_zset(storage, &args[0], |zset| {
            let slice = filter_and_slice_zset(zset, &min, &max, reverse, limit);
            Ok(members_to_resp(&slice, withscores))
        })
        .await?;
    Ok(res.unwrap_or_else(|| RespValue::Array(Some(Vec::new()))))
}

async fn revrange_top(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    key: &[u8],
    with_scores: bool,
) -> Result<Option<RespValue>> {
    if txn.zset_delta_count(key).is_some() {
        return Ok(None);
    }
    let stored = txn.get_value(storage, key).await?;
    if stored.as_ref().and_then(zset_subkey::is_zset_header).is_none() {
        return Ok(None);
    }
    let encoded = AiDbEngine::encode_key(txn.db_index, key);
    let prefix = score_prefix(&encoded);
    let members = zset_subkey::member_prefix(&encoded);
    if txn.in_flight.as_ref().is_some_and(|buf| {
        buf.has_delete_with_prefix(&prefix) || buf.has_delete_with_prefix(&members)
    }) {
        return Ok(None);
    }
    let inflight = txn
        .in_flight
        .as_ref()
        .and_then(|buf| buf.last_key_with_prefix(&prefix));
    let stored_last = match storage.raw_prefix_last(prefix.clone()).await {
        Ok(pair) => pair.map(|(engine_key, _)| engine_key),
        Err(_) => return Ok(None),
    };
    let best = match (inflight, stored_last) {
        (Some(left), Some(right)) if left > right => Some(left),
        (Some(_), Some(right)) => Some(right),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };
    let Some(best) = best else {
        return Ok(Some(RespValue::Array(Some(Vec::new()))));
    };
    let Some((member, score)) = score_index_entry(&prefix, &best) else {
        return Ok(None);
    };
    Ok(Some(members_to_resp(&[(member, score)], with_scores)))
}

fn filter_and_slice_zset(
    zset: &BTreeMap<Vec<u8>, f64>,
    min: &ScoreBound,
    max: &ScoreBound,
    reverse: bool,
    limit: Option<(usize, Option<usize>)>,
) -> Vec<(Vec<u8>, f64)> {
    let mut filtered: Vec<(&Vec<u8>, f64)> = zset
        .iter()
        .filter_map(|(k, s)| {
            if score_in_range(*s, min, max) {
                Some((k, *s))
            } else {
                None
            }
        })
        .collect();
    if reverse {
        filtered.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.cmp(a.0))
        });
    } else {
        filtered.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(b.0))
        });
    }
    let (start, end) = match limit {
        Some((offset, Some(count))) => {
            let s = offset.min(filtered.len());
            let e = (offset + count).min(filtered.len());
            (s, e)
        }
        Some((offset, None)) => (offset.min(filtered.len()), filtered.len()),
        None => (0, filtered.len()),
    };
    filtered[start..end]
        .iter()
        .map(|(k, s)| ((*k).clone(), *s))
        .collect()
}

pub(super) async fn exec_zcard(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("ZCARD"));
    }
    if let Some(count) = txn.zset_delta_count(&args[0]) {
        return Ok(router::integer(count as i64));
    }
    let stored = txn.get_value(storage, &args[0]).await?;
    if let Some(count) = stored.as_ref().and_then(crate::storage::zset_subkey::is_zset_header) {
        return Ok(router::integer(count as i64));
    }
    let len = txn
        .with_zset(storage, &args[0], |zset| Ok(zset.len() as i64))
        .await?
        .unwrap_or(0);
    Ok(router::integer(len))
}
