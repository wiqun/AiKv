//! 脚本事务缓冲 + commit (保留 TTL)
//!
//! `write_buffer` 只放改过的 key。`seen` 记住本次脚本已经从存储读到的原始值和 TTL，
//! 后续 `redis.call` 与提交都复用它，只读不会把 key 标脏。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::command::zset::index::{ScoreIndex, ZSetBuf};
use crate::error::{Error, Result};
use crate::storage::subkey;
use crate::storage::{
    now_ms, AiDbEngine, CollectionKind, KvStorage, StoredBatchOp, StoredValue, ValueType,
    TTL_NO_EXPIRY, WRONGTYPE,
};

/// 第一次碰到存储时记下的原值。`cached` 在值被搬进写缓冲后清空。
struct StorageTouch {
    existed: bool,
    expires_at: Option<u64>,
    cached: Option<StoredValue>,
    /// 本次脚本对未改动的 zset 已经建过的分数索引。
    zset_index: Option<ScoreIndex>,
}

#[derive(Debug, Clone)]
pub enum ExtendedBatchOp {
    SetString(Vec<u8>),
    SetList(VecDeque<Vec<u8>>),
    SetHash(HashMap<Vec<u8>, Vec<u8>>),
    SetSet(HashSet<Vec<u8>>),
    SetZSet(ZSetBuf),
    Delete,
}

/// 同一轮组提交里，前面脚本已经成功、但还没落盘的写。
pub(super) struct BurstOverlay {
    ops: HashMap<Vec<u8>, StoredBatchOp>,
}

impl BurstOverlay {
    pub(super) fn new() -> Self {
        Self {
            ops: HashMap::new(),
        }
    }

    fn get(&self, key: &[u8]) -> Option<&StoredBatchOp> {
        self.ops.get(key)
    }

    pub(super) fn len(&self) -> usize {
        self.ops.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub(super) fn apply_script(&mut self, ops: Vec<(Vec<u8>, StoredBatchOp)>) {
        for (key, op) in ops {
            self.ops.insert(key, op);
        }
    }

    pub(super) fn drain_batch(&mut self) -> Vec<(Vec<u8>, StoredBatchOp)> {
        std::mem::take(&mut self.ops).into_iter().collect()
    }
}

pub struct ScriptTransaction {
    pub db_index: usize,
    write_buffer: HashMap<Vec<u8>, ExtendedBatchOp>,
    /// 本次脚本已经确认过的存储原值。不参与提交。
    seen: Mutex<HashMap<Vec<u8>, StorageTouch>>,
    /// 显式 TTL 更新: Some(ts) = 过期时间; None = 清除 TTL
    expire_at: HashMap<Vec<u8>, Option<u64>>,
    /// 本轮更早的脚本写过、尚未提交的值。当前脚本自己的 `write_buffer` 优先。
    overlay: Option<Arc<Mutex<BurstOverlay>>>,
}

impl ScriptTransaction {
    pub fn new(db_index: usize) -> Self {
        Self::with_overlay(db_index, None)
    }

    pub(super) fn with_overlay(db_index: usize, overlay: Option<Arc<Mutex<BurstOverlay>>>) -> Self {
        Self {
            db_index,
            write_buffer: HashMap::new(),
            seen: Mutex::new(HashMap::new()),
            expire_at: HashMap::new(),
            overlay,
        }
    }

    fn overlay_get(&self, key: &[u8]) -> Option<StoredBatchOp> {
        let overlay = self.overlay.as_ref()?;
        overlay.lock().unwrap().get(key).cloned()
    }

    fn note_origin_if_unseen(&self, key: &[u8]) {
        self.seen
            .lock()
            .unwrap()
            .entry(key.to_vec())
            .or_insert(StorageTouch {
                existed: false,
                expires_at: None,
                cached: None,
                zset_index: None,
            });
    }

    fn detach_cache(&self, key: &[u8]) {
        if let Some(touch) = self.seen.lock().unwrap().get_mut(key) {
            touch.cached = None;
        }
    }

    pub fn set_string(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.note_origin_if_unseen(&key);
        self.detach_cache(&key);
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetString(value));
    }

