//! 同一条 key 上的集群入队顺序.
//! key 锁在在途缓冲写入后放开, 入队在锁外按 key 排队.

use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;

use crate::error::{Error, Result};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishState {
    Pending,
    Ready,
    Failed,
}

type TailShards = Arc<[Mutex<FxHashMap<Vec<u8>, watch::Sender<PublishState>>>; SHARD_COUNT]>;

pub struct EnqueueChain {
    shards: TailShards,
}

impl Default for EnqueueChain {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Publication {
    tx: watch::Sender<PublishState>,
    prev: Vec<watch::Receiver<PublishState>>,
    settled: bool,
    /// 本次发布登记过的 key, 完成后若仍是队尾就从对应分片中摘掉, 否则会随写过的不同 key 单调增长.
    keys: Vec<Vec<u8>>,
    shards: TailShards,
}

impl EnqueueChain {
    pub fn new() -> Self {
        Self {
            shards: Arc::new(std::array::from_fn(|_| Mutex::new(FxHashMap::default()))),
        }
    }

    #[cfg(test)]
    fn tail_count(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    pub fn publish(&self, keys: &[&[u8]]) -> Publication {
        let (tx, _rx) = watch::channel(PublishState::Pending);
        if keys.len() == 1 {
            let key = keys[0];
            let shard = &self.shards[shard_index(key)];
            let prev = {
                let mut tails = shard.lock();
                tails
                    .insert(key.to_vec(), tx.clone())
                    .map(|old| vec![old.subscribe()])
                    .unwrap_or_default()
            };
            return Publication {
                tx,
                prev,
                settled: false,
                keys: vec![key.to_vec()],
                shards: Arc::clone(&self.shards),
            };
        }

        let mut prev = Vec::new();
        let mut seen = FxHashSet::with_capacity_and_hasher(keys.len(), Default::default());
        let mut owned = Vec::with_capacity(keys.len());
        let mut by_shard: Vec<(usize, Vec<u8>)> = Vec::with_capacity(keys.len());
        for key in keys {
            if !seen.insert(*key) {
                continue;
            }
            let key_vec = key.to_vec();
            by_shard.push((shard_index(key), key_vec.clone()));
            owned.push(key_vec);
        }
        // 按分片升序加锁，避免多 key 并发发布产生交叉锁序。
        by_shard.sort_unstable_by_key(|(idx, _)| *idx);
        let mut i = 0;
        while i < by_shard.len() {
            let idx = by_shard[i].0;
            let mut tails = self.shards[idx].lock();
            while i < by_shard.len() && by_shard[i].0 == idx {
                let key_vec = std::mem::take(&mut by_shard[i].1);
                if let Some(old) = tails.insert(key_vec, tx.clone()) {
                    prev.push(old.subscribe());
                }
                i += 1;
            }
        }
        Publication {
            tx,
            prev,
            settled: false,
            keys: owned,
            shards: Arc::clone(&self.shards),
        }
    }
}

impl Publication {
    pub async fn wait_predecessors(&mut self) -> Result<()> {
        for rx in &mut self.prev {
            wait_one(rx).await?;
        }
        Ok(())
    }

    pub fn succeed(&mut self) {
        self.settle(PublishState::Ready);
    }

    pub fn fail(&mut self) {
        self.settle(PublishState::Failed);
    }

