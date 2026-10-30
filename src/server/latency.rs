//! 命令延迟统计 (Phase 10-ext Step 0, 内存版)

use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MAX_LATENCY_HISTORY: usize = 128;
/// 只有不低于该耗时 (微秒) 的调用才进入历史样本, 与 Redis latency-monitor 只记录超阈值事件一致.
const HISTORY_MIN_US: u64 = 1_000;

/// 直方图桶上界 (微秒), 与 Redis latency-tracking 对齐
pub const LATENCY_BUCKET_BOUNDS: &[u64] = &[
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1_024, 2_048, 4_096, 8_192, 16_384, 32_768, 65_536,
    131_072, 262_144, 524_288, 1_048_576,
];

#[derive(Debug)]
struct CommandLatency {
    calls: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    buckets: Vec<AtomicU64>,
    /// 历史样本 (timestamp_s, duration_ms), 最多保留 MAX_LATENCY_HISTORY 条
    history: Mutex<VecDeque<(u64, u64)>>,
}

impl CommandLatency {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
            buckets: LATENCY_BUCKET_BOUNDS
                .iter()
                .map(|_| AtomicU64::new(0))
                .collect(),
            history: Mutex::new(VecDeque::new()),
        }
    }

    fn record(&self, usec: u64) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(usec, Ordering::Relaxed);
        self.max_us.fetch_max(usec, Ordering::Relaxed);
        // 只累加一个桶 (首个上界 >= usec 的桶; 超出最大桶的样本计入最后一个桶), 累积值在快照时再求和.
        let idx = LATENCY_BUCKET_BOUNDS
            .partition_point(|&bound| bound < usec)
            .min(LATENCY_BUCKET_BOUNDS.len() - 1);
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        if usec >= HISTORY_MIN_US {
            let ts_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut hist = self.history.lock();
            if hist.len() >= MAX_LATENCY_HISTORY {
                hist.pop_front();
            }
            hist.push_back((ts_s, usec / 1000));
        }
    }

    /// 各桶的累积计数 (<= 对应上界的样本数), 与 Redis latency-tracking 的直方图语义一致.
    fn cumulative_buckets(&self) -> Vec<u64> {
        let mut running = 0u64;
        self.buckets
            .iter()
            .map(|b| {
                running += b.load(Ordering::Relaxed);
                running
            })
            .collect()
    }

    fn snapshot(&self) -> CommandLatencySnapshot {
        let calls = self.calls.load(Ordering::Relaxed);
        let total_us = self.total_us.load(Ordering::Relaxed);
        let max_us = self.max_us.load(Ordering::Relaxed);
        let cumulative = self.cumulative_buckets();
        let buckets = LATENCY_BUCKET_BOUNDS
            .iter()
            .zip(cumulative.iter())
            .filter(|(_, &count)| count > 0)
            .map(|(&bound, &count)| (bound, count))
            .collect();
        CommandLatencySnapshot {
            calls,
            total_us,
            max_us,
            buckets,
            p50_us: percentile_from_buckets(&cumulative, 0.50),
            p95_us: percentile_from_buckets(&cumulative, 0.95),
            p99_us: percentile_from_buckets(&cumulative, 0.99),
            p999_us: percentile_from_buckets(&cumulative, 0.999),
        }
    }

    fn history_snapshot(&self) -> Vec<(u64, u64)> {
        self.history.lock().iter().cloned().collect()
    }

    fn reset(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.total_us.store(0, Ordering::Relaxed);
        self.max_us.store(0, Ordering::Relaxed);
        for bucket in &self.buckets {
            bucket.store(0, Ordering::Relaxed);
        }
        self.history.lock().clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLatencySnapshot {
    pub calls: u64,
    pub total_us: u64,
    pub max_us: u64,
    pub buckets: Vec<(u64, u64)>,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
}

#[derive(Debug, Default)]
pub struct LatencyStats {
    by_command: RwLock<HashMap<String, Arc<CommandLatency>>>,
}

impl LatencyStats {
    /// 热路径: 命令名已是大写 (调用方绝大多数如此) 时只取读锁并按 `&str` 查表, 不分配.
    pub fn record(&self, command: &str, duration_us: u64) {
        if !command.bytes().any(|b| b.is_ascii_lowercase()) {
            if let Some(lat) = self.by_command.read().get(command) {
                lat.record(duration_us);
                return;
            }
        }
        let key = command.to_ascii_uppercase();
        let lat = Arc::clone(
            self.by_command
                .write()
                .entry(key)
                .or_insert_with(|| Arc::new(CommandLatency::new())),
        );
        lat.record(duration_us);
    }

    pub fn snapshot(&self, command: &str) -> Option<CommandLatencySnapshot> {
        let map = self.by_command.read();
        map.get(&command.to_ascii_uppercase()).map(|c| c.snapshot())
    }

    pub fn histogram_snapshots(
        &self,
        filter: Option<&[&str]>,
    ) -> Vec<(String, CommandLatencySnapshot)> {
        let map = self.by_command.read();
        let filter_upper: Option<Vec<String>> =
            filter.map(|names| names.iter().map(|n| n.to_ascii_uppercase()).collect());
        let mut out = Vec::new();
        for (cmd, stats) in map.iter() {
            if let Some(ref names) = filter_upper {
                if !names.contains(cmd) {
                    continue;
                }
            }
            let snap = stats.snapshot();
            if snap.calls == 0 {
                continue;
            }
            out.push((cmd.to_ascii_lowercase(), snap));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn history(&self, command: &str) -> Vec<(u64, u64)> {
        let map = self.by_command.read();
        match map.get(&command.to_ascii_uppercase()) {
            None => vec![],
            Some(lat) => lat.history_snapshot(),
        }
    }

    pub fn reset(&self, filter: Option<&[&str]>) -> u64 {
        let mut map = self.by_command.write();
        match filter {
            None => {
                let count = map.len() as u64;
                map.clear();
                count
            }
            Some(names) => {
                let mut count = 0u64;
                for name in names {
                    let key = name.to_ascii_uppercase();
                    if let Some(stats) = map.get(&key) {
                        stats.reset();
                        count += 1;
                    }
                }
                count
            }
        }
    }
}

fn percentile_from_buckets(cumulative: &[u64], quantile: f64) -> u64 {
    let total = cumulative.last().copied().unwrap_or(0);
    if total == 0 {
        return 0;
    }
    let target = ((total as f64) * quantile).ceil() as u64;
    for (i, &count) in cumulative.iter().enumerate() {
        if count >= target {
            return LATENCY_BUCKET_BOUNDS[i];
        }
    }
    LATENCY_BUCKET_BOUNDS.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_record_and_histogram() {
        let stats = LatencyStats::default();
        stats.record("GET", 10);
        stats.record("GET", 100);
        stats.record("GET", 1000);

        let snap = stats.snapshot("GET").unwrap();
        assert_eq!(snap.calls, 3);
        assert_eq!(snap.total_us, 1110);
        assert_eq!(snap.max_us, 1000);
        assert!(!snap.buckets.is_empty());
        assert!(snap.p50_us >= 10);

        let hists = stats.histogram_snapshots(None);
        assert_eq!(hists.len(), 1);
        assert_eq!(hists[0].0, "get");

        assert_eq!(stats.reset(None), 1);
        assert!(stats.snapshot("GET").is_none());
    }

    /// 旧实现的累积直方图算法 (每个 >= 样本的桶都 +1, 超出最大桶的样本只计入最后一个桶), 作为等价性参照.
    fn reference_cumulative(samples: &[u64]) -> Vec<(u64, u64)> {
        let mut cum = vec![0u64; LATENCY_BUCKET_BOUNDS.len()];
        for &us in samples {
            for (i, &b) in LATENCY_BUCKET_BOUNDS.iter().enumerate() {
                if us <= b {
                    cum[i] += 1;
                }
            }
            if us > *LATENCY_BUCKET_BOUNDS.last().unwrap() {
                *cum.last_mut().unwrap() += 1;
            }
        }
        LATENCY_BUCKET_BOUNDS
            .iter()
            .zip(cum)
            .filter(|(_, c)| *c > 0)
            .map(|(&b, c)| (b, c))
            .collect()
    }

    #[test]
    fn histogram_buckets_match_reference_cumulative_semantics() {
        // 确定性伪随机样本, 覆盖 0, 桶边界, 超出最大桶.
        let mut x = 0x2545F4914F6CDD1Du64;
        let mut samples = vec![0, 1, 2, 3, 1_048_576, 1_048_577, 5_000_000];
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            samples.push(x % 3_000_000);
        }
        let stats = LatencyStats::default();
        for &us in &samples {
            stats.record("SET", us);
        }
        let snap = stats.snapshot("set").unwrap();
        assert_eq!(snap.calls, samples.len() as u64);
        assert_eq!(snap.buckets, reference_cumulative(&samples));
        assert_eq!(snap.max_us, *samples.iter().max().unwrap());
    }

    /// 历史样本只记录 >= 1ms 的慢调用 (与 Redis latency-monitor 只记录超阈值事件一致), 避免每条命令
    /// 都读一次系统时钟并抢历史锁. 旧实现把每次调用 (绝大多数是 0ms) 都记入历史.
    #[test]
    fn history_records_only_slow_calls() {
        let stats = LatencyStats::default();
        stats.record("GET", 10);
        stats.record("GET", 999);
        assert!(stats.history("GET").is_empty(), "亚毫秒调用不应进入历史");
        stats.record("GET", 2_500);
        let hist = stats.history("get");
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].1, 2, "历史样本以毫秒为单位");
        assert_eq!(
            stats.snapshot("GET").unwrap().calls,
            3,
            "计数仍包含全部调用"
        );
    }

    #[test]
    fn concurrent_records_are_all_counted() {
        let stats = std::sync::Arc::new(LatencyStats::default());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let stats = std::sync::Arc::clone(&stats);
                std::thread::spawn(move || {
                    for i in 0..10_000u64 {
                        stats.record("GET", i % 50);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(stats.snapshot("GET").unwrap().calls, 80_000);
    }
}
