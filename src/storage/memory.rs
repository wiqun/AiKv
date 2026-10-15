//! 内存存储引擎 (`MemoryEngine`): 直接实现 `KvStorage`, 不经过 `StorageAdapter` /
//! postcard / key 前缀编码. 每个逻辑 DB 一个 `RwLock<HashMap<Vec<u8>, StoredValue>>`.
//!
//! # Invariant
//!
//! - String `get`/`set` 仅 String; 非 String → `WRONGTYPE` (与 aidb 路径一致).
//! - 惰性过期: `get_entry_read` / `keys` / `scan` / `ttl` 遇过期 key 即从 map 删除并
//!   经 `StorageObservation` 计数 (与 aidb 路径 `try_lazy_expire_delete` 语义一致).
//! - `write_batch` 顺序 await 各操作, **非原子**; aidb 路径单 `WriteBatch` 原子
//!   (行为差异见 `docs/modules/03-storage.md` 已知限制).
//! - `swap_db` 以 `mem::take` 整体交换两个 DB 的 map (O(1)).
//! - `glob_match` 仅支持 `*` / `?`, 不支持 `[abc]` 字符类.
//! - `create_checkpoint` → `ERR Persistence not supported on memory engine`;
//!   `flush` / `close_engine` 为 no-op.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use parking_lot::RwLock;
use rand::seq::SliceRandom;

use crate::error::{Error, Result};
use crate::storage::observation::{PointReadCounter, StorageObservation};
use crate::storage::types::{
    compute_avg_ttl_ms, now_ms, ExpiresCounters, KeyspaceStats, KvStorage, ScanResult,
    StorageEngineKind, StoredValue, ValueType, WriteOp, TTL_NO_EXPIRY, WRONGTYPE,
};
use crate::storage::watch_registry::WatchRegistry;
use crate::storage::watch_version::{
    decode_version, encode_version, is_watch_meta_user_key, meta_user_key,
};

pub struct MemoryEngine {
    databases: Vec<RwLock<HashMap<Vec<u8>, StoredValue>>>,
    observation: Option<Arc<StorageObservation>>,
    point_reads: OnceLock<Arc<PointReadCounter>>,
    watchers: Arc<WatchRegistry>,
    expires: ExpiresCounters,
}

impl MemoryEngine {
    pub fn new(db_count: usize) -> Arc<Self> {
        Self::with_observation(db_count, None)
    }

    pub fn with_observation(
        db_count: usize,
        observation: Option<Arc<StorageObservation>>,
    ) -> Arc<Self> {
        let databases = (0..db_count).map(|_| RwLock::new(HashMap::new())).collect();
        Arc::new(Self {
            databases,
            observation,
            point_reads: OnceLock::new(),
            watchers: WatchRegistry::new(),
            expires: ExpiresCounters::new(),
        })
    }

    fn check_db(&self, db: usize) -> Result<()> {
        if db >= self.databases.len() {
            return Err(Error::Command(format!(
                "ERR DB index is out of range (db={db}, max={})",
                self.databases.len()
            )));
        }
        Ok(())
    }

    fn remove_expired(map: &mut HashMap<Vec<u8>, StoredValue>, key: &[u8]) -> bool {
        if let Some(stored) = map.get(key) {
            if stored.is_expired() {
                map.remove(key);
                return true;
            }
        }
        false
    }

    fn evict_expired_at_key(&self, db: usize, map: &mut HashMap<Vec<u8>, StoredValue>, key: &[u8]) {
        if Self::remove_expired(map, key) {
            self.record_expired(1);
            self.expires.on_ttl_clear(db);
        }
    }

    fn record_expired(&self, count: u64) {
        if count == 0 {
            return;
        }
        if let Some(obs) = &self.observation {
            for _ in 0..count {
                obs.record_expired_key();
            }
        }
    }

