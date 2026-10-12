//! `KvStorageAdapter` 的集合 (subkey 布局) 读写.

use crate::error::Result;
use crate::storage::adapter::{AdapterWriteOp, KvStorageAdapter};
use crate::storage::collection::{
    next_collection_version, CollectionWrite, HeaderWrite, HASH_MAX_INLINE_FIELDS,
    SET_MAX_INLINE_MEMBERS,
};
use crate::storage::subkey::{encode_subkey, subkey_version_prefix};
use crate::storage::types::{CollectionKind, StoredValue, ValueType};

/// 暂存写每批条数; 远低于集群批处理器的 512 条.
const STAGE_CHUNK: usize = 256;

/// 集合展开后的 `(sub, value)` 列表.
type Entries = Vec<(Vec<u8>, Vec<u8>)>;

impl KvStorageAdapter {
    pub(super) async fn subkey_get_many_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
        subs: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.check_db(db)?;
        let mut out = Vec::with_capacity(subs.len());
        for sub in subs {
            out.push(
                self.storage
                    .get(encode_subkey(db, key, version, kind, sub))
                    .await?,
            );
        }
        Ok(out)
    }

    pub(super) async fn subkey_scan_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.check_db(db)?;
        let prefix = subkey_version_prefix(db, key, version, kind);
        let skip = prefix.len();
        let mut rows = self.storage.scan_prefix(&prefix).await?;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows
            .into_iter()
            .map(|(physical, value)| (physical[skip..].to_vec(), value))
            .collect())
    }

    pub(super) async fn commit_collection_impl(
        &self,
        db: usize,
        key: &[u8],
        write: CollectionWrite,
    ) -> Result<()> {
        self.check_db(db)?;
        let CollectionWrite {
            header,
            version,
            kind,
            puts,
            deletes,
        } = write;
        let mut ops = Vec::with_capacity(puts.len() + deletes.len() + 2);
        for (sub, value) in puts {
            ops.push(AdapterWriteOp::Put {
                key: encode_subkey(db, key, version, kind, &sub),
                value,
            });
        }
        for sub in deletes {
            ops.push(AdapterWriteOp::Delete {
                key: encode_subkey(db, key, version, kind, &sub),
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

    /// 新版本 subkey 分批写入; 调用方随后再写 header.
    pub(super) async fn stage_subkeys(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
        entries: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<()> {
        for chunk in entries.chunks(STAGE_CHUNK) {
            let ops = chunk
                .iter()
                .map(|(sub, value)| AdapterWriteOp::Put {
                    key: encode_subkey(db, key, version, kind, sub),
                    value: value.clone(),
                })
                .collect();
            self.storage.apply_writes(ops).await?;
        }
        Ok(())
    }

    /// 超过阈值的 Hash/Set 转为新版本 header, 其余原样返回.
    pub(super) async fn stage_if_large(
        &self,
        db: usize,
        key: &[u8],
        value: StoredValue,
    ) -> Result<StoredValue> {
        let StoredValue { value, expires_at } = value;
        let (kind, entries): (CollectionKind, Entries) = match value {
            ValueType::Hash(map) if map.len() > HASH_MAX_INLINE_FIELDS => {
                (CollectionKind::Hash, map.into_iter().collect())
            }
            ValueType::Set(set) if set.len() > SET_MAX_INLINE_MEMBERS => (
                CollectionKind::Set,
                set.into_iter().map(|member| (member, Vec::new())).collect(),
            ),
            other => {
                return Ok(StoredValue {
                    value: other,
                    expires_at,
                })
            }
        };
        let version = next_collection_version();
        let count = entries.len() as u64;
        self.stage_subkeys(db, key, version, kind, entries).await?;
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
        let rows = self.subkey_scan_impl(db, key, version, kind).await?;
        let value = match kind {
            CollectionKind::Hash => ValueType::Hash(rows.into_iter().collect()),
            CollectionKind::Set => {
                ValueType::Set(rows.into_iter().map(|(member, _)| member).collect())
            }
        };
        Ok(Some(StoredValue {
            value,
            expires_at: stored.expires_at,
        }))
    }

    /// subkey 布局的值复制为目标 key 的新版本 (暂存写), 返回目标 header; 其它值原样返回.
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
        let rows = self
            .subkey_scan_impl(src_db, src_key, version, kind)
            .await?;
        let new_version = next_collection_version();
        self.stage_subkeys(dst_db, dst_key, new_version, kind, rows)
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
