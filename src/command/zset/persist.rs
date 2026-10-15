//! ZSET 写入: 不超过 inline 上限时整包覆盖, 超过后只提交变动的 member 和 score 索引.

use std::collections::{BTreeMap, HashMap};

use crate::error::Result;
use crate::storage::types::{StorageEngineKind, WriteOp};
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
    let stored = storage.get_typed(db, key).await?;
    if let Some(stored) = stored.as_ref() {
        if zset_subkey::is_zset_header(stored).is_some() {
            return add_to_header(storage, db, key, stored, pairs).await;
        }
    }
    let expires = stored.as_ref().and_then(|s| s.expires_at);
    let mut map = match stored {
        None => BTreeMap::new(),
        Some(stored) => stored.as_zset()?.clone(),
    };
    let added = insert_pairs(&mut map, pairs);
    store_map(storage, db, key, map, expires).await?;
    Ok(added)
}

pub async fn remove_members(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    members: &[Vec<u8>],
) -> Result<i64> {
    let Some(stored) = storage.get_typed(db, key).await? else {
        return Ok(0);
    };
    if zset_subkey::is_zset_header(&stored).is_some() {
        return remove_from_header(storage, db, key, &stored, members).await;
    }
    let expires = stored.expires_at;
    let mut map = stored.as_zset()?.clone();
    let removed = remove_pairs(&mut map, members);
    if map.is_empty() {
        storage.delete(db, key).await?;
    } else {
        store_map(storage, db, key, map, expires).await?;
    }
    Ok(removed)
}

async fn add_to_header(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    stored: &crate::storage::StoredValue,
    pairs: &[(f64, Vec<u8>)],
) -> Result<i64> {
    let base = zset_subkey::is_zset_header(stored).unwrap_or(0);
    let (mut ops, added, count) = header_add_ops(storage, db, key, base, pairs).await?;
    ops.push((
        key.to_vec(),
        WriteOp::PutStored(zset_subkey::header(count, stored.expires_at)),
    ));
    storage.write_batch(db, ops).await?;
    Ok(added)
}

async fn header_add_ops(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    mut count: u32,
    pairs: &[(f64, Vec<u8>)],
) -> Result<(Vec<(Vec<u8>, WriteOp)>, i64, u32)> {
    let mut ops = Vec::new();
    let mut overlay: HashMap<Vec<u8>, f64> = HashMap::new();
    let mut added = 0i64;
    for (score, member) in pairs {
        let old = match overlay.get(member) {
            Some(score) => Some(*score),
            None => zset_subkey::read_member_score(storage, db, key, member).await?,
        };
        if old.is_none() {
            added += 1;
            count = count.saturating_add(1);
        }
        overlay.insert(member.clone(), *score);
        if old == Some(*score) {
            continue;
        }
        ops.extend(zset_subkey::member_change_ops(
            db,
            key,
            member,
            old,
            Some(*score),
        ));
    }
    Ok((ops, added, count))
}

async fn remove_from_header(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    stored: &crate::storage::StoredValue,
    members: &[Vec<u8>],
) -> Result<i64> {
    let mut count = zset_subkey::is_zset_header(stored).unwrap_or(0);
    let mut ops = Vec::new();
    let mut removed = 0i64;
    let mut seen = HashMap::<Vec<u8>, ()>::new();
    for member in members {
        if seen.insert(member.clone(), ()).is_some() {
            continue;
        }
        let old = zset_subkey::read_member_score(storage, db, key, member).await?;
        if old.is_none() {
            continue;
        }
        removed += 1;
        count = count.saturating_sub(1);
        ops.extend(zset_subkey::member_change_ops(db, key, member, old, None));
    }
    if removed == 0 {
        return Ok(0);
    }
    push_header_or_delete(&mut ops, key, count, stored.expires_at);
    storage.write_batch(db, ops).await?;
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
        WriteOp::PutStored(zset_subkey::header(count, expires_at)),
    ));
}

async fn store_map(
    storage: &dyn KvStorage,
    db: usize,
    key: &[u8],
    map: BTreeMap<Vec<u8>, f64>,
    expires_at: Option<u64>,
) -> Result<()> {
    if storage.engine_kind() != StorageEngineKind::AiDb || map.len() <= zset_subkey::ZSET_MAX_INLINE {
        let mut stored = crate::storage::StoredValue::new_zset(map);
        stored.expires_at = expires_at;
        return storage.rewrite_stored(db, key, stored).await;
    }
    storage
        .write_batch(db, zset_subkey::replace_ops(db, key, &map, expires_at))
        .await
}

fn insert_pairs(map: &mut BTreeMap<Vec<u8>, f64>, pairs: &[(f64, Vec<u8>)]) -> i64 {
    let mut added = 0i64;
    for (score, member) in pairs {
        if !map.contains_key(member) {
            added += 1;
        }
        map.insert(member.clone(), *score);
    }
    added
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
