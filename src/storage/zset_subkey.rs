//! 大 ZSET 的 subkey 编码.
//!
//! 不超过 `ZSET_MAX_INLINE` 的集合仍是一整块 `ValueType::ZSet`.
//! 超过后, 用户 key 只留 `CollectionHeader`, 每个 member 和按 score 排序的索引各写一条 subkey.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::storage::aidb::AiDbEngine;
use crate::storage::types::{CollectionKind, KvStorage, StoredValue, ValueType, WriteOp};

/// 与 Hash / Set 的 inline 上限一致.
pub const ZSET_MAX_INLINE: usize = 64;

const MEMBER_TAG: u8 = b'Z';
const SCORE_TAG: u8 = b'Y';

/// 把 f64 编成可按字节序比较的 8 字节. 负数取反, 非负数翻转符号位.
pub fn unsort_score(bytes: [u8; 8]) -> f64 {
    let mut bits = u64::from_be_bytes(bytes);
    if bits & (1 << 63) == 0 {
        bits = !bits;
    } else {
        bits ^= 1 << 63;
    }
    f64::from_bits(bits)
}

pub fn sortable_score(score: f64) -> [u8; 8] {
    let mut bits = score.to_bits();
    if bits & (1 << 63) == 0 {
        bits ^= 1 << 63;
    } else {
        bits = !bits;
    }
    bits.to_be_bytes()
}

pub fn member_key(encoded_user: &[u8], member: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(encoded_user.len() + 4 + member.len());
    key.extend_from_slice(encoded_user);
    key.push(0x01);
    key.push(MEMBER_TAG);
    key.extend_from_slice(&(member.len() as u16).to_be_bytes());
    key.extend_from_slice(member);
    key
}

pub fn score_key(encoded_user: &[u8], score: f64, member: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(encoded_user.len() + 10 + member.len());
    key.extend_from_slice(encoded_user);
    key.push(0x01);
    key.push(SCORE_TAG);
    key.extend_from_slice(&sortable_score(score));
    key.extend_from_slice(member);
    key
}

pub fn member_prefix(encoded_user: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(encoded_user.len() + 2);
    prefix.extend_from_slice(encoded_user);
    prefix.push(0x01);
    prefix.push(MEMBER_TAG);
    prefix
}

pub fn score_prefix(encoded_user: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(encoded_user.len() + 2);
    prefix.extend_from_slice(encoded_user);
    prefix.push(0x01);
    prefix.push(SCORE_TAG);
    prefix
}

pub fn encode_score(score: f64) -> Vec<u8> {
    score.to_le_bytes().to_vec()
}

pub fn decode_score(raw: &[u8]) -> Option<f64> {
    let bytes: [u8; 8] = raw.try_into().ok()?;
    Some(f64::from_le_bytes(bytes))
}

/// 从 score 索引 key 取出 (score 的可排序字节之后的) member.
pub fn score_index_entry(prefix: &[u8], full: &[u8]) -> Option<(Vec<u8>, f64)> {
    if full.len() < prefix.len() + 8 || !full.starts_with(prefix) {
        return None;
    }
    let at = prefix.len();
    let bytes: [u8; 8] = full[at..at + 8].try_into().ok()?;
    Some((full[at + 8..].to_vec(), unsort_score(bytes)))
}

pub fn member_from_score_key(encoded_user: &[u8], full: &[u8]) -> Option<Vec<u8>> {
    let prefix_len = encoded_user.len() + 2 + 8;
    if full.len() < prefix_len || !full.starts_with(&score_prefix(encoded_user)) {
        return None;
    }
    Some(full[prefix_len..].to_vec())
}

pub fn header(count: u32, expires_at: Option<u64>) -> StoredValue {
    StoredValue {
        value: ValueType::CollectionHeader {
            kind: CollectionKind::ZSet,
            count,
        },
        expires_at,
    }
}

pub fn is_zset_header(stored: &StoredValue) -> Option<u32> {
    match stored.value {
        ValueType::CollectionHeader {
            kind: CollectionKind::ZSet,
            count,
        } => Some(count),
        _ => None,
    }
}

/// 用整份 map 覆盖成 subkey. 调用方负责已经不存在旧的 subkey.
pub fn replace_ops(
    db: usize,
    user_key: &[u8],
    map: &BTreeMap<Vec<u8>, f64>,
    expires_at: Option<u64>,
) -> Vec<(Vec<u8>, WriteOp)> {
    let encoded = AiDbEngine::encode_key(db, user_key);
    let mut ops = Vec::with_capacity(1 + map.len() * 2);
    ops.push((
        user_key.to_vec(),
        WriteOp::PutStored(header(map.len() as u32, expires_at)),
    ));
    for (member, score) in map {
        push_member_put(&mut ops, &encoded, member, *score);
    }
    ops
}

