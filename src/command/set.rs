//! Set 命令: SADD/SREM/SISMEMBER/SMEMBERS/SCARD/SPOP/SRANDMEMBER/SUNION/SINTER/SDIFF/
//! SUNIONSTORE/SINTERSTORE/SDIFFSTORE/SMOVE/SSCAN.
//!
//! # 存储表示 (双格式, 见 `storage::collection`)
//!
//! ```text
//! 小 set (≤ 64 members): ValueType::Set(HashSet<member>), 内联于 StoredValue
//! 大 set (> 64 members): ValueType::CollectionHeader { kind: Set, count, version }
//!                        member 为独立 subkey, 物理 key 带版本号, 见 `storage::subkey`
//! ```
//!
//! 读写一律经 `SetState` 判定格式. 多 member 写入只调用一次 `commit_collection`
//! 或 `rewrite_stored`. `*STORE` 写目标时走 `put_value`, 因此同样遵守阈值.
//! 类型分轨: 非 Set → WRONGTYPE. `*STORE` 的目标按 Redis 语义直接覆盖任意类型
//! (结果为空时删除目标).

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use rand::seq::SliceRandom;

use crate::command::router::{self, KeyLock};
use crate::command::scan_util;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::collection::{CollectionWrite, HeaderWrite};
use crate::storage::memory::glob_match;
use crate::storage::subkey::SubTag;
use crate::storage::{CollectionKind, KvStorage, StoredValue, ValueType};

pub struct SetCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

/// 一次读取到的 Set 存储状态.
enum SetState {
    Missing,
    Inline(StoredValue),
    Subkey {
        expires_at: Option<u64>,
        count: u64,
        version: u64,
    },
}

