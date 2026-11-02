//! 脚本执行位置路由: 轻脚本在当前 worker 线程就地执行, 重脚本派发到脚本线程池.
//!
//! 跨线程交接一次约几十微秒 (虚拟机上更高), 对百微秒级的轻脚本是主要开销, 对毫秒级的重脚本可以忽略;
//! 而重脚本就地执行会占住 tokio worker, 拖慢同 worker 上的其他连接与 Raft 协程.
//! 因此按脚本 sha 记录执行耗时的指数滑动平均 (EWMA), 低于阈值就地执行, 否则进线程池.
//! 业界同类做法: Redis/Kvrocks 就地执行; TiKV 的 yatp 多级反馈队列按任务累计耗时降级.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

/// EWMA 达到该耗时 (微秒) 的脚本进线程池.
const POOL_THRESHOLD_US: u64 = 1_000;
/// 最多记录的脚本数; 超出后新脚本不再记录, 一律视为轻脚本就地执行, 避免动态脚本文本撑爆内存.
const MAX_TRACKED_SCRIPTS: usize = 4096;

/// 解析 `AIKV_SCRIPT_OFF_WORKER`: `1`/`true` 强制线程池, `0`/`false` 强制就地, 其余 (含 `auto`, 空串, 未设置) 为自适应.
pub(super) fn parse_off_worker_override(raw: Option<&str>) -> Option<bool> {
    match raw {
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") => Some(true),
        Some(v) if v == "0" || v.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

pub(super) struct ScriptRouter {
    forced: Option<bool>,
    costs: RwLock<HashMap<String, AtomicU64>>,
}

impl ScriptRouter {
    pub(super) fn new(forced: Option<bool>) -> Self {
        Self {
            forced,
            costs: RwLock::new(HashMap::new()),
        }
    }

    pub(super) fn from_env() -> Self {
        Self::new(parse_off_worker_override(
            std::env::var("AIKV_SCRIPT_OFF_WORKER").ok().as_deref(),
        ))
    }

    /// 该脚本是否应派发到线程池. 未见过的脚本先就地执行, 首次执行后即有耗时记录.
    pub(super) fn use_pool(&self, sha1: &str) -> bool {
        if let Some(forced) = self.forced {
            return forced;
        }
        self.costs
            .read()
            .get(sha1)
            .is_some_and(|c| c.load(Ordering::Relaxed) >= POOL_THRESHOLD_US)
    }

    /// 记录一次执行耗时 (微秒, 与执行位置无关), 更新该脚本的 EWMA (alpha = 1/8).
    pub(super) fn record(&self, sha1: &str, cost_us: u64) {
        if self.forced.is_some() {
            return;
        }
        let cost_us = cost_us.max(1);
        if let Some(c) = self.costs.read().get(sha1) {
            Self::update(c, cost_us);
            return;
        }
        let mut costs = self.costs.write();
        if let Some(c) = costs.get(sha1) {
            Self::update(c, cost_us);
        } else if costs.len() < MAX_TRACKED_SCRIPTS {
            costs.insert(sha1.to_string(), AtomicU64::new(cost_us));
        }
    }

    pub(super) fn clear(&self) {
        self.costs.write().clear();
    }

    fn update(cell: &AtomicU64, cost_us: u64) {
        let old = cell.load(Ordering::Relaxed);
        let new = if old == 0 {
            cost_us
        } else {
            // old + (x - old) / 8, 用有符号运算避免下溢.
            (old as i64 + (cost_us as i64 - old as i64) / 8).max(1) as u64
        };
        cell.store(new, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_override_accepts_only_explicit_bool() {
        assert_eq!(parse_off_worker_override(Some("1")), Some(true));
        assert_eq!(parse_off_worker_override(Some("TRUE")), Some(true));
        assert_eq!(parse_off_worker_override(Some("0")), Some(false));
        assert_eq!(parse_off_worker_override(Some("False")), Some(false));
        assert_eq!(parse_off_worker_override(Some("auto")), None);
        assert_eq!(parse_off_worker_override(Some("")), None);
        assert_eq!(parse_off_worker_override(None), None);
    }

    #[test]
    fn unseen_script_runs_inline() {
        let r = ScriptRouter::new(None);
        assert!(!r.use_pool("sha-new"));
    }

    #[test]
    fn heavy_script_moves_to_pool_and_light_script_stays_inline() {
        let r = ScriptRouter::new(None);
        r.record("heavy", 10_000);
        r.record("light", 100);
        assert!(r.use_pool("heavy"), "10ms 脚本必须进线程池");
        assert!(!r.use_pool("light"), "100µs 脚本必须就地执行");
    }

    #[test]
    fn script_returns_inline_after_it_gets_cheap_again() {
        let r = ScriptRouter::new(None);
        r.record("s", 10_000);
        assert!(r.use_pool("s"));
        for _ in 0..60 {
            r.record("s", 100);
        }
        assert!(!r.use_pool("s"), "持续变轻后 EWMA 应衰减回阈值以下");
    }

    #[test]
    fn forced_override_ignores_cost() {
        let pool = ScriptRouter::new(Some(true));
        pool.record("light", 10);
        assert!(pool.use_pool("light"));
        let inline = ScriptRouter::new(Some(false));
        inline.record("heavy", 50_000);
        assert!(!inline.use_pool("heavy"));
    }

    #[test]
    fn tracking_is_bounded() {
        let r = ScriptRouter::new(None);
        for i in 0..(MAX_TRACKED_SCRIPTS + 100) {
            r.record(&format!("s{i}"), 10_000);
        }
        assert_eq!(r.costs.read().len(), MAX_TRACKED_SCRIPTS);
        assert!(!r.use_pool("s-never-recorded"), "超出上限的脚本视为轻脚本");
        r.clear();
        assert!(r.costs.read().is_empty());
    }
}
