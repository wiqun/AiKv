//! ZSET 写入: 不超过 inline 上限时整包覆盖, 超过后只提交变动的 member 和 score 索引.

use std::collections::{BTreeMap, HashMap};

use crate::command::shared::{commit_ops_in_lock, InFlightContext};
use crate::command::zset::helpers::{apply_zadd_flags, ZAddFlags, ZAddOutcome};
use crate::error::Result;
use crate::storage::aidb::AiDbEngine;
use crate::storage::types::{StorageEngineKind, StoredValue, WriteOp};
use crate::storage::{zset_subkey, KvStorage, ValueType};

pub async fn member_score(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    member: &[u8],
) -> Result<Option<f64>> {
    let Some(stored) = storage.get_typed(db, key).await? else {
        return Ok(None);
    };
    if zset_subkey::is_zset_header(&stored).is_some() {
        return zset_subkey::read_member_score(storage, db, key, member).await;
    }
    Ok(stored.as_zset()?.get(member).copied())
}

pub async fn load_map(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
) -> Result<Option<BTreeMap<Vec<u8>, f64>>> {
    let Some(stored) = storage.get_typed(db, key).await? else {
        return Ok(None);
    };
    if zset_subkey::is_zset_header(&stored).is_some() {
        return Ok(Some(zset_subkey::load_members(storage, db, key).await?));
    }
    Ok(Some(stored.as_zset()?.clone()))
}

pub async fn add_members(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    pairs: &[(f64, Vec<u8>)],
) -> Result<i64> {
    match add_members_with_flags(storage, db, key, pairs, ZAddFlags::default()).await? {
        ZAddOutcome::Count(n) => Ok(n),
        ZAddOutcome::Score(_) => Ok(0),
    }
}

/// ZADD 读改写流水线执行上下文, 整合多参数避免函数签名膨胀.
pub struct ZAddContext<'a> {
    pub storage: &'a dyn KvStorage,
    pub in_flight: &'a InFlightContext,
    pub db: usize,
    pub key: &'a [u8],
}

pub async fn add_members_with_flags(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    pairs: &[(f64, Vec<u8>)],
    flags: ZAddFlags,
) -> Result<ZAddOutcome> {
    let in_flight = InFlightContext::new();
    let ctx = ZAddContext {
        storage,
        in_flight: &in_flight,
        db,
        key,
    };
    add_members_pipeline(&ctx, pairs, flags, ()).await
}

pub async fn add_members_pipeline<G>(
    ctx: &ZAddContext<'_>,
    pairs: &[(f64, Vec<u8>)],
    flags: ZAddFlags,
    guard: G,
) -> Result<ZAddOutcome> {
    let stored = match ctx.in_flight.in_flight.get(ctx.db, ctx.key) {
        Some(Some(s)) => Some(s),
        Some(None) => None,
        None => ctx.storage.get_typed(ctx.db, ctx.key).await?,
    };
    if let Some(stored) = stored.as_ref() {
        if zset_subkey::is_zset_header(stored).is_some() {
            return add_to_header(ctx, stored, pairs, flags, guard).await;
        }
    }
    let expires = stored.as_ref().and_then(|s| s.expires_at);
    let mut map = match stored {
        None => BTreeMap::new(),
        Some(stored) => stored.as_zset()?.clone(),
    };
    let outcome = apply_zadd_flags(&mut map, pairs, flags)?;
    let ops = if ctx.storage.engine_kind() == StorageEngineKind::AiDb
        && map.len() > zset_subkey::ZSET_MAX_INLINE
    {
        zset_subkey::replace_ops(ctx.db, ctx.key, &map, expires)
    } else {
        let mut stored = StoredValue::new_zset(map);
        stored.expires_at = expires;
        vec![(ctx.key.to_vec(), WriteOp::put_stored(stored))]
    };
    commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await?;
    Ok(outcome)
}

pub async fn remove_members(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    members: &[Vec<u8>],
) -> Result<i64> {
    let in_flight = InFlightContext::new();
    let ctx = ZAddContext {
        storage,
        in_flight: &in_flight,
        db,
        key,
    };
    remove_members_pipeline(&ctx, members, ()).await
}

