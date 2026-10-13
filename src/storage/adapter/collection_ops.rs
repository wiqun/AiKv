//! `KvStorageAdapter` 的集合 (subkey 布局) 读写.
//!
//! Hash/Set 走 `subkey_*` / `commit_collection` 通用路径. ZSet 额外提供同步快路径
//! (`*_sync`), 仅在 `StorageAdapter::sync_kv` 可用时由 `KvStorage::zset_*` 调用, 不经 spawn_blocking.

use std::collections::{BTreeMap, HashMap};

use crate::error::Result;
use crate::storage::adapter::{AdapterWriteOp, KvStorageAdapter, SyncKv};
use crate::storage::collection::{
    next_collection_version, CollectionWrite, HeaderWrite, ZSetDelta, HASH_MAX_INLINE_FIELDS,
    SET_MAX_INLINE_MEMBERS, ZSET_MAX_INLINE_MEMBERS,
};
use crate::storage::subkey::{
    decode_zset_score_or_err, decode_zset_score_sub, encode_subkey, encode_zset_score, subkey_tags,
    subkey_version_prefix, zset_score_range_keys, zset_score_sub, ScoreSpan, SubTag,
};
use crate::storage::types::{CollectionKind, StoredValue, ValueType};
use crate::storage::watch_version::{
    decode_version, encode_version, is_watch_meta_user_key, meta_user_key,
};

/// 暂存写每批条数; 远低于集群批处理器的 512 条.
const STAGE_CHUNK: usize = 256;

/// `zset_range` 的收集状态: 跳过 `skip` 条后收集至多 `left` 条 (None 为不限).
struct RangeCollector {
    prefix_len: usize,
    skip: usize,
    left: Option<usize>,
    out: Vec<(Vec<u8>, f64)>,
}

impl RangeCollector {
    /// 处理一条 `R` 物理 key; 返回 false 表示已满足 limit, 调用方应停止.
    fn push(&mut self, physical: &[u8]) -> bool {
        if self.skip > 0 {
            self.skip -= 1;
            return true;
        }
        let Some((score, member)) = decode_zset_score_sub(&physical[self.prefix_len..]) else {
            return true;
        };
        self.out.push((member.to_vec(), score));
        match self.left.as_mut() {
            Some(left) => {
                *left -= 1;
                *left > 0
            }
            None => true,
        }
    }
}

/// 集合展开后的 `(tag, sub, value)` 列表.
type Entries = Vec<(SubTag, Vec<u8>, Vec<u8>)>;

/// 超过内联阈值的集合拆为 subkey 列表 `(kind, entries, count)`; 未超过则原样返回.
fn large_entries(
    value: ValueType,
) -> std::result::Result<(CollectionKind, Entries, u64), ValueType> {
    match value {
        ValueType::Hash(map) if map.len() > HASH_MAX_INLINE_FIELDS => {
            let count = map.len() as u64;
            let entries = map
                .into_iter()
                .map(|(field, value)| (SubTag::Hash, field, value))
                .collect();
            Ok((CollectionKind::Hash, entries, count))
        }
        ValueType::Set(set) if set.len() > SET_MAX_INLINE_MEMBERS => {
            let count = set.len() as u64;
            let entries = set
                .into_iter()
                .map(|member| (SubTag::Set, member, Vec::new()))
                .collect();
            Ok((CollectionKind::Set, entries, count))
        }
        ValueType::ZSet(zset) if zset.len() > ZSET_MAX_INLINE_MEMBERS => {
            let count = zset.len() as u64;
            let mut entries = Vec::with_capacity(zset.len() * 2);
            for (member, score) in zset {
                let score_sub = zset_score_sub(score, &member);
                entries.push((SubTag::ZMember, member, encode_zset_score(score).to_vec()));
                entries.push((SubTag::ZScore, score_sub, Vec::new()));
            }
            Ok((CollectionKind::ZSet, entries, count))
        }
        other => Err(other),
    }
}