    fn purge_expired(map: &mut HashMap<Vec<u8>, StoredValue>) -> u64 {
        let mut removed = 0u64;
        map.retain(|_, v| {
            if v.is_expired() {
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
    }

    fn get_entry_read(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        match map.get(key) {
            None => Ok(None),
            Some(stored) if stored.is_expired() => {
                drop(map);
                let mut map = self.databases[db].write();
                if Self::remove_expired(&mut map, key) {
                    self.record_expired(1);
                    self.expires.on_ttl_clear(db);
                    if !is_watch_meta_user_key(key) {
                        self.bump_watch_version_map(&mut map, db, key);
                    }
                }
                Ok(None)
            }
            Some(stored) => Ok(Some(stored.clone())),
        }
    }

    fn read_watch_version_from_map(map: &HashMap<Vec<u8>, StoredValue>, user_key: &[u8]) -> u64 {
        if is_watch_meta_user_key(user_key) {
            return 0;
        }
        let meta = meta_user_key(user_key);
        map.get(&meta).map_or(0, |stored| match &stored.value {
            ValueType::String(v) => decode_version(v),
            _ => 0,
        })
    }

    fn bump_watch_version_map(
        &self,
        map: &mut HashMap<Vec<u8>, StoredValue>,
        db: usize,
        user_key: &[u8],
    ) {
        if is_watch_meta_user_key(user_key) || !self.watchers.is_watched(db, user_key) {
            return;
        }
        let next = Self::read_watch_version_from_map(map, user_key).saturating_add(1);
        let meta = meta_user_key(user_key);
        map.insert(meta, StoredValue::string(encode_version(next).to_vec()));
    }
}

/// 轻量 glob: 仅 `*` 与 `?`, 不支持 `[abc]` 字符类
pub fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    glob_match_impl(pattern, text, 0, 0)
}

fn glob_match_impl(pattern: &[u8], text: &[u8], pi: usize, ti: usize) -> bool {
    if pi == pattern.len() {
        return ti == text.len();
    }
    if pattern[pi] == b'*' {
        if pi + 1 == pattern.len() {
            return true;
        }
        for j in ti..=text.len() {
            if glob_match_impl(pattern, text, pi + 1, j) {
                return true;
            }
        }
        return false;
    }
    if ti >= text.len() {
        return false;
    }
    if pattern[pi] == b'?' || pattern[pi] == text[ti] {
        return glob_match_impl(pattern, text, pi + 1, ti + 1);
    }
    false
}

impl MemoryEngine {
    fn delete_sync(&self, db: usize, key: &[u8]) -> Result<bool> {
        self.check_db(db)?;
        let mut map = self.databases[db].write();
        if Self::remove_expired(&mut map, key) {
            self.expires.on_ttl_clear(db);
        }
        let removed = map.remove(key);
        if let Some(stored) = &removed {
            if stored.expires_at.is_some() {
                self.expires.on_ttl_clear(db);
            }
        }
        let deleted = removed.is_some();
        if deleted && !is_watch_meta_user_key(key) {
            self.bump_watch_version_map(&mut map, db, key);
        }
        Ok(deleted)
    }
    fn ttl_sync(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        let Some(stored) = map.get(key) else {
            return Ok(None);
        };
        if stored.is_expired() {
            drop(map);
            let mut map = self.databases[db].write();
            self.evict_expired_at_key(db, &mut map, key);
            return Ok(None);
        }
        match stored.expires_at {
            None => Ok(Some(TTL_NO_EXPIRY)),
            Some(expires_at) => {
                let now = now_ms();
                if now >= expires_at {
                    drop(map);
                    let mut map = self.databases[db].write();
                    self.evict_expired_at_key(db, &mut map, key);
                    Ok(None)
                } else {
                    Ok(Some((expires_at - now) as i64))
                }
            }
        }
    }
    fn get_typed_sync(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        let result = self.get_entry_read(db, key);
        if let Ok(value) = &result {
            if let Some(counter) = self.point_reads.get() {
                counter.note(value.is_some());
            }
        }
        result
    }
    fn insert_stored(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        self.check_db(db)?;
        let wrote_ttl = value.expires_at.is_some();
        let mut map = self.databases[db].write();
        map.insert(key.to_vec(), value);
        if wrote_ttl {
            self.expires.on_ttl_write(db);
        }
        if !is_watch_meta_user_key(key) {
            self.bump_watch_version_map(&mut map, db, key);
        }
        Ok(())
    }
}

#[async_trait]
impl KvStorage for MemoryEngine {
    async fn get(&self, db: usize, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let stored = self.get_typed(db, key).await?;
        match stored {
            None => Ok(None),
            Some(s) => match s.value {
                ValueType::String(v) => Ok(Some(v)),
                _ => Err(Error::Command(WRONGTYPE.into())),
            },
        }
    }

    async fn set(&self, db: usize, key: &[u8], value: &[u8]) -> Result<()> {
        self.set_typed(db, key, StoredValue::string(value.to_vec()))
            .await
    }

    async fn set_with_ttl(
        &self,
        db: usize,
        key: &[u8],
        value: &[u8],
        expire_at_ms: u64,
    ) -> Result<()> {
        self.set_typed(
            db,
            key,
            StoredValue {
                value: ValueType::String(value.to_vec()),
                expires_at: Some(expire_at_ms),
            },
        )
        .await
    }

    async fn delete(&self, db: usize, key: &[u8]) -> Result<bool> {
        self.delete_sync(db, key)
    }

    async fn exists(&self, db: usize, key: &[u8]) -> Result<bool> {
        Ok(self.get_typed(db, key).await?.is_some())
    }

    async fn mget(&self, db: usize, keys: &[Vec<u8>]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let stored = self.get_typed(db, key).await?;
            out.push(match stored {
                None => None,
                Some(s) => match s.value {
                    ValueType::String(v) => Some(v),
                    _ => None,
                },
            });
        }
        Ok(out)
    }

    async fn mset(&self, db: usize, pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        for (key, value) in pairs {
            self.set(db, key, value).await?;
        }
        Ok(())
    }

    async fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
        for (key, op) in ops {
            match op {
                WriteOp::Put(value) => self.set(db, &key, &value).await?,
                WriteOp::PutStored(stored) => self.set_typed(db, &key, stored).await?,
                WriteOp::Delete => {
                    self.delete(db, &key).await?;
                }
                WriteOp::PutEngine { .. } | WriteOp::DeleteEngine => {
                    return Err(Error::Storage("raw subkey access not supported".into()));
                }
            }
        }
        Ok(())
    }