pub async fn remove_members_pipeline<G>(
    ctx: &ZAddContext<'_>,
    members: &[Vec<u8>],
    guard: G,
) -> Result<i64> {
    let stored = match ctx.in_flight.in_flight.get(ctx.db, ctx.key) {
        Some(Some(s)) => Some(s),
        Some(None) => None,
        None => ctx.storage.get_typed(ctx.db, ctx.key).await?,
    };
    let Some(stored) = stored else {
        drop(guard);
        return Ok(0);
    };
    if zset_subkey::is_zset_header(&stored).is_some() {
        return remove_from_header_pipeline(ctx, &stored, members, guard).await;
    }
    let expires = stored.expires_at;
    let mut map = stored.as_zset()?.clone();
    let removed = remove_pairs(&mut map, members);
    if removed == 0 {
        drop(guard);
        return Ok(0);
    }
    let ops = if map.is_empty() {
        vec![(ctx.key.to_vec(), WriteOp::Delete)]
    } else {
        let mut stored = StoredValue::new_zset(map);
        stored.expires_at = expires;
        vec![(ctx.key.to_vec(), WriteOp::put_stored(stored))]
    };
    commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await?;
    Ok(removed)
}

async fn add_to_header<G>(
    ctx: &ZAddContext<'_>,
    stored: &crate::storage::StoredValue,
    pairs: &[(f64, Vec<u8>)],
    flags: ZAddFlags,
    guard: G,
) -> Result<ZAddOutcome> {
    let base = zset_subkey::is_zset_header(stored).unwrap_or(0);
    let (mut ops, outcome, count) = header_add_ops(ctx, base, pairs, flags).await?;
    if !ops.is_empty() {
        ops.push((
            ctx.key.to_vec(),
            WriteOp::put_stored(zset_subkey::header(count, stored.expires_at)),
        ));
    }
    commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await?;
    Ok(outcome)
}

async fn read_member_score_in_flight(
    ctx: &ZAddContext<'_>,
    encoded_user: &[u8],
    member: &[u8],
) -> Result<Option<f64>> {
    let member_engine = zset_subkey::member_key(encoded_user, member);
    match ctx.in_flight.in_flight.get_raw(&member_engine) {
        Some(Some(raw)) => Ok(zset_subkey::decode_score(&raw)),
        Some(None) => Ok(None),
        None => zset_subkey::read_member_score(ctx.storage, ctx.db, ctx.key, member).await,
    }
}

async fn header_add_ops(
    ctx: &ZAddContext<'_>,
    count: u32,
    pairs: &[(f64, Vec<u8>)],
    flags: ZAddFlags,
) -> Result<(Vec<(Vec<u8>, WriteOp)>, ZAddOutcome, u32)> {
    let encoded = AiDbEngine::encode_key(ctx.db, ctx.key);
    if flags.incr {
        return header_incr_op(ctx, &encoded, count, &pairs[0], flags).await;
    }
    header_batch_ops(ctx, &encoded, count, pairs, flags).await
}

async fn header_incr_op(
    ctx: &ZAddContext<'_>,
    encoded: &[u8],
    mut count: u32,
    pair: &(f64, Vec<u8>),
    flags: ZAddFlags,
) -> Result<(Vec<(Vec<u8>, WriteOp)>, ZAddOutcome, u32)> {
    let (incr_score, member) = pair;
    let old = read_member_score_in_flight(ctx, encoded, member).await?;
    if old.is_none() && flags.xx {
        return Ok((Vec::new(), ZAddOutcome::Score(None), count));
    }
    if old.is_some() && flags.nx {
        return Ok((Vec::new(), ZAddOutcome::Score(None), count));
    }
    let current = old.unwrap_or(0.0);
    let new_score = current + incr_score;
    if new_score.is_nan() {
        return Err(crate::error::Error::Command(
            "ERR resulting score is not a number (NaN)".into(),
        ));
    }
    if let Some(old_val) = old {
        if flags.gt && new_score <= old_val {
            return Ok((Vec::new(), ZAddOutcome::Score(None), count));
        }
        if flags.lt && new_score >= old_val {
            return Ok((Vec::new(), ZAddOutcome::Score(None), count));
        }
    } else {
        count = count.saturating_add(1);
    }
    let ops = zset_subkey::member_change_ops(ctx.db, ctx.key, member, old, Some(new_score));
    Ok((ops, ZAddOutcome::Score(Some(new_score)), count))
}

