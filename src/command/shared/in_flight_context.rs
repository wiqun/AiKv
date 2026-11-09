//! 跨命令模块全局共享的在途写缓冲与入队依赖链.

use std::sync::Arc;

use crate::command::script::enqueue_chain::EnqueueChain;
use crate::command::script::in_flight::InFlightBuffer;
use crate::error::Result;
use crate::storage::types::BatchWriteHandle;

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
