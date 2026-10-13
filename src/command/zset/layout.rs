//! ZSET 子键布局的 2a 命令路径: ZADD / ZREM / ZSCORE / ZCARD.
//!
//! 存储形态由 header 决定: 内联整值 `ValueType::ZSet`, 或子键布局 `CollectionHeader{kind: ZSet}`.
//! 子键布局下 `Z` (member → score) 与 `R` (`sortable_score || member` 分数索引) 分开存放.
//!
//! # 布局切换策略 (避免 64/65 边界抖动)
//!
//! - 内联 → 子键: 写入后成员数 > `ZSET_MAX_INLINE_MEMBERS` (64), 经 `rewrite_stored` → `put_value`
//!   整集暂存为新版本.
//! - 子键 → 内联: 2a `ZREM` 后剩余成员数 ≤ `ZSET_DEMOTE_MEMBERS` (32) 时回落. 旧版本 subkey 不立即删除,
//!   交给 compaction GC 的宽限期回收, 避免无锁读 (ZSCORE/ZCARD) 在切换期间看到半删除的状态.
//! - 成员数在 33..=64 的子键化集合保持子键布局, 小幅增减不切换.
//!
//! 其它 ZSET 命令仍走整值路径 (`get_expanded` 展开), 写回时由 `put_value` 按 64 阈值决定布局.
//!
//! # 同步快路径
//!
//! 2a 命令经 `KvStorage::zset_*` 访问存储; 单机 AiDb 下不经 `spawn_blocking`. 集群适配器无同步路径, 回落为 async.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;

use super::helpers::{format_score, parse_score};
use super::ZSetCommands;
use crate::command::blocking::BlockingRegistry;
use crate::command::router;
use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::collection::{CollectionWrite, HeaderWrite, ZSET_DEMOTE_MEMBERS};
use crate::storage::subkey::{encode_zset_score, zset_score_sub, ScoreSpan, SubTag};
use crate::storage::{CollectionKind, StoredValue, ValueType};

/// `(count, version)` 为子键布局; `None` 为内联整值. 类型不符返回 WRONGTYPE.
pub(super) fn subkey_header(stored: &StoredValue) -> Result<Option<(u64, u64)>> {
    match &stored.value {
        ValueType::ZSet(_) => Ok(None),
        ValueType::CollectionHeader {
            kind: CollectionKind::ZSet,
            count,
            version,
        } => Ok(Some((*count, *version))),
        _ => Err(router::wrongtype()),
    }
}

/// 追加 member 的 `Z` 与 `R` 写入.
pub(super) fn push_member_puts(
    puts: &mut Vec<(SubTag, Vec<u8>, Vec<u8>)>,
    member: &[u8],
    score: f64,
) {
    puts.push((
        SubTag::ZMember,
        member.to_vec(),
        encode_zset_score(score).to_vec(),
    ));
    puts.push((SubTag::ZScore, zset_score_sub(score, member), Vec::new()));
}

impl ZSetCommands {
    pub async fn zadd(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZADD", args, 3)?;
        if !(args.len() - 1).is_multiple_of(2) {
            return Err(router::wrong_args("ZADD", ""));
        }
        let key = &args[0];
        let mut pairs = Vec::with_capacity((args.len() - 1) / 2);
        for chunk in args[1..].chunks(2) {
            pairs.push((chunk[1].to_vec(), parse_score(&chunk[0])?));
        }
        let _lock = self.key_lock.lock(key).await;
        let added = match self.storage.zset_load(db, key).await? {
            None => {
                self.zadd_inline(db, key, StoredValue::new_zset(BTreeMap::new()), pairs)
                    .await?
            }
            Some(stored) => match subkey_header(&stored)? {
                None => self.zadd_inline(db, key, stored, pairs).await?,
                Some((count, version)) => {
                    self.zadd_subkey(db, key, stored.expires_at, count, version, pairs)
                        .await?
                }
            },
        };
        BlockingRegistry::global().notify(key, RespValue::SimpleString("OK".into()));
        Ok(router::integer(added))
    }

    /// 内联整值路径: 修改整表后经 `rewrite_stored` 写回 (超过阈值时由 `put_value` 转为子键).
    async fn zadd_inline(
        &self,
        db: usize,
        key: &[u8],
        mut stored: StoredValue,
        pairs: Vec<(Vec<u8>, f64)>,
    ) -> Result<i64> {
        let zset = stored.as_zset_mut()?;
        let mut added = 0i64;
        for (member, score) in pairs {
            if !zset.contains_key(&member) {
                added += 1;
            }
            zset.insert(member, score);
        }
        self.storage.rewrite_stored(db, key, stored).await?;
        Ok(added)
    }

