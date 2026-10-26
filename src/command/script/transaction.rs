use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;

use crate::error::{Error, Result};
use crate::storage::subkey;
use crate::storage::{
    now_ms, zset_subkey, AiDbEngine, CollectionKind, ScriptIo, StorageEngineKind, StoredValue,
    ValueType, WriteOp, WRONGTYPE,
};

use super::in_flight::InFlightBuffer;

#[derive(Debug, Clone)]
pub enum ExtendedBatchOp {
    SetString(Vec<u8>),
    SetList(VecDeque<Vec<u8>>),
    SetHash(HashMap<Vec<u8>, Vec<u8>>),
    SetSet(HashSet<Vec<u8>>),
    SetZSet(BTreeMap<Vec<u8>, f64>),
    Delete,
}

pub struct ScriptTransaction {
    pub db_index: usize,
    write_buffer: HashMap<Vec<u8>, ExtendedBatchOp>,
    /// 显式 TTL 更新: Some(ts) = 过期时间; None = 清除 TTL
    expire_at: HashMap<Vec<u8>, Option<u64>>,
    /// 脚本读操作缓存的原始 TTL，避免 commit 时产生额外的点查 RPC
    orig_expires_at: Mutex<HashMap<Vec<u8>, Option<u64>>>,
    /// 脚本执行期间的读缓存 (避免循环调用同一个/同批 key 产生重复点查 RPC)
    read_cache: Mutex<HashMap<Vec<u8>, Option<StoredValue>>>,
    pub(crate) in_flight: Option<Arc<InFlightBuffer>>,
    /// 大 ZSET 的增量修改. 提交时写 subkey, 不把整份集合塞回 inline blob.
    pub(crate) zset_deltas: HashMap<Vec<u8>, ZSetDelta>,
}

/// 一次脚本里某个大 ZSET member 的分数变化. `before` 是脚本开始前的分数.
#[derive(Debug, Clone)]
pub struct ZSetMemberChange {
    pub before: Option<f64>,
    pub after: Option<f64>,
}

/// 大 ZSET 在脚本中的增量. `count` 是提交后的成员数.
#[derive(Debug, Clone)]
pub struct ZSetDelta {
    pub count: u32,
    pub members: HashMap<Vec<u8>, ZSetMemberChange>,
}

impl ScriptTransaction {
    #[allow(dead_code)]
    pub fn new(db_index: usize) -> Self {
        Self::with_in_flight(db_index, None)
    }

    pub fn with_in_flight(db_index: usize, in_flight: Option<Arc<InFlightBuffer>>) -> Self {
        Self {
            db_index,
            write_buffer: HashMap::new(),
            expire_at: HashMap::new(),
            orig_expires_at: Mutex::new(HashMap::new()),
            read_cache: Mutex::new(HashMap::new()),
            in_flight,
            zset_deltas: HashMap::new(),
        }
    }