    fn settle(&mut self, state: PublishState) {
        if self.settled {
            return;
        }
        self.settled = true;
        let _ = self.tx.send_replace(state);
        // 已经订阅了本次发布的后继持有自己的 receiver, 不受影响; 之后新来的发布不需要等已完成的前驱.
        if self.keys.len() == 1 {
            let key = &self.keys[0];
            let mut tails = self.shards[shard_index(key)].lock();
            if tails
                .get(key)
                .is_some_and(|tail| tail.same_channel(&self.tx))
            {
                tails.remove(key);
            }
            return;
        }
        for key in &self.keys {
            let mut tails = self.shards[shard_index(key)].lock();
            if tails
                .get(key)
                .is_some_and(|tail| tail.same_channel(&self.tx))
            {
                tails.remove(key);
            }
        }
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.fail();
    }
}

async fn wait_one(rx: &mut watch::Receiver<PublishState>) -> Result<()> {
    loop {
        let state = *rx.borrow();
        match state {
            PublishState::Ready => return Ok(()),
            PublishState::Failed => {
                return Err(Error::Command("ERR previous script write failed".into()));
            }
            PublishState::Pending => {
                if rx.changed().await.is_err() {
                    return Err(Error::Command("ERR previous script write failed".into()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EnqueueChain;

    #[tokio::test]
    async fn successor_runs_after_previous_enqueue_succeeds() {
        let chain = EnqueueChain::new();
        let mut first = chain.publish(&[b"a"]);
        let mut second = chain.publish(&[b"a"]);
        let wait = tokio::spawn(async move {
            second.wait_predecessors().await.expect("pred");
            second.succeed();
        });
        tokio::task::yield_now().await;
        first.succeed();
        wait.await.expect("join");
    }

    #[tokio::test]
    async fn successor_fails_when_previous_enqueue_fails() {
        let chain = EnqueueChain::new();
        let mut first = chain.publish(&[b"a"]);
        let mut second = chain.publish(&[b"a"]);
        first.fail();
        let err = second.wait_predecessors().await.expect_err("pred");
        assert!(
            err.to_string().contains("previous script write failed"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn successor_sees_ready_after_previous_publication_dropped() {
        let chain = EnqueueChain::new();
        let mut first = chain.publish(&[b"a"]);
        first.succeed();
        drop(first);
        let mut second = chain.publish(&[b"a"]);
        second.wait_predecessors().await.expect("pred still ready");
        second.succeed();
    }

    #[tokio::test]
    async fn different_keys_do_not_wait() {
        let chain = EnqueueChain::new();
        let mut other = chain.publish(&[b"a"]);
        let mut mine = chain.publish(&[b"b"]);
        mine.wait_predecessors().await.expect("no pred");
        mine.succeed();
        other.succeed();
    }

    #[test]
    fn settled_publications_do_not_accumulate_tails() {
        let chain = EnqueueChain::new();
        for i in 0..1000u32 {
            let key = i.to_be_bytes();
            let mut p = chain.publish(&[key.as_slice()]);
            p.succeed();
        }
        assert_eq!(chain.tail_count(), 0, "已完成的发布不应继续占着 key");
    }

    #[test]
    fn a_pending_tail_is_kept_and_a_later_publication_still_waits_for_it() {
        let chain = EnqueueChain::new();
        let mut first = chain.publish(&[b"a"]);
        let mut second = chain.publish(&[b"a"]);
        assert_eq!(chain.tail_count(), 1);
        first.succeed();
        // first 完成时队尾已是 second, 不能被误删.
        assert_eq!(chain.tail_count(), 1);
        second.succeed();
        assert_eq!(chain.tail_count(), 0);
    }

    #[tokio::test]
    async fn successor_published_before_settle_still_observes_failure() {
        let chain = EnqueueChain::new();
        let first = chain.publish(&[b"a"]);
        let mut second = chain.publish(&[b"a"]);
        drop(first); // 未 succeed 就丢弃 = 失败
        let err = second.wait_predecessors().await.expect_err("pred failed");
        assert!(err.to_string().contains("previous script write failed"));
    }

    #[tokio::test]
    async fn multi_key_publish_dedupes_and_cleans_up_shards() {
        let chain = EnqueueChain::new();
        let mut first = chain.publish(&[b"k1", b"k2", b"k1", b"k3"]);
        let mut second = chain.publish(&[b"k3", b"k4"]);
        assert_eq!(chain.tail_count(), 4);
        first.succeed();
        second.wait_predecessors().await.expect("k3 pred ready");
        second.succeed();
        assert_eq!(chain.tail_count(), 0);
    }
}
