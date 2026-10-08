//! 分桶 KeyLock. 多 key 按桶号加锁, 同一个桶只锁一次.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::error::{Error, Result};

/// 分桶 key 写锁桶数. 碰撞率 = 并发数 / 桶数.
pub(super) const KEY_LOCK_BUCKETS: usize = 4096;

/// 分桶 key 写锁 (Hash / SET NX/XX / INCR 等)
pub struct KeyLock {
    locks: Vec<Mutex<()>>,
}

impl KeyLock {
    pub fn new(buckets: usize) -> Self {
        let buckets = buckets.max(1);
        Self {
            locks: (0..buckets).map(|_| Mutex::new(())).collect(),
        }
    }

    fn bucket(&self, key: &[u8]) -> usize {
        hash_key(key) % self.locks.len()
    }

    fn sort_keys_by_bucket(&self, keys: &mut Vec<&[u8]>) {
        keys.sort_by(|left, right| {
            self.bucket(left)
                .cmp(&self.bucket(right))
                .then(left.cmp(right))
        });
        keys.dedup_by(|left, right| self.bucket(left) == self.bucket(right));
    }

    pub async fn lock(&self, key: &[u8]) -> tokio::sync::MutexGuard<'_, ()> {
        let idx = self.bucket(key);
        self.locks[idx].lock().await
    }

    /// 按桶号加锁. 同一个桶只锁一次.
    pub async fn lock_two(
        &self,
        a: &[u8],
        b: &[u8],
    ) -> (
        tokio::sync::MutexGuard<'_, ()>,
        Option<tokio::sync::MutexGuard<'_, ()>>,
    ) {
        if a == b || self.bucket(a) == self.bucket(b) {
            return (self.lock(a).await, None);
        }
        if self.bucket(a) < self.bucket(b) {
            let ga = self.lock(a).await;
            let gb = self.lock(b).await;
            (ga, Some(gb))
        } else {
            let gb = self.lock(b).await;
            let ga = self.lock(a).await;
            (ga, Some(gb))
        }
    }

    /// 多 key 按桶号加锁. 同一个桶只锁一次. Drop 时逆序释放.
    pub async fn lock_keys_sorted<'a>(&'a self, keys: &[&[u8]]) -> KeyLocksGuard<'a> {
        let mut unique: Vec<&[u8]> = keys.to_vec();
        self.sort_keys_by_bucket(&mut unique);
        let mut guards = Vec::with_capacity(unique.len());
        for k in unique {
            guards.push(self.lock(k).await);
        }
        KeyLocksGuard { locks: guards }
    }

    /// 多 key 按桶号加锁, 带总超时. 超时或部分失败时已持有锁随 guard drop 释放.
    pub async fn lock_keys_sorted_with_timeout<'a>(
        &'a self,
        keys: &[&[u8]],
        timeout: Duration,
    ) -> Result<KeyLocksGuard<'a>> {
        let mut unique: Vec<&[u8]> = keys.to_vec();
        self.sort_keys_by_bucket(&mut unique);
        if unique.is_empty() {
            return Ok(KeyLocksGuard { locks: Vec::new() });
        }

        let deadline = Instant::now() + timeout;
        let mut guards = Vec::with_capacity(unique.len());
        for k in unique {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(script_lock_timeout_err(timeout));
            }
            match tokio::time::timeout(remaining, self.lock(k)).await {
                Ok(guard) => guards.push(guard),
                Err(_) => return Err(script_lock_timeout_err(timeout)),
            }
        }
        Ok(KeyLocksGuard { locks: guards })
    }
}

fn script_lock_timeout_err(timeout: Duration) -> Error {
    Error::Command(format!("ERR Lock acquisition timeout after {timeout:?}"))
}

/// 多 key 锁 RAII guard; Vec 逆序 drop 释放锁
pub struct KeyLocksGuard<'a> {
    locks: Vec<tokio::sync::MutexGuard<'a, ()>>,
}

impl Drop for KeyLocksGuard<'_> {
    fn drop(&mut self) {
        while self.locks.pop().is_some() {}
    }
}

fn hash_key(key: &[u8]) -> usize {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish() as usize
}

#[cfg(test)]
mod tests {
    use super::KeyLock;

    #[tokio::test]
    async fn one_bucket_locks_two_keys_without_waiting() {
        let lock = KeyLock::new(1);
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            lock.lock_two(b"a", b"b"),
        )
        .await;
        let (_first, second) = result.expect("same bucket must not deadlock");
        assert!(second.is_none());
    }
}
