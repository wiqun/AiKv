//! Hash 命令: HSET/HMSET/HGET/HMGET/HDEL/HEXISTS/HLEN/HKEYS/HVALS/HGETALL/HSETNX/
//! HINCRBY/HINCRBYFLOAT/HSCAN. HMSET 走 HSET 逻辑并返回 `OK`.
//!
//! # 存储表示 (双格式)
//!
//! ```text
//! 小 hash (≤ 64 fields): ValueType::Hash(HashMap<field, value>)
//!                        postcard 存于 StoredValue
//! 大 hash (> 64 fields): ValueType::CollectionHeader { kind: Hash, count }
//!                        field 独立 subkey:
//!                        {encoded_user_key}\x01H{field_len:2B}{field} → raw value
//! ```
//!
//! 写路径: `key_lock.lock(key)` → `load_or_create_hash` (兼容两种格式) → 修改 →
//! 超过 `HASH_MAX_INLINE_FIELDS` (64) 时 `migrate_hash_to_subkey`; 删空后 `delete`.
//! subkey 读写走 `raw_subkey_*`, 仅持久化引擎支持 (Memory 引擎返回 Err).
//! 类型分轨: `get_typed`/`set_typed`; 非 Hash → WRONGTYPE.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router::{self, KeyLock};
use crate::command::scan_util;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::memory::glob_match;
use crate::storage::subkey;
use crate::storage::{AiDbEngine, CollectionKind, KvStorage, StoredValue, ValueType, WRONGTYPE};

/// 小集合 inline 编码于 StoredValue; 超过此阈值自动切到 subkey 格式.
const HASH_MAX_INLINE_FIELDS: usize = 64;

pub struct HashCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

