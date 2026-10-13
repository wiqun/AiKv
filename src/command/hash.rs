//! Hash 命令: HSET/HMSET/HGET/HMGET/HDEL/HEXISTS/HLEN/HKEYS/HVALS/HGETALL/HSETNX/
//! HINCRBY/HINCRBYFLOAT/HSCAN. HMSET 走 HSET 逻辑并返回 `OK`.
//!
//! # 存储表示 (双格式, 见 `storage::collection`)
//!
//! ```text
//! 小 hash (≤ 64 fields): ValueType::Hash(HashMap<field, value>), 内联于 StoredValue
//! 大 hash (> 64 fields): ValueType::CollectionHeader { kind: Hash, count, version }
//!                        field 为独立 subkey, 物理 key 带版本号, 见 `storage::subkey`
//! ```
//!
//! 读写一律经 `HashState` 判定格式, 不直接触碰 subkey 编码.
//! 多 field 写入 (HSET 多对、HDEL 多个) 只调用一次 `commit_collection` 或 `rewrite_stored`.
//! 类型分轨: 非 Hash → WRONGTYPE.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use bytes::Bytes;

use crate::command::router::{self, KeyLock};
use crate::command::scan_util;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::collection::{CollectionWrite, HeaderWrite};
use crate::storage::memory::glob_match;
use crate::storage::subkey::SubTag;
use crate::storage::{CollectionKind, KvStorage, StoredValue, ValueType, WRONGTYPE};

pub struct HashCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

