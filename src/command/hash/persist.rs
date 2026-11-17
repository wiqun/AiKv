//! Hash 写路径持久化: 在持有 KeyLock 期间读写在途缓冲并登记入队序，
//! 随后立即释放 KeyLock，锁外入队并等待批处理提交，消除跨 RTT 桶锁争用.

use std::collections::{HashMap, HashSet};

use crate::command::hash::{format_hash_float, parse_f64_field_bytes, parse_i64_field};
use crate::command::router;
use crate::command::shared::{commit_ops_in_lock, InFlightContext};
use crate::error::{Error, Result};
use crate::storage::subkey;
use crate::storage::{
    AiDbEngine, CollectionKind, KvStorage, StorageEngineKind, StoredValue, ValueType, WriteOp,
};

pub struct HashWriteContext<'a> {
    pub storage: &'a dyn KvStorage,
    pub in_flight: &'a InFlightContext,
    pub db: usize,
    pub key: &'a [u8],
}

async fn load_stored_in_flight(ctx: &HashWriteContext<'_>) -> Result<Option<StoredValue>> {
    match ctx.in_flight.in_flight.get(ctx.db, ctx.key) {
        Some(Some(s)) => Ok(Some(s)),
        Some(None) => Ok(None),
        None => ctx.storage.get_typed(ctx.db, ctx.key).await,
    }
}

async fn read_subkey_in_flight(
    ctx: &HashWriteContext<'_>,
    subkey_key: &[u8],
) -> Result<Option<Vec<u8>>> {
    match ctx.in_flight.in_flight.get_raw(subkey_key) {
        Some(Some(v)) => Ok(Some(v)),
        Some(None) => Ok(None),
        None => {
            ctx.storage
                .raw_subkey_get(ctx.db, subkey_key.to_vec())
                .await
        }
    }
}

async fn commit_inline_map<G>(
    ctx: &HashWriteContext<'_>,
    stored: StoredValue,
    guard: G,
) -> Result<()> {
    let is_aidb = ctx.storage.engine_kind() == StorageEngineKind::AiDb;
    if is_aidb {
        if let ValueType::Hash(ref map) = stored.value {
            if map.len() > subkey::hash_max_inline_fields() {
                let ops = subkey::replace_hash_ops(ctx.db, ctx.key, map, stored.expires_at);
                return commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard)
                    .await;
            }
        }
    }
    let saw_ttl = stored.expires_at.is_some();
    let ops = vec![(ctx.key.to_vec(), WriteOp::put_stored(stored))];
    commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await?;
    if saw_ttl && !is_aidb {
        ctx.storage.release_seen_ttl(ctx.db).await?;
    }
    Ok(())
}

async fn commit_header_ops<G>(
    ctx: &HashWriteContext<'_>,
    mut ops: Vec<(Vec<u8>, WriteOp)>,
    new_count: u32,
    expires_at: Option<u64>,
    update_header: bool,
    guard: G,
) -> Result<()> {
    if new_count == 0 {
        ops.push((ctx.key.to_vec(), WriteOp::Delete));
    } else if update_header {
        ops.push((
            ctx.key.to_vec(),
            WriteOp::put_stored(StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: new_count,
                },
                expires_at,
            }),
        ));
    }
    commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await
}

fn empty_inline_hash() -> StoredValue {
    StoredValue {
        value: ValueType::Hash(HashMap::new()),
        expires_at: None,
    }
}

pub async fn hset_pipeline<G>(
    ctx: &HashWriteContext<'_>,
    pairs: &[(&[u8], &[u8])],
    guard: G,
) -> Result<i64> {
    let mut stored = load_stored_in_flight(ctx)
        .await?
        .unwrap_or_else(empty_inline_hash);

    match &mut stored.value {
        ValueType::Hash(ref mut map) => {
            let mut new_fields = 0i64;
            let mut changed = false;
            for (field, value) in pairs {
                if let Some(existing) = map.get_mut(*field) {
                    if existing.as_slice() != *value {
                        existing.clear();
                        existing.extend_from_slice(value);
                        changed = true;
                    }
                } else {
                    new_fields += 1;
                    changed = true;
                    map.insert(field.to_vec(), value.to_vec());
                }
            }
            if !changed {
                drop(guard);
                return Ok(0);
            }
            commit_inline_map(ctx, stored, guard).await?;
            Ok(new_fields)
        }
        ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count,
        } => {
            let base_count = *count;
            let expires_at = stored.expires_at;
            hset_subkey_header(ctx, base_count, expires_at, pairs, guard).await
        }
        _ => {
            drop(guard);
            Err(router::wrongtype())
        }
    }
}