    /// 子键路径: 只读取待写成员的旧分数, 按差异提交 `Z`/`R` 与 header. 同名成员以后者为准.
    async fn zadd_subkey(
        &self,
        db: usize,
        key: &[u8],
        expires_at: Option<u64>,
        count: u64,
        version: u64,
        pairs: Vec<(Vec<u8>, f64)>,
    ) -> Result<i64> {
        let target: BTreeMap<Vec<u8>, f64> = pairs.into_iter().collect();
        let members: Vec<&[u8]> = target.keys().map(Vec::as_slice).collect();
        let current = self
            .storage
            .zset_subkey_scores(db, key, version, &members)
            .await?;
        let mut puts = Vec::new();
        let mut deletes = Vec::new();
        let mut added = 0u64;
        for ((member, score), old) in target.iter().zip(current) {
            match old {
                None => {
                    added += 1;
                    push_member_puts(&mut puts, member, *score);
                }
                Some(old) if old == *score => {}
                Some(old) => {
                    deletes.push((SubTag::ZScore, zset_score_sub(old, member)));
                    push_member_puts(&mut puts, member, *score);
                }
            }
        }
        if puts.is_empty() {
            return Ok(0);
        }
        self.storage
            .zset_commit(
                db,
                key,
                CollectionWrite {
                    header: HeaderWrite::Put(StoredValue {
                        value: ValueType::CollectionHeader {
                            kind: CollectionKind::ZSet,
                            count: count + added,
                            version,
                        },
                        expires_at,
                    }),
                    version,
                    puts,
                    deletes,
                },
            )
            .await?;
        Ok(added as i64)
    }

    pub async fn zrem(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("ZREM", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let Some(stored) = self.storage.zset_load(db, key).await? else {
            return Ok(router::integer(0));
        };
        let removed = match subkey_header(&stored)? {
            None => self.zrem_inline(db, key, stored, &args[1..]).await?,
            Some((count, version)) => {
                self.zrem_subkey(db, key, stored.expires_at, count, version, &args[1..])
                    .await?
            }
        };
        Ok(router::integer(removed))
    }

    async fn zrem_inline(
        &self,
        db: usize,
        key: &[u8],
        mut stored: StoredValue,
        members: &[Bytes],
    ) -> Result<i64> {
        let zset = stored.as_zset_mut()?;
        let mut count = 0i64;
        for member in members {
            if zset.remove(member.as_ref()).is_some() {
                count += 1;
            }
        }
        if zset.is_empty() {
            self.storage.delete(db, key).await?;
        } else {
            self.storage.rewrite_stored(db, key, stored).await?;
        }
        Ok(count)
    }

    /// 子键路径: 删除存在成员的 `Z`/`R`; 剩余成员数 ≤ 32 时回落为内联 (见模块文档).
    async fn zrem_subkey(
        &self,
        db: usize,
        key: &[u8],
        expires_at: Option<u64>,
        count: u64,
        version: u64,
        members: &[Bytes],
    ) -> Result<i64> {
        let unique: BTreeSet<&[u8]> = members.iter().map(|m| m.as_ref()).collect();
        let subs: Vec<&[u8]> = unique.into_iter().collect();
        let current = self
            .storage
            .zset_subkey_scores(db, key, version, &subs)
            .await?;
        let mut deletes = Vec::new();
        let mut removed = 0u64;
        for (member, score) in subs.iter().zip(current) {
            if let Some(score) = score {
                removed += 1;
                deletes.push((SubTag::ZMember, member.to_vec()));
                deletes.push((SubTag::ZScore, zset_score_sub(score, member)));
            }
        }
        if removed == 0 {
            return Ok(0);
        }
        let remaining = count.saturating_sub(removed);
        if remaining > 0 && remaining as usize <= ZSET_DEMOTE_MEMBERS {
            return self.demote_subkey_zset(db, key, &subs, removed).await;
        }
        let header = if remaining == 0 {
            HeaderWrite::Delete {
                had_ttl: expires_at.is_some(),
            }
        } else {
            HeaderWrite::Put(StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::ZSet,
                    count: remaining,
                    version,
                },
                expires_at,
            })
        };
        self.storage
            .zset_commit(
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
        Ok(removed as i64)
    }

