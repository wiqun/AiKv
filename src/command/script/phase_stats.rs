//! 把锁内 Lua 时间拆开. 平均值是微秒总和除以脚本条数.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::call_stats::ScriptCallStats;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ScriptPhaseStats {
    pub(super) hop_us: u64,
    pub(super) setup_us: u64,
    pub(super) gap_us: u64,
    pub(super) teardown_us: u64,
    pub(super) prepare_us: u64,
}

pub(super) fn gap_us(call_wall_us: u64, calls: ScriptCallStats) -> u64 {
    call_wall_us.saturating_sub(calls.accounted_us())
}

pub(super) fn micros_between(start: Instant, end: Instant) -> u64 {
    end.checked_duration_since(start)
        .map(|elapsed| elapsed.as_micros() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default)]
struct EvalSpan {
    setup_us: u64,
    call_wall_us: u64,
    teardown_us: u64,
}

std::thread_local! {
    static EVAL_SPAN: std::cell::Cell<EvalSpan> = const { std::cell::Cell::new(EvalSpan {
        setup_us: 0,
        call_wall_us: 0,
        teardown_us: 0,
    }) };
}

pub(super) fn store_eval_span(setup_us: u64, call_wall_us: u64, teardown_us: u64) {
    EVAL_SPAN.with(|cell| {
        cell.set(EvalSpan {
            setup_us,
            call_wall_us,
            teardown_us,
        });
    });
}

pub(super) fn begin_eval_span() {
    EVAL_SPAN.with(|cell| cell.set(EvalSpan::default()));
}

pub(super) fn take_eval_span() -> (u64, u64, u64) {
    let span = EVAL_SPAN.with(|cell| cell.replace(EvalSpan::default()));
    (span.setup_us, span.call_wall_us, span.teardown_us)
}

struct PhaseSampler {
    samples: AtomicU64,
    hop_us: AtomicU64,
    setup_us: AtomicU64,
    gap_us: AtomicU64,
    teardown_us: AtomicU64,
    prepare_us: AtomicU64,
}

impl PhaseSampler {
    const fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            hop_us: AtomicU64::new(0),
            setup_us: AtomicU64::new(0),
            gap_us: AtomicU64::new(0),
            teardown_us: AtomicU64::new(0),
            prepare_us: AtomicU64::new(0),
        }
    }

    fn record(&self, stats: ScriptPhaseStats) {
        self.hop_us.fetch_add(stats.hop_us, Ordering::Relaxed);
        self.setup_us.fetch_add(stats.setup_us, Ordering::Relaxed);
        self.gap_us.fetch_add(stats.gap_us, Ordering::Relaxed);
        self.teardown_us
            .fetch_add(stats.teardown_us, Ordering::Relaxed);
        self.prepare_us
            .fetch_add(stats.prepare_us, Ordering::Relaxed);
        let n = self.samples.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(1000) {
            self.log(n);
        }
    }

    fn log(&self, n: u64) {
        tracing::info!(
            target: "perf",
            "[SCRIPT_PHASE_PERF] samples={} avg_hop_us={} avg_setup_us={} avg_gap_us={} avg_teardown_us={} avg_prepare_us={}",
            n,
            self.hop_us.load(Ordering::Relaxed) / n,
            self.setup_us.load(Ordering::Relaxed) / n,
            self.gap_us.load(Ordering::Relaxed) / n,
            self.teardown_us.load(Ordering::Relaxed) / n,
            self.prepare_us.load(Ordering::Relaxed) / n
        );
    }
}

static PHASE_SAMPLER: PhaseSampler = PhaseSampler::new();

pub(super) fn record_script_phase(stats: ScriptPhaseStats) {
    PHASE_SAMPLER.record(stats);
}

#[cfg(test)]
mod tests {
    use super::gap_us;
    use crate::command::script::call_stats::ScriptCallStats;

    #[test]
    fn gap_excludes_redis_call_time() {
        let calls = ScriptCallStats {
            string_us: 120,
            zadd_us: 30,
            ..ScriptCallStats::default()
        };
        assert_eq!(gap_us(500, calls), 350);
        assert_eq!(gap_us(100, calls), 0);
    }
}
