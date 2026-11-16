//! 事务在途可见性内存缓冲: 解决 Raft 提交异步期 Lua 读旧快照与锁争用问题.

use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::storage::aidb::AiDbEngine;
use crate::storage::types::StoredValue;
use crate::storage::WriteOp;

const SHARD_COUNT: usize = 64;

#[inline]
fn shard_index(key: &[u8]) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in key {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    ((hash >> 32) ^ hash) as usize & (SHARD_COUNT - 1)
}

#[derive(Clone)]
pub enum InFlightValue {
    String(Arc<Vec<u8>>),
    Stored(Arc<crate::storage::types::StoredValue>),
    /// member / score 索引的原始字节, key 已经是 engine key.
    Raw(Arc<Vec<u8>>),
    Deleted,
}

type InFlightEntry = (InFlightValue, u64);
type InFlightShards = Arc<[RwLock<FxHashMap<Vec<u8>, InFlightEntry>>; SHARD_COUNT]>;

#[derive(Default)]
struct ReadGate {
    readers: usize,
    pending: Vec<(Vec<Vec<u8>>, u64)>,
}

/// 记录未完成持久化的最新在途数据版本 (按 key hash 分为 64 桶以消除全局锁争用).
#[derive(Clone)]
pub struct InFlightBuffer {
    shards: InFlightShards,
    seq_gen: Arc<AtomicU64>,
    gate: Arc<Mutex<ReadGate>>,
}

impl Default for InFlightBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// 范围读取进行中时, 推迟清除在途条目.
pub struct InFlightReadPin {
    buffer: InFlightBuffer,
}

impl Drop for InFlightReadPin {
    fn drop(&mut self) {
        self.buffer.unpin_reads();
    }
}

impl InFlightBuffer {
    pub fn new() -> Self {
        Self {
            shards: Arc::new(std::array::from_fn(|_| RwLock::new(FxHashMap::default()))),
            seq_gen: Arc::new(AtomicU64::new(1)),
            gate: Arc::new(Mutex::new(ReadGate::default())),
        }
    }

    /// 挡住提交完成后的清除, 直到本次存储扫描结束.
    pub fn pin_reads(&self) -> InFlightReadPin {
        self.gate.lock().readers += 1;
        InFlightReadPin {
            buffer: self.clone(),
        }
    }

    fn unpin_reads(&self) {
        let pending = {
            let mut gate = self.gate.lock();
            gate.readers = gate.readers.saturating_sub(1);
            if gate.readers > 0 {
                return;
            }
            std::mem::take(&mut gate.pending)
        };
        for (keys, seq) in pending {
            self.remove_now(&keys, seq);
        }
    }

    /// 获取在途未落盘的最新值.
    /// Some(Some(v)): 存在在途最新版本; Some(None): 在途已被删除 (Tombstone); None: 无在途变更.
    pub fn get(&self, db: usize, key: &[u8]) -> Option<Option<StoredValue>> {
        let enc_key = AiDbEngine::encode_key(db, key);
        let map = self.shards[shard_index(&enc_key)].read();
        let (v, _) = map.get(&enc_key)?;
        match v {
            InFlightValue::String(bytes) => Some(Some(StoredValue::string((**bytes).clone()))),
            InFlightValue::Stored(stored) => Some(Some((**stored).clone())),
            InFlightValue::Deleted => Some(None),
            InFlightValue::Raw(_) => None,
        }
    }

