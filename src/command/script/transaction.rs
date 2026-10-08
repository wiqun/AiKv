//! 脚本事务缓冲 + commit (保留 TTL)

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::storage::subkey;
use crate::storage::{
    now_ms, AiDbEngine, CollectionKind, KvStorage, StoredValue, ValueType, WRONGTYPE,
};

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
}

impl ScriptTransaction {
    pub fn new(db_index: usize) -> Self {
        Self {
            db_index,
            write_buffer: HashMap::new(),
            expire_at: HashMap::new(),
        }
    }

    pub fn set_string(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetString(value));
    }

    pub fn set_list(&mut self, key: Vec<u8>, list: VecDeque<Vec<u8>>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetList(list));
    }

    pub fn set_hash(&mut self, key: Vec<u8>, hash: HashMap<Vec<u8>, Vec<u8>>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetHash(hash));
    }

    pub fn set_set(&mut self, key: Vec<u8>, set: HashSet<Vec<u8>>) {
        self.write_buffer.insert(key, ExtendedBatchOp::SetSet(set));
    }

    pub fn set_zset(&mut self, key: Vec<u8>, zset: BTreeMap<Vec<u8>, f64>) {
        self.write_buffer
            .insert(key, ExtendedBatchOp::SetZSet(zset));
    }

    pub fn delete(&mut self, key: Vec<u8>) {
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
        storage.get(self.db_index, key).await
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
        let Some(stored) = storage.get_typed(self.db_index, key).await? else {
            return Ok(None);
        };
        // 透明展开 CollectionHeader -> 完整集合, 使 Lua 脚本读路径无需感知 subkey 格式.
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

    pub async fn exists(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<bool> {
        if let Some(op) = self.write_buffer.get(key) {
            return Ok(!matches!(op, ExtendedBatchOp::Delete));
        }
        storage.exists(self.db_index, key).await
    }

    async fn resolve_expires_at(&self, storage: &dyn KvStorage, key: &[u8]) -> Result<Option<u64>> {
        if let Some(exp) = self.expire_at.get(key) {
            return Ok(*exp);
        }
        Ok(storage
            .get_typed(self.db_index, key)
            .await?
            .and_then(|s| s.expires_at))
    }

    pub async fn commit(self, storage: &dyn KvStorage) -> Result<()> {
        let mut keys: HashSet<Vec<u8>> = self.write_buffer.keys().cloned().collect();
        keys.extend(self.expire_at.keys().cloned());

        for key in keys {
            if let Some(op) = self.write_buffer.get(&key) {
                match op {
                    ExtendedBatchOp::Delete => {
                        storage.delete(self.db_index, &key).await?;
                    }
                    ExtendedBatchOp::SetString(value) => {
                        let preserved = !self.expire_at.contains_key(&key);
                        let exp = self.resolve_expires_at(storage, &key).await?;
                        if let Some(at) = exp {
                            storage.set_with_ttl(self.db_index, &key, value, at).await?;
                            if preserved {
                                storage.release_seen_ttl(self.db_index).await?;
                            }
                        } else {
                            storage.set(self.db_index, &key, value).await?;
                        }
                    }
                    _ => {
                        let preserved = !self.expire_at.contains_key(&key);
                        let mut stored = op_to_stored(op)?;
                        stored.expires_at = self.resolve_expires_at(storage, &key).await?;
                        let saw_ttl = preserved && stored.expires_at.is_some();
                        storage.set_typed(self.db_index, &key, stored).await?;
                        if saw_ttl {
                            storage.release_seen_ttl(self.db_index).await?;
                        }
                    }
                }
            } else if self.expire_at.contains_key(&key) {
                if let Some(mut stored) = storage.get_typed(self.db_index, &key).await? {
                    let saw_ttl = stored.expires_at.is_some();
                    stored.expires_at = self.expire_at.get(&key).copied().flatten();
                    storage.set_typed(self.db_index, &key, stored).await?;
                    if saw_ttl {
                        storage.release_seen_ttl(self.db_index).await?;
                    }
                }
            }
        }

        // 批量 Delete/Put 无 TTL 的 string 已通过上面逐 key 处理
        Ok(())
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
