//! 同一条 key 上的集群入队顺序.
//! key 锁在在途缓冲写入后放开, 入队在锁外按 key 排队.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;

use crate::error::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishState {
    Pending,
    Ready,
    Failed,
}

type Tails = Arc<Mutex<HashMap<Vec<u8>, watch::Sender<PublishState>>>>;

pub(super) struct EnqueueChain {
    tails: Tails,
}

pub(super) struct Publication {
    tx: watch::Sender<PublishState>,
    prev: Vec<watch::Receiver<PublishState>>,
    settled: bool,
    /// 本次发布登记过的 key, 完成后若仍是队尾就从 `tails` 里摘掉, 否则 `tails` 会随写过的不同 key 单调增长.
    keys: Vec<Vec<u8>>,
    tails: Tails,
}

impl EnqueueChain {
    pub(super) fn new() -> Self {
        Self {
            tails: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    fn tail_count(&self) -> usize {
        self.tails.lock().len()
    }

    pub(super) fn publish(&self, keys: &[&[u8]]) -> Publication {
        let (tx, _rx) = watch::channel(PublishState::Pending);
        let mut prev = Vec::new();
        let mut seen = HashSet::new();
        let mut owned = Vec::new();
        let mut tails = self.tails.lock();
        for key in keys {
            if !seen.insert(*key) {
                continue;
            }
            owned.push(key.to_vec());
            if let Some(old) = tails.insert(key.to_vec(), tx.clone()) {
                prev.push(old.subscribe());
            }
        }
        drop(tails);
        Publication {
            tx,
            prev,
            settled: false,
            keys: owned,
            tails: Arc::clone(&self.tails),
        }
    }
}

impl Publication {
    pub(super) async fn wait_predecessors(&mut self) -> Result<()> {
        for rx in &mut self.prev {
            wait_one(rx).await?;
        }
        Ok(())
    }

    pub(super) fn succeed(&mut self) {
        self.settle(PublishState::Ready);
    }

    fn fail(&mut self) {
        self.settle(PublishState::Failed);
    }

    fn settle(&mut self, state: PublishState) {
        if self.settled {
            return;
        }
        self.settled = true;
        let _ = self.tx.send_replace(state);
        // 已经订阅了本次发布的后继持有自己的 receiver, 不受影响; 之后新来的发布不需要等已完成的前驱.
        let mut tails = self.tails.lock();
        for key in &self.keys {
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
}