impl SetCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self { storage, key_lock }
    }

    pub async fn sadd(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SADD", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let state = self.load_state(db, key).await?;
        let members = args[1..].iter().map(|m| m.to_vec()).collect();
        let added = self.add_members(db, key, state, members).await?;
        Ok(router::integer(added))
    }

    pub async fn srem(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SREM", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let state = self.load_state(db, key).await?;
        let members = args[1..].iter().map(|m| m.to_vec()).collect();
        let removed = self.remove_members(db, key, state, members).await?;
        Ok(router::integer(removed.len() as i64))
    }

    pub async fn sismember(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SISMEMBER", args, 2)?;
        let found = self.contains(db, &args[0], &args[1]).await?;
        Ok(router::integer(i64::from(found)))
    }

    pub async fn smembers(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SMEMBERS", args, 1)?;
        let members = self.load_members(db, &args[0]).await?;
        Ok(array_of_bulk(members))
    }

    pub async fn scard(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SCARD", args, 1)?;
        let len = match self.load_state(db, &args[0]).await? {
            SetState::Missing => 0,
            SetState::Inline(stored) => match stored.value {
                ValueType::Set(set) => set.len() as i64,
                _ => unreachable!("inline state is always a Set"),
            },
            SetState::Subkey { count, .. } => count as i64,
        };
        Ok(router::integer(len))
    }

    pub async fn spop(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SPOP", args, 1)?;
        let count = if args.len() > 1 {
            Some(parse_i64(&args[1])?)
        } else {
            None
        };
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let state = self.load_state(db, key).await?;
        if matches!(state, SetState::Missing) {
            return Ok(router::nil_bulk());
        }
        let count = count.unwrap_or(1);
        if count == 0 {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let mut members = self.members_of(db, key, &state).await?;
        members.shuffle(&mut rand::thread_rng());
        let n = (count as usize).min(members.len());
        members.truncate(n);
        let picked = self.remove_members(db, key, state, members).await?;

        if count == 1 && picked.len() == 1 {
            return Ok(router::bulk(picked.into_iter().next().unwrap()));
        }
        Ok(array_of_bulk(picked))
    }

    pub async fn srandmember(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SRANDMEMBER", args, 1)?;
        let count = if args.len() > 1 {
            Some(parse_i64(&args[1])?)
        } else {
            None
        };
        let mut members = self.load_members(db, &args[0]).await?;
        if members.is_empty() {
            return if count.is_some() {
                Ok(RespValue::Array(Some(vec![])))
            } else {
                Ok(router::nil_bulk())
            };
        }
        let count = count.unwrap_or(1);
        if count == 0 {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let mut rng = rand::thread_rng();
        if count > 0 {
            members.shuffle(&mut rng);
            let n = (count as usize).min(members.len());
            let picked: Vec<Vec<u8>> = members.into_iter().take(n).collect();
            if args.len() == 1 {
                return Ok(router::bulk(picked.into_iter().next().unwrap()));
            }
            return Ok(array_of_bulk(picked));
        }
        let picked: Vec<Vec<u8>> = (0..(-count) as usize)
            .map(|_| members.choose(&mut rng).cloned().unwrap())
            .collect();
        Ok(array_of_bulk(picked))
    }

    pub async fn sunion(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SUNION", args, 1)?;
        let result = self.compute_union(db, args).await?;
        Ok(array_of_bulk(result.into_iter().collect()))
    }

    pub async fn sinter(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SINTER", args, 1)?;
        let result = self.compute_inter(db, args).await?;
        Ok(array_of_bulk(result.into_iter().collect()))
    }

    pub async fn sdiff(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SDIFF", args, 1)?;
        let result = self.compute_diff(db, args).await?;
        Ok(array_of_bulk(result.into_iter().collect()))
    }

    pub async fn sunionstore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SUNIONSTORE", args, 2)?;
        let result = self.compute_union(db, &args[1..]).await?;
        self.store_set(db, &args[0], result).await
    }

    pub async fn sinterstore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SINTERSTORE", args, 2)?;
        let result = self.compute_inter(db, &args[1..]).await?;
        self.store_set(db, &args[0], result).await
    }

    pub async fn sdiffstore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SDIFFSTORE", args, 2)?;
        let result = self.compute_diff(db, &args[1..]).await?;
        self.store_set(db, &args[0], result).await
    }

    pub async fn smove(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SMOVE", args, 3)?;
        let source = &args[0];
        let dest = &args[1];
        let member = args[2].to_vec();

        let (_lock_a, _lock_b) = self.key_lock.lock_two(source, dest).await;
        let same_key = source == dest;

        if !self.contains(db, source, &member).await? {
            return Ok(router::integer(0));
        }
        // 先读目标状态: 目标类型错误时源不被修改 (与 Redis 一致).
        let dest_state = self.load_state(db, dest).await?;
        if same_key {
            return Ok(router::integer(1));
        }
        let src_state = self.load_state(db, source).await?;
        self.remove_members(db, source, src_state, vec![member.clone()])
            .await?;
        self.add_members(db, dest, dest_state, vec![member]).await?;
        Ok(router::integer(1))
    }

    pub async fn sscan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SSCAN", args, 2)?;
        let cursor = scan_util::parse_u64(&args[1])?;
        let opts = scan_util::parse_scan_options("SSCAN", args, 2)?;

        let mut members: Vec<Vec<u8>> = self
            .load_members(db, &args[0])
            .await?
            .into_iter()
            .filter(|m| {
                opts.pattern
                    .as_ref()
                    .is_none_or(|p| glob_match(p, m.as_slice()))
            })
            .collect();
        members.sort();
        let (next_cursor, page) = scan_util::paginate_slice(&members, cursor, opts.count);
        Ok(scan_util::scan_response_bulk(next_cursor, page))
    }

    /// `*STORE` 的写入: 结果为空时删除目标 (若存在), 否则按阈值写入 (可覆盖任意类型).
    async fn store_set(&self, db: usize, dest: &Bytes, set: HashSet<Vec<u8>>) -> Result<RespValue> {
        let _lock = self.key_lock.lock(dest).await;
        let old = self.storage.get_typed(db, dest).await?;
        let len = set.len() as i64;
        if set.is_empty() {
            if old.is_some() {
                self.storage.delete(db, dest).await?;
            }
            return Ok(router::integer(0));
        }
        self.storage
            .put_value(db, dest, StoredValue::new_set(set))
            .await?;
        if old.and_then(|v| v.expires_at).is_some() {
            self.storage.release_seen_ttl(db).await?;
        }
        Ok(router::integer(len))
    }

    async fn compute_union(&self, db: usize, keys: &[Bytes]) -> Result<HashSet<Vec<u8>>> {
        let mut result = HashSet::new();
        for key in keys {
            result.extend(self.load_members(db, key).await?);
        }
        Ok(result)
    }

    async fn compute_inter(&self, db: usize, keys: &[Bytes]) -> Result<HashSet<Vec<u8>>> {
        let mut result: Option<HashSet<Vec<u8>>> = None;
        for key in keys {
            let set: HashSet<Vec<u8>> = self.load_members(db, key).await?.into_iter().collect();
            result = Some(match result {
                None => set,
                Some(mut acc) => {
                    acc.retain(|m| set.contains(m));
                    acc
                }
            });
        }
        Ok(result.unwrap_or_default())
    }

    async fn compute_diff(&self, db: usize, keys: &[Bytes]) -> Result<HashSet<Vec<u8>>> {
        let mut result: HashSet<Vec<u8>> =
            self.load_members(db, &keys[0]).await?.into_iter().collect();
        for key in &keys[1..] {
            let set: HashSet<Vec<u8>> = self.load_members(db, key).await?.into_iter().collect();
            result.retain(|m| !set.contains(m));
        }
        Ok(result)
    }

    /// 读取 Set 状态; 类型不符返回 WRONGTYPE.
    async fn load_state(&self, db: usize, key: &[u8]) -> Result<SetState> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(SetState::Missing);
        };
        match stored.value {
            ValueType::Set(_) => Ok(SetState::Inline(stored)),
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                count,
                version,
            } => Ok(SetState::Subkey {
                expires_at: stored.expires_at,
                count,
                version,
            }),
            _ => Err(router::wrongtype()),
        }
    }

    fn header(count: u64, version: u64, expires_at: Option<u64>) -> StoredValue {
        StoredValue {
            value: ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                count,
                version,
            },
            expires_at,
        }
    }

    /// 返回新增成员数. 调用方持有 key 锁.
    async fn add_members(
        &self,
        db: usize,
        key: &[u8],
        state: SetState,
        members: Vec<Vec<u8>>,
    ) -> Result<i64> {
        let members: BTreeSet<Vec<u8>> = members.into_iter().collect();
        match state {
            SetState::Missing => {
                let added = members.len() as i64;
                let set: HashSet<Vec<u8>> = members.into_iter().collect();
                self.storage
                    .rewrite_stored(db, key, StoredValue::new_set(set))
                    .await?;
                Ok(added)
            }
            SetState::Inline(mut stored) => {
                let ValueType::Set(set) = &mut stored.value else {
                    unreachable!("inline state is always a Set")
                };
                let added = members
                    .into_iter()
                    .filter(|m| set.insert(m.clone()))
                    .count() as i64;
                if added > 0 {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(added)
            }
            SetState::Subkey {
                expires_at,
                count,
                version,
            } => {
                let subs: Vec<&[u8]> = members.iter().map(Vec::as_slice).collect();
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, SubTag::Set, &subs)
                    .await?;
                let puts: Vec<(SubTag, Vec<u8>, Vec<u8>)> = members
                    .iter()
                    .zip(existing)
                    .filter(|(_, v)| v.is_none())
                    .map(|(m, _)| (SubTag::Set, m.clone(), Vec::new()))
                    .collect();
                let added = puts.len() as u64;
                if added == 0 {
                    return Ok(0);
                }
                self.storage
                    .commit_collection(
                        db,
                        key,
                        CollectionWrite {
                            header: HeaderWrite::Put(Self::header(
                                count + added,
                                version,
                                expires_at,
                            )),
                            version,
                            puts,
                            deletes: Vec::new(),
                        },
                    )
                    .await?;
                Ok(added as i64)
            }
        }
    }

    /// 返回实际删除的成员. 调用方持有 key 锁.
    async fn remove_members(
        &self,
        db: usize,
        key: &[u8],
        state: SetState,
        members: Vec<Vec<u8>>,
    ) -> Result<Vec<Vec<u8>>> {
        let members: BTreeSet<Vec<u8>> = members.into_iter().collect();
        match state {
            SetState::Missing => Ok(Vec::new()),
            SetState::Inline(mut stored) => {
                let ValueType::Set(set) = &mut stored.value else {
                    unreachable!("inline state is always a Set")
                };
                let removed: Vec<Vec<u8>> = members.into_iter().filter(|m| set.remove(m)).collect();
                if set.is_empty() {
                    self.storage.delete(db, key).await?;
                } else if !removed.is_empty() {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(removed)
            }
            SetState::Subkey {
                expires_at,
                count,
                version,
            } => {
                let subs: Vec<&[u8]> = members.iter().map(Vec::as_slice).collect();
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, SubTag::Set, &subs)
                    .await?;
                let removed: Vec<Vec<u8>> = members
                    .into_iter()
                    .zip(existing)
                    .filter_map(|(m, v)| v.map(|_| m))
                    .collect();
                if removed.is_empty() {
                    return Ok(removed);
                }
                let remaining = count.saturating_sub(removed.len() as u64);
                let header = if remaining == 0 {
                    HeaderWrite::Delete {
                        had_ttl: expires_at.is_some(),
                    }
                } else {
                    HeaderWrite::Put(Self::header(remaining, version, expires_at))
                };
                self.storage
                    .commit_collection(
                        db,
                        key,
                        CollectionWrite {
                            header,
                            version,
                            puts: Vec::new(),
                            deletes: removed.iter().map(|m| (SubTag::Set, m.clone())).collect(),
                        },
                    )
                    .await?;
                Ok(removed)
            }
        }
    }

    async fn contains(&self, db: usize, key: &[u8], member: &[u8]) -> Result<bool> {
        match self.load_state(db, key).await? {
            SetState::Missing => Ok(false),
            SetState::Inline(stored) => match stored.value {
                ValueType::Set(set) => Ok(set.contains(member)),
                _ => unreachable!("inline state is always a Set"),
            },
            SetState::Subkey { version, .. } => Ok(self
                .storage
                .subkey_get_many(db, key, version, SubTag::Set, &[member])
                .await?
                .pop()
                .flatten()
                .is_some()),
        }
    }

    async fn load_members(&self, db: usize, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        let state = self.load_state(db, key).await?;
        self.members_of(db, key, &state).await
    }

    async fn members_of(&self, db: usize, key: &[u8], state: &SetState) -> Result<Vec<Vec<u8>>> {
        match state {
            SetState::Missing => Ok(Vec::new()),
            SetState::Inline(stored) => match &stored.value {
                ValueType::Set(set) => Ok(set.iter().cloned().collect()),
                _ => unreachable!("inline state is always a Set"),
            },
            SetState::Subkey { version, .. } => Ok(self
                .storage
                .subkey_scan(db, key, *version, SubTag::Set)
                .await?
                .into_iter()
                .map(|(m, _)| m)
                .collect()),
        }
    }
}

fn array_of_bulk(items: Vec<Vec<u8>>) -> RespValue {
    RespValue::Array(Some(items.into_iter().map(router::bulk).collect()))
}

fn parse_i64(b: &Bytes) -> Result<i64> {
    let s =
        std::str::from_utf8(b).map_err(|_| Error::Command("ERR value is not an integer".into()))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command("ERR value is not an integer".into()))
}