    pub fn set_list(&mut self, key: Vec<u8>, list: VecDeque<Vec<u8>>) {
        self.note_origin_if_unseen(&key);
        self.detach_cache(&key);
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetList(list));
    }

    pub fn set_set(&mut self, key: Vec<u8>, set: HashSet<Vec<u8>>) {
        self.note_origin_if_unseen(&key);
        self.detach_cache(&key);
        self.write_buffer.insert(key, ExtendedBatchOp::SetSet(set));
    }

    /// 只读有序集合。写缓冲或本次已装入的原值直接借用，不克隆，也不标脏。
    pub async fn with_zset<R>(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
        f: impl FnOnce(&BTreeMap<Vec<u8>, f64>) -> Result<R>,
    ) -> Result<Option<R>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                ExtendedBatchOp::SetZSet(zset) => Ok(Some(f(zset.members())?)),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        self.ensure_cached(storage, key).await?;
        let seen = self.seen.lock().unwrap();
        match seen.get(key).and_then(|touch| touch.cached.as_ref()) {
            None => Ok(None),
            Some(stored) => {
                let zset = stored.as_zset()?;
                Ok(Some(f(zset)?))
            }
        }
    }

    /// 按分数访问有序集合。索引在本脚本内复用，只读不标脏。
    pub async fn with_score_index<R>(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
        f: impl FnOnce(&ScoreIndex) -> Result<R>,
    ) -> Result<Option<R>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                ExtendedBatchOp::SetZSet(zset) => Ok(Some(f(zset.index())?)),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        self.ensure_cached(storage, key).await?;
        let mut seen = self.seen.lock().unwrap();
        let Some(touch) = seen.get_mut(key) else {
            return Ok(None);
        };
        if touch.zset_index.is_none() {
            let built = {
                let Some(stored) = touch.cached.as_ref() else {
                    return Ok(None);
                };
                Some(ScoreIndex::from_members(stored.as_zset()?))
            };
            touch.zset_index = built;
        }
        let index = touch
            .zset_index
            .as_ref()
            .expect("zset index just populated");
        Ok(Some(f(index)?))
    }

    /// 只读哈希。写缓冲或本次已装入的原值直接借用，不克隆，也不标脏。
    pub async fn with_hash<R>(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
        f: impl FnOnce(&HashMap<Vec<u8>, Vec<u8>>) -> Result<R>,
    ) -> Result<Option<R>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                ExtendedBatchOp::SetHash(hash) => Ok(Some(f(hash)?)),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        self.ensure_cached(storage, key).await?;
        let seen = self.seen.lock().unwrap();
        match seen.get(key).and_then(|touch| touch.cached.as_ref()) {
            None => Ok(None),
            Some(stored) => {
                let hash = stored.as_hash()?;
                Ok(Some(f(hash)?))
            }
        }
    }

    /// 原地修改缓冲中的哈希。第一次从存储或只读缓存装入，后续不再克隆整表。
    pub async fn update_hash<R>(
        &mut self,
        storage: &dyn KvStorage,
        key: &[u8],
        create_if_missing: bool,
        remove_if_empty: bool,
        f: impl FnOnce(&mut HashMap<Vec<u8>, Vec<u8>>) -> Result<R>,
    ) -> Result<Option<R>> {
        if !self
            .prepare_hash_mut(storage, key, create_if_missing)
            .await?
        {
            return Ok(None);
        }
        let (result, empty) = {
            let hash = match self.write_buffer.get_mut(key) {
                Some(ExtendedBatchOp::SetHash(hash)) => hash,
                _ => return Err(Error::Command("ERR internal hash buffer missing".into())),
            };
            let result = f(hash)?;
            (result, remove_if_empty && hash.is_empty())
        };
        if empty {
            self.delete(key.to_vec());
        }
        Ok(Some(result))
    }

    async fn prepare_hash_mut(
        &mut self,
        storage: &dyn KvStorage,
        key: &[u8],
        create_if_missing: bool,
    ) -> Result<bool> {
        match self.write_buffer.get(key) {
            Some(ExtendedBatchOp::SetHash(_)) => return Ok(true),
            Some(ExtendedBatchOp::Delete) => {
                if !create_if_missing {
                    return Ok(false);
                }
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetHash(HashMap::new()));
                return Ok(true);
            }
            Some(_) => return Err(Error::Command(WRONGTYPE.into())),
            None => {}
        }
        self.ensure_cached(storage, key).await?;
        let taken = take_cached_hash(&self.seen, key)?;
        match taken {
            CachedTake::Missing => {
                if !create_if_missing {
                    return Ok(false);
                }
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetHash(HashMap::new()));
                Ok(true)
            }
            CachedTake::Ready(hash) => {
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetHash(hash));
                Ok(true)
            }
        }
    }

    /// 原地修改缓冲中的有序集合。第一次从存储装入后，后续调用不再克隆整表。
    /// `create_if_missing` 用于 ZADD；`remove_if_empty` 用于 ZREM 删空后记成删除。
    pub async fn update_zset<R>(
        &mut self,
        storage: &dyn KvStorage,
        key: &[u8],
        create_if_missing: bool,
        remove_if_empty: bool,
        f: impl FnOnce(&mut ZSetBuf) -> Result<R>,
    ) -> Result<Option<R>> {
        if !self
            .prepare_zset_mut(storage, key, create_if_missing)
            .await?
        {
            return Ok(None);
        }
        let (result, empty) = {
            let zset = match self.write_buffer.get_mut(key) {
                Some(ExtendedBatchOp::SetZSet(zset)) => zset,
                _ => return Err(Error::Command("ERR internal zset buffer missing".into())),
            };
            let result = f(zset)?;
            (result, remove_if_empty && zset.members().is_empty())
        };
        if empty {
            self.delete(key.to_vec());
        }
        Ok(Some(result))
    }

    async fn prepare_zset_mut(
        &mut self,
        storage: &dyn KvStorage,
        key: &[u8],
        create_if_missing: bool,
    ) -> Result<bool> {
        let kind = match self.write_buffer.get(key) {
            Some(ExtendedBatchOp::SetZSet(_)) => 0,
            Some(ExtendedBatchOp::Delete) => 1,
            Some(_) => 2,
            None => 3,
        };
        match kind {
            0 => return Ok(true),
            2 => return Err(Error::Command(WRONGTYPE.into())),
            1 => {
                if !create_if_missing {
                    return Ok(false);
                }
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetZSet(ZSetBuf::new()));
                return Ok(true);
            }
            _ => {}
        }
        self.ensure_cached(storage, key).await?;
        let taken = take_cached_zset(&self.seen, key)?;
        match taken {
            CachedTake::Missing => {
                if !create_if_missing {
                    return Ok(false);
                }
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetZSet(ZSetBuf::new()));
                Ok(true)
            }
            CachedTake::Ready(zset) => {
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetZSet(zset));
                Ok(true)
            }
        }
    }

    pub fn delete(&mut self, key: Vec<u8>) {
        self.detach_cache(&key);
        self.write_buffer.insert(key, ExtendedBatchOp::Delete);
    }

    pub fn set_expire_at(&mut self, key: Vec<u8>, expires_at: Option<u64>) {
        self.expire_at.insert(key, expires_at);
    }

    pub async fn get(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::SetString(v) => Ok(Some(v.clone())),
                ExtendedBatchOp::Delete => Ok(None),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        self.ensure_cached(storage, key).await?;
        let seen = self.seen.lock().unwrap();
        match seen.get(key).and_then(|touch| touch.cached.as_ref()) {
            None => Ok(None),
            Some(stored) => match &stored.value {
                ValueType::String(v) => Ok(Some(v.clone())),
                _ => Err(Error::Command(WRONGTYPE.into())),
            },
        }
    }

    pub async fn get_value(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<Option<StoredValue>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                _ => Ok(Some(op_to_stored(op)?)),
            };
        }
        self.ensure_cached(storage, key).await?;
        Ok(self
            .seen
            .lock()
            .unwrap()
            .get(key)
            .and_then(|touch| touch.cached.clone()))
    }

    async fn ensure_cached(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<()> {
        if self.write_buffer.contains_key(key) || self.seen.lock().unwrap().contains_key(key) {
            return Ok(());
        }
        let loaded = self.fetch_stored(storage, key).await?;
        self.seen.lock().unwrap().insert(
            key.to_vec(),
            StorageTouch {
                existed: loaded.is_some(),
                expires_at: loaded.as_ref().and_then(|stored| stored.expires_at),
                cached: loaded,
                zset_index: None,
            },
        );
        Ok(())
    }

    /// 从存储装入并展开 subkey 集合。调用方负责放进 `seen`。
    async fn fetch_stored(
        &self,
        storage: &dyn KvStorage,
        key: &[u8],
    ) -> Result<Option<StoredValue>> {
        if let Some(op) = self.overlay_get(key) {
            return match op {
                StoredBatchOp::Delete { .. } => Ok(None),
                StoredBatchOp::Put(stored) => Ok(Some(stored)),
            };
        }
        let Some(stored) = storage.get_typed(self.db_index, key).await? else {
            return Ok(None);
        };
        match stored.value {
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(self.db_index, key);
                let prefix = subkey::hash_subkey_prefix(&encoded);
                let map = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
                let map_c = map.clone();
                storage
                    .raw_subkey_for_each(
                        self.db_index,
                        prefix,
                        Box::new(move |enc, raw| {
                            if let Some((kind, field)) = subkey::decode_subkey(&enc) {
                                if kind == CollectionKind::Hash {
                                    map_c.lock().unwrap().insert(field, raw);
                                }
                            }
                            Ok(())
                        }),
                    )
                    .await?;
                Ok(Some(StoredValue {
                    value: ValueType::Hash(
                        std::sync::Arc::try_unwrap(map)
                            .unwrap()
                            .into_inner()
                            .unwrap(),
                    ),
                    expires_at: stored.expires_at,
                }))
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(self.db_index, key);
                let prefix = subkey::set_subkey_prefix(&encoded);
                let set_h = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
                let set_c = set_h.clone();
                storage
                    .raw_subkey_for_each(
                        self.db_index,
                        prefix,
                        Box::new(move |enc, _raw| {
                            if let Some((kind, member)) = subkey::decode_subkey(&enc) {
                                if kind == CollectionKind::Set {
                                    set_c.lock().unwrap().insert(member);
                                }
                            }
                            Ok(())
                        }),
                    )
                    .await?;
                Ok(Some(StoredValue {
                    value: ValueType::Set(
                        std::sync::Arc::try_unwrap(set_h)
                            .unwrap()
                            .into_inner()
                            .unwrap(),
                    ),
                    expires_at: stored.expires_at,
                }))
            }
            _ => Ok(Some(stored)),
        }
    }

    /// 脚本缓冲里的剩余 TTL（毫秒）。`None` 表示 key 不存在，`Some(TTL_NO_EXPIRY)` 表示没有过期时间。
    pub async fn ttl_ms(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<Option<i64>> {
        if matches!(self.write_buffer.get(key), Some(ExtendedBatchOp::Delete)) {
            return Ok(None);
        }
        if let Some(exp) = self.expire_at.get(key) {
            return Ok(remaining_ttl(*exp));
        }
        if !self.seen.lock().unwrap().contains_key(key) {
            if self.write_buffer.contains_key(key) {
                return Ok(Some(TTL_NO_EXPIRY));
            }
            self.ensure_cached(storage, key).await?;
        }
        let seen = self.seen.lock().unwrap();
        let Some(touch) = seen.get(key) else {
            return Ok(None);
        };
        if touch.cached.is_none() && !self.write_buffer.contains_key(key) {
            return Ok(None);
        }
        Ok(remaining_ttl(touch.expires_at))
    }

    pub async fn exists(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<bool> {
        if let Some(op) = self.write_buffer.get(key) {
            return Ok(!matches!(op, ExtendedBatchOp::Delete));
        }
        self.ensure_cached(storage, key).await?;
        Ok(self
            .seen
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|touch| touch.cached.is_some()))
    }

    async fn resolve_expires_at(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<Option<u64>> {
        if let Some(exp) = self.expire_at.get(key) {
            return Ok(*exp);
        }
        if let Some(touch) = self.seen.lock().unwrap().get(key) {
            return Ok(touch.expires_at);
        }
        if let Some(op) = self.overlay_get(key) {
            return Ok(match op {
                StoredBatchOp::Delete { .. } => None,
                StoredBatchOp::Put(stored) => stored.expires_at,
            });
        }
        Ok(storage
            .get_typed(self.db_index, key)
            .await?
            .and_then(|stored| stored.expires_at))
    }

    fn delete_existed(&self, key: &[u8]) -> Option<bool> {
        self.seen
            .lock()
            .unwrap()
            .get(key)
            .map(|touch| touch.existed)
    }

    pub async fn commit(mut self, storage: &dyn KvStorage) -> Result<()> {
        let db = self.db_index;
        let (ops, release_ttl) = self.collect_ops(storage).await?;
        storage.apply_stored_batch(db, ops).await?;
        for _ in 0..release_ttl {
            storage.release_seen_ttl(db).await?;
        }
        Ok(())
    }

    pub(super) async fn collect_ops(
        &mut self,
        storage: &dyn KvStorage,
    ) -> Result<(Vec<(Vec<u8>, StoredBatchOp)>, u64)> {
        let db = self.db_index;
        let mut keys: Vec<Vec<u8>> = self.write_buffer.keys().cloned().collect();
        for key in self.expire_at.keys() {
            if !self.write_buffer.contains_key(key) {
                keys.push(key.clone());
            }
        }

        let mut ops = Vec::with_capacity(keys.len());
        let mut release_ttl = 0u64;
        let mut write_buffer = std::mem::take(&mut self.write_buffer);
        for key in keys {
            if let Some(op) = write_buffer.remove(&key) {
                match op {
                    ExtendedBatchOp::Delete => match self.delete_existed(&key) {
                        Some(false) => {}
                        Some(true) => {
                            let had_ttl = self
                                .seen
                                .lock()
                                .unwrap()
                                .get(&key)
                                .and_then(|touch| touch.expires_at)
                                .is_some();
                            ops.push((key, StoredBatchOp::Delete { had_ttl }));
                        }
                        None => match self.overlay_get(&key) {
                            Some(StoredBatchOp::Delete { .. }) => {}
                            Some(StoredBatchOp::Put(stored)) => {
                                ops.push((
                                    key,
                                    StoredBatchOp::Delete {
                                        had_ttl: stored.expires_at.is_some(),
                                    },
                                ));
                            }
                            None => {
                                if let Some(stored) = storage.get_typed(db, &key).await? {
                                    ops.push((
                                        key,
                                        StoredBatchOp::Delete {
                                            had_ttl: stored.expires_at.is_some(),
                                        },
                                    ));
                                }
                            }
                        },
                    },
                    other => {
                        let preserved = !self.expire_at.contains_key(&key);
                        let exp = self.resolve_expires_at(storage, &key).await?;
                        if preserved && exp.is_some() {
                            release_ttl += 1;
                        }
                        ops.push((key, StoredBatchOp::Put(op_into_stored(other, exp)?)));
                    }
                }
            } else if let Some(mut stored) = self.take_cached(&key) {
                let saw_ttl = stored.expires_at.is_some();
                stored.expires_at = self.expire_at.get(&key).copied().flatten();
                if saw_ttl {
                    release_ttl += 1;
                }
                ops.push((key, StoredBatchOp::Put(stored)));
            } else if let Some(StoredBatchOp::Put(mut stored)) = self.overlay_get(&key) {
                let saw_ttl = stored.expires_at.is_some();
                stored.expires_at = self.expire_at.get(&key).copied().flatten();
                if saw_ttl {
                    release_ttl += 1;
                }
                ops.push((key, StoredBatchOp::Put(stored)));
            } else if let Some(mut stored) = storage.get_typed(db, &key).await? {
                let saw_ttl = stored.expires_at.is_some();
                stored.expires_at = self.expire_at.get(&key).copied().flatten();
                if saw_ttl {
                    release_ttl += 1;
                }
                ops.push((key, StoredBatchOp::Put(stored)));
            }
        }

        Ok((ops, release_ttl))
    }

    fn take_cached(&self, key: &[u8]) -> Option<StoredValue> {
        self.seen.lock().unwrap().get_mut(key)?.cached.take()
    }
}

fn remaining_ttl(expires_at: Option<u64>) -> Option<i64> {
    match expires_at {
        None => Some(TTL_NO_EXPIRY),
        Some(at) => {
            let now = now_ms();
            if now >= at {
                None
            } else {
                Some((at - now) as i64)
            }
        }
    }
}

enum CachedTake<T> {
    Missing,
    Ready(T),
}

fn take_cached_hash(
    seen: &Mutex<HashMap<Vec<u8>, StorageTouch>>,
    key: &[u8],
) -> Result<CachedTake<HashMap<Vec<u8>, Vec<u8>>>> {
    let mut seen = seen.lock().unwrap();
    let Some(touch) = seen.get_mut(key) else {
        return Ok(CachedTake::Missing);
    };
    let is_hash = matches!(
        touch.cached.as_ref().map(|stored| &stored.value),
        Some(ValueType::Hash(_))
    );
    if touch.cached.is_some() && !is_hash {
        return Err(Error::Command(WRONGTYPE.into()));
    }
    match touch.cached.take() {
        None => Ok(CachedTake::Missing),
        Some(stored) => match stored.value {
            ValueType::Hash(hash) => Ok(CachedTake::Ready(hash)),
            _ => unreachable!("hash type checked above"),
        },
    }
}

fn take_cached_zset(
    seen: &Mutex<HashMap<Vec<u8>, StorageTouch>>,
    key: &[u8],
) -> Result<CachedTake<ZSetBuf>> {
    let mut seen = seen.lock().unwrap();
    let Some(touch) = seen.get_mut(key) else {
        return Ok(CachedTake::Missing);
    };
    let is_zset = matches!(
        touch.cached.as_ref().map(|stored| &stored.value),
        Some(ValueType::ZSet(_))
    );
    if touch.cached.is_some() && !is_zset {
        return Err(Error::Command(WRONGTYPE.into()));
    }
    let index = touch.zset_index.take();
    match touch.cached.take() {
        None => Ok(CachedTake::Missing),
        Some(stored) => match stored.value {
            ValueType::ZSet(zset) => Ok(CachedTake::Ready(ZSetBuf::from_parts(zset, index))),
            _ => unreachable!("zset type checked above"),
        },
    }
}

fn op_into_stored(op: ExtendedBatchOp, expires_at: Option<u64>) -> Result<StoredValue> {
    let mut stored = match op {
        ExtendedBatchOp::SetString(value) => StoredValue::string(value),
        ExtendedBatchOp::SetList(list) => StoredValue::new_list(list),
        ExtendedBatchOp::SetHash(hash) => StoredValue {
            value: ValueType::Hash(hash),
            expires_at: None,
        },
        ExtendedBatchOp::SetSet(set) => StoredValue::new_set(set),
        ExtendedBatchOp::SetZSet(zset) => StoredValue::new_zset(zset.into_members()),
        ExtendedBatchOp::Delete => {
            return Err(Error::Command("ERR internal delete op".into()));
        }
    };
    stored.expires_at = expires_at;
    Ok(stored)
}

fn op_to_stored(op: &ExtendedBatchOp) -> Result<StoredValue> {
    match op {
        ExtendedBatchOp::SetString(v) => Ok(StoredValue::string(v.clone())),
        ExtendedBatchOp::SetList(list) => Ok(StoredValue::new_list(list.clone())),
        ExtendedBatchOp::SetHash(hash) => Ok(StoredValue {
            value: ValueType::Hash(hash.clone()),
            expires_at: None,
        }),
        ExtendedBatchOp::SetSet(set) => Ok(StoredValue::new_set(set.clone())),
        ExtendedBatchOp::SetZSet(zset) => Ok(StoredValue::new_zset(zset.members().clone())),
        ExtendedBatchOp::Delete => Err(Error::Command("ERR internal delete op".into())),
    }
}

pub fn parse_i64_arg(b: &Bytes, label: &str) -> Result<i64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command(format!("ERR {label} is not an integer")))?;
    s.parse::<i64>()
        .map_err(|_| Error::Command(format!("ERR {label} is not an integer")))
}

pub fn parse_f64_arg(b: &Bytes, label: &str) -> Result<f64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Command(format!("ERR {label} is not a float")))?;
    s.parse::<f64>()
        .map_err(|_| Error::Command(format!("ERR {label} is not a float")))
}

pub fn expire_seconds_to_at(seconds: i64) -> Option<u64> {
    if seconds <= 0 {
        None
    } else {
        Some(now_ms().saturating_add(seconds as u64 * 1000))
    }
}