    pub fn set_string(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.read_cache.lock().remove(&key);
        self.orig_expires_at
            .lock()
            .entry(key.clone())
            .or_insert(None);
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetString(value));
    }

    pub fn set_list(&mut self, key: Vec<u8>, list: VecDeque<Vec<u8>>) {
        self.read_cache.lock().remove(&key);
        self.orig_expires_at
            .lock()
            .entry(key.clone())
            .or_insert(None);
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetList(list));
    }

    pub fn set_hash(&mut self, key: Vec<u8>, hash: HashMap<Vec<u8>, Vec<u8>>) {
        self.read_cache.lock().remove(&key);
        self.orig_expires_at
            .lock()
            .entry(key.clone())
            .or_insert(None);
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetHash(hash));
    }

    pub fn set_set(&mut self, key: Vec<u8>, set: HashSet<Vec<u8>>) {
        self.read_cache.lock().remove(&key);
        self.orig_expires_at
            .lock()
            .entry(key.clone())
            .or_insert(None);
        self.write_buffer.insert(key, ExtendedBatchOp::SetSet(set));
    }

    pub fn set_zset(&mut self, key: Vec<u8>, zset: BTreeMap<Vec<u8>, f64>) {
        self.read_cache.lock().remove(&key);
        self.orig_expires_at
            .lock()
            .entry(key.clone())
            .or_insert(None);
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetZSet(zset));
    }

    pub fn delete(&mut self, key: Vec<u8>) {
        self.read_cache.lock().remove(&key);
        self.write_buffer.insert(key, ExtendedBatchOp::Delete);
    }

    pub fn replace_stored(&mut self, key: Vec<u8>, stored: StoredValue) -> Result<()> {
        let expires_at = stored.expires_at;
        let op = match stored.value {
            ValueType::String(value) => ExtendedBatchOp::SetString(value),
            ValueType::List(value) => ExtendedBatchOp::SetList(value),
            ValueType::Hash(value) => ExtendedBatchOp::SetHash(value),
            ValueType::Set(value) => ExtendedBatchOp::SetSet(value),
            ValueType::ZSet(value) => ExtendedBatchOp::SetZSet(value),
            ValueType::CollectionHeader { .. } => {
                return Err(Error::Command(
                    "ERR DUMP payload version or checksum error".into(),
                ));
            }
        };
        self.read_cache.lock().remove(&key);
        self.orig_expires_at
            .lock()
            .entry(key.clone())
            .or_insert(None);
        self.zset_deltas.remove(&key);
        self.write_buffer.insert(key.clone(), op);
        self.expire_at.insert(key, expires_at);
        Ok(())
    }

    pub fn set_expire_at(&mut self, key: Vec<u8>, expires_at: Option<u64>) {
        self.expire_at.insert(key, expires_at);
    }

    pub fn get(&self, storage: &dyn ScriptIo, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::SetString(v) => Ok(Some(v.clone())),
                ExtendedBatchOp::Delete => Ok(None),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        let stored = self.get_value(storage, key)?;
        match stored {
            Some(s) => match s.value {
                ValueType::String(v) => Ok(Some(v)),
                _ => Err(Error::Command(WRONGTYPE.into())),
            },
            None => Ok(None),
        }
    }

    pub fn get_value(&self, storage: &dyn ScriptIo, key: &[u8]) -> Result<Option<StoredValue>> {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                _ => Ok(Some(op_to_stored(op)?)),
            };
        }
        if let Some(cached) = self.read_cache.lock().get(key) {
            return Ok(cached.clone());
        }
        if let Some(in_flight) = &self.in_flight {
            if let Some(in_flight_res) = in_flight.get(self.db_index, key) {
                self.read_cache
                    .lock()
                    .insert(key.to_vec(), in_flight_res.clone());
                self.orig_expires_at.lock().insert(
                    key.to_vec(),
                    in_flight_res.as_ref().and_then(|s| s.expires_at),
                );
                return Ok(in_flight_res);
            }
        }
        let Some(stored) = storage.get_typed(self.db_index, key)? else {
            self.read_cache.lock().insert(key.to_vec(), None);
            self.orig_expires_at.lock().insert(key.to_vec(), None);
            return Ok(None);
        };
        self.orig_expires_at
            .lock()
            .insert(key.to_vec(), stored.expires_at);
        // 透明展开 CollectionHeader -> 完整集合, 使 Lua 脚本读路径无需感知 subkey 格式.
        let expanded = match stored.value {
            ValueType::CollectionHeader {
                kind: CollectionKind::Hash,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(self.db_index, key);
                let prefix = subkey::hash_subkey_prefix(&encoded);
                let map = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
                let map_c = map.clone();
                storage.raw_subkey_for_each(
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
                )?;
                StoredValue {
                    value: ValueType::Hash(
                        std::sync::Arc::try_unwrap(map)
                            .unwrap()
                            .into_inner()
                            .unwrap(),
                    ),
                    expires_at: stored.expires_at,
                }
            }
            ValueType::CollectionHeader {
                kind: CollectionKind::Set,
                ..
            } => {
                let encoded = AiDbEngine::encode_key(self.db_index, key);
                let prefix = subkey::set_subkey_prefix(&encoded);
                let set_h = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
                let set_c = set_h.clone();
                storage.raw_subkey_for_each(
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
                )?;
                StoredValue {
                    value: ValueType::Set(
                        std::sync::Arc::try_unwrap(set_h)
                            .unwrap()
                            .into_inner()
                            .unwrap(),
                    ),
                    expires_at: stored.expires_at,
                }
            }
            _ => stored,
        };
        self.read_cache
            .lock()
            .insert(key.to_vec(), Some(expanded.clone()));
        Ok(Some(expanded))
    }

    pub fn exists(&self, storage: &dyn ScriptIo, key: &[u8]) -> Result<bool> {
        if let Some(op) = self.write_buffer.get(key) {
            return Ok(!matches!(op, ExtendedBatchOp::Delete));
        }
        if let Some(cached) = self.read_cache.lock().get(key) {
            return Ok(cached.is_some());
        }
        Ok(self.get_value(storage, key)?.is_some())
    }

    pub fn ensure_zset_mut(
        &mut self,
        storage: &dyn ScriptIo,
        key: &[u8],
    ) -> Result<Option<&mut BTreeMap<Vec<u8>, f64>>> {
        if !self.write_buffer.contains_key(key) {
            let loaded = self.get_value(storage, key)?;
            if let Some(stored) = loaded {
                let zset = stored.as_zset()?.clone();
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetZSet(zset));
            }
        }
        self.read_cache.lock().remove(key);
        match self.write_buffer.get_mut(key) {
            Some(ExtendedBatchOp::SetZSet(zset)) => Ok(Some(zset)),
            Some(ExtendedBatchOp::Delete) => Ok(None),
            None => Ok(None),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn with_zset<F, R>(&self, storage: &dyn ScriptIo, key: &[u8], f: F) -> Result<Option<R>>
    where
        F: FnOnce(&BTreeMap<Vec<u8>, f64>) -> Result<R>,
    {
        if self.zset_deltas.contains_key(key) {
            let map = self.materialize_zset_delta(storage, key)?;
            return f(&map).map(Some);
        }
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                ExtendedBatchOp::SetZSet(zset) => f(zset).map(Some),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        let stored = self.get_value(storage, key)?;
        match stored {
            None => Ok(None),
            Some(stored) if zset_subkey::is_zset_header(&stored).is_some() => {
                let map = self.visible_subkey_members(storage, key)?;
                f(&map).map(Some)
            }
            Some(stored) => f(stored.as_zset()?).map(Some),
        }
    }

    pub fn ensure_hash_mut(
        &mut self,
        storage: &dyn ScriptIo,
        key: &[u8],
    ) -> Result<Option<&mut HashMap<Vec<u8>, Vec<u8>>>> {
        if !self.write_buffer.contains_key(key) {
            let loaded = self.get_value(storage, key)?;
            if let Some(stored) = loaded {
                let hash = stored.as_hash()?.clone();
                self.write_buffer
                    .insert(key.to_vec(), ExtendedBatchOp::SetHash(hash));
            }
        }
        self.read_cache.lock().remove(key);
        match self.write_buffer.get_mut(key) {
            Some(ExtendedBatchOp::SetHash(hash)) => Ok(Some(hash)),
            Some(ExtendedBatchOp::Delete) => Ok(None),
            None => Ok(None),
            _ => Err(Error::Command(WRONGTYPE.into())),
        }
    }

    pub fn with_hash<F, R>(&self, storage: &dyn ScriptIo, key: &[u8], f: F) -> Result<Option<R>>
    where
        F: FnOnce(&HashMap<Vec<u8>, Vec<u8>>) -> Result<R>,
    {
        if let Some(op) = self.write_buffer.get(key) {
            return match op {
                ExtendedBatchOp::Delete => Ok(None),
                ExtendedBatchOp::SetHash(hash) => f(hash).map(Some),
                _ => Err(Error::Command(WRONGTYPE.into())),
            };
        }
        if let Some(cached) = self.read_cache.lock().get(key) {
            return match cached {
                Some(stored) => match stored.as_hash() {
                    Ok(hash) => f(hash).map(Some),
                    Err(e) => Err(e),
                },
                None => Ok(None),
            };
        }
        let stored = self.get_value(storage, key)?;
        match stored {
            Some(stored) => match stored.as_hash() {
                Ok(hash) => f(hash).map(Some),
                Err(e) => Err(e),
            },
            None => Ok(None),
        }
    }

    pub(crate) fn resolve_expires_at(
        &self,
        storage: &dyn ScriptIo,
        key: &[u8],
    ) -> Result<Option<u64>> {
        if let Some(exp) = self.expire_at.get(key) {
            return Ok(*exp);
        }
        if let Some(exp) = self.orig_expires_at.lock().get(key) {
            return Ok(*exp);
        }
        if let Some(cached) = self.read_cache.lock().get(key) {
            return Ok(cached.as_ref().and_then(|s| s.expires_at));
        }
        if let Some(in_flight) = &self.in_flight {
            if let Some(in_flight_res) = in_flight.get(self.db_index, key) {
                return Ok(in_flight_res.as_ref().and_then(|s| s.expires_at));
            }
        }
        let exp = storage
            .get_typed(self.db_index, key)?
            .and_then(|s| s.expires_at);
        self.orig_expires_at.lock().insert(key.to_vec(), exp);
        Ok(exp)
    }

    pub fn prepare_batch_ops(&self, storage: &dyn ScriptIo) -> Result<Vec<(Vec<u8>, WriteOp)>> {
        let mut keys: HashSet<Vec<u8>> = self.write_buffer.keys().cloned().collect();
        keys.extend(self.expire_at.keys().cloned());

        let mut batch_ops: Vec<(Vec<u8>, WriteOp)> = Vec::with_capacity(keys.len());

        for key in keys {
            if let Some(op) = self.write_buffer.get(&key) {
                match op {
                    ExtendedBatchOp::Delete => {
                        batch_ops.push((key, WriteOp::Delete));
                    }
                    ExtendedBatchOp::SetString(value) => {
                        let exp = self.resolve_expires_at(storage, &key)?;
                        let stored = StoredValue {
                            value: ValueType::String(value.clone()),
                            expires_at: exp,
                        };
                        batch_ops.push((key, WriteOp::put_stored(stored)));
                    }
                    ExtendedBatchOp::SetZSet(zset) => {
                        let exp = self.resolve_expires_at(storage, &key)?;
                        batch_ops.extend(zset_subkey::commit_inline(
                            self.db_index,
                            key,
                            zset,
                            exp,
                            storage.engine_kind() == StorageEngineKind::AiDb,
                        ));
                    }
                    _ => {
                        let exp = self.resolve_expires_at(storage, &key)?;
                        let mut stored = op_to_stored(op)?;
                        stored.expires_at = exp;
                        batch_ops.push((key, WriteOp::put_stored(stored)));
                    }
                }
            } else if let Some(exp) = self.expire_at.get(&key) {
                if let Some(mut stored) = storage.get_typed(self.db_index, &key)? {
                    stored.expires_at = *exp;
                    batch_ops.push((key, WriteOp::put_stored(stored)));
                }
            }
        }
        self.append_zset_deltas(storage, &mut batch_ops)?;
        Ok(batch_ops)
    }

    pub fn prepare_batch_ops_owned(
        mut self,
        storage: &dyn ScriptIo,
    ) -> Result<Vec<(Vec<u8>, WriteOp)>> {
        let write_buffer = std::mem::take(&mut self.write_buffer);
        let mut batch_ops: Vec<(Vec<u8>, WriteOp)> =
            Vec::with_capacity(write_buffer.len() + self.expire_at.len());
        self.append_owned_writes(storage, write_buffer, &mut batch_ops)?;
        self.append_zset_deltas(storage, &mut batch_ops)?;
        let expire_at = std::mem::take(&mut self.expire_at);
        for (key, exp) in expire_at {
            if self.zset_deltas.contains_key(&key) {
                continue;
            }
            if let Some(mut stored) = storage.get_typed(self.db_index, &key)? {
                stored.expires_at = exp;
                batch_ops.push((key, WriteOp::put_stored(stored)));
            }
        }
        Ok(batch_ops)
    }

    fn append_owned_writes(
        &mut self,
        storage: &dyn ScriptIo,
        write_buffer: HashMap<Vec<u8>, ExtendedBatchOp>,
        batch_ops: &mut Vec<(Vec<u8>, WriteOp)>,
    ) -> Result<()> {
        for (key, op) in write_buffer {
            match op {
                ExtendedBatchOp::Delete => {
                    self.expire_at.remove(&key);
                    batch_ops.push((key, WriteOp::Delete));
                }
                ExtendedBatchOp::SetString(value) => {
                    let exp = self.resolve_expires_at(storage, &key)?;
                    self.expire_at.remove(&key);
                    batch_ops.push((
                        key,
                        WriteOp::put_stored(StoredValue {
                            value: ValueType::String(value),
                            expires_at: exp,
                        }),
                    ));
                }
                ExtendedBatchOp::SetZSet(zset) => {
                    let exp = self.resolve_expires_at(storage, &key)?;
                    self.expire_at.remove(&key);
                    let subkeys = storage.engine_kind() == StorageEngineKind::AiDb;
                    batch_ops.extend(zset_subkey::commit_inline(
                        self.db_index,
                        key,
                        &zset,
                        exp,
                        subkeys,
                    ));
                }
                _ => {
                    let exp = self.resolve_expires_at(storage, &key)?;
                    self.expire_at.remove(&key);
                    let mut stored = op_into_stored(op)?;
                    stored.expires_at = exp;
                    batch_ops.push((key, WriteOp::put_stored(stored)));
                }
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn commit(self, storage: &dyn ScriptIo) -> Result<()> {
        let batch_ops = self.prepare_batch_ops(storage)?;
        if !batch_ops.is_empty() {
            storage.write_batch(self.db_index, batch_ops)?;
        }
        Ok(())
    }
}

fn op_into_stored(op: ExtendedBatchOp) -> Result<StoredValue> {
    match op {
        ExtendedBatchOp::SetString(v) => Ok(StoredValue::string(v)),
        ExtendedBatchOp::SetList(list) => Ok(StoredValue::new_list(list)),
        ExtendedBatchOp::SetHash(hash) => Ok(StoredValue {
            value: ValueType::Hash(hash),
            expires_at: None,
        }),
        ExtendedBatchOp::SetSet(set) => Ok(StoredValue::new_set(set)),
        ExtendedBatchOp::SetZSet(zset) => Ok(StoredValue::new_zset(zset)),
        ExtendedBatchOp::Delete => Err(Error::Command("ERR internal delete op".into())),
    }
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
        ExtendedBatchOp::SetZSet(zset) => Ok(StoredValue::new_zset(zset.clone())),
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
