//! 正在执行的脚本数. 只有「当前仅此一个脚本」时, 单机才在脚本线程内同步落盘.
//!
//! 计数覆盖整个 `execute_script` (含等 key 锁与提交完成), 所以 "独占" 意味着没有别的脚本持有
//! 或等待 key 锁, 也没有别的脚本的异步提交还没完成.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
pub(super) struct ActiveScripts {
    running: AtomicUsize,
    kill_requested: AtomicBool,
    dirty: AtomicBool,
}

pub(super) struct ActiveGuard<'a> {
    owner: &'a ActiveScripts,
}

impl ActiveScripts {
    /// 登记一个脚本开始执行. 返回的 `bool` 为 `true` 表示进入时没有其他脚本在执行.
    pub(super) fn enter(&self) -> (ActiveGuard<'_>, bool) {
        let before = self.running.fetch_add(1, Ordering::AcqRel);
        (ActiveGuard { owner: self }, before == 0)
    }

    pub(super) fn running_count(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }

    pub(super) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    pub(super) fn request_kill(&self) {
        self.kill_requested.store(true, Ordering::Release);
    }

    pub(super) fn is_kill_requested(&self) -> bool {
        self.kill_requested.load(Ordering::Acquire)
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        if self.owner.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.owner.kill_requested.store(false, Ordering::Release);
            self.owner.dirty.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_concurrent_script_is_alone() {
        let active = ActiveScripts::default();
        let (first, first_alone) = active.enter();
        assert!(first_alone);
        let (second, second_alone) = active.enter();
        assert!(!second_alone, "已有脚本在执行时不是独占");
        drop(second);
        let (third, third_alone) = active.enter();
        assert!(!third_alone, "第一个脚本还没结束");
        drop(third);
        drop(first);
        let (_again, again_alone) = active.enter();
        assert!(again_alone, "全部结束后重新独占");
    }

    #[test]
    fn guard_releases_on_early_return() {
        let active = ActiveScripts::default();
        let run = |active: &ActiveScripts| -> Result<(), ()> {
            let (_guard, _alone) = active.enter();
            Err(())
        };
        assert!(run(&active).is_err());
        let (_g, alone) = active.enter();
        assert!(alone, "错误返回也必须归还计数");
    }
}