    async fn keys(&self, db: usize, pattern: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        let keys: Vec<Vec<u8>> = map
            .iter()
            .filter(|(_, v)| !v.is_expired())
            .filter(|(k, _)| !is_watch_meta_user_key(k))
            .filter(|(k, _)| glob_match(pattern, k))
            .map(|(k, _)| k.clone())
            .collect();
        drop(map);
        // 惰性删除过期 key
        let mut map = self.databases[db].write();
        let removed = Self::purge_expired(&mut map);
        self.record_expired(removed);
        self.expires.on_ttl_clear_n(db, removed);
        Ok(keys)
    }

    async fn scan(
        &self,
        db: usize,
        cursor: u64,
        pattern: &[u8],
        count: usize,
    ) -> Result<ScanResult> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        let mut valid: Vec<Vec<u8>> = map
            .iter()
            .filter(|(_, v)| !v.is_expired())
            .filter(|(k, _)| !is_watch_meta_user_key(k))
            .filter(|(k, _)| pattern.is_empty() || glob_match(pattern, k))
            .map(|(k, _)| k.clone())
            .collect();
        drop(map);
        valid.sort();

        let skip = cursor as usize;
        let end = (skip + count).min(valid.len());
        let keys = valid[skip..end].to_vec();
        let next_cursor = if end < valid.len() { end as u64 } else { 0 };
        Ok(ScanResult {
            cursor: next_cursor,
            keys,
        })
    }

    async fn len(&self, db: usize) -> Result<usize> {
        self.check_db(db)?;
        let mut n = 0usize;
        for map in &self.databases {
            n += map
                .read()
                .iter()
                .filter(|(k, v)| !is_watch_meta_user_key(k) && !v.is_expired())
                .count();
        }
        Ok(n)
    }

    async fn expires(&self, db: usize) -> Result<u64> {
        self.check_db(db)?;
        Ok(self.expires.get(db))
    }

    async fn release_seen_ttl(&self, db: usize) -> Result<()> {
        self.check_db(db)?;
        self.expires.on_ttl_clear(db);
        Ok(())
    }

    async fn sample_avg_ttl(&self, db: usize) -> Result<u64> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        let now = now_ms();
        let remaining = map.iter().filter_map(|(k, v)| {
            if is_watch_meta_user_key(k) || v.is_expired() {
                return None;
            }
            v.expires_at.filter(|exp| *exp > now).map(|exp| exp - now)
        });
        Ok(compute_avg_ttl_ms(remaining))
    }

    async fn keyspace_stats(&self, db: usize) -> Result<KeyspaceStats> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        let now = now_ms();
        let mut keys = 0usize;
        let mut expires = 0usize;
        let mut ttl_remaining = Vec::new();
        for (k, v) in map.iter() {
            if is_watch_meta_user_key(k) || v.is_expired() {
                continue;
            }
            keys += 1;
            if let Some(exp) = v.expires_at {
                expires += 1;
                if exp > now {
                    ttl_remaining.push(exp - now);
                }
            }
        }
        let avg_ttl = crate::storage::types::compute_avg_ttl_ms(ttl_remaining.into_iter());
        Ok(KeyspaceStats {
            keys,
            expires,
            avg_ttl,
        })
    }

    async fn clear(&self, db: usize) -> Result<()> {
        self.check_db(db)?;
        self.databases[db].write().clear();
        self.expires.reset_db(db);
        Ok(())
    }

    async fn clear_all(&self) -> Result<()> {
        for db in &self.databases {
            db.write().clear();
        }
        self.expires.reset_all();
        Ok(())
    }

    async fn expire(&self, db: usize, key: &[u8], ttl_ms: u64) -> Result<bool> {
        self.check_db(db)?;
        let mut map = self.databases[db].write();
        if ttl_ms == 0 {
            if Self::remove_expired(&mut map, key) {
                self.expires.on_ttl_clear(db);
            }
            let removed = map.remove(key);
            if let Some(stored) = &removed {
                if stored.expires_at.is_some() {
                    self.expires.on_ttl_clear(db);
                }
            }
            return Ok(removed.is_some());
        }
        let Some(stored) = map.get(key) else {
            return Ok(false);
        };
        if stored.is_expired() {
            map.remove(key);
            self.record_expired(1);
            self.expires.on_ttl_clear(db);
            return Ok(false);
        }
        let saw_ttl = stored.expires_at.is_some();
        let expires_at = now_ms().saturating_add(ttl_ms);
        if let Some(entry) = map.get_mut(key) {
            entry.expires_at = Some(expires_at);
        }
        self.expires.on_ttl_write(db);
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn expire_at(&self, db: usize, key: &[u8], timestamp_ms: u64) -> Result<bool> {
        self.check_db(db)?;
        let mut map = self.databases[db].write();
        let now = now_ms();
        if timestamp_ms <= now {
            if Self::remove_expired(&mut map, key) {
                self.expires.on_ttl_clear(db);
            }
            let removed = map.remove(key);
            if let Some(stored) = &removed {
                if stored.expires_at.is_some() {
                    self.expires.on_ttl_clear(db);
                }
            }
            return Ok(removed.is_some());
        }
        let Some(stored) = map.get(key) else {
            return Ok(false);
        };
        if stored.is_expired() {
            map.remove(key);
            self.record_expired(1);
            self.expires.on_ttl_clear(db);
            return Ok(false);
        }
        let saw_ttl = stored.expires_at.is_some();
        if let Some(entry) = map.get_mut(key) {
            entry.expires_at = Some(timestamp_ms);
        }
        self.expires.on_ttl_write(db);
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        self.ttl_sync(db, key)
    }

    async fn persist(&self, db: usize, key: &[u8]) -> Result<bool> {
        self.check_db(db)?;
        let mut map = self.databases[db].write();
        if Self::remove_expired(&mut map, key) {
            self.expires.on_ttl_clear(db);
        }
        let Some(entry) = map.get_mut(key) else {
            return Ok(false);
        };
        let saw_ttl = entry.expires_at.is_some();
        entry.expires_at = None;
        if saw_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(true)
    }

    async fn db_count(&self) -> Result<usize> {
        Ok(self.databases.len())
    }

    async fn swap_db(&self, a: usize, b: usize) -> Result<()> {
        self.check_db(a)?;
        self.check_db(b)?;
        if a == b {
            return Ok(());
        }
        let map_a = std::mem::take(&mut *self.databases[a].write());
        let map_b = std::mem::take(&mut *self.databases[b].write());
        *self.databases[a].write() = map_b;
        *self.databases[b].write() = map_a;
        self.expires.swap(a, b);
        Ok(())
    }

    async fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        self.get_typed_sync(db, key)
    }

    fn script_io(&self) -> &dyn crate::storage::script_io::ScriptIo {
        self
    }

    fn set_point_read_counter(&self, counter: Arc<PointReadCounter>) {
        let _ = self.point_reads.set(counter);
    }

    async fn set_typed(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        self.insert_stored(db, key, value)
    }

    async fn get_watch_version(&self, db: usize, key: &[u8]) -> Result<u64> {
        self.check_db(db)?;
        let map = self.databases[db].read();
        Ok(Self::read_watch_version_from_map(&map, key))
    }

    fn watch_registry(&self) -> Arc<WatchRegistry> {
        Arc::clone(&self.watchers)
    }

    async fn rename_key(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<()> {
        self.check_db(db)?;
        if old_key == new_key {
            return Ok(());
        }
        let mut map = self.databases[db].write();
        if Self::remove_expired(&mut map, old_key) {
            self.expires.on_ttl_clear(db);
        }
        let Some(value) = map.remove(old_key) else {
            return Err(Error::Command("ERR no such key".into()));
        };
        map.insert(new_key.to_vec(), value);
        Ok(())
    }

    async fn rename_key_nx(&self, db: usize, old_key: &[u8], new_key: &[u8]) -> Result<bool> {
        self.check_db(db)?;
        if old_key == new_key {
            return Ok(true);
        }
        let mut map = self.databases[db].write();
        if Self::remove_expired(&mut map, old_key) {
            self.expires.on_ttl_clear(db);
        }
        let Some(value) = map.remove(old_key) else {
            return Err(Error::Command("ERR no such key".into()));
        };
        if map.contains_key(new_key) {
            map.insert(old_key.to_vec(), value);
            return Ok(false);
        }
        map.insert(new_key.to_vec(), value);
        Ok(true)
    }

    async fn copy_key(
        &self,
        src_db: usize,
        dst_db: usize,
        src_key: &[u8],
        dst_key: &[u8],
        replace: bool,
    ) -> Result<bool> {
        let Some(stored) = self.get_typed(src_db, src_key).await? else {
            return Ok(false);
        };
        if src_db == dst_db && src_key == dst_key {
            return Ok(replace);
        }
        let existing = self.get_typed(dst_db, dst_key).await?;
        if existing.is_some() && !replace {
            return Ok(false);
        }
        let saw_ttl = existing.and_then(|value| value.expires_at).is_some();
        self.set_typed(dst_db, dst_key, stored.clone()).await?;
        if saw_ttl {
            self.expires.on_ttl_clear(dst_db);
        }
        Ok(true)
    }

    async fn random_key(&self, db: usize) -> Result<Option<Vec<u8>>> {
        let keys = self.keys(db, b"*").await?;
        if keys.is_empty() {
            return Ok(None);
        }
        let mut rng = rand::thread_rng();
        Ok(keys.choose(&mut rng).cloned())
    }

    async fn memory_usage_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        for db in &self.databases {
            let map = db.read();
            for (key, value) in map.iter() {
                if !value.is_expired() {
                    total += key.len() as u64 + value.approximate_heap_bytes();
                }
            }
        }
        Ok(total)
    }
}

