//! 事务在途可见性内存缓冲: 解决 Raft 提交异步期 Lua 读旧快照与锁争用问题.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};

use crate::storage::aidb::AiDbEngine;
use crate::storage::types::StoredValue;
use crate::storage::WriteOp;

#[derive(Clone)]
pub enum InFlightValue {
    Present(StoredValue),
    /// member / score 索引的原始字节, key 已经是 engine key.
    Raw(Vec<u8>),
    Deleted,
}

type InFlightEntry = (InFlightValue, u64);
type InFlightMap = Arc<RwLock<HashMap<Vec<u8>, InFlightEntry>>>;

#[derive(Default)]
struct ReadGate {
    readers: usize,
    pending: Vec<(Vec<Vec<u8>>, u64)>,
}

/// 记录未完成持久化的最新在途数据版本.
#[derive(Clone, Default)]
pub struct InFlightBuffer {
    map: InFlightMap,
    seq_gen: Arc<AtomicU64>,
    gate: Arc<Mutex<ReadGate>>,
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
            map: Arc::new(RwLock::new(HashMap::new())),
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
        let map = self.map.read();
        let (v, _) = map.get(&enc_key)?;
        match v {
            InFlightValue::Present(stored) => Some(Some(stored.clone())),
            InFlightValue::Deleted => Some(None),
            InFlightValue::Raw(_) => None,
        }
    }

    /// 前缀下是否有在途删除. 有的话按 score 取最大值可能读到已经删掉的索引.
    pub fn has_delete_with_prefix(&self, prefix: &[u8]) -> bool {
        let map = self.map.read();
        map.iter().any(|(key, (value, _))| {
            key.starts_with(prefix) && matches!(value, InFlightValue::Deleted)
        })
    }

    /// 前缀下字典序最大的在途写入 key.
    pub fn last_key_with_prefix(&self, prefix: &[u8]) -> Option<Vec<u8>> {
        let map = self.map.read();
        let mut best: Option<Vec<u8>> = None;
        for (key, (value, _)) in map.iter() {
            if !key.starts_with(prefix) || matches!(value, InFlightValue::Deleted) {
                continue;
            }
            if best.as_ref().is_none_or(|current| key > current) {
                best = Some(key.clone());
            }
        }
        best
    }

    /// 前缀下的原始写入. `None` 表示在途删除.
    pub fn prefix_raw_updates(&self, prefix: &[u8]) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
        let map = self.map.read();
        map.iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .filter_map(|(key, (value, _))| match value {
                InFlightValue::Deleted => Some((key.clone(), None)),
                InFlightValue::Raw(bytes) => Some((key.clone(), Some(bytes.clone()))),
                InFlightValue::Present(_) => None,
            })
            .collect()
    }

    /// engine key 上的原始字节. `Some(None)` 表示在途删除.
    pub fn get_raw(&self, engine_key: &[u8]) -> Option<Option<Vec<u8>>> {
        let map = self.map.read();
        let (value, _) = map.get(engine_key)?;
        match value {
            InFlightValue::Raw(bytes) => Some(Some(bytes.clone())),
            InFlightValue::Deleted => Some(None),
            InFlightValue::Present(_) => None,
        }
    }

    /// 将刚刚完成 Lua 内存计算的变更提前发布到在途缓冲并生成 seq.
    /// 返回实际写入缓冲的 engine key, 供提交后按原样删除.
    pub fn insert_batch(&self, db: usize, ops: &[(Vec<u8>, WriteOp)]) -> (u64, Vec<Vec<u8>>) {
        let seq = self.seq_gen.fetch_add(1, Ordering::Relaxed);
        let mut map = self.map.write();
        let mut encoded_keys = Vec::with_capacity(ops.len());
        for (k, op) in ops {
            let enc_key = engine_key(db, k, op);
            let val = match op {
                WriteOp::Put(raw) => InFlightValue::Present(StoredValue::string(raw.clone())),
                WriteOp::PutStored(stored) => InFlightValue::Present(stored.clone()),
                WriteOp::PutEngine { value } => InFlightValue::Raw(value.clone()),
                WriteOp::Delete | WriteOp::DeleteEngine => InFlightValue::Deleted,
            };
            encoded_keys.push(enc_key.clone());
            map.insert(enc_key, (val, seq));
        }
        (seq, encoded_keys)
    }

    /// 持久化完成后立即移出在途缓冲, 交由底层状态机提供最新数据.
    #[allow(dead_code)]
    pub fn remove_batch(&self, db: usize, ops: &[(Vec<u8>, WriteOp)], seq: u64) {
        let mut map = self.map.write();
        for (k, _) in ops {
            let enc_key = AiDbEngine::encode_key(db, k);
            if let Some((_, s)) = map.get(&enc_key) {
                if *s == seq {
                    map.remove(&enc_key);
                }
            }
        }
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
        let mut map = self.map.write();
        for key in keys {
            if let Some((_, stored_seq)) = map.get(key) {
                if *stored_seq == seq {
                    map.remove(key);
                }
            }
        }
    }

    /// 清理所有缓冲.
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.map.write().clear();
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
        assert!(buf.get_raw(&key).is_some(), "scan still needs the in-flight delete");
        drop(pin);
        assert!(buf.get_raw(&key).is_none());
    }
}
