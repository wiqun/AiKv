//! 存储层可观测性事件 (与 server metrics 解耦).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 跨存储引擎共享的观测计数器.
#[derive(Debug, Default)]
pub struct StorageObservation {
    expired_keys: AtomicU64,
    orphan_subkeys_deleted: AtomicU64,
    pending_compaction_expired: AtomicU64,
}

impl StorageObservation {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_expired_key(&self) {
        self.expired_keys.fetch_add(1, Ordering::Relaxed);
        self.pending_compaction_expired
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn drain_expired_keys(&self) -> u64 {
        self.expired_keys.swap(0, Ordering::Relaxed)
    }

    /// 累加因 O(1) 删除/覆盖集合 header 而产生的逻辑孤儿 subkey 估算数.
    pub fn record_orphan_subkeys(&self, count: u64) {
        if count > 0 {
            self.orphan_subkeys_deleted
                .fetch_add(count, Ordering::Relaxed);
            self.pending_compaction_expired
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    /// 取出并清零本周期累计的孤儿 subkey 估算数 (用于过期/孤儿压实触发评估).
    pub fn drain_orphan_subkeys(&self) -> u64 {
        self.orphan_subkeys_deleted.swap(0, Ordering::Relaxed)
    }

    /// 判断本周期累计过期键与逻辑删除孤儿数是否达到触发 compaction 的最小阈值.
    pub fn should_trigger_ttl_compaction(&self, min_expired: u64) -> bool {
        min_expired > 0 && self.pending_compaction_expired.load(Ordering::Relaxed) >= min_expired
    }

    /// 触发后台 compaction 后清零待回收累积计数.
    pub fn reset_compaction_expired(&self) {
        self.pending_compaction_expired.store(0, Ordering::Relaxed);
    }
}

/// 存储点查的 hit / miss. 引擎和 `ServerMetrics` 共用同一份.
#[derive(Debug, Default)]
pub struct PointReadCounter {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl PointReadCounter {
    pub fn note(&self, hit: bool) {
        if hit {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_compaction_trigger_respects_threshold_and_resets() {
        let obs = StorageObservation::new();
        assert!(!obs.should_trigger_ttl_compaction(10));
        for _ in 0..5 {
            obs.record_expired_key();
        }
        assert!(!obs.should_trigger_ttl_compaction(10));
        obs.record_orphan_subkeys(5);
        assert!(obs.should_trigger_ttl_compaction(10));
        obs.reset_compaction_expired();
        assert!(!obs.should_trigger_ttl_compaction(10));
        // 不得影响 ServerMetrics 专用的 drain_expired_keys
        assert_eq!(obs.drain_expired_keys(), 5);
    }
}
