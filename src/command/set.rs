//! Set 命令: SADD/SREM/SMEMBERS/SISMEMBER/SCARD/SPOP/SRANDMEMBER/SMOVE/SSCAN,
//! 以及集合运算 SUNION/SINTER/SDIFF 与其 *STORE 变体.
//!
//! # 存储表示 (双格式)
//!
//! ```text
//! 小 set (≤ 64 members): ValueType::Set(HashSet<member>)
//!                        postcard 存于 StoredValue
//! 大 set (> 64 members): ValueType::CollectionHeader { kind: Set, count }
//!                        member 独立 subkey:
//!                        {encoded_user_key}\x01S{member_len:2B}{member} → 空值 (仅存在性)
//! ```
//!
//! 写路径: `key_lock.lock(key)` → `load_or_create_set` → 修改 → 超过
//! `SET_MAX_INLINE_MEMBERS` (64) 时 `migrate_set_to_subkey`; 删空后 `delete`.
//! SMOVE 跨源/目标双 key 用 `lock_two`; store 类集合运算读多 key 不加锁.
//! 类型分轨: `get_typed`/`set_typed`; 非 Set → WRONGTYPE.

use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use rand::seq::SliceRandom;
use tracing::instrument;

use crate::command::router::{self, KeyLock};
use crate::command::scan_util;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::memory::glob_match;
use crate::storage::subkey;
use crate::storage::{AiDbEngine, CollectionKind, KvStorage, StoredValue, ValueType};

/// 小集合 inline 编码于 StoredValue; 超过此阈值自动切到 subkey 格式.
const SET_MAX_INLINE_MEMBERS: usize = 64;

pub struct SetCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
}

