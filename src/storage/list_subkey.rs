//! 大 List 的 subkey 编码.
//!
//! 不超过 `list_max_inline()` (默认 64) 的列表仍以内联 `ValueType::List(VecDeque)` 存储.
//! 超过后, 用户 key 存 `CollectionHeader { kind: List, count, version, head, tail }`,
//! 每个元素以 `{encoded_user_key}\x01L{version:8B_be}{index:8B_be}` 存储.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::Result;
use crate::storage::subkey::{encode_versioned_prefix, LIST_TAG, SUBKEY_SEPARATOR};
use crate::storage::types::{next_collection_version, KvStorage, StoredValue, ValueType, WriteOp};
use crate::storage::{AiDbEngine, ScriptIo};

/// 默认 List 内联上限阈值.
pub const LIST_MAX_INLINE_ELEMENTS: usize = 64;

/// 双向游标初始基准 (`u64::MAX / 2`), 允许向左右各自生长 `2^63` 次不冲突.
pub const LIST_INITIAL_CURSOR: u64 = u64::MAX / 2;

static CONFIGURED_LIST_MAX_INLINE: AtomicUsize = AtomicUsize::new(LIST_MAX_INLINE_ELEMENTS);

/// 设置进程级 List 内联存储阈值 (启动时由配置初始化).
pub fn set_list_max_inline(val: usize) {
    CONFIGURED_LIST_MAX_INLINE.store(val.max(1), Ordering::Relaxed);
}

#[inline]
pub fn list_max_inline() -> usize {
    CONFIGURED_LIST_MAX_INLINE.load(Ordering::Relaxed)
}

/// 构造 List 元素 subkey 的完整 engine key.
///
/// 格式: `{encoded_user}\x01L{version:8B_be}{index:8B_be}`
pub fn encode_list_element_key(encoded_user: &[u8], version: u64, index: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(encoded_user.len() + 18);
    key.extend_from_slice(encoded_user);
    key.push(SUBKEY_SEPARATOR);
    key.push(LIST_TAG);
    key.extend_from_slice(&version.to_be_bytes());
    key.extend_from_slice(&index.to_be_bytes());
    key
}

/// 构造 List subkey 的版本化扫描前缀: `{encoded_user}\x01L{version:8B_be}`.
pub fn list_subkey_prefix(encoded_user: &[u8], version: u64) -> Vec<u8> {
    encode_versioned_prefix(encoded_user, LIST_TAG, version)
}

/// 构造 List `CollectionHeader`.
pub fn list_header_stored(
    count: u32,
    version: u64,
    head: u64,
    tail: u64,
    expires_at: Option<u64>,
) -> StoredValue {
    StoredValue {
        value: ValueType::list_header(count, version, head, tail),
        expires_at,
    }
}

/// 将整个 `VecDeque` 写为 `CollectionHeader` + 连续序列号 subkeys (使用全新 `version`).
pub fn replace_list_ops(
    db: usize,
    user_key: &[u8],
    list: &VecDeque<Vec<u8>>,
    expires_at: Option<u64>,
) -> Vec<(Vec<u8>, WriteOp)> {
    let encoded = AiDbEngine::encode_key(db, user_key);
    let version = next_collection_version();
    let count = list.len() as u32;
    let head = LIST_INITIAL_CURSOR;
    let tail = head + list.len() as u64;
    let mut ops = Vec::with_capacity(1 + list.len());
    ops.push((
        user_key.to_vec(),
        WriteOp::put_stored(list_header_stored(count, version, head, tail, expires_at)),
    ));
    for (i, item) in list.iter().enumerate() {
        let sk = encode_list_element_key(&encoded, version, head + i as u64);
        ops.push((sk, WriteOp::put_engine(item.clone())));
    }
    ops
}

/// 按规范化闭区间 `[start_idx, stop_idx]` 从存储中读取 List 子集.
pub async fn load_list_range(
    storage: &dyn KvStorage,
    _db: usize,
    encoded_user: &[u8],
    version: u64,
    head: u64,
    start_idx: usize,
    stop_idx: usize,
) -> Result<Vec<Vec<u8>>> {
    if start_idx > stop_idx {
        return Ok(Vec::new());
    }
    let start_k = encode_list_element_key(encoded_user, version, head + start_idx as u64);
    let stop_k = encode_list_element_key(encoded_user, version, head + stop_idx as u64);
    let end_k = AiDbEngine::prefix_end(&stop_k).unwrap_or_else(|| {
        let mut next = stop_k;
        next.push(0);
        next
    });
    let limit = stop_idx - start_idx + 1;
    let pairs = storage.raw_range_scan(start_k, end_k, limit, false).await?;
    Ok(pairs.into_iter().map(|(_, v)| v).collect())
}

/// 全量顺序读取 subkey List (供 `DUMP` / `LREM` / `LINSERT` / Lua 展开等使用).
pub async fn load_list_full(
    storage: &dyn KvStorage,
    db: usize,
    encoded_user: &[u8],
    version: u64,
) -> Result<VecDeque<Vec<u8>>> {
    let prefix = list_subkey_prefix(encoded_user, version);
    let list = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::new()));
    let list_c = std::sync::Arc::clone(&list);
    storage
        .raw_subkey_for_each(
            db,
            prefix,
            Box::new(move |_k, v| {
                list_c.lock().unwrap().push_back(v);
                Ok(())
            }),
        )
        .await?;
    Ok(std::sync::Arc::try_unwrap(list)
        .unwrap()
        .into_inner()
        .unwrap())
}

/// 同步全量顺序读取 subkey List (供 `ScriptTransaction` 使用).
pub fn load_list_full_sync(
    storage: &dyn ScriptIo,
    db: usize,
    encoded_user: &[u8],
    version: u64,
) -> Result<VecDeque<Vec<u8>>> {
    let prefix = list_subkey_prefix(encoded_user, version);
    let list = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::new()));
    let list_c = std::sync::Arc::clone(&list);
    storage.raw_subkey_for_each(
        db,
        prefix,
        Box::new(move |_k, v| {
            list_c.lock().unwrap().push_back(v);
            Ok(())
        }),
    )?;
    Ok(std::sync::Arc::try_unwrap(list)
        .unwrap()
        .into_inner()
        .unwrap())
}
