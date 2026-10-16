//! 同一条 key 上的集群入队顺序.
//! key 锁在在途缓冲写入后放开, 入队在锁外按 key 排队.

use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;
use tokio::sync::watch;

use crate::error::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishState {
    Pending,
    Ready,
    Failed,
}

pub(super) struct EnqueueChain {
    tails: Mutex<HashMap<Vec<u8>, watch::Sender<PublishState>>>,
}

pub(super) struct Publication {
    tx: watch::Sender<PublishState>,
    prev: Vec<watch::Receiver<PublishState>>,
    settled: bool,
}

impl EnqueueChain {
    pub(super) fn new() -> Self {
        Self {
            tails: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn publish(&self, keys: &[&[u8]]) -> Publication {
        let (tx, _rx) = watch::channel(PublishState::Pending);
        let mut prev = Vec::new();
        let mut seen = HashSet::new();
        let mut tails = self.tails.lock();
        for key in keys {
            if !seen.insert(*key) {
                continue;
            }
            if let Some(old) = tails.insert(key.to_vec(), tx.clone()) {
                prev.push(old.subscribe());
            }
        }
        Publication {
            tx,
            prev,
            settled: false,
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
}