    /// 是否没有任何未落盘 (或尚未清理) 的在途写.
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.read().is_empty())
    }

    /// 前缀下字典序最大的在途写入 key.
    pub fn last_key_with_prefix(&self, prefix: &[u8]) -> Option<Vec<u8>> {
        let mut best: Option<Vec<u8>> = None;
        for shard in self.shards.iter() {
            let map = shard.read();
            for (key, (value, _)) in map.iter() {
                if !key.starts_with(prefix) || matches!(value, InFlightValue::Deleted) {
                    continue;
                }
                if best.as_ref().is_none_or(|current| key > current) {
                    best = Some(key.clone());
                }
            }
        }
        best
    }

    /// 半开区间 `[start, end)` 内字典序最大的在途写入 key.
    pub fn last_key_in_range(&self, start: &[u8], end: &[u8]) -> Option<Vec<u8>> {
        let mut best: Option<Vec<u8>> = None;
        for shard in self.shards.iter() {
            let map = shard.read();
            for (key, (value, _)) in map.iter() {
                if key.as_slice() < start
                    || key.as_slice() >= end
                    || matches!(value, InFlightValue::Deleted)
                {
                    continue;
                }
                if best.as_ref().is_none_or(|current| key > current) {
                    best = Some(key.clone());
                }
            }
        }
        best
    }

    /// 前缀下的原始写入. `None` 表示在途删除.
    pub fn prefix_raw_updates(&self, prefix: &[u8]) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        for shard in self.shards.iter() {
            let map = shard.read();
            for (key, (value, _)) in map.iter() {
                if !key.starts_with(prefix) {
                    continue;
                }
                match value {
                    InFlightValue::Deleted => out.push((key.clone(), None)),
                    InFlightValue::Raw(bytes) => out.push((key.clone(), Some((**bytes).clone()))),
                    InFlightValue::String(_) | InFlightValue::Stored(_) => {}
                }
            }
        }
        out
    }

    /// engine key 上的原始字节. `Some(None)` 表示在途删除.
    pub fn get_raw(&self, engine_key: &[u8]) -> Option<Option<Vec<u8>>> {
        let map = self.shards[shard_index(engine_key)].read();
        let (value, _) = map.get(engine_key)?;
        match value {
            InFlightValue::Raw(bytes) => Some(Some((**bytes).clone())),
            InFlightValue::Deleted => Some(None),
            InFlightValue::String(_) | InFlightValue::Stored(_) => None,
        }
    }

    /// 将刚刚完成 Lua 内存计算的变更提前发布到在途缓冲并生成 seq.
    /// 返回实际写入缓冲的 engine key, 供提交后按原样删除.
    pub fn insert_batch(&self, db: usize, ops: &[(Vec<u8>, WriteOp)]) -> (u64, Vec<Vec<u8>>) {
        let seq = self.seq_gen.fetch_add(1, Ordering::Relaxed);
        if ops.len() == 1 {
            let (k, op) = &ops[0];
            let enc_key = engine_key(db, k, op);
            let val = to_in_flight_value(op);
            self.shards[shard_index(&enc_key)]
                .write()
                .insert(enc_key.clone(), (val, seq));
            return (seq, vec![enc_key]);
        }

        let mut encoded_keys = Vec::with_capacity(ops.len());
        let mut grouped: Vec<(usize, Vec<u8>, InFlightValue)> = Vec::with_capacity(ops.len());
        for (k, op) in ops {
            let enc_key = engine_key(db, k, op);
            let val = to_in_flight_value(op);
            encoded_keys.push(enc_key.clone());
            grouped.push((shard_index(&enc_key), enc_key, val));
        }
        // 按分片升序加锁，保证同一分片只加锁一次且无交叉锁序。
        grouped.sort_unstable_by_key(|(idx, _, _)| *idx);
        let mut i = 0;
        while i < grouped.len() {
            let idx = grouped[i].0;
            let mut map = self.shards[idx].write();
            while i < grouped.len() && grouped[i].0 == idx {
                let enc_key = std::mem::take(&mut grouped[i].1);
                let val = std::mem::replace(&mut grouped[i].2, InFlightValue::Deleted);
                map.insert(enc_key, (val, seq));
                i += 1;
            }
        }
        (seq, encoded_keys)
    }

    /// 持久化完成后立即移出在途缓冲, 交由底层状态机提供最新数据.
    #[allow(dead_code)]
    pub fn remove_batch(&self, db: usize, ops: &[(Vec<u8>, WriteOp)], seq: u64) {
        let keys: Vec<Vec<u8>> = ops
            .iter()
            .map(|(k, _)| AiDbEngine::encode_key(db, k))
            .collect();
        self.remove_now(&keys, seq);
    }

    #[allow(dead_code)]
    pub fn remove_batch_keys(&self, db: usize, keys: &[Vec<u8>], seq: u64) {
        let encoded: Vec<Vec<u8>> = keys
            .iter()
            .map(|key| AiDbEngine::encode_key(db, key))
            .collect();
        self.remove_encoded(&encoded, seq);
    }

    pub fn remove_encoded(&self, keys: &[Vec<u8>], seq: u64) {
        let mut gate = self.gate.lock();
        if gate.readers > 0 {
            gate.pending.push((keys.to_vec(), seq));
            return;
        }
        drop(gate);
        self.remove_now(keys, seq);
    }

    fn remove_now(&self, keys: &[Vec<u8>], seq: u64) {
        if keys.len() == 1 {
            let key = &keys[0];
            let mut map = self.shards[shard_index(key)].write();
            if let Some((_, stored_seq)) = map.get(key) {
                if *stored_seq == seq {
                    map.remove(key);
                }
            }
            return;
        }

        let mut indices: Vec<(usize, usize)> = keys
            .iter()
            .enumerate()
            .map(|(pos, k)| (shard_index(k), pos))
            .collect();
        indices.sort_unstable_by_key(|(idx, _)| *idx);
        let mut i = 0;
        while i < indices.len() {
            let idx = indices[i].0;
            let mut map = self.shards[idx].write();
            while i < indices.len() && indices[i].0 == idx {
                let key = &keys[indices[i].1];
                if let Some((_, stored_seq)) = map.get(key) {
                    if *stored_seq == seq {
                        map.remove(key);
                    }
                }
                i += 1;
            }
        }
    }

    /// 清理所有缓冲.
    #[allow(dead_code)]
    pub fn clear(&self) {
        for shard in self.shards.iter() {
            shard.write().clear();
        }
    }
}

