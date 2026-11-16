//! 分桶 KeyLock 与多 key 字典序加锁.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::error::{Error, Result};

/// 分桶 key 写锁桶数. 碰撞率 = 并发数 / 桶数.
pub(super) const KEY_LOCK_BUCKETS: usize = 4096;
const SMALL_DEDUP_LIMIT: usize = 8;

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

    pub async fn lock(&self, key: &[u8]) -> tokio::sync::MutexGuard<'_, ()> {
        let idx = hash_key(key) % self.locks.len();
        self.locks[idx].lock().await
    }

    /// 按 key 字节序加锁; 同一 key 只锁一次 (避免 Mutex 重入死锁)
    pub async fn lock_two(
        &self,
        a: &[u8],
        b: &[u8],
    ) -> (
        tokio::sync::MutexGuard<'_, ()>,
        Option<tokio::sync::MutexGuard<'_, ()>>,
    ) {
        if a == b {
            return (self.lock(a).await, None);
        }
        if a < b {
            let ga = self.lock(a).await;
            let gb = self.lock(b).await;
            (ga, Some(gb))
        } else {
            let gb = self.lock(b).await;
            let ga = self.lock(a).await;
            (ga, Some(gb))
        }
    }

    /// 单 key 带超时加锁 (零堆分配快路径).
    pub async fn lock_one_with_timeout<'a>(
        &'a self,
        key: &[u8],
        timeout: Duration,
    ) -> Result<KeyLocksGuard<'a>> {
        let guard = tokio::time::timeout(timeout, self.lock(key))
            .await
            .map_err(|_| script_lock_timeout_err(timeout))?;
        Ok(KeyLocksGuard::single(guard))
    }

    /// 多 key 字典序加锁 (去重); Drop 时逆序释放
    pub async fn lock_keys_sorted<'a>(&'a self, keys: &[&[u8]]) -> KeyLocksGuard<'a> {
        match keys.len() {
            0 => return KeyLocksGuard::empty(),
            1 => return KeyLocksGuard::single(self.lock(keys[0]).await),
            _ => {}
        }
        let unique = unique_keys_by_bucket(self.locks.len(), keys);
        let mut guards = Vec::with_capacity(unique.len());
        for k in unique {
            guards.push(self.lock(k).await);
        }
        KeyLocksGuard::multi(guards)
    }

    /// 多 key 字典序加锁, 带总超时; 超时或部分失败时已持有锁随 guard drop 释放.
    pub async fn lock_keys_sorted_with_timeout<'a>(
        &'a self,
        keys: &[&[u8]],
        timeout: Duration,
    ) -> Result<KeyLocksGuard<'a>> {
        match keys.len() {
            0 => return Ok(KeyLocksGuard::empty()),
            1 => return self.lock_one_with_timeout(keys[0], timeout).await,
            _ => {}
        }
        let unique = unique_keys_by_bucket(self.locks.len(), keys);
        if unique.is_empty() {
            return Ok(KeyLocksGuard::empty());
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
        Ok(KeyLocksGuard::multi(guards))
    }
}

fn script_lock_timeout_err(timeout: Duration) -> Error {
    Error::Command(format!("ERR Lock acquisition timeout after {timeout:?}"))
}

/// 多 key 锁 RAII guard; 单 key 零堆分配, 多 key Vec 逆序 drop 释放锁.
pub struct KeyLocksGuard<'a> {
    single: Option<tokio::sync::MutexGuard<'a, ()>>,
    locks: Vec<tokio::sync::MutexGuard<'a, ()>>,
}

impl<'a> KeyLocksGuard<'a> {
    fn empty() -> Self {
        Self {
            single: None,
            locks: Vec::new(),
        }
    }

    fn single(guard: tokio::sync::MutexGuard<'a, ()>) -> Self {
        Self {
            single: Some(guard),
            locks: Vec::new(),
        }
    }

    fn multi(locks: Vec<tokio::sync::MutexGuard<'a, ()>>) -> Self {
        Self {
            single: None,
            locks,
        }
    }
}

impl Drop for KeyLocksGuard<'_> {
    fn drop(&mut self) {
        while self.locks.pop().is_some() {}
        self.single.take();
    }
}

/// 按 key 字节序加锁, 同一把桶锁只取一次. 桶锁不可重入, 两个 key 落到同一桶时第二次 lock 会把自己卡住.
fn unique_keys_by_bucket<'a>(buckets: usize, keys: &[&'a [u8]]) -> Vec<&'a [u8]> {
    let buckets = buckets.max(1);
    let mut unique: Vec<&[u8]> = keys.to_vec();
    unique.sort();
    unique.dedup();
    if unique.len() <= SMALL_DEDUP_LIMIT {
        let mut seen_buckets = [usize::MAX; SMALL_DEDUP_LIMIT];
        let mut count = 0usize;
        unique.retain(|key| {
            let idx = hash_key(key) % buckets;
            if seen_buckets[..count].contains(&idx) {
                false
            } else {
                seen_buckets[count] = idx;
                count += 1;
                true
            }
        });
        return unique;
    }
    let mut seen = vec![false; buckets];
    unique.retain(|key| {
        let idx = hash_key(key) % buckets;
        if seen[idx] {
            false
        } else {
            seen[idx] = true;
            true
        }
    });
    unique
}

fn hash_key(key: &[u8]) -> usize {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_key_lock_fast_path_blocks_second_waiter_until_drop() {
        let lock = KeyLock::new(64);
        let guard = lock
            .lock_keys_sorted_with_timeout(&[b"k1"], Duration::from_millis(100))
            .await
            .unwrap();
        let err = lock
            .lock_one_with_timeout(b"k1", Duration::from_millis(20))
            .await;
        assert!(err.is_err());
        drop(guard);
        assert!(lock
            .lock_one_with_timeout(b"k1", Duration::from_millis(50))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn small_multi_key_bucket_collision_does_not_deadlock() {
        let lock = KeyLock::new(1);
        let _guard = lock
            .lock_keys_sorted_with_timeout(&[b"k1", b"k2", b"k3"], Duration::from_millis(50))
            .await
            .unwrap();
    }
}