impl KvStorageAdapter {
    pub(super) async fn subkey_get_many_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        tag: SubTag,
        subs: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.check_db(db)?;
        let mut out = Vec::with_capacity(subs.len());
        for sub in subs {
            out.push(
                self.storage
                    .get(encode_subkey(db, key, version, tag, sub))
                    .await?,
            );
        }
        Ok(out)
    }

    /// ZSET 子键布局按分数区间读取 `(member, score)`, 方向由 `reverse` 决定.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn zset_range_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        span: &ScoreSpan,
        reverse: bool,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<Vec<(Vec<u8>, f64)>> {
        self.check_db(db)?;
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let Some((start, end)) = zset_score_range_keys(db, key, version, span) else {
            return Ok(Vec::new());
        };
        let collector = std::sync::Arc::new(std::sync::Mutex::new(RangeCollector {
            prefix_len: subkey_version_prefix(db, key, version, SubTag::ZScore).len(),
            skip: offset,
            left: limit,
            out: Vec::new(),
        }));
        match self.storage.sync_kv() {
            Some(kv) => {
                let sink = std::sync::Arc::clone(&collector);
                kv.visit_range_sync(&start, &end, reverse, &mut |physical, _| {
                    Ok(sink.lock().unwrap().push(physical))
                })?;
            }
            None => {
                let sink = std::sync::Arc::clone(&collector);
                self.storage
                    .visit_range_dir(
                        start,
                        end,
                        reverse,
                        Box::new(move |physical, _| Ok(sink.lock().unwrap().push(&physical))),
                    )
                    .await?;
            }
        }
        let mut guard = collector.lock().unwrap();
        Ok(std::mem::take(&mut guard.out))
    }

    pub(super) async fn subkey_scan_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        tag: SubTag,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.check_db(db)?;
        let prefix = subkey_version_prefix(db, key, version, tag);
        let skip = prefix.len();
        let mut rows = self.storage.scan_prefix(&prefix).await?;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows
            .into_iter()
            .map(|(physical, value)| (physical[skip..].to_vec(), value))
            .collect())
    }

    /// 把一次提交翻译为扁平 KV 写操作; 返回 (ops, 是否清掉了 header 上的 TTL).
    fn collection_write_ops(
        db: usize,
        key: &[u8],
        write: CollectionWrite,
    ) -> Result<(Vec<AdapterWriteOp>, bool)> {
        let CollectionWrite {
            header,
            version,
            puts,
            deletes,
        } = write;
        let mut ops = Vec::with_capacity(puts.len() + deletes.len() + 2);
        for (tag, sub, value) in puts {
            ops.push(AdapterWriteOp::Put {
                key: encode_subkey(db, key, version, tag, &sub),
                value,
            });
        }
        for (tag, sub) in deletes {
            ops.push(AdapterWriteOp::Delete {
                key: encode_subkey(db, key, version, tag, &sub),
            });
        }
        let cleared_ttl = match header {
            HeaderWrite::Put(stored) => {
                ops.push(AdapterWriteOp::Put {
                    key: Self::encode(db, key),
                    value: Self::serialize(&stored)?,
                });
                false
            }
            HeaderWrite::Delete { had_ttl } => {
                ops.push(AdapterWriteOp::Delete {
                    key: Self::encode(db, key),
                });
                had_ttl
            }
        };
        Ok((ops, cleared_ttl))
    }

    /// ZSET 增量提交的扁平写操作. 返回 (ops, header 带 TTL, 清掉了 header 的 TTL).
    /// 视图成员数落入内联阈值时降级为内联整值, 与 `ZREM` 的回落规则一致.
    pub(super) async fn zset_delta_ops(
        &self,
        db: usize,
        key: &[u8],
        delta: ZSetDelta,
    ) -> Result<(Vec<AdapterWriteOp>, bool, bool)> {
        if delta.demotes() {
            let rows = self
                .subkey_scan_impl(db, key, delta.version, SubTag::ZMember)
                .await?;
            let mut zset = BTreeMap::new();
            for (member, raw) in rows {
                zset.insert(member, decode_zset_score_or_err(&raw)?);
            }
            for (member, change) in &delta.members {
                match change.current {
                    Some(score) => zset.insert(member.clone(), score),
                    None => zset.remove(member),
                };
            }
            let stored = StoredValue {
                value: ValueType::ZSet(zset),
                expires_at: delta.expires_at,
            };
            let header_ttl = stored.expires_at.is_some();
            let op = AdapterWriteOp::Put {
                key: Self::encode(db, key),
                value: Self::serialize(&stored)?,
            };
            return Ok((vec![op], header_ttl, false));
        }
        let write = delta.into_collection_write();
        let header_ttl = matches!(
            &write.header,
            HeaderWrite::Put(StoredValue {
                expires_at: Some(_),
                ..
            })
        );
        let (ops, cleared_ttl) = Self::collection_write_ops(db, key, write)?;
        Ok((ops, header_ttl, cleared_ttl))
    }

    pub(super) async fn commit_collection_impl(
        &self,
        db: usize,
        key: &[u8],
        write: CollectionWrite,
    ) -> Result<()> {
        self.check_db(db)?;
        let (mut ops, cleared_ttl) = Self::collection_write_ops(db, key, write)?;
        if let Some((meta_key, meta)) = self.next_watch_meta_put(db, key).await? {
            ops.push(AdapterWriteOp::Put {
                key: meta_key,
                value: meta,
            });
        }
        self.storage.apply_writes(ops).await?;
        if cleared_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(())
    }

    /// `commit_collection_impl` 的同步版本: 直接经 `SyncKv` 写入, 不 spawn.
    pub(super) fn commit_collection_sync(
        &self,
        kv: &dyn SyncKv,
        db: usize,
        key: &[u8],
        write: CollectionWrite,
    ) -> Result<()> {
        self.check_db(db)?;
        let (mut ops, cleared_ttl) = Self::collection_write_ops(db, key, write)?;
        if let Some((meta_key, meta)) = self.next_watch_meta_put_sync(kv, db, key)? {
            ops.push(AdapterWriteOp::Put {
                key: meta_key,
                value: meta,
            });
        }
        kv.write_sync(ops)?;
        if cleared_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(())
    }

    /// `next_watch_meta_put` 的同步版本.
    fn next_watch_meta_put_sync(
        &self,
        kv: &dyn SyncKv,
        db: usize,
        user_key: &[u8],
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        if is_watch_meta_user_key(user_key) || !self.watchers.is_watched(db, user_key) {
            return Ok(None);
        }
        let meta = meta_user_key(user_key);
        let current = match kv.get_sync(&Self::encode(db, &meta))? {
            Some(bytes) => decode_version(&bytes),
            None => 0,
        };
        Ok(Some((
            Self::encode(db, &meta),
            encode_version(current.saturating_add(1)).to_vec(),
        )))
    }

    /// ZSet 子键布局下一组 member 的分数 (同步版本, 按入参顺序).
    pub(super) fn zset_subkey_scores_sync(
        &self,
        kv: &dyn SyncKv,
        db: usize,
        key: &[u8],
        version: u64,
        members: &[&[u8]],
    ) -> Result<Vec<Option<f64>>> {
        self.check_db(db)?;
        let mut out = Vec::with_capacity(members.len());
        for member in members {
            let physical = encode_subkey(db, key, version, SubTag::ZMember, member);
            out.push(match kv.get_sync(&physical)? {
                Some(raw) => Some(decode_zset_score_or_err(&raw)?),
                None => None,
            });
        }
        Ok(out)
    }

    /// 新版本 subkey 分批写入; 调用方随后再写 header.
    pub(super) async fn stage_subkeys(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        entries: Entries,
    ) -> Result<()> {
        let mut rest = entries.into_iter();
        loop {
            let chunk: Vec<_> = rest.by_ref().take(STAGE_CHUNK).collect();
            if chunk.is_empty() {
                return Ok(());
            }
            let ops = chunk
                .into_iter()
                .map(|(tag, sub, value)| AdapterWriteOp::Put {
                    key: encode_subkey(db, key, version, tag, &sub),
                    value,
                })
                .collect();
            self.storage.apply_writes(ops).await?;
        }
    }

    /// 超过阈值的 Hash/Set/ZSet 转为新版本 header, 其余原样返回.
    pub(super) async fn stage_if_large(
        &self,
        db: usize,
        key: &[u8],
        value: StoredValue,
    ) -> Result<StoredValue> {
        let StoredValue { value, expires_at } = value;
        let (kind, entries, count) = match large_entries(value) {
            Ok(large) => large,
            Err(inline) => {
                return Ok(StoredValue {
                    value: inline,
                    expires_at,
                })
            }
        };
        let version = next_collection_version();
        self.stage_subkeys(db, key, version, entries).await?;
        Ok(StoredValue {
            value: ValueType::CollectionHeader {
                kind,
                count,
                version,
            },
            expires_at,
        })
    }

    pub(super) async fn put_value_impl(
        &self,
        db: usize,
        key: &[u8],
        value: StoredValue,
    ) -> Result<()> {
        self.check_db(db)?;
        let value = self.stage_if_large(db, key, value).await?;
        self.set_typed_raw(db, key, value).await
    }

    pub(super) async fn get_expanded_impl(
        &self,
        db: usize,
        key: &[u8],
    ) -> Result<Option<StoredValue>> {
        let Some(stored) = self.load_typed(db, key).await? else {
            return Ok(None);
        };
        let ValueType::CollectionHeader { kind, version, .. } = stored.value else {
            return Ok(Some(stored));
        };
        let value = match kind {
            CollectionKind::Hash => {
                let rows = self
                    .subkey_scan_impl(db, key, version, SubTag::Hash)
                    .await?;
                ValueType::Hash(rows.into_iter().collect::<HashMap<_, _>>())
            }
            CollectionKind::Set => {
                let rows = self.subkey_scan_impl(db, key, version, SubTag::Set).await?;
                ValueType::Set(rows.into_iter().map(|(member, _)| member).collect())
            }
            CollectionKind::ZSet => {
                let rows = self
                    .subkey_scan_impl(db, key, version, SubTag::ZMember)
                    .await?;
                let mut zset = BTreeMap::new();
                for (member, raw) in rows {
                    zset.insert(member, decode_zset_score_or_err(&raw)?);
                }
                ValueType::ZSet(zset)
            }
        };
        Ok(Some(StoredValue {
            value,
            expires_at: stored.expires_at,
        }))
    }

    /// subkey 布局的值复制为目标 key 的新版本 (暂存写), 返回目标 header; 其它值原样返回.
    /// 复制该集合的全部标签 (ZSet 同时复制 `Z` 与 `R`).
    async fn restage(
        &self,
        src_db: usize,
        src_key: &[u8],
        dst_db: usize,
        dst_key: &[u8],
        value: StoredValue,
    ) -> Result<StoredValue> {
        let ValueType::CollectionHeader {
            kind,
            count,
            version,
        } = value.value
        else {
            return Ok(value);
        };
        let mut entries: Entries = Vec::new();
        for tag in subkey_tags(kind) {
            for (sub, raw) in self
                .subkey_scan_impl(src_db, src_key, version, *tag)
                .await?
            {
                entries.push((*tag, sub, raw));
            }
        }
        let new_version = next_collection_version();
        self.stage_subkeys(dst_db, dst_key, new_version, entries)
            .await?;
        Ok(StoredValue {
            value: ValueType::CollectionHeader {
                kind,
                count,
                version: new_version,
            },
            expires_at: value.expires_at,
        })
    }

    /// RENAME / COPY / MOVE 的统一实现. 目标原有 subkey 与源旧版本 subkey 都交给 GC.
    pub(super) async fn transfer_key(
        &self,
        src_db: usize,
        dst_db: usize,
        old_key: &[u8],
        new_key: &[u8],
        value: StoredValue,
        delete_source: bool,
    ) -> Result<()> {
        self.check_db(src_db)?;
        self.check_db(dst_db)?;
        let value = self
            .restage(src_db, old_key, dst_db, new_key, value)
            .await?;
        let had_ttl = value.expires_at.is_some();
        let mut ops = vec![AdapterWriteOp::Put {
            key: Self::encode(dst_db, new_key),
            value: Self::serialize(&value)?,
        }];
        if let Some((key, meta)) = self.next_watch_meta_put(dst_db, new_key).await? {
            ops.push(AdapterWriteOp::Put { key, value: meta });
        }
        if delete_source {
            ops.push(AdapterWriteOp::Delete {
                key: Self::encode(src_db, old_key),
            });
            if let Some((key, meta)) = self.next_watch_meta_put(src_db, old_key).await? {
                ops.push(AdapterWriteOp::Put { key, value: meta });
            }
        }
        self.storage.apply_writes(ops).await?;
        if had_ttl {
            self.expires.on_ttl_write(dst_db);
            if delete_source {
                self.expires.on_ttl_clear(src_db);
            }
        }
        Ok(())
    }

    pub(super) async fn move_key_impl(
        &self,
        src_db: usize,
        dst_db: usize,
        key: &[u8],
    ) -> Result<bool> {
        let Some(value) = self.load_typed(src_db, key).await? else {
            return Ok(false);
        };
        if self.load_typed(dst_db, key).await?.is_some() {
            return Ok(false);
        }
        self.transfer_key(src_db, dst_db, key, key, value, true)
            .await?;
        Ok(true)
    }

    /// SWAPDB: 先把两边的大集合以新版本暂存到对侧, 再只清主键区间并写回.
    /// 清区间与写回之间不原子, 与原实现一致.
    pub(super) async fn swap_db_impl(&self, a: usize, b: usize) -> Result<()> {
        let entries_a = self.collect_db_entries(a).await?;
        let entries_b = self.collect_db_entries(b).await?;
        let mut into_b = Vec::with_capacity(entries_a.len());
        for (key, value) in entries_a {
            into_b.push((key.clone(), self.restage(a, &key, b, &key, value).await?));
        }
        let mut into_a = Vec::with_capacity(entries_b.len());
        for (key, value) in entries_b {
            into_a.push((key.clone(), self.restage(b, &key, a, &key, value).await?));
        }
        self.clear_main_range(a).await?;
        self.clear_main_range(b).await?;
        for (key, value) in into_a {
            self.set_typed_raw(a, &key, value).await?;
        }
        for (key, value) in into_b {
            self.set_typed_raw(b, &key, value).await?;
        }
        Ok(())
    }
}