impl HashCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self { storage, key_lock }
    }

    pub async fn hset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 3 || args.len().is_multiple_of(2) {
            return Err(router::wrong_args("HSET", ""));
        }
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let mut stored = self.load_or_create_hash(db, key).await?;

        // 区分 inline / subkey 格式
        match &stored.value {
            ValueType::Hash(_) => {
                let ValueType::Hash(ref mut map) = stored.value else {
                    unreachable!()
                };
                let mut new_fields = 0i64;
                for chunk in args[1..].chunks(2) {
                    let field = chunk[0].to_vec();
                    let value = chunk[1].to_vec();
                    if !map.contains_key(&field) {
                        new_fields += 1;
                    }
                    map.insert(field, value);
                }
                // 超过阈值则转换为 subkey 格式
                if map.len() > HASH_MAX_INLINE_FIELDS {
                    self.migrate_hash_to_subkey(db, key, &stored.expires_at, map)
                        .await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(new_fields))
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            } => {
                let mut old_count = *count;
                let mut new_fields = 0i64;
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                for chunk in args[1..].chunks(2) {
                    let field = chunk[0].as_ref();
                    let value = chunk[1].to_vec();
                    let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, field);
                    // 检查是否已存在
                    let existed = self
                        .storage
                        .raw_subkey_get(db, subkey_key.clone())
                        .await
                        .unwrap_or_default()
                        .is_some();
                    if !existed {
                        new_fields += 1;
                        old_count += 1;
                    }
                    self.storage.raw_subkey_set(db, subkey_key, value).await?;
                }
                // 更新元数据 count
                stored.value = ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: old_count,
                };
                self.storage.rewrite_stored(db, key, stored).await?;
                Ok(router::integer(new_fields))
            }
            _ => Err(router::wrongtype()),
        }
    }

    /// HMSET — Redis 兼容: 始终返回 `OK` (HSET 返回新增字段数).
    pub async fn hmset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.hset(db, args).await?;
        Ok(RespValue::SimpleString("OK".into()))
    }

    pub async fn hget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HGET", args, 2)?;
        let key_bytes = &args[0];
        let field = args[1].as_ref();

        let Some(stored) = self.storage.get_typed(db, key_bytes).await? else {
            return Ok(router::nil_bulk());
        };

        match &stored.value {
            ValueType::Hash(map) => match map.get(field) {
                Some(v) => Ok(router::bulk(v.clone())),
                None => Ok(router::nil_bulk()),
            },
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key_bytes);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, field);
                let raw = self
                    .storage
                    .raw_subkey_get(db, subkey_key)
                    .await
                    .unwrap_or_default();
                match raw {
                    Some(v) => Ok(router::bulk(v)),
                    None => Ok(router::nil_bulk()),
                }
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn hdel(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HDEL", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::integer(0));
        };

        match &stored.value {
            ValueType::Hash(_) => {
                let ValueType::Hash(ref mut map) = stored.value else {
                    unreachable!()
                };
                let mut count = 0i64;
                for field in &args[1..] {
                    if map.remove(field.as_ref()).is_some() {
                        count += 1;
                    }
                }
                if map.is_empty() {
                    self.storage.delete(db, key).await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(count))
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ref count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                let mut removed = 0i64;
                for field in &args[1..] {
                    let subkey_key =
                        subkey::encode_hash_field_key(&encoded_user_key, field.as_ref());
                    if self
                        .storage
                        .raw_subkey_delete(db, subkey_key)
                        .await
                        .unwrap_or_default()
                    {
                        removed += 1;
                    }
                }
                let new_count = count.saturating_sub(removed as u32);
                if new_count == 0 {
                    // 清理所有 subkey (防御性: 理论上上面已删除)
                    self.delete_all_hash_subkeys(db, &encoded_user_key).await;
                    self.storage.delete(db, key).await?;
                } else {
                    stored.value = ValueType::CollectionHeader {
                        kind: CollectionKind::Hash,
                        count: new_count,
                    };
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(removed))
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn hexists(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HEXISTS", args, 2)?;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let exists = match &stored.value {
            ValueType::Hash(map) => map.contains_key(args[1].as_ref()),
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, args[1].as_ref());
                self.storage
                    .raw_subkey_get(db, subkey_key)
                    .await
                    .unwrap_or_default()
                    .is_some()
            }
            _ => return Err(router::wrongtype()),
        };
        Ok(router::integer(i64::from(exists)))
    }

    pub async fn hlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HLEN", args, 1)?;
        let len = match self.storage.get_typed(db, &args[0]).await? {
            None => 0,
            Some(stored) => match stored.value {
                ValueType::Hash(map) => map.len() as i64,
                ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count,
                } => count as i64,
                _ => return Err(router::wrongtype()),
            },
        };
        Ok(router::integer(len))
    }

    pub async fn hkeys(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HKEYS", args, 1)?;
        let keys = self.load_hash_all_fields(db, &args[0]).await?;
        Ok(array_of_bulk(keys.into_keys().collect()))
    }

    pub async fn hvals(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HVALS", args, 1)?;
        let keys = self.load_hash_all_fields(db, &args[0]).await?;
        Ok(array_of_bulk(keys.into_values().collect()))
    }

    pub async fn hgetall(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HGETALL", args, 1)?;
        let map = self.load_hash_all_fields(db, &args[0]).await?;
        let mut items = Vec::new();
        for (k, v) in map {
            items.push(router::bulk(k));
            items.push(router::bulk(v));
        }
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn hmget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HMGET", args, 2)?;
        let stored = self.storage.get_typed(db, &args[0]).await?;
        let mut items = Vec::with_capacity(args.len() - 1);

        match stored {
            None => {
                for _ in &args[1..] {
                    items.push(router::nil_bulk());
                }
            }
            Some(stored) => match &stored.value {
                ValueType::Hash(map) => {
                    for field in &args[1..] {
                        match map.get(field.as_ref()) {
                            Some(v) => items.push(router::bulk(v.clone())),
                            None => items.push(router::nil_bulk()),
                        }
                    }
                }
                ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    ..
                } => {
                    let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                    for field in &args[1..] {
                        let subkey_key =
                            subkey::encode_hash_field_key(&encoded_user_key, field.as_ref());
                        let raw = self
                            .storage
                            .raw_subkey_get(db, subkey_key)
                            .await
                            .unwrap_or_default();
                        match raw {
                            Some(v) => items.push(router::bulk(v)),
                            None => items.push(router::nil_bulk()),
                        }
                    }
                }
                _ => return Err(router::wrongtype()),
            },
        }
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn hsetnx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HSETNX", args, 3)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let mut stored = match self.storage.get_typed(db, key).await? {
            None => StoredValue {
                value: ValueType::Hash(HashMap::new()),
                expires_at: None,
            },
            Some(s) => match s.value {
                ValueType::Hash(_) => s,
                ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    ..
                } => s,
                _ => return Err(router::wrongtype()),
            },
        };

        match &mut stored.value {
            ValueType::Hash(ref mut map) => {
                if map.contains_key(args[1].as_ref()) {
                    return Ok(router::integer(0));
                }
                map.insert(args[1].to_vec(), args[2].to_vec());
                if map.len() > HASH_MAX_INLINE_FIELDS {
                    self.migrate_hash_to_subkey(db, key, &stored.expires_at, map)
                        .await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, args[1].as_ref());
                let existed = self
                    .storage
                    .raw_subkey_get(db, subkey_key.clone())
                    .await
                    .unwrap_or_default()
                    .is_some();
                if existed {
                    return Ok(router::integer(0));
                }
                self.storage
                    .raw_subkey_set(db, subkey_key, args[2].to_vec())
                    .await?;
                stored.value = ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    count: *count + 1,
                };
                self.storage.rewrite_stored(db, key, stored).await?;
            }
            _ => return Err(router::wrongtype()),
        }
        Ok(router::integer(1))
    }

    pub async fn hscan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HSCAN", args, 2)?;
        let cursor = scan_util::parse_u64(&args[1])?;
        let opts = scan_util::parse_scan_options("HSCAN", args, 2)?;

        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(hscan_response(0, &[]));
        };

        let pairs: Vec<(Vec<u8>, Vec<u8>)> = match stored.value {
            ValueType::Hash(map) => {
                let mut p: Vec<_> = map
                    .into_iter()
                    .filter(|(field, _)| {
                        opts.pattern
                            .as_ref()
                            .is_none_or(|p| glob_match(p, field.as_slice()))
                    })
                    .collect();
                p.sort_by(|a, b| a.0.cmp(&b.0));
                p
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                let mut p = self
                    .scan_hash_subkeys(db, &encoded_user_key, opts.pattern.as_deref())
                    .await?;
                p.sort_by(|a, b| a.0.cmp(&b.0));
                p
            }
            _ => return Err(router::wrongtype()),
        };

        let (next_cursor, page) = scan_util::paginate_slice(&pairs, cursor, opts.count);
        Ok(hscan_response(next_cursor, page))
    }

    pub async fn hincrbyfloat(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HINCRBYFLOAT", args, 3)?;
        let delta = parse_f64_field_bytes(&args[2])?;
        if !delta.is_finite() {
            return Err(Error::Command("ERR value is not a valid float".into()));
        }
        let key = &args[0];
        let field = args[1].to_vec();
        let _lock = self.key_lock.lock(key).await;
        let mut stored = self.load_or_create_hash(db, key).await?;

        let new_val = match &stored.value {
            ValueType::Hash(ref _map) => {
                let ValueType::Hash(ref mut map) = stored.value else {
                    unreachable!()
                };
                let current = match map.get(&field) {
                    None => 0.0f64,
                    Some(existing) => parse_f64_field_bytes(existing)?,
                };
                let new_val = current + delta;
                if !new_val.is_finite() {
                    return Err(Error::Command(
                        "ERR increment would produce NaN or Infinity".into(),
                    ));
                }
                let s = format_hash_float(new_val);
                map.insert(field, s.as_bytes().to_vec());
                if map.len() > HASH_MAX_INLINE_FIELDS {
                    self.migrate_hash_to_subkey(db, key, &stored.expires_at, map)
                        .await?;
                    return Ok(router::bulk(s.into_bytes()));
                }
                self.storage.rewrite_stored(db, key, stored).await?;
                return Ok(router::bulk(s.into_bytes()));
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, &field);
                let current = match self
                    .storage
                    .raw_subkey_get(db, subkey_key.clone())
                    .await
                    .unwrap_or_default()
                {
                    None => 0.0f64,
                    Some(existing) => parse_f64_field_bytes(&existing)?,
                };
                let new_val = current + delta;
                if !new_val.is_finite() {
                    return Err(Error::Command(
                        "ERR increment would produce NaN or Infinity".into(),
                    ));
                }
                let s = format_hash_float(new_val);
                let existed = self
                    .storage
                    .raw_subkey_get(db, subkey_key.clone())
                    .await
                    .unwrap_or_default()
                    .is_some();
                self.storage
                    .raw_subkey_set(db, subkey_key, s.as_bytes().to_vec())
                    .await?;
                // 仅新 field 才增加 count
                if !existed {
                    stored.value = ValueType::CollectionHeader {
                        kind: CollectionKind::Hash,
                        count: *count + 1,
                    };
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                s.as_bytes().to_vec()
            }
            _ => return Err(router::wrongtype()),
        };

        Ok(router::bulk(new_val))
    }

    pub async fn hincrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HINCRBY", args, 3)?;
        let delta = parse_i64_field(&args[2])?;
        let key = &args[0];
        let field = args[1].to_vec();
        let _lock = self.key_lock.lock(key).await;
        let mut stored = self.load_or_create_hash(db, key).await?;

        let new_val = match &stored.value {
            ValueType::Hash(ref _map) => {
                let ValueType::Hash(ref mut map) = stored.value else {
                    unreachable!()
                };
                let new_val = match map.get(&field) {
                    None => delta,
                    Some(existing) => {
                        let v = parse_i64_field(existing)?;
                        v.checked_add(delta).ok_or_else(|| {
                            Error::Command("ERR increment or decrement would overflow".into())
                        })?
                    }
                };
                map.insert(field, new_val.to_string().into_bytes());
                if map.len() > HASH_MAX_INLINE_FIELDS {
                    self.migrate_hash_to_subkey(db, key, &stored.expires_at, map)
                        .await?;
                    return Ok(router::integer(new_val));
                }
                self.storage.rewrite_stored(db, key, stored).await?;
                return Ok(router::integer(new_val));
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, &field);
                let (current_val, existed) = match self
                    .storage
                    .raw_subkey_get(db, subkey_key.clone())
                    .await
                    .unwrap_or_default()
                {
                    None => (None, false),
                    Some(existing) => (Some(parse_i64_field(&existing)?), true),
                };
                let new_val = match current_val {
                    None => delta,
                    Some(v) => v.checked_add(delta).ok_or_else(|| {
                        Error::Command("ERR increment or decrement would overflow".into())
                    })?,
                };
                self.storage
                    .raw_subkey_set(db, subkey_key, new_val.to_string().into_bytes())
                    .await?;
                if !existed {
                    stored.value = ValueType::CollectionHeader {
                        kind: CollectionKind::Hash,
                        count: *count + 1,
                    };
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                new_val
            }
            _ => return Err(router::wrongtype()),
        };

        Ok(router::integer(new_val))
    }

    // ---- helpers ----

    /// 读取 hash 的所有 field → value (兼容 inline 和 subkey).
    async fn load_hash_all_fields(
        &self,
        db: usize,
        key: &[u8],
    ) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(HashMap::new());
        };
        match stored.value {
            ValueType::Hash(map) => Ok(map),
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                self.scan_hash_subkeys(db, &encoded_user_key, None)
                    .await
                    .map(|pairs| pairs.into_iter().collect())
            }
            _ => Err(router::wrongtype()),
        }
    }

    /// 加载或创建 hash (兼容所有格式).
    async fn load_or_create_hash(&self, db: usize, key: &[u8]) -> Result<StoredValue> {
        match self.storage.get_typed(db, key).await? {
            None => Ok(StoredValue {
                value: ValueType::Hash(HashMap::new()),
                expires_at: None,
            }),
            Some(stored) => match stored.value {
                ValueType::Hash(_) => Ok(stored),
                ValueType::CollectionHeader {
                    kind: CollectionKind::Hash,
                    ..
                } => Ok(stored),
                _ => Err(router::wrongtype()),
            },
        }
    }

    /// 加载 hash 的所有 fields (兼容所有格式). 用于只读路径.
    #[allow(dead_code)]
    async fn load_hash(&self, db: usize, key: &[u8]) -> Result<Option<HashMap<Vec<u8>, Vec<u8>>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(None);
        };
        match stored.value {
            ValueType::Hash(map) => Ok(Some(map)),
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                Ok(Some(
                    self.scan_hash_subkeys(db, &encoded_user_key, None)
                        .await?
                        .into_iter()
                        .collect(),
                ))
            }
            _ => Err(router::wrongtype()),
        }
    }

    /// 扫描 subkey hash 的所有 field → value.
    async fn scan_hash_subkeys(
        &self,
        db: usize,
        encoded_user_key: &[u8],
        pattern: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let prefix = subkey::hash_subkey_prefix(encoded_user_key);
        let out = Arc::new(std::sync::Mutex::new(Vec::new()));
        let out_c = out.clone();
        let pattern = pattern.map(|p| p.to_vec());

        let _ = self
            .storage
            .raw_subkey_for_each(
                db,
                prefix,
                Box::new(move |encoded, raw| {
                    if let Some((kind, field)) = subkey::decode_subkey(&encoded) {
                        if kind != CollectionKind::Hash {
                            return Ok(());
                        }
                        if let Some(ref pat) = pattern {
                            if !glob_match(pat, &field) {
                                return Ok(());
                            }
                        }
                        out_c.lock().unwrap().push((field, raw));
                    }
                    Ok(())
                }),
            )
            .await;

        Ok(Arc::try_unwrap(out).unwrap().into_inner().unwrap())
    }

    /// 将 inline hash map 迁移为 subkey 格式.
    async fn migrate_hash_to_subkey(
        &self,
        db: usize,
        key: &[u8],
        expires_at: &Option<u64>,
        map: &HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<()> {
        let encoded_user_key = AiDbEngine::encode_key(db, key);
        let count = map.len() as u32;

        for (field, value) in map {
            let subkey_key = subkey::encode_hash_field_key(&encoded_user_key, field);
            self.storage
                .raw_subkey_set(db, subkey_key, value.clone())
                .await?;
        }

        let metadata = StoredValue {
            value: ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
            },
            expires_at: *expires_at,
        };
        self.storage.rewrite_stored(db, key, metadata).await
    }

    /// 删除某个 key 下的所有 hash subkey entries.
    async fn delete_all_hash_subkeys(&self, _db: usize, encoded_user_key: &[u8]) {
        // 收集并删除所有 subkey entries.
        // 注意: raw_subkey_for_each 不能被 await 在 &mut dyn FnMut 闭包中, 所以先收集.
        // 这里简化: 依赖引擎的 delete_range 可行, 但如果 prefix 跨越非 subkey keys 则有风险.
        // 当前 subkey 都位于 encoded_user_key\x01H 前缀下, 相对独立.
        //
        // 对于 StorageAdapter 没有 delete_range 只适用于前缀范围内的场景,
        // 在调用处 (hdel count==0) 我们已经逐字段删除了, 此函数作为防御性保险.
        // 暂留空实现; 清理由调用方逐个 delete 完成.
        let _ = encoded_user_key;
    }
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn hscan_response(cursor: u64, page: &[(Vec<u8>, Vec<u8>)]) -> RespValue {
    let mut items = vec![RespValue::BulkString(Some(Bytes::from(cursor.to_string())))];
    let mut pairs = Vec::new();
    for (field, value) in page {
        pairs.push(router::bulk(field.clone()));
        pairs.push(router::bulk(value.clone()));
    }
    items.push(RespValue::Array(Some(pairs)));
    RespValue::Array(Some(items))
}

fn parse_i64_field(b: &[u8]) -> Result<i64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Command(WRONGTYPE.into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command(WRONGTYPE.into()))
}

pub(crate) fn parse_f64_field_bytes(b: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR hash value is not a valid float".into()))?;
    s.parse::<f64>()
        .map_err(|_| Error::Command("ERR hash value is not a valid float".into()))
}

pub(crate) fn format_hash_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