async fn hset_subkey_header<G>(
    ctx: &HashWriteContext<'_>,
    base_count: u32,
    expires_at: Option<u64>,
    pairs: &[(&[u8], &[u8])],
    guard: G,
) -> Result<i64> {
    type SubkeyPendingMap = HashMap<Vec<u8>, (Option<Vec<u8>>, Vec<u8>)>;
    let encoded_user = AiDbEngine::encode_key(ctx.db, ctx.key);
    // 记录 subkey -> (原有值, 最终待写值), 仅写入新增或值发生变化的 field.
    let mut overlay: SubkeyPendingMap = HashMap::with_capacity(pairs.len());
    let mut new_fields = 0i64;
    for (field, value) in pairs {
        let subkey_key = subkey::encode_hash_field_key(&encoded_user, field);
        if let Some(entry) = overlay.get_mut(&subkey_key) {
            entry.1.clear();
            entry.1.extend_from_slice(value);
        } else {
            let prev = read_subkey_in_flight(ctx, &subkey_key).await?;
            if prev.is_none() {
                new_fields += 1;
            }
            overlay.insert(subkey_key, (prev, value.to_vec()));
        }
    }
    let ops: Vec<(Vec<u8>, WriteOp)> = overlay
        .into_iter()
        .filter(|(_, (prev, new_val))| prev.as_deref() != Some(new_val.as_slice()))
        .map(|(k, (_, new_val))| (k, WriteOp::put_engine(new_val)))
        .collect();
    if ops.is_empty() {
        drop(guard);
        return Ok(0);
    }
    let new_count = base_count.saturating_add(new_fields as u32);
    commit_header_ops(ctx, ops, new_count, expires_at, new_fields > 0, guard).await?;
    Ok(new_fields)
}

pub async fn hdel_pipeline<G>(
    ctx: &HashWriteContext<'_>,
    fields: &[&[u8]],
    guard: G,
) -> Result<i64> {
    let Some(mut stored) = load_stored_in_flight(ctx).await? else {
        drop(guard);
        return Ok(0);
    };

    match &mut stored.value {
        ValueType::Hash(ref mut map) => {
            let mut removed = 0i64;
            for field in fields {
                if map.remove(*field).is_some() {
                    removed += 1;
                }
            }
            if removed == 0 {
                drop(guard);
                return Ok(0);
            }
            if map.is_empty() {
                let ops = vec![(ctx.key.to_vec(), WriteOp::Delete)];
                commit_ops_in_lock(ctx.storage, ctx.in_flight, ctx.db, ctx.key, ops, guard).await?;
            } else {
                commit_inline_map(ctx, stored, guard).await?;
            }
            Ok(removed)
        }
        ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count,
        } => {
            let base_count = *count;
            let expires_at = stored.expires_at;
            hdel_subkey_header(ctx, base_count, expires_at, fields, guard).await
        }
        _ => {
            drop(guard);
            Err(router::wrongtype())
        }
    }
}

async fn hdel_subkey_header<G>(
    ctx: &HashWriteContext<'_>,
    base_count: u32,
    expires_at: Option<u64>,
    fields: &[&[u8]],
    guard: G,
) -> Result<i64> {
    let encoded_user = AiDbEngine::encode_key(ctx.db, ctx.key);
    let mut seen: HashSet<Vec<u8>> = HashSet::with_capacity(fields.len());
    let mut ops = Vec::with_capacity(fields.len() + 1);
    let mut removed = 0i64;
    for field in fields {
        let subkey_key = subkey::encode_hash_field_key(&encoded_user, field);
        if !seen.insert(subkey_key.clone()) {
            continue;
        }
        if read_subkey_in_flight(ctx, &subkey_key).await?.is_some() {
            removed += 1;
            ops.push((subkey_key, WriteOp::DeleteEngine));
        }
    }
    if removed == 0 {
        drop(guard);
        return Ok(0);
    }
    let new_count = base_count.saturating_sub(removed as u32);
    commit_header_ops(ctx, ops, new_count, expires_at, true, guard).await?;
    Ok(removed)
}

