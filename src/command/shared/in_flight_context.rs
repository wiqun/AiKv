//! 跨命令模块全局共享的在途写缓冲与入队依赖链.

use std::sync::Arc;

use crate::command::script::enqueue_chain::EnqueueChain;
use crate::command::script::in_flight::InFlightBuffer;
use crate::error::Result;
use crate::storage::types::{BatchWriteHandle, WriteOp};
use crate::storage::KvStorage;

/// 封装并发无锁提交所需的在途写缓冲与 Key 级别发布队列.
#[derive(Clone)]
pub struct InFlightContext {
    pub in_flight: Arc<InFlightBuffer>,
    pub enqueue_chain: Arc<EnqueueChain>,
}

impl Default for InFlightContext {
    fn default() -> Self {
        Self::new()
    }
}

impl InFlightContext {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(InFlightBuffer::new()),
            enqueue_chain: Arc::new(EnqueueChain::new()),
        }
    }
}

/// 绑定底层批处理句柄与在途写清理生命周期的 Commit 结构.
pub struct InFlightCommitHandle {
    inner: Option<Box<dyn BatchWriteHandle>>,
    in_flight: Arc<InFlightBuffer>,
    inserted: (u64, Vec<Vec<u8>>),
}

impl InFlightCommitHandle {
    pub fn new(
        inner: Box<dyn BatchWriteHandle>,
        in_flight: Arc<InFlightBuffer>,
        inserted: (u64, Vec<Vec<u8>>),
    ) -> Self {
        Self {
            inner: Some(inner),
            in_flight,
            inserted,
        }
    }

    pub async fn wait(mut self) -> Result<()> {
        let Some(inner) = self.inner.take() else {
            return Ok(());
        };
        let result = inner.wait().await;
        self.in_flight
            .remove_encoded(&self.inserted.1, self.inserted.0);
        result
    }
}

/// 在持有 KeyLock 的临界区内登记在途缓冲并发布入队序列，随后立即释放 KeyLock (drop(guard))，
/// 在锁外入队底层存储并立刻放行后续等待请求 (publication.succeed())，
/// 使后续并发请求能涌入 batcher 形成批量提交.
pub async fn commit_ops_in_lock<G>(
    storage: &dyn KvStorage,
    in_flight_ctx: &InFlightContext,
    db: usize,
    key: &[u8],
    ops: Vec<(Vec<u8>, WriteOp)>,
    guard: G,
) -> Result<()> {
    if ops.is_empty() {
        drop(guard);
        return Ok(());
    }

    let inserted = in_flight_ctx.in_flight.insert_batch(db, &ops);
    let mut publication = in_flight_ctx.enqueue_chain.publish(&[key]);

    // 关键步骤: 内存修改与在途登记完成后，立刻提前释放 KeyLock!
    drop(guard);

    if !crate::storage::deferred::is_active() {
        tokio::task::yield_now().await;
    }

    if let Err(err) = publication.wait_predecessors().await {
        in_flight_ctx
            .in_flight
            .remove_encoded(&inserted.1, inserted.0);
        return Err(err);
    }

    let handle = match storage.enqueue_batch(db, ops).await {
        Ok(handle) => handle,
        Err(err) => {
            in_flight_ctx
                .in_flight
                .remove_encoded(&inserted.1, inserted.0);
            return Err(err);
        }
    };

    // 关键高并发步骤: 成功入队 batcher 后立刻放行后继，
    // 使后续并发请求能够涌入 batcher 形成批量提案.
    publication.succeed();

    let commit = InFlightCommitHandle::new(handle, Arc::clone(&in_flight_ctx.in_flight), inserted);
    match crate::storage::deferred::defer(Box::new(commit)) {
        Ok(()) => Ok(()),
        Err(commit) => commit.wait().await,
    }
}

#[async_trait::async_trait]
impl BatchWriteHandle for InFlightCommitHandle {
    async fn wait(self: Box<Self>) -> Result<()> {
        (*self).wait().await
    }
}

impl Drop for InFlightCommitHandle {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let in_flight = Arc::clone(&self.in_flight);
            let inserted = std::mem::take(&mut self.inserted);
            match tokio::runtime::Handle::try_current() {
                Ok(rt) => {
                    rt.spawn(async move {
                        let _ = inner.wait().await;
                        in_flight.remove_encoded(&inserted.1, inserted.0);
                    });
                }
                Err(_) => in_flight.remove_encoded(&inserted.1, inserted.0),
            }
        }
    }
}