    /// ZINCRBY 子键路径: 读取旧分数后写入新分数 (同一 header 版本内增量提交). 返回新分数.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn zincrby_subkey(
        &self,
        db: usize,
        key: &[u8],
        expires_at: Option<u64>,
        count: u64,
        version: u64,
        member: &[u8],
        increment: f64,
    ) -> Result<f64> {
        let old = self
            .storage
            .zset_subkey_scores(db, key, version, &[member])
            .await?
            .pop()
            .flatten();
        let new_score = old.unwrap_or(0.0) + increment;
        if old == Some(new_score) {
            return Ok(new_score);
        }
        let mut puts = Vec::new();
        let mut deletes = Vec::new();
        push_member_puts(&mut puts, member, new_score);
        if let Some(old) = old {
            deletes.push((SubTag::ZScore, zset_score_sub(old, member)));
        }
        let count = if old.is_none() { count + 1 } else { count };
        self.storage
            .zset_commit(
                db,
                key,
                CollectionWrite {
                    header: HeaderWrite::Put(StoredValue {
                        value: ValueType::CollectionHeader {
                            kind: CollectionKind::ZSet,
                            count,
                            version,
                        },
                        expires_at,
                    }),
                    version,
                    puts,
                    deletes,
                },
            )
            .await?;
        Ok(new_score)
    }

    /// 子键区间读取 (供 ZRANGE/ZRANGEBYSCORE/ZCOUNT 使用): 分数区间内按序取 `offset..offset+limit`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn range_subkey(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        span: &ScoreSpan,
        reverse: bool,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<Vec<(Vec<u8>, f64)>> {
        self.storage
            .zset_range(db, key, version, span, reverse, offset, limit)
            .await
    }

    /// ZPOPMIN/ZPOPMAX 子键路径: 只从 `R` 索引两端取 `want` 个成员并删除, 不展开整表.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn pop_subkey(
        &self,
        db: usize,
        key: &[u8],
        expires_at: Option<u64>,
        count: u64,
        version: u64,
        want: usize,
        max: bool,
    ) -> Result<RespValue> {
        let picked = self
            .range_subkey(db, key, version, &ScoreSpan::FULL, max, 0, Some(want))
            .await?;
        if picked.is_empty() {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let mut deletes = Vec::with_capacity(picked.len() * 2);
        let mut items = Vec::with_capacity(picked.len() * 2);
        for (member, score) in &picked {
            deletes.push((SubTag::ZMember, member.clone()));
            deletes.push((SubTag::ZScore, zset_score_sub(*score, member)));
            items.push(router::bulk(member.clone()));
            items.push(router::bulk(format_score(*score).into_bytes()));
        }
        let remaining = count.saturating_sub(picked.len() as u64);
        if remaining > 0 && remaining as usize <= ZSET_DEMOTE_MEMBERS {
            let members: Vec<&[u8]> = picked.iter().map(|(m, _)| m.as_slice()).collect();
            self.demote_subkey_zset(db, key, &members, picked.len() as u64)
                .await?;
            return Ok(RespValue::Array(Some(items)));
        }
        let header = if remaining == 0 {
            HeaderWrite::Delete {
                had_ttl: expires_at.is_some(),
            }
        } else {
            HeaderWrite::Put(StoredValue {
                value: ValueType::CollectionHeader {
                    kind: CollectionKind::ZSet,
                    count: remaining,
                    version,
                },
                expires_at,
            })
        };
        self.storage
            .zset_commit(
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
        Ok(RespValue::Array(Some(items)))
    }

    /// 子键 → 内联回落 (低频路径, 走 async): 展开整表, 删除成员后以内联整值写回.
    async fn demote_subkey_zset(
        &self,
        db: usize,
        key: &[u8],
        members: &[&[u8]],
        removed: u64,
    ) -> Result<i64> {
        let Some(mut full) = self.storage.get_expanded(db, key).await? else {
            return Ok(removed as i64);
        };
        let zset = full.as_zset_mut()?;
        for member in members {
            zset.remove(*member);
        }
        self.storage.rewrite_stored(db, key, full).await?;
        Ok(removed as i64)
    }

    pub async fn zscore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZSCORE", args, 2)?;
        let member = args[1].as_ref();
        let Some(stored) = self.storage.zset_load(db, &args[0]).await? else {
            return Ok(router::nil_bulk());
        };
        let score = match subkey_header(&stored)? {
            None => stored.as_zset()?.get(member).copied(),
            Some((_, version)) => self
                .storage
                .zset_subkey_scores(db, &args[0], version, &[member])
                .await?
                .pop()
                .flatten(),
        };
        Ok(match score {
            None => router::nil_bulk(),
            Some(score) => router::bulk(format_score(score).into_bytes()),
        })
    }

    pub async fn zcard(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("ZCARD", args, 1)?;
        let Some(stored) = self.storage.zset_load(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let len = match subkey_header(&stored)? {
            None => stored.as_zset()?.len() as i64,
            Some((count, _)) => count as i64,
        };
        Ok(router::integer(len))
    }
}