async fn header_batch_ops(
    ctx: &ZAddContext<'_>,
    encoded: &[u8],
    mut count: u32,
    pairs: &[(f64, Vec<u8>)],
    flags: ZAddFlags,
) -> Result<(Vec<(Vec<u8>, WriteOp)>, ZAddOutcome, u32)> {
    let mut ops = Vec::new();
    let mut overlay: HashMap<Vec<u8>, f64> = HashMap::new();
    let mut added = 0i64;
    let mut changed = 0i64;

    for (score, member) in pairs {
        let old = match overlay.get(member) {
            Some(score) => Some(*score),
            None => read_member_score_in_flight(ctx, encoded, member).await?,
        };
        match old {
            None => {
                if flags.xx {
                    continue;
                }
                added += 1;
                changed += 1;
                count = count.saturating_add(1);
                overlay.insert(member.clone(), *score);
                ops.extend(zset_subkey::member_change_ops(
                    ctx.db,
                    ctx.key,
                    member,
                    None,
                    Some(*score),
                ));
            }
            Some(old_score) => {
                if flags.nx {
                    continue;
                }
                if flags.gt && *score <= old_score {
                    continue;
                }
                if flags.lt && *score >= old_score {
                    continue;
                }
                if *score != old_score {
                    changed += 1;
                }
                overlay.insert(member.clone(), *score);
                if old_score == *score {
                    continue;
                }
                ops.extend(zset_subkey::member_change_ops(
                    ctx.db,
                    ctx.key,
                    member,
                    Some(old_score),
                    Some(*score),
                ));
            }
        }
    }

    let outcome = if flags.ch {
        ZAddOutcome::Count(changed)
    } else {
        ZAddOutcome::Count(added)
    };
    Ok((ops, outcome, count))
}

async fn remove_from_header_pipeline<G>(
    ctx: &ZAddContext<'_>,
    stored: &crate::storage::StoredValue,
    members: &[Vec<u8>],
    guard: G,
) -> Result<i64> {
    let mut count = zset_subkey::is_zset_header(stored).unwrap_or(0);
    let mut ops = Vec::new();
    let mut removed = 0i64;
    let mut seen = HashMap::<Vec<u8>, ()>::new();
    let encoded = AiDbEngine::encode_key(ctx.db, ctx.key);
    for member in members {
        if seen.insert(member.clone(), ()).is_some() {
            continue;
        }
        let old = read_member_score_in_flight(ctx, &encoded, member).await?;
        if old.is_none() {
            continue;
        }
        removed += 1;
        count = count.saturating_sub(1);
        ops.extend(zset_subkey::member_change_ops(
            ctx.db, ctx.key, member, old, None,
        ));
    }
    if removed == 0 {
        drop(guard);
        return Ok(0);
    }
    push_header_or_delete(&mut ops, ctx.key, count, stored.expires_at);
    commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await?;
    if count == 0 {
        zset_subkey::invalidate_score_bounds_hint(ctx.db, ctx.key);
    }
    Ok(removed)
}

fn push_header_or_delete(
    ops: &mut Vec<(Vec<u8>, WriteOp)>,
    key: &[u8],
    count: u32,
    expires_at: Option<u64>,
) {
    if count == 0 {
        ops.push((key.to_vec(), WriteOp::Delete));
        return;
    }
    ops.push((
        key.to_vec(),
        WriteOp::put_stored(zset_subkey::header(count, expires_at)),
    ));
}

fn remove_pairs(map: &mut BTreeMap<Vec<u8>, f64>, members: &[Vec<u8>]) -> i64 {
    let mut removed = 0i64;
    for member in members {
        if map.remove(member).is_some() {
            removed += 1;
        }
    }
    removed
}

pub fn header_count(value: &ValueType) -> Option<u32> {
    match value {
        ValueType::CollectionHeader {
            kind: crate::storage::CollectionKind::ZSet,
            count,
        } => Some(*count),
        _ => None,
    }
}
