//! Lua `redis.call` ZSet 域 `exec_*` (ZADD/ZREM/ZSCORE/ZRANK/ZRANGE/ZCARD).
//! 写操作通过 `ScriptTransaction::update_zset` 原地修改缓冲，避免每次调用克隆整表。

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{parse_f64_arg, parse_i64_arg, ScriptTransaction};
use crate::command::zset::helpers::{
    bytes_to_str, members_to_resp, parse_limit_args, parse_score_bound,
};
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::KvStorage;

use super::wrong_count;

pub(super) async fn exec_zadd(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
        return Err(wrong_count("ZADD"));
    }
    let added = txn
        .update_zset(storage, &args[0], true, false, |zset| {
            let mut added = 0i64;
            for i in (1..args.len()).step_by(2) {
                let score = parse_f64_arg(&args[i], "score")?;
                if zset.insert(args[i + 1].to_vec(), score).is_none() {
                    added += 1;
                }
            }
            Ok(added)
        })
        .await?;
    Ok(router::integer(added.unwrap_or(0)))
}

pub(super) async fn exec_zrem(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 2 {
        return Err(wrong_count("ZREM"));
    }
    let count = txn
        .update_zset(storage, &args[0], false, true, |zset| {
            let mut count = 0i64;
            for m in &args[1..] {
                if zset.remove(m.as_ref()).is_some() {
                    count += 1;
                }
            }
            Ok(count)
        })
        .await?;
    Ok(router::integer(count.unwrap_or(0)))
}

pub(super) async fn exec_zscore(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("ZSCORE"));
    }
    let found = txn
        .with_zset(storage, &args[0], |zset| {
            Ok(zset.get(args[1].as_ref()).copied())
        })
        .await?;
    Ok(match found.flatten() {
        Some(score) => router::bulk(score.to_string().into_bytes()),
        None => RespValue::Null,
    })
}

pub(super) async fn exec_zrank(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 2 {
        return Err(wrong_count("ZRANK"));
    }
    let rank = txn
        .with_score_index(storage, &args[0], |index| {
            Ok(index.rank(args[1].as_ref(), false))
        })
        .await?;
    Ok(match rank.flatten() {
        Some(rank) => router::integer(rank as i64),
        None => RespValue::Null,
    })
}

pub(super) async fn exec_zrange(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    range_by_rank(storage, txn, args, false).await
}

pub(super) async fn exec_zrevrange(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    range_by_rank(storage, txn, args, true).await
}

async fn range_by_rank(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
    reverse: bool,
) -> Result<RespValue> {
    let cmd = if reverse { "ZREVRANGE" } else { "ZRANGE" };
    if args.len() < 3 {
        return Err(wrong_count(cmd));
    }
    let start = parse_i64_arg(&args[1], "start")?;
    let stop = parse_i64_arg(&args[2], "stop")?;
    let with_scores = args.len() > 3 && args[3].eq_ignore_ascii_case(b"WITHSCORES");
    let page = txn
        .with_score_index(storage, &args[0], |index| {
            let len = index.len() as i64;
            let start_idx = (if start < 0 { len + start } else { start }).clamp(0, len) as usize;
            let stop_idx =
                (if stop < 0 { len + stop + 1 } else { stop + 1 }).clamp(0, len) as usize;
            let window = index.collect_rank_window(start_idx, stop_idx, reverse);
            let mut out = Vec::new();
            for (member, score) in window {
                out.push(router::bulk(member));
                if with_scores {
                    out.push(router::bulk(score.to_string().into_bytes()));
                }
            }
            Ok(out)
        })
        .await?;
    Ok(RespValue::Array(Some(page.unwrap_or_default())))
}

pub(super) async fn exec_zrangebyscore(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    range_by_score(storage, txn, args, false).await
}

pub(super) async fn exec_zrevrangebyscore(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    range_by_score(storage, txn, args, true).await
}

async fn range_by_score(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
    reverse: bool,
) -> Result<RespValue> {
    let cmd = if reverse {
        "ZREVRANGEBYSCORE"
    } else {
        "ZRANGEBYSCORE"
    };
    if args.len() < 3 {
        return Err(wrong_count(cmd));
    }
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
    let mut with_scores = false;
    let mut limit = None;
    let mut i = 3;
    while i < args.len() {
        if args[i].eq_ignore_ascii_case(b"WITHSCORES") {
            with_scores = true;
            i += 1;
        } else if args[i].eq_ignore_ascii_case(b"LIMIT") {
            if i + 2 >= args.len() {
                return Err(router::wrong_args(cmd, ""));
            }
            limit = Some(parse_limit_args(cmd, &args[i + 1], &args[i + 2])?);
            i += 3;
        } else {
            return Err(router::wrong_args(cmd, ""));
        }
    }
    let sorted = txn
        .with_score_index(storage, &args[0], |index| {
            let (offset, count) = limit.unwrap_or((0, None));
            Ok(index.collect_score_window(&min, &max, reverse, offset, count))
        })
        .await?;
    Ok(members_to_resp(&sorted.unwrap_or_default(), with_scores))
}

pub(super) async fn exec_zcard(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("ZCARD"));
    }
    let n = txn
        .with_zset(storage, &args[0], |zset| Ok(zset.len()))
        .await?
        .unwrap_or(0);
    Ok(router::integer(n as i64))
}