/// 一个 member 的分数变化. `new == None` 表示删除.
pub fn member_change_ops(
    db: usize,
    user_key: &[u8],
    member: &[u8],
    old: Option<f64>,
    new: Option<f64>,
) -> Vec<(Vec<u8>, WriteOp)> {
    let encoded = AiDbEngine::encode_key(db, user_key);
    let mut ops = Vec::with_capacity(2);
    if let Some(score) = old {
        let key = score_key(&encoded, score, member);
        ops.push((key, WriteOp::DeleteEngine));
    }
    if let Some(score) = new {
        push_member_put(&mut ops, &encoded, member, score);
    } else {
        let key = member_key(&encoded, member);
        ops.push((key, WriteOp::DeleteEngine));
    }
    ops
}

/// 小集合整包写入; 超过 inline 上限且后端支持 subkey 时改写 header + 索引.
pub fn commit_inline(
    db: usize,
    key: Vec<u8>,
    zset: &BTreeMap<Vec<u8>, f64>,
    expires_at: Option<u64>,
    persist_subkeys: bool,
) -> Vec<(Vec<u8>, WriteOp)> {
    if persist_subkeys && zset.len() > ZSET_MAX_INLINE {
        return replace_ops(db, &key, zset, expires_at);
    }
    let mut stored = StoredValue::new_zset(zset.clone());
    stored.expires_at = expires_at;
    vec![(key, WriteOp::PutStored(stored))]
}

pub async fn read_member_score(
    storage: &dyn KvStorage,
    db: usize,
    user_key: &[u8],
    member: &[u8],
) -> Result<Option<f64>> {
    let encoded = AiDbEngine::encode_key(db, user_key);
    let raw = storage
        .raw_subkey_get(db, member_key(&encoded, member))
        .await?;
    Ok(raw.as_deref().and_then(decode_score))
}

#[cfg(test)]
pub static LOAD_MEMBERS_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub async fn load_members(
    storage: &dyn KvStorage,
    db: usize,
    user_key: &[u8],
) -> Result<BTreeMap<Vec<u8>, f64>> {
    #[cfg(test)]
    LOAD_MEMBERS_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let encoded = AiDbEngine::encode_key(db, user_key);
    let prefix = member_prefix(&encoded);
    let prefix_len = prefix.len();
    let map = Arc::new(Mutex::new(BTreeMap::new()));
    let map_c = Arc::clone(&map);
    storage
        .raw_subkey_for_each(
            db,
            prefix,
            Box::new(move |enc, raw| {
                let Some(member) = member_from_encoded(&enc, prefix_len) else {
                    return Ok(());
                };
                if let Some(score) = decode_score(&raw) {
                    map_c.lock().expect("zset members").insert(member, score);
                }
                Ok(())
            }),
        )
        .await?;
    Ok(Arc::try_unwrap(map)
        .unwrap_or_else(|_| unreachable!("member map still shared"))
        .into_inner()
        .expect("zset members"))
}

pub(crate) fn member_from_encoded(enc: &[u8], prefix_len: usize) -> Option<Vec<u8>> {
    if enc.len() < prefix_len + 2 {
        return None;
    }
    let len = u16::from_be_bytes([enc[prefix_len], enc[prefix_len + 1]]) as usize;
    let start = prefix_len + 2;
    let end = start.checked_add(len)?;
    if end > enc.len() {
        return None;
    }
    Some(enc[start..end].to_vec())
}

fn push_member_put(ops: &mut Vec<(Vec<u8>, WriteOp)>, encoded_user: &[u8], member: &[u8], score: f64) {
    let member_engine = member_key(encoded_user, member);
    ops.push((
        member_engine,
        WriteOp::PutEngine {
            value: encode_score(score),
        },
    ));
    let score_engine = score_key(encoded_user, score, member);
    ops.push((score_engine, WriteOp::PutEngine { value: Vec::new() }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sortable_score_orders_negatives_before_positives() {
        let neg = sortable_score(-1.5);
        let zero = sortable_score(0.0);
        let pos = sortable_score(2.0);
        assert!(neg < zero);
        assert!(zero < pos);
        assert_eq!(unsort_score(neg), -1.5);
        assert_eq!(unsort_score(zero), 0.0);
        assert_eq!(unsort_score(pos), 2.0);
    }
}