pub async fn hsetnx_pipeline<G>(
    ctx: &HashWriteContext<'_>,
    field: &[u8],
    value: &[u8],
    guard: G,
) -> Result<i64> {
    let mut stored = load_stored_in_flight(ctx)
        .await?
        .unwrap_or_else(empty_inline_hash);

    match &mut stored.value {
        ValueType::Hash(ref mut map) => {
            if map.contains_key(field) {
                drop(guard);
                return Ok(0);
            }
            map.insert(field.to_vec(), value.to_vec());
            commit_inline_map(ctx, stored, guard).await?;
            Ok(1)
        }
        ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count,
        } => {
            let base_count = *count;
            let expires_at = stored.expires_at;
            let encoded_user = AiDbEngine::encode_key(ctx.db, ctx.key);
            let subkey_key = subkey::encode_hash_field_key(&encoded_user, field);
            if read_subkey_in_flight(ctx, &subkey_key).await?.is_some() {
                drop(guard);
                return Ok(0);
            }
            let ops = vec![(subkey_key, WriteOp::put_engine(value.to_vec()))];
            commit_header_ops(ctx, ops, base_count + 1, expires_at, true, guard).await?;
            Ok(1)
        }
        _ => {
            drop(guard);
            Err(router::wrongtype())
        }
    }
}

pub async fn hincrby_pipeline<G>(
    ctx: &HashWriteContext<'_>,
    field: &[u8],
    delta: i64,
    guard: G,
) -> Result<i64> {
    let mut stored = load_stored_in_flight(ctx)
        .await?
        .unwrap_or_else(empty_inline_hash);

    match &mut stored.value {
        ValueType::Hash(ref mut map) => {
            let new_val = match map.get(field) {
                None => delta,
                Some(existing) => {
                    parse_i64_field(existing)?
                        .checked_add(delta)
                        .ok_or_else(|| {
                            Error::Command("ERR increment or decrement would overflow".into())
                        })?
                }
            };
            map.insert(field.to_vec(), new_val.to_string().into_bytes());
            commit_inline_map(ctx, stored, guard).await?;
            Ok(new_val)
        }
        ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count,
        } => {
            let base_count = *count;
            let expires_at = stored.expires_at;
            let encoded_user = AiDbEngine::encode_key(ctx.db, ctx.key);
            let subkey_key = subkey::encode_hash_field_key(&encoded_user, field);
            let current = read_subkey_in_flight(ctx, &subkey_key).await?;
            let existed = current.is_some();
            let new_val = match current {
                None => delta,
                Some(existing) => {
                    parse_i64_field(&existing)?
                        .checked_add(delta)
                        .ok_or_else(|| {
                            Error::Command("ERR increment or decrement would overflow".into())
                        })?
                }
            };
            let ops = vec![(
                subkey_key,
                WriteOp::put_engine(new_val.to_string().into_bytes()),
            )];
            let new_count = base_count + u32::from(!existed);
            commit_header_ops(ctx, ops, new_count, expires_at, !existed, guard).await?;
            Ok(new_val)
        }
        _ => {
            drop(guard);
            Err(router::wrongtype())
        }
    }
}

pub async fn hincrbyfloat_pipeline<G>(
    ctx: &HashWriteContext<'_>,
    field: &[u8],
    delta: f64,
    guard: G,
) -> Result<Vec<u8>> {
    let mut stored = load_stored_in_flight(ctx)
        .await?
        .unwrap_or_else(empty_inline_hash);

    match &mut stored.value {
        ValueType::Hash(ref mut map) => {
            let current = match map.get(field) {
                None => 0.0f64,
                Some(existing) => parse_f64_field_bytes(existing)?,
            };
            let new_val = current + delta;
            if !new_val.is_finite() {
                return Err(Error::Command(
                    "ERR increment would produce NaN or Infinity".into(),
                ));
            }
            let bytes = format_hash_float(new_val).into_bytes();
            map.insert(field.to_vec(), bytes.clone());
            commit_inline_map(ctx, stored, guard).await?;
            Ok(bytes)
        }
        ValueType::CollectionHeader {
            kind: CollectionKind::Hash,
            count,
        } => {
            let base_count = *count;
            let expires_at = stored.expires_at;
            let encoded_user = AiDbEngine::encode_key(ctx.db, ctx.key);
            let subkey_key = subkey::encode_hash_field_key(&encoded_user, field);
            let current_raw = read_subkey_in_flight(ctx, &subkey_key).await?;
            let existed = current_raw.is_some();
            let current = match current_raw {
                None => 0.0f64,
                Some(existing) => parse_f64_field_bytes(&existing)?,
            };
            let new_val = current + delta;
            if !new_val.is_finite() {
                return Err(Error::Command(
                    "ERR increment would produce NaN or Infinity".into(),
                ));
            }
            let bytes = format_hash_float(new_val).into_bytes();
            let ops = vec![(subkey_key, WriteOp::put_engine(bytes.clone()))];
            let new_count = base_count + u32::from(!existed);
            commit_header_ops(ctx, ops, new_count, expires_at, !existed, guard).await?;
            Ok(bytes)
        }
        _ => {
            drop(guard);
            Err(router::wrongtype())
        }
    }
}