/// 一次读取到的 Hash 存储状态.
enum HashState {
    Missing,
    Inline(StoredValue),
    Subkey {
        expires_at: Option<u64>,
        count: u64,
        version: u64,
    },
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
        let state = self.load_state(db, key).await?;
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = args[1..]
            .chunks(2)
            .map(|c| (c[0].to_vec(), c[1].to_vec()))
            .collect();
        let added = self.write_fields(db, key, state, pairs).await?;
        Ok(router::integer(added))
    }

    pub async fn hmset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        self.hset(db, args).await?;
        Ok(RespValue::SimpleString("OK".into()))
    }

    pub async fn hget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HGET", args, 2)?;
        match self
            .read_fields(db, &args[0], &[args[1].as_ref()])
            .await?
            .pop()
            .flatten()
        {
            Some(v) => Ok(router::bulk(v)),
            None => Ok(router::nil_bulk()),
        }
    }

    pub async fn hdel(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HDEL", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let fields: Vec<&[u8]> = args[1..]
            .iter()
            .map(|f| f.as_ref())
            .collect::<BTreeSet<&[u8]>>()
            .into_iter()
            .collect();
        match self.load_state(db, key).await? {
            HashState::Missing => Ok(router::integer(0)),
            HashState::Inline(mut stored) => {
                let ValueType::Hash(map) = &mut stored.value else {
                    unreachable!("inline state is always a Hash")
                };
                let removed = fields.iter().filter(|f| map.remove(**f).is_some()).count() as i64;
                if map.is_empty() {
                    self.storage.delete(db, key).await?;
                } else if removed > 0 {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(removed))
            }
            HashState::Subkey {
                expires_at,
                count,
                version,
            } => {
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, SubTag::Hash, &fields)
                    .await?;
                let deletes: Vec<(SubTag, Vec<u8>)> = fields
                    .iter()
                    .zip(existing)
                    .filter_map(|(f, v)| v.map(|_| (SubTag::Hash, f.to_vec())))
                    .collect();
                let removed = deletes.len() as u64;
                if removed == 0 {
                    return Ok(router::integer(0));
                }
                let remaining = count.saturating_sub(removed);
                let header = if remaining == 0 {
                    HeaderWrite::Delete {
                        had_ttl: expires_at.is_some(),
                    }
                } else {
                    HeaderWrite::Put(StoredValue {
                        value: ValueType::CollectionHeader {
                            kind: CollectionKind::Hash,
                            count: remaining,
                            version,
                        },
                        expires_at,
                    })
                };
                self.storage
                    .commit_collection(
                        db,
                        key,
                        CollectionWrite {
                            header,
                            version,
                            puts: Vec::new(),
                            deletes,
                        },
                    )
                    .await?;
                Ok(router::integer(removed as i64))
            }
        }
    }

    pub async fn hexists(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HEXISTS", args, 2)?;
        let found = self
            .read_fields(db, &args[0], &[args[1].as_ref()])
            .await?
            .pop()
            .flatten()
            .is_some();
        Ok(router::integer(i64::from(found)))
    }

    pub async fn hlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HLEN", args, 1)?;
        let len = match self.load_state(db, &args[0]).await? {
            HashState::Missing => 0,
            HashState::Inline(stored) => match stored.value {
                ValueType::Hash(map) => map.len() as i64,
                _ => unreachable!("inline state is always a Hash"),
            },
            HashState::Subkey { count, .. } => count as i64,
        };
        Ok(router::integer(len))
    }

    pub async fn hkeys(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HKEYS", args, 1)?;
        let rows = self.load_all(db, &args[0]).await?;
        Ok(array_of_bulk(rows.into_iter().map(|(f, _)| f).collect()))
    }

    pub async fn hvals(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HVALS", args, 1)?;
        let rows = self.load_all(db, &args[0]).await?;
        Ok(array_of_bulk(rows.into_iter().map(|(_, v)| v).collect()))
    }

    pub async fn hgetall(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HGETALL", args, 1)?;
        let rows = self.load_all(db, &args[0]).await?;
        let mut items = Vec::with_capacity(rows.len() * 2);
        for (k, v) in rows {
            items.push(router::bulk(k));
            items.push(router::bulk(v));
        }
        Ok(RespValue::Array(Some(items)))
    }

    pub async fn hmget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HMGET", args, 2)?;
        let fields: Vec<&[u8]> = args[1..].iter().map(|f| f.as_ref()).collect();
        let values = self.read_fields(db, &args[0], &fields).await?;
        Ok(RespValue::Array(Some(
            values
                .into_iter()
                .map(|v| match v {
                    Some(v) => router::bulk(v),
                    None => router::nil_bulk(),
                })
                .collect(),
        )))
    }

    pub async fn hsetnx(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HSETNX", args, 3)?;
        let key = &args[0];
        let field = args[1].to_vec();
        let value = args[2].to_vec();
        let _lock = self.key_lock.lock(key).await;
        let state = self.load_state(db, key).await?;
        let existing = match &state {
            HashState::Missing => None,
            HashState::Inline(stored) => match &stored.value {
                ValueType::Hash(map) => map.get(&field).cloned(),
                _ => unreachable!("inline state is always a Hash"),
            },
            HashState::Subkey { version, .. } => self
                .storage
                .subkey_get_many(db, key, *version, SubTag::Hash, &[&field])
                .await?
                .pop()
                .flatten(),
        };
        if existing.is_some() {
            return Ok(router::integer(0));
        }
        self.write_fields(db, key, state, vec![(field, value)])
            .await?;
        Ok(router::integer(1))
    }

    pub async fn hscan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("HSCAN", args, 2)?;
        let cursor = scan_util::parse_u64(&args[1])?;
        let opts = scan_util::parse_scan_options("HSCAN", args, 2)?;

        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = self
            .load_all(db, &args[0])
            .await?
            .into_iter()
            .filter(|(field, _)| {
                opts.pattern
                    .as_ref()
                    .is_none_or(|p| glob_match(p, field.as_slice()))
            })
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
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
        let state = self.load_state(db, key).await?;
        let current = self.current_field(db, key, &state, &field).await?;
        let base = match current {
            None => 0.0f64,
            Some(existing) => parse_f64_field_bytes(&existing)?,
        };
        let new_val = base + delta;
        if !new_val.is_finite() {
            return Err(Error::Command(
                "ERR increment would produce NaN or Infinity".into(),
            ));
        }
        let s = format_hash_float(new_val);
        self.write_fields(db, key, state, vec![(field, s.clone().into_bytes())])
            .await?;
        Ok(router::bulk(s.into_bytes()))
    }

    pub async fn hincrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("HINCRBY", args, 3)?;
        let delta = parse_i64_field(&args[2])?;
        let key = &args[0];
        let field = args[1].to_vec();
        let _lock = self.key_lock.lock(key).await;
        let state = self.load_state(db, key).await?;
        let current = self.current_field(db, key, &state, &field).await?;
        let new_val = match current {
            None => delta,
            Some(existing) => parse_i64_field(&existing)?
                .checked_add(delta)
                .ok_or_else(|| {
                    Error::Command("ERR increment or decrement would overflow".into())
                })?,
        };
        self.write_fields(
            db,
            key,
            state,
            vec![(field, new_val.to_string().into_bytes())],
        )
        .await?;
        Ok(router::integer(new_val))
    }

    /// 读取 Hash 状态; 类型不符返回 WRONGTYPE.
    async fn load_state(&self, db: usize, key: &[u8]) -> Result<HashState> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(HashState::Missing);
        };
        match stored.value {
            ValueType::Hash(_) => Ok(HashState::Inline(stored)),
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                count,
                version,
            } => Ok(HashState::Subkey {
                expires_at: stored.expires_at,
                count,
                version,
            }),
            _ => Err(router::wrongtype()),
        }
    }

    /// 写入 field (同名后者覆盖前者), 返回新增 field 数. 调用方持有 key 锁.
    async fn write_fields(
        &self,
        db: usize,
        key: &[u8],
        state: HashState,
        updates: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<i64> {
        let updates: BTreeMap<Vec<u8>, Vec<u8>> = updates.into_iter().collect();
        match state {
            HashState::Missing => {
                let added = updates.len() as i64;
                let map: HashMap<Vec<u8>, Vec<u8>> = updates.into_iter().collect();
                self.storage
                    .rewrite_stored(
                        db,
                        key,
                        StoredValue {
                            value: ValueType::Hash(map),
                            expires_at: None,
                        },
                    )
                    .await?;
                Ok(added)
            }
            HashState::Inline(mut stored) => {
                let ValueType::Hash(map) = &mut stored.value else {
                    unreachable!("inline state is always a Hash")
                };
                let mut added = 0i64;
                for (field, value) in updates {
                    if map.insert(field, value).is_none() {
                        added += 1;
                    }
                }
                self.storage.rewrite_stored(db, key, stored).await?;
                Ok(added)
            }
            HashState::Subkey {
                expires_at,
                count,
                version,
            } => {
                let fields: Vec<&[u8]> = updates.keys().map(Vec::as_slice).collect();
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, SubTag::Hash, &fields)
                    .await?;
                let added = existing.iter().filter(|v| v.is_none()).count() as u64;
                self.storage
                    .commit_collection(
                        db,
                        key,
                        CollectionWrite {
                            header: HeaderWrite::Put(StoredValue {
                                value: ValueType::CollectionHeader {
                                    kind: CollectionKind::Hash,
                                    count: count + added,
                                    version,
                                },
                                expires_at,
                            }),
                            version,
                            puts: updates
                                .into_iter()
                                .map(|(field, value)| (SubTag::Hash, field, value))
                                .collect(),
                            deletes: Vec::new(),
                        },
                    )
                    .await?;
                Ok(added as i64)
            }
        }
    }

    /// 按顺序读取 field; 不存在为 None.
    async fn read_fields(
        &self,
        db: usize,
        key: &[u8],
        fields: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        match self.load_state(db, key).await? {
            HashState::Missing => Ok(vec![None; fields.len()]),
            HashState::Inline(stored) => {
                let ValueType::Hash(map) = stored.value else {
                    unreachable!("inline state is always a Hash")
                };
                Ok(fields.iter().map(|f| map.get(*f).cloned()).collect())
            }
            HashState::Subkey { version, .. } => {
                self.storage
                    .subkey_get_many(db, key, version, SubTag::Hash, fields)
                    .await
            }
        }
    }

    /// 读取全部 (field, value), 按存储顺序返回 (Inline 为哈希序, Subkey 为 field 字节序).
    async fn load_all(&self, db: usize, key: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        match self.load_state(db, key).await? {
            HashState::Missing => Ok(Vec::new()),
            HashState::Inline(stored) => {
                let ValueType::Hash(map) = stored.value else {
                    unreachable!("inline state is always a Hash")
                };
                Ok(map.into_iter().collect())
            }
            HashState::Subkey { version, .. } => {
                self.storage
                    .subkey_scan(db, key, version, SubTag::Hash)
                    .await
            }
        }
    }

    /// 读取单个 field 的当前值 (用于 HINCRBY 系列).
    async fn current_field(
        &self,
        db: usize,
        key: &[u8],
        state: &HashState,
        field: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        match state {
            HashState::Missing => Ok(None),
            HashState::Inline(stored) => match &stored.value {
                ValueType::Hash(map) => Ok(map.get(field).cloned()),
                _ => unreachable!("inline state is always a Hash"),
            },
            HashState::Subkey { version, .. } => Ok(self
                .storage
                .subkey_get_many(db, key, *version, SubTag::Hash, &[field])
                .await?
                .pop()
                .flatten()),
        }
    }
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn hscan_response(cursor: u64, page: &[(Vec<u8>, Vec<u8>)]) -> RespValue {
    let mut items = vec![RespValue::BulkString(Some(Bytes::from(cursor.to_string())))];
    let mut pairs = Vec::with_capacity(page.len() * 2);
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

fn parse_f64_field_bytes(b: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command("ERR hash value is not a valid float".into()))?;
    s.parse::<f64>()
        .map_err(|_| Error::Command("ERR hash value is not a valid float".into()))
}

fn format_hash_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}
