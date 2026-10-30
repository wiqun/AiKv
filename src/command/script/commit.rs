//! 脚本写的「延迟确认」句柄: 把等待 Raft 确认与提交后的在途清理绑在一起.
//!
//! 脚本入队之后 (`publication.succeed()`), 若处于流水线的延迟确认作用域内, 就不再当场等待提交,
//! 而是把本句柄登记给流水线, 由流水线在批末或遇到需要读到已应用数据的命令时按序统一等待.
//! 在等待完成之前, 本脚本的写必须一直留在 `InFlightBuffer` 覆盖层里, 供后续脚本读到.

use std::sync::Arc;

use crate::command::script::in_flight::InFlightBuffer;
use crate::command::script::SEG_TRACKER;
use crate::error::Result;
use crate::storage::types::BatchWriteHandle;

pub(super) struct ScriptCommit {
    inner: Option<Box<dyn BatchWriteHandle>>,
    in_flight: Arc<InFlightBuffer>,
    inserted: (u64, Vec<Vec<u8>>),
    seg_id: usize,
    slot_idx: usize,
}

impl ScriptCommit {
    pub(super) fn new(
        inner: Box<dyn BatchWriteHandle>,
        in_flight: Arc<InFlightBuffer>,
        inserted: (u64, Vec<Vec<u8>>),
        seg_id: usize,
        slot_idx: usize,
    ) -> Self {
        Self {
            inner: Some(inner),
            in_flight,
            inserted,
            seg_id,
            slot_idx,
        }
    }
}

/// 提交结束 (无论成败) 后清理: 把本脚本的写移出在途覆盖层, 并登记段追踪完成.
fn finish(
    in_flight: &InFlightBuffer,
    inserted: &(u64, Vec<Vec<u8>>),
    seg_id: usize,
    slot_idx: usize,
) {
    in_flight.remove_encoded(&inserted.1, inserted.0);
    SEG_TRACKER.on_committed(seg_id, slot_idx);
}

#[async_trait::async_trait]
impl BatchWriteHandle for ScriptCommit {
    async fn wait(mut self: Box<Self>) -> Result<()> {
        let inner = self.inner.take().expect("ScriptCommit waited twice");
        let result = inner.wait().await;
        finish(&self.in_flight, &self.inserted, self.seg_id, self.slot_idx);
        result
    }
}

impl Drop for ScriptCommit {
    /// 句柄未被等待就被丢弃 (如连接异常中断): 写已经入队, 仍会提交. 在后台等它结束再清理覆盖层,
    /// 既不让其他脚本在提交完成前读到旧值, 也不泄漏覆盖层条目.
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        let in_flight = Arc::clone(&self.in_flight);
        let inserted = std::mem::take(&mut self.inserted);
        let (seg_id, slot_idx) = (self.seg_id, self.slot_idx);
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    let _ = inner.wait().await;
                    finish(&in_flight, &inserted, seg_id, slot_idx);
                });
            }
            Err(_) => finish(&in_flight, &inserted, seg_id, slot_idx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::storage::WriteOp;
    use std::time::Duration;
    use tokio::sync::oneshot;

    struct Gate(oneshot::Receiver<bool>);

    #[async_trait::async_trait]
    impl BatchWriteHandle for Gate {
        async fn wait(self: Box<Self>) -> Result<()> {
            match self.0.await {
                Ok(true) => Ok(()),
                _ => Err(Error::Storage("commit failed".into())),
            }
        }
    }

    fn pending_commit(buffer: &Arc<InFlightBuffer>) -> (ScriptCommit, oneshot::Sender<bool>) {
        let ops = vec![(b"k".to_vec(), WriteOp::put(b"v".to_vec()))];
        let inserted = buffer.insert_batch(0, &ops);
        let (seg_id, slot_idx) = SEG_TRACKER.on_start();
        SEG_TRACKER.on_enqueued(slot_idx);
        let (tx, rx) = oneshot::channel();
        let commit = ScriptCommit::new(
            Box::new(Gate(rx)),
            Arc::clone(buffer),
            inserted,
            seg_id,
            slot_idx,
        );
        (commit, tx)
    }

    #[tokio::test]
    async fn successful_wait_clears_in_flight_only_after_commit() {
        let buffer = Arc::new(InFlightBuffer::new());
        let (commit, tx) = pending_commit(&buffer);
        assert!(buffer.get(0, b"k").is_some(), "提交前写必须留在覆盖层");
        let waiter = tokio::spawn(Box::new(commit).wait());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(buffer.get(0, b"k").is_some(), "提交未完成时不得清理覆盖层");
        tx.send(true).unwrap();
        waiter.await.unwrap().expect("commit ok");
        assert!(buffer.get(0, b"k").is_none(), "提交完成后必须清理覆盖层");
    }

    #[tokio::test]
    async fn failed_commit_still_clears_in_flight_and_reports_error() {
        let buffer = Arc::new(InFlightBuffer::new());
        let (commit, tx) = pending_commit(&buffer);
        tx.send(false).unwrap();
        assert!(Box::new(commit).wait().await.is_err());
        assert!(buffer.get(0, b"k").is_none(), "失败也必须清理覆盖层");
    }

    /// 流水线异常中断时句柄会在未等待的情况下被丢弃: 必须等提交真正结束后再清理覆盖层, 而不是立刻移除或泄漏.
    #[tokio::test]
    async fn dropped_without_wait_cleans_up_after_commit_finishes() {
        let buffer = Arc::new(InFlightBuffer::new());
        let (commit, tx) = pending_commit(&buffer);
        drop(commit);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(buffer.get(0, b"k").is_some(), "提交未完成时不得提前清理");
        tx.send(true).unwrap();
        for _ in 0..100 {
            if buffer.get(0, b"k").is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("提交完成后覆盖层仍未清理 (泄漏)");
    }
}