fn to_in_flight_value(op: &WriteOp) -> InFlightValue {
    match op {
        WriteOp::Put(raw) => InFlightValue::String(Arc::clone(raw)),
        WriteOp::PutStored(stored) => InFlightValue::Stored(Arc::clone(stored)),
        WriteOp::PutEngine { value } => InFlightValue::Raw(Arc::clone(value)),
        WriteOp::Delete | WriteOp::DeleteEngine => InFlightValue::Deleted,
    }
}

fn engine_key(db: usize, key: &[u8], op: &WriteOp) -> Vec<u8> {
    match op {
        WriteOp::PutEngine { .. } | WriteOp::DeleteEngine => key.to_vec(),
        _ => AiDbEngine::encode_key(db, key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_read_keeps_inflight_delete_until_scan_finishes() {
        let buf = InFlightBuffer::new();
        let key = b"member-key".to_vec();
        let (seq, keys) = buf.insert_batch(0, &[(key.clone(), WriteOp::DeleteEngine)]);
        let pin = buf.pin_reads();
        buf.remove_encoded(&keys, seq);
        assert!(
            buf.get_raw(&key).is_some(),
            "scan still needs the in-flight delete"
        );
        drop(pin);
        assert!(buf.get_raw(&key).is_none());
    }

    #[test]
    fn is_empty_tracks_unfinished_writes() {
        let buf = InFlightBuffer::new();
        assert!(buf.is_empty());
        let (seq, keys) = buf.insert_batch(0, &[(b"k".to_vec(), WriteOp::put(b"v".to_vec()))]);
        assert!(!buf.is_empty(), "有未落盘的写");
        buf.remove_encoded(&keys, seq);
        assert!(buf.is_empty());
    }

    #[test]
    fn multi_shard_insert_scan_and_remove_work_consistently() {
        let buf = InFlightBuffer::new();
        let ops: Vec<(Vec<u8>, WriteOp)> = (0..128)
            .map(|i| {
                (
                    format!("pref:{i:03}").into_bytes(),
                    WriteOp::put_engine(vec![i as u8]),
                )
            })
            .collect();
        let (seq, keys) = buf.insert_batch(0, &ops);
        assert!(!buf.is_empty());
        assert_eq!(
            buf.last_key_with_prefix(b"pref:"),
            Some(b"pref:127".to_vec())
        );
        assert_eq!(buf.prefix_raw_updates(b"pref:").len(), 128);
        buf.remove_encoded(&keys, seq);
        assert!(buf.is_empty());
    }
}
