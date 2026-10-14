//! 事务在途可见性内存缓冲: 解决 Raft 提交异步期 Lua 读旧快照与锁争用问题.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;

use crate::storage::aidb::AiDbEngine;
use crate::storage::types::StoredValue;
use crate::storage::WriteOp;

#[derive(Clone)]
pub enum InFlightValue {
    Present(StoredValue),
    Deleted,
}

type InFlightEntry = (InFlightValue, u64);
type InFlightMap = Arc<RwLock<HashMap<Vec<u8>, InFlightEntry>>>;

/// 记录未完成持久化的最新在途数据版本.
#[derive(Clone, Default)]
pub struct InFlightBuffer {
    map: InFlightMap,
    seq_gen: Arc<AtomicU64>,
}

impl InFlightBuffer {
    pub fn new() -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
            seq_gen: Arc::new(AtomicU64::new(1)),
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
        }
    }

    /// 将刚刚完成 Lua 内存计算的变更提前发布到在途缓冲并生成 seq.
    pub fn insert_batch(&self, db: usize, ops: &[(Vec<u8>, WriteOp)]) -> u64 {
        let seq = self.seq_gen.fetch_add(1, Ordering::Relaxed);
        let mut map = self.map.write();
        for (k, op) in ops {
            let enc_key = AiDbEngine::encode_key(db, k);
            let val = match op {
                WriteOp::Put(raw) => InFlightValue::Present(StoredValue::string(raw.clone())),
                WriteOp::PutStored(stored) => InFlightValue::Present(stored.clone()),
                WriteOp::Delete => InFlightValue::Deleted,
            };
            map.insert(enc_key, (val, seq));
        }
        seq
    }

    /// 持久化完成后立即移出在途缓冲, 交由底层状态机提供最新数据.
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

    /// 清理所有缓冲.
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.map.write().clear();
    }
}
