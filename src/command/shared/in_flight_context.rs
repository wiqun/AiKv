//! 跨命令模块全局共享的在途写缓冲与入队依赖链.

use std::sync::Arc;

use crate::command::script::enqueue_chain::EnqueueChain;
use crate::command::script::in_flight::InFlightBuffer;

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