impl crate::storage::script_io::ScriptIo for MemoryEngine {
    fn engine_kind(&self) -> StorageEngineKind {
        StorageEngineKind::Memory
    }

    fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        self.get_typed_sync(db, key)
    }

    fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>> {
        self.ttl_sync(db, key)
    }

    fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()> {
        for (key, op) in ops {
            match op {
                WriteOp::Put(value) => {
                    self.insert_stored(db, &key, StoredValue::string(value))?;
                }
                WriteOp::PutStored(stored) => self.insert_stored(db, &key, stored)?,
                WriteOp::Delete => {
                    self.delete_sync(db, &key)?;
                }
                WriteOp::PutEngine { .. } | WriteOp::DeleteEngine => {
                    return Err(Error::Storage("raw subkey access not supported".into()));
                }
            }
        }
        Ok(())
    }

    fn raw_prefix_last(&self, _prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }

    fn raw_range_last(&self, _start: Vec<u8>, _end: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }

    fn raw_subkey_for_each(
        &self,
        _db: usize,
        _prefix: Vec<u8>,
        _f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()> {
        Err(Error::Storage("raw subkey access not supported".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_and_question() {
        assert!(glob_match(b"*", b"foo"));
        assert!(glob_match(b"f*", b"foo"));
        assert!(!glob_match(b"f*", b"bar"));
        assert!(glob_match(b"f?o", b"foo"));
        assert!(!glob_match(b"f?o", b"fooo"));
    }
}
