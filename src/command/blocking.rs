//! 阻塞命令基础设施 (BLPOP/BRPOP/BLMOVE/BZPOPMIN/BZPOPMAX)
//!
//! key 上无等待者时 notify 开销为一次 DashMap 查找.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::{oneshot, Notify};

use crate::protocol::RespValue;
use crate::server::ServerMetrics;

/// 后台清理过期 waiter 的 tick 间隔
const EVICTION_INTERVAL: Duration = Duration::from_secs(1);

/// 阻塞命令类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum BlockingCmd {
    Blpop,
    Brpop,
    Blmove,
    Bzpopmin,
    Bzpopmax,
}

enum WaiterSignal {
    Oneshot(oneshot::Sender<RespValue>),
    Notify(Arc<Notify>),
}

/// 挂起的阻塞请求
pub struct PendingRequest {
    signal: WaiterSignal,
    /// 过期时间 (被 evict_expired 读取)
    pub deadline: Instant,
}

/// 全局阻塞注册表 — key → 挂起请求列表
pub struct BlockingRegistry {
    /// key → pending blocking requests
    waiters: DashMap<Vec<u8>, Vec<PendingRequest>>,
}

impl Default for BlockingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockingRegistry {
    pub fn new() -> Self {
        Self {
            waiters: DashMap::new(),
        }
    }

    /// 注册阻塞等待.返回 oneshot::Receiver.
    pub fn register(&self, key: Vec<u8>, timeout: Duration) -> oneshot::Receiver<RespValue> {
        let (tx, rx) = oneshot::channel();
        let pending = PendingRequest {
            signal: WaiterSignal::Oneshot(tx),
            deadline: Instant::now() + timeout,
        };
        self.waiters.entry(key).or_default().push(pending);
        rx
    }

    /// 注册共享 `Notify` 事件唤醒 (用于 `BLPOP` / `BRPOP` 多 key 等待).
    pub fn register_notify(&self, key: Vec<u8>, notify: Arc<Notify>, timeout: Duration) {
        let pending = PendingRequest {
            signal: WaiterSignal::Notify(notify),
            deadline: Instant::now() + timeout,
        };
        self.waiters.entry(key).or_default().push(pending);
    }

    /// 通知等待某个 key 的所有阻塞请求
    pub fn notify(&self, key: &[u8], response: RespValue) {
        let mut entry = match self.waiters.get_mut(key) {
            Some(e) => e,
            None => return,
        };
        let pending = std::mem::take(&mut *entry);
        for req in pending {
            match req.signal {
                WaiterSignal::Oneshot(tx) => {
                    let _ = tx.send(response.clone());
                }
                WaiterSignal::Notify(n) => {
                    n.notify_one();
                }
            }
        }
    }

    /// 清理过期请求 (由后台定时器调用)
    pub fn evict_expired(&self) {
        let now = Instant::now();
        self.waiters.retain(|_, waiters| {
            waiters.retain(|r| r.deadline > now);
            !waiters.is_empty()
        });
    }

    /// 全局单例
    pub fn global() -> &'static BlockingRegistry {
        static REGISTRY: OnceLock<BlockingRegistry> = OnceLock::new();
        REGISTRY.get_or_init(BlockingRegistry::new)
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.waiters.len()
    }

    #[cfg(test)]
    pub(crate) fn waiter_count(&self, key: &[u8]) -> usize {
        self.waiters.get(key).map(|v| v.len()).unwrap_or(0)
    }
}

/// 启动后台过期 waiter 清理 (server 启动时调用一次)
pub fn start_background_eviction() {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(EVICTION_INTERVAL);
        loop {
            interval.tick().await;
            BlockingRegistry::global().evict_expired();
        }
    });
}

/// Redis 阻塞超时返回 nil Array (not nil bulk)
pub fn nil_blocking_response() -> RespValue {
    RespValue::Array(None)
}

/// 阻塞等待期间保持 `blocked_clients` +1, Drop 时 -1.
pub struct BlockedClientGuard {
    metrics: Option<Arc<ServerMetrics>>,
}

impl BlockedClientGuard {
    pub fn enter(metrics: &Option<Arc<ServerMetrics>>) -> Self {
        if let Some(m) = metrics {
            m.on_client_blocked();
        }
        Self {
            metrics: metrics.clone(),
        }
    }
}

impl Drop for BlockedClientGuard {
    fn drop(&mut self) {
        if let Some(m) = &self.metrics {
            m.on_client_unblocked();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fresh_registry() -> Arc<BlockingRegistry> {
        Arc::new(BlockingRegistry::new())
    }

    #[test]
    fn evict_expired_removes_stale_waiters() {
        let registry = fresh_registry();
        let _rx = registry.register(b"q".to_vec(), Duration::from_millis(10));
        assert_eq!(registry.waiter_count(b"q"), 1);

        std::thread::sleep(Duration::from_millis(20));
        registry.evict_expired();

        assert_eq!(registry.entry_count(), 0);
    }

    #[test]
    fn evict_expired_keeps_active_waiters() {
        let registry = fresh_registry();
        let _rx = registry.register(b"q".to_vec(), Duration::from_secs(60));
        registry.evict_expired();
        assert_eq!(registry.waiter_count(b"q"), 1);
    }

    #[test]
    fn evict_expired_removes_empty_entries_after_notify() {
        let registry = fresh_registry();
        let _rx = registry.register(b"q".to_vec(), Duration::from_secs(60));
        registry.notify(b"q", RespValue::SimpleString("OK".into()));
        assert_eq!(registry.waiter_count(b"q"), 0);
        assert_eq!(registry.entry_count(), 1);

        registry.evict_expired();
        assert_eq!(registry.entry_count(), 0);
    }

    #[test]
    fn blocked_client_guard_balances_metrics() {
        use std::sync::Arc;

        use crate::server::ServerMetrics;

        let metrics = Arc::new(ServerMetrics::default());
        assert_eq!(metrics.blocked_clients(), 0);

        {
            let _guard = super::BlockedClientGuard::enter(&Some(Arc::clone(&metrics)));
            assert_eq!(metrics.blocked_clients(), 1);
        }
        assert_eq!(metrics.blocked_clients(), 0);
    }

    #[test]
    fn evict_expired_is_idempotent() {
        let registry = fresh_registry();
        registry.evict_expired();
        registry.evict_expired();
        assert_eq!(registry.entry_count(), 0);
    }
}
