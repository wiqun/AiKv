//! Lua `redis.call` ZSet 域 `exec_*` (ZADD/ZREM/ZSCORE/ZRANK/ZRANGE/ZCARD).
//!
//! 子键布局的 ZSET 走 `transaction::zset_view`: 读取只触达所需分数区间, 写入只更新成员增量,
//! 不展开整表. 内联 ZSET 仍走 `ScriptTransaction::update_zset` / `with_zset` (成员数 ≤ 阈值, 整表在内存中).

use bytes::Bytes;

use crate::command::router;
use crate::command::script::transaction::{
    parse_f64_arg, parse_i64_arg, zset_view, ScriptTransaction,
};
use crate::command::zset::helpers::{
    bytes_to_str, members_to_resp, parse_limit_args, parse_score_bound, ScoreBound,
};
use crate::command::zset::index::ScoreIndex;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::subkey::ScoreSpan;
use crate::storage::KvStorage;

use super::wrong_count;

fn span_of(min: &ScoreBound, max: &ScoreBound) -> ScoreSpan {
    ScoreSpan::new(min.endpoint(), max.endpoint())
}

pub(super) async fn exec_zadd(
    storage: &dyn KvStorage,
    txn: &mut ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
        return Err(wrong_count("ZADD"));
    }
    let mut pairs = Vec::with_capacity((args.len() - 1) / 2);
    for i in (1..args.len()).step_by(2) {
        let score = parse_f64_arg(&args[i], "score")?;
        pairs.push((args[i + 1].to_vec(), score));
    }
    if txn.zset_sub(storage, &args[0]).await? {
        let db = txn.db_index;
        let delta = txn
            .zset_delta_mut(&args[0])
            .ok_or_else(|| Error::Command("ERR internal zset delta missing".into()))?;
        let added = zset_view::add(storage, db, &args[0], delta, &pairs).await?;
        return Ok(router::integer(added));
    }
    let added = txn
        .update_zset(storage, &args[0], true, false, |zset| {
            let mut added = 0i64;
            for (member, score) in &pairs {
                if zset.insert(member.clone(), *score).is_none() {
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
    if txn.zset_sub(storage, &args[0]).await? {
        let db = txn.db_index;
        let members: Vec<&[u8]> = args[1..].iter().map(|m| m.as_ref()).collect();
        let delta = txn
            .zset_delta_mut(&args[0])
            .ok_or_else(|| Error::Command("ERR internal zset delta missing".into()))?;
        let removed = zset_view::remove(storage, db, &args[0], delta, &members).await?;
        if delta.visible_count() == 0 {
            txn.delete(args[0].to_vec());
        }
        return Ok(router::integer(removed));
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
    let score = if let Some(delta) = txn.zset_snapshot(storage, &args[0]).await? {
        zset_view::score_of(storage, txn.db_index, &args[0], &delta, args[1].as_ref()).await?
    } else {
        txn.with_zset(storage, &args[0], |zset| {
            Ok(zset.get(args[1].as_ref()).copied())
        })
        .await?
        .flatten()
    };
    Ok(match score {
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
    let rank = if let Some(delta) = txn.zset_snapshot(storage, &args[0]).await? {
        zset_view::rank_of(
            storage,
            txn.db_index,
            &args[0],
            &delta,
            args[1].as_ref(),
            false,
        )
        .await?
    } else {
        txn.with_zset(storage, &args[0], |zset| {
            Ok(ScoreIndex::from_members(zset).rank(args[1].as_ref(), false))
        })
        .await?
        .flatten()
    };
    Ok(match rank {
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

/// 按名次取窗口: `start`/`stop` 与 ZRANGE 相同 (负数从尾部计, 闭区间).
fn rank_window(len: usize, start: i64, stop: i64) -> (usize, usize) {
    let len = len as i64;
    let start_idx = (if start < 0 { len + start } else { start }).clamp(0, len) as usize;
    let stop_idx = (if stop < 0 { len + stop + 1 } else { stop + 1 }).clamp(0, len) as usize;
    (start_idx, stop_idx.max(start_idx))
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
    let window: Vec<(Vec<u8>, f64)> = if let Some(delta) =
        txn.zset_snapshot(storage, &args[0]).await?
    {
        let (start_idx, stop_idx) = rank_window(delta.visible_count() as usize, start, stop);
        zset_view::range(
            storage,
            txn.db_index,
            &args[0],
            &delta,
            &ScoreSpan::FULL,
            reverse,
            start_idx,
            Some(stop_idx - start_idx),
        )
        .await?
    } else {
        txn.with_zset(storage, &args[0], |zset| {
            let (start_idx, stop_idx) = rank_window(zset.len(), start, stop);
            Ok(ScoreIndex::from_members(zset).collect_rank_window(start_idx, stop_idx, reverse))
        })
        .await?
        .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (member, score) in window {
        out.push(router::bulk(member));
        if with_scores {
            out.push(router::bulk(score.to_string().into_bytes()));
        }
    }
    Ok(RespValue::Array(Some(out)))
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
    let (offset, count) = limit.unwrap_or((0, None));
    let sorted = if let Some(delta) = txn.zset_snapshot(storage, &args[0]).await? {
        zset_view::range(
            storage,
            txn.db_index,
            &args[0],
            &delta,
            &span_of(&min, &max),
            reverse,
            offset,
            count,
        )
        .await?
    } else {
        txn.with_zset(storage, &args[0], |zset| {
            Ok(ScoreIndex::from_members(zset)
                .collect_score_window(&min, &max, reverse, offset, count))
        })
        .await?
        .unwrap_or_default()
    };
    Ok(members_to_resp(&sorted, with_scores))
}

pub(super) async fn exec_zcard(
    storage: &dyn KvStorage,
    txn: &ScriptTransaction,
    args: &[Bytes],
) -> Result<RespValue> {
    if args.len() != 1 {
        return Err(wrong_count("ZCARD"));
    }
    let n = if let Some(delta) = txn.zset_snapshot(storage, &args[0]).await? {
        delta.visible_count() as i64
    } else {
        txn.with_zset(storage, &args[0], |zset| Ok(zset.len()))
            .await?
            .unwrap_or(0) as i64
    };
    Ok(router::integer(n))
}