impl SetCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self { storage, key_lock }
    }

    #[instrument(level = "debug", name = "cmd_set", skip(self, args), fields(cmd.name = "SADD"))]
    pub async fn sadd(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SADD", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let mut stored = self.load_or_create_set(db, key).await?;

        match &mut stored.value {
            ValueType::Set(ref mut set) => {
                let mut count = 0i64;
                for member in &args[1..] {
                    if set.insert(member.to_vec()) {
                        count += 1;
                    }
                }
                if set.len() > SET_MAX_INLINE_MEMBERS {
                    self.migrate_set_to_subkey(db, key, &stored.expires_at, set)
                        .await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(count))
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ref mut count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                let mut new_members = 0i64;
                for member in &args[1..] {
                    let subkey_key =
                        subkey::encode_set_member_key(&encoded_user_key, member.as_ref());
                    let existed = self
                        .storage
                        .raw_subkey_get(db, subkey_key.clone())
                        .await
                        .unwrap_or_default()
                        .is_some();
                    if !existed {
                        new_members += 1;
                        *count += 1;
                    }
                    // Set member 值用空字节 (仅 key 存在性)
                    self.storage.raw_subkey_set(db, subkey_key, vec![]).await?;
                }
                self.storage.rewrite_stored(db, key, stored).await?;
                Ok(router::integer(new_members))
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn srem(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SREM", args, 2)?;
        let key = &args[0];
        let _lock = self.key_lock.lock(key).await;
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::integer(0));
        };

        match &mut stored.value {
            ValueType::Set(ref mut set) => {
                let mut count = 0i64;
                for member in &args[1..] {
                    if set.remove(member.as_ref()) {
                        count += 1;
                    }
                }
                if set.is_empty() {
                    self.storage.delete(db, key).await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(count))
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ref mut count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                let mut removed = 0i64;
                for member in &args[1..] {
                    let subkey_key =
                        subkey::encode_set_member_key(&encoded_user_key, member.as_ref());
                    if self
                        .storage
                        .raw_subkey_delete(db, subkey_key)
                        .await
                        .unwrap_or_default()
                    {
                        removed += 1;
                    }
                }
                *count = count.saturating_sub(removed as u32);
                if *count == 0 {
                    self.storage.delete(db, key).await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(removed))
            }
            _ => Err(router::wrongtype()),
        }
    }

    pub async fn sismember(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SISMEMBER", args, 2)?;
        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(router::integer(0));
        };
        let exists = match &stored.value {
            ValueType::Set(set) => set.contains(args[1].as_ref()),
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                let subkey_key = subkey::encode_set_member_key(&encoded_user_key, args[1].as_ref());
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

    pub async fn smembers(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SMEMBERS", args, 1)?;
        let set = self.load_set_full(db, &args[0]).await?;
        Ok(array_of_bulk(set.into_iter().collect()))
    }

    pub async fn scard(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SCARD", args, 1)?;
        let len = match self.storage.get_typed(db, &args[0]).await? {
            None => 0,
            Some(stored) => match stored.value {
                ValueType::Set(set) => set.len() as i64,
                ValueType::CollectionHeader {
                    kind: CollectionKind::Set,
                    count,
                } => count as i64,
                _ => return Err(router::wrongtype()),
            },
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
        let Some(mut stored) = self.storage.get_typed(db, key).await? else {
            return Ok(router::nil_bulk());
        };
        let count = count.unwrap_or(1);
        if count == 0 {
            return Ok(RespValue::Array(Some(vec![])));
        }

        let mut members: Vec<Vec<u8>> = match &stored.value {
            ValueType::Set(set) => set.iter().cloned().collect(),
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                self.scan_set_subkeys(db, &encoded_user_key, None).await?
            }
            _ => return Err(router::wrongtype()),
        };
        members.shuffle(&mut rand::thread_rng());
        let n = (count as usize).min(members.len());
        let picked: Vec<Vec<u8>> = members.into_iter().take(n).collect();

        match &mut stored.value {
            ValueType::Set(ref mut set) => {
                for m in &picked {
                    set.remove(m);
                }
                if set.is_empty() {
                    self.storage.delete(db, key).await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ref mut count,
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                for m in &picked {
                    let subkey_key = subkey::encode_set_member_key(&encoded_user_key, m);
                    let _ = self.storage.raw_subkey_delete(db, subkey_key).await;
                    *count = count.saturating_sub(1);
                }
                if *count == 0 {
                    self.storage.delete(db, key).await?;
                } else {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
            }
            _ => unreachable!(),
        }

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
        let set = self.load_set_full(db, &args[0]).await?;
        if set.is_empty() {
            return if count.is_some() {
                Ok(RespValue::Array(Some(vec![])))
            } else {
                Ok(router::nil_bulk())
            };
        }
        let mut members: Vec<Vec<u8>> = set.into_iter().collect();
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
        let dest = &args[0];
        let result = self.compute_union(db, &args[1..]).await?;
        self.store_set(db, dest, result).await
    }

    pub async fn sinterstore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SINTERSTORE", args, 2)?;
        let dest = &args[0];
        let result = self.compute_inter(db, &args[1..]).await?;
        self.store_set(db, dest, result).await
    }

    pub async fn sdiffstore(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SDIFFSTORE", args, 2)?;
        let dest = &args[0];
        let result = self.compute_diff(db, &args[1..]).await?;
        self.store_set(db, dest, result).await
    }

    #[instrument(level = "debug", name = "cmd_set", skip(self, args), fields(cmd.name = "SMOVE"))]
    pub async fn smove(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("SMOVE", args, 3)?;
        let source = &args[0];
        let dest = &args[1];
        let member = args[2].to_vec();

        let (_lock_a, _lock_b) = self.key_lock.lock_two(source, dest).await;
        let same_key = source == dest;

        let Some(mut src_stored) = self.storage.get_typed(db, source).await? else {
            return Ok(router::integer(0));
        };

        // 检查 source 中是否存在 member
        let exists_in_src = match &src_stored.value {
            ValueType::Set(set) => set.contains(&member),
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(db, source);
                let subkey_key = subkey::encode_set_member_key(&encoded, &member);
                self.storage
                    .raw_subkey_get(db, subkey_key)
                    .await
                    .unwrap_or_default()
                    .is_some()
            }
            _ => return Err(router::wrongtype()),
        };
        if !exists_in_src {
            return Ok(router::integer(0));
        }

        if same_key {
            return Ok(router::integer(1));
        }

        // 从 source 删除
        match &mut src_stored.value {
            ValueType::Set(ref mut set) => {
                set.remove(&member);
                if set.is_empty() {
                    self.storage.delete(db, source).await?;
                } else {
                    self.storage.rewrite_stored(db, source, src_stored).await?;
                }
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ref mut count,
            } => {
                let encoded = AiDbEngine::encode_key(db, source);
                let subkey_key = subkey::encode_set_member_key(&encoded, &member);
                self.storage.raw_subkey_delete(db, subkey_key).await?;
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.storage.delete(db, source).await?;
                } else {
                    self.storage.rewrite_stored(db, source, src_stored).await?;
                }
            }
            _ => unreachable!(),
        }

        // 添加到 dest
        let mut dest_stored = match self.storage.get_typed(db, dest).await? {
            None => StoredValue::new_set(HashSet::new()),
            Some(stored) => match stored.value {
                ValueType::Set(_) => stored,
                ValueType::CollectionHeader {
                    kind: CollectionKind::Set,
                    ..
                } => stored,
                _ => return Err(router::wrongtype()),
            },
        };

        match &mut dest_stored.value {
            ValueType::Set(ref mut set) => {
                set.insert(member);
                self.storage.rewrite_stored(db, dest, dest_stored).await?;
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ref mut count,
            } => {
                let encoded = AiDbEngine::encode_key(db, dest);
                let subkey_key = subkey::encode_set_member_key(&encoded, &member);
                self.storage.raw_subkey_set(db, subkey_key, vec![]).await?;
                *count += 1;
                self.storage.rewrite_stored(db, dest, dest_stored).await?;
            }
            _ => unreachable!(),
        }

        Ok(router::integer(1))
    }

    #[instrument(level = "debug", name = "cmd_set", skip(self, args), fields(cmd.name = "SSCAN"))]
    pub async fn sscan(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SSCAN", args, 2)?;
        let cursor = scan_util::parse_u64(&args[1])?;
        let opts = scan_util::parse_scan_options("SSCAN", args, 2)?;

        let Some(stored) = self.storage.get_typed(db, &args[0]).await? else {
            return Ok(scan_util::scan_response_bulk(0, &[]));
        };

        let members: Vec<Vec<u8>> = match stored.value {
            ValueType::Set(set) => {
                let mut m: Vec<Vec<u8>> = set
                    .into_iter()
                    .filter(|m| {
                        opts.pattern
                            .as_ref()
                            .is_none_or(|p| glob_match(p, m.as_slice()))
                    })
                    .collect();
                m.sort();
                m
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, &args[0]);
                let mut m = self
                    .scan_set_subkeys(db, &encoded_user_key, opts.pattern.as_deref())
                    .await?;
                m.sort();
                m
            }
            _ => return Err(router::wrongtype()),
        };

        let (next_cursor, page) = scan_util::paginate_slice(&members, cursor, opts.count);
        Ok(scan_util::scan_response_bulk(next_cursor, page))
    }

    // ---- helpers ----

    async fn store_set(&self, db: usize, dest: &Bytes, set: HashSet<Vec<u8>>) -> Result<RespValue> {
        let _lock = self.key_lock.lock(dest).await;
        if let Some(stored) = self.storage.get_typed(db, dest).await? {
            if !matches!(
                stored.value,
                ValueType::Set(_) | ValueType::CollectionHeader { .. }
            ) {
                return Err(router::wrongtype());
            }
        }
        let len = set.len() as i64;
        self.storage
            .set_typed(db, dest, StoredValue::new_set(set))
            .await?;
        Ok(router::integer(len))
    }

    async fn compute_union(&self, db: usize, keys: &[Bytes]) -> Result<HashSet<Vec<u8>>> {
        let mut result = HashSet::new();
        for key in keys {
            let set = self.load_set_full(db, key).await?;
            result.extend(set);
        }
        Ok(result)
    }

    async fn compute_inter(&self, db: usize, keys: &[Bytes]) -> Result<HashSet<Vec<u8>>> {
        let mut result: Option<HashSet<Vec<u8>>> = None;
        for key in keys {
            let set = self.load_set_full(db, key).await?;
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
        let mut result = self.load_set_full(db, &keys[0]).await?;
        for key in &keys[1..] {
            let set = self.load_set_full(db, key).await?;
            result.retain(|m| !set.contains(m));
        }
        Ok(result)
    }

    /// 加载 set 全部 members (兼容所有格式).
    async fn load_set_full(&self, db: usize, key: &[u8]) -> Result<HashSet<Vec<u8>>> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(HashSet::new());
        };
        match stored.value {
            ValueType::Set(set) => Ok(set),
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded_user_key = AiDbEngine::encode_key(db, key);
                Ok(self
                    .scan_set_subkeys(db, &encoded_user_key, None)
                    .await?
                    .into_iter()
                    .collect())
            }
            _ => Err(router::wrongtype()),
        }
    }

    /// 加载或创建 set (兼容所有格式).
    async fn load_or_create_set(&self, db: usize, key: &[u8]) -> Result<StoredValue> {
        match self.storage.get_typed(db, key).await? {
            None => Ok(StoredValue::new_set(HashSet::new())),
            Some(stored) => match stored.value {
                ValueType::Set(_) => Ok(stored),
                ValueType::CollectionHeader {
                    kind: CollectionKind::Set,
                    ..
                } => Ok(stored),
                _ => Err(router::wrongtype()),
            },
        }
    }

    /// 扫描 subkey set 的所有 members.
    async fn scan_set_subkeys(
        &self,
        db: usize,
        encoded_user_key: &[u8],
        pattern: Option<&[u8]>,
    ) -> Result<Vec<Vec<u8>>> {
        let prefix = subkey::set_subkey_prefix(encoded_user_key);
        let out = Arc::new(std::sync::Mutex::new(Vec::new()));
        let out_c = out.clone();
        let pattern = pattern.map(|p| p.to_vec());

        let _ = self
            .storage
            .raw_subkey_for_each(
                db,
                prefix,
                Box::new(move |encoded, _raw| {
                    if let Some((kind, member)) = subkey::decode_subkey(&encoded) {
                        if kind != CollectionKind::Set {
                            return Ok(());
                        }
                        if let Some(ref pat) = pattern {
                            if !glob_match(pat, &member) {
                                return Ok(());
                            }
                        }
                        out_c.lock().unwrap().push(member);
                    }
                    Ok(())
                }),
            )
            .await;

        Ok(Arc::try_unwrap(out).unwrap().into_inner().unwrap())
    }

    /// 将 inline set 迁移为 subkey 格式.
    async fn migrate_set_to_subkey(
        &self,
        db: usize,
        key: &[u8],
        expires_at: &Option<u64>,
        set: &HashSet<Vec<u8>>,
    ) -> Result<()> {
        let encoded_user_key = AiDbEngine::encode_key(db, key);
        let count = set.len() as u32;

        for member in set {
            let subkey_key = subkey::encode_set_member_key(&encoded_user_key, member);
            self.storage.raw_subkey_set(db, subkey_key, vec![]).await?;
        }

        let metadata = StoredValue {
            value: ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                count,
            },
            expires_at: *expires_at,
        };
        self.storage.rewrite_stored(db, key, metadata).await
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
