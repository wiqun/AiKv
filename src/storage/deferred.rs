//! 流水线「延迟确认」: 允许一批盲写先按序入队, 稍后统一等待 Raft 确认.
//!
//! 集群里一次纯 `SET` 的耗时几乎全是等待 Raft 提交 (约 2ms). 流水线内逐条等待会把 N 条写拉长到 N 次提交;
//! 并发执行又会破坏流水线内的命令顺序. 折中: 命令仍按序执行, 写在 `enqueue` 之后不等确认,
//! 把确认 handle 登记到当前任务的作用域里, 由流水线在遇到读或批末时统一等待.
//!
//! 只有不读旧值的盲写才应在作用域内执行 (见 `server::connection::pipeline::is_blind_write`).

use std::cell::RefCell;
use std::future::Future;

use crate::storage::types::BatchWriteHandle;

tokio::task_local! {
    static SCOPE: RefCell<Vec<Box<dyn BatchWriteHandle>>>;
}

/// 在当前任务内开启延迟确认作用域, 返回 `f` 的结果与期间登记的 handle (按登记顺序).
pub async fn scope<F: Future>(f: F) -> (F::Output, Vec<Box<dyn BatchWriteHandle>>) {
    SCOPE
        .scope(RefCell::new(Vec::new()), async {
            let out = f.await;
            let handles = SCOPE.with(|s| std::mem::take(&mut *s.borrow_mut()));
            (out, handles)
        })
        .await
}

/// 当前任务是否处在延迟确认作用域内 (即流水线里的可延迟命令).
pub fn is_active() -> bool {
    SCOPE.try_with(|_| ()).is_ok()
}

/// 作用域内登记 `handle` 并返回 `Ok(())`, 调用方不再等待; 作用域外把 `handle` 原样退回, 由调用方自己等待.
pub fn defer(handle: Box<dyn BatchWriteHandle>) -> Result<(), Box<dyn BatchWriteHandle>> {
    let mut slot = Some(handle);
    let registered = SCOPE.try_with(|s| {
        if let Some(h) = slot.take() {
            s.borrow_mut().push(h);
        }
    });
    match (registered, slot) {
        (Ok(()), _) => Ok(()),
        (Err(_), Some(h)) => Err(h),
        (Err(_), None) => unreachable!("handle 只在登记成功时被取走"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, Result};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Tagged {
        tag: usize,
        order: Arc<AtomicUsize>,
        seen: Arc<parking_lot::Mutex<Vec<usize>>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl BatchWriteHandle for Tagged {
        async fn wait(self: Box<Self>) -> Result<()> {
            self.order.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().push(self.tag);
            if self.fail {
                Err(Error::Storage("boom".into()))
            } else {
                Ok(())
            }
        }
    }

    fn tagged(
        tag: usize,
        seen: &Arc<parking_lot::Mutex<Vec<usize>>>,
        fail: bool,
    ) -> Box<dyn BatchWriteHandle> {
        Box::new(Tagged {
            tag,
            order: Arc::new(AtomicUsize::new(0)),
            seen: Arc::clone(seen),
            fail,
        })
    }

    #[tokio::test]
    async fn outside_scope_handle_is_returned_to_caller() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let back = defer(tagged(1, &seen, false)).expect_err("作用域外必须退回 handle");
        back.wait().await.unwrap();
        assert_eq!(*seen.lock(), vec![1]);
    }

    #[tokio::test]
    async fn inside_scope_handles_are_collected_in_registration_order_and_not_awaited() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (out, handles) = scope(async {
            for tag in [3, 1, 2] {
                assert!(defer(tagged(tag, &seen, false)).is_ok());
            }
            "done"
        })
        .await;
        assert_eq!(out, "done");
        assert!(seen.lock().is_empty(), "作用域内登记不应触发等待");
        assert_eq!(handles.len(), 3);
        for h in handles {
            h.wait().await.unwrap();
        }
        assert_eq!(*seen.lock(), vec![3, 1, 2], "必须保持登记顺序");
    }

    #[tokio::test]
    async fn scopes_do_not_leak_into_each_other() {
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (_, first) = scope(async {
            let _ = defer(tagged(1, &seen, false));
        })
        .await;
        let (_, second) = scope(async {}).await;
        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "新作用域不应继承上一个作用域的 handle");
        // 退出作用域后恢复为「退回 handle」语义.
        assert!(defer(tagged(2, &seen, false)).is_err());
    }
    #[tokio::test]
    async fn is_active_only_inside_a_scope() {
        assert!(!is_active());
        let (inside, _) = scope(async { is_active() }).await;
        assert!(inside);
        assert!(!is_active());
    }
}
