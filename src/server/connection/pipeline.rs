//! Connection Pipeline 并发优化: 将多笔安全命令并行执行并聚合写凑批.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use parking_lot::Mutex;

use crate::error::Result;
use crate::protocol::RespValue;

use super::{Connection, ConnectionMode};

impl Connection {
    /// 尝试对同一批中的命令进行流水线并发执行.
    /// 若包含事务、阻塞或状态敏感命令, 则回退到顺序执行.
    pub(super) async fn process_pipeline_batch(&mut self, batch: Vec<RespValue>) -> Result<()> {
        if let Some(timing) = self.pending_timing.take() {
            timing.report_from_buffer();
        }
        if self.tx_state.in_multi || self.mode != ConnectionMode::Normal {
            return self.process_sequential_batch(batch).await;
        }

        let mut parsed = Vec::with_capacity(batch.len());
        for frame in &batch {
            let Some((cmd, args)) = parse_command_parts(frame) else {
                return self.process_sequential_batch(batch).await;
            };
            if !is_safe_concurrent_command(&cmd) {
                return self.process_sequential_batch(batch).await;
            }
            parsed.push((cmd, args));
        }

        self.execute_concurrent_pipeline(parsed).await
    }

    async fn process_sequential_batch(&mut self, batch: Vec<RespValue>) -> Result<()> {
        for frame in batch {
            let is_immediate_flush = super::should_flush_immediately(&frame);
            if super::is_blocking_command(&frame) {
                self.flush_responses().await?;
            }
            if let Err(e) = self.process_value(frame).await {
                let _ = self.flush_responses().await;
                return Err(e);
            }
            if is_immediate_flush || self.quit {
                self.flush_responses().await?;
            }
        }
        Ok(())
    }

    async fn execute_concurrent_pipeline(
        &mut self,
        commands: Vec<(String, Vec<Bytes>)>,
    ) -> Result<()> {
        let router = self.state.router().clone();
        let client_id = self.client_id;
        let remote = self.remote;
        let proto = self.protocol_version;
        let current_db = self.current_db;
        #[cfg(feature = "cluster")]
        let cluster_state = self.cluster_state.clone();

        let gate = Arc::clone(&self.state.transaction_gate);
        let _guard = gate.read_owned().await;

        let total = commands.len();
        let tracker = Arc::new(BatchTracker::new(total));
        let t_enter = Instant::now();
        let mut join_set = tokio::task::JoinSet::new();

        for (idx, (cmd, args)) in commands.into_iter().enumerate() {
            let router = router.clone();
            #[cfg(feature = "cluster")]
            let cluster_state = cluster_state.clone();
            let track = super::should_track_observability(&cmd);
            let arg_strings: Vec<String> = if track {
                args.iter()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .collect()
            } else {
                Vec::new()
            };
            let started = track.then(Instant::now);
            let b_tracker = Arc::clone(&tracker);
            join_set.spawn(async move {
                CURRENT_BATCH
                    .scope(b_tracker, async move {
                        let mut db = current_db;
                        let result = router
                            .execute_with_client(
                                &cmd,
                                &args,
                                &mut db,
                                Some(client_id),
                                Some(remote),
                                proto,
                                #[cfg(feature = "cluster")]
                                Some(&cluster_state),
                            )
                            .await;
                        (idx, cmd, args, arg_strings, started, result)
                    })
                    .await
            });
        }

        let mut results = Vec::with_capacity(total);
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(item) => results.push(item),
                Err(e) => {
                    return Err(crate::error::Error::Command(format!(
                        "pipeline task failed: {e}"
                    )))
                }
            }
        }
        let t_join = Instant::now();
        drop(_guard);

        results.sort_unstable_by_key(|(idx, ..)| *idx);
        self.finish_pipeline_results(results).await?;
        let t_flushed = Instant::now();

        let t_last_enq = *tracker.t_last_enq.lock();
        self.pending_timing = Some(PendingBatchTiming {
            cmds: total,
            t_enter,
            t_last_enq,
            t_join,
            t_flushed,
        });
        Ok(())
    }

    async fn finish_pipeline_results(&mut self, results: Vec<PipelineTaskResult>) -> Result<()> {
        for (_idx, cmd, args, arg_strings, started, result) in results {
            self.finish_immediate_command(&cmd, &args, arg_strings, started, result)
                .await?;
        }
        self.flush_responses().await?;
        Ok(())
    }

    /// 把内核里已经到达的字节读进解析器, 不等下一次阻塞读.
    /// 返回值表示读完这批之后连接是否该停.
    pub(super) fn drain_ready(&mut self, buf: &mut [u8]) -> std::io::Result<ReadyDrain> {
        let mut extra = 0usize;
        let stop = loop {
            if self.parser.buffer_len() >= self.parser.max_buffer_size() {
                break ReadyDrain::Overflow;
            }
            match self.stream.try_read(buf) {
                Ok(0) => break ReadyDrain::PeerClosed,
                Ok(n) if self.parser.buffer_len() + n > self.parser.max_buffer_size() => {
                    break ReadyDrain::Overflow;
                }
                Ok(n) => {
                    self.parser.feed(&buf[..n]);
                    extra += n;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    break ReadyDrain::Open;
                }
                Err(err) => return Err(err),
            }
        };
        if extra > 0 {
            self.state.metrics().on_net_input_bytes(extra as u64);
        }
        Ok(stop)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadyDrain {
    Open,
    PeerClosed,
    Overflow,
}

pub(super) fn close_reason(drained: ReadyDrain) -> Option<&'static str> {
    match drained {
        ReadyDrain::Open => None,
        ReadyDrain::PeerClosed => Some("client_closed"),
        ReadyDrain::Overflow => Some("buffer_overflow"),
    }
}

type PipelineTaskResult = (
    usize,
    String,
    Vec<Bytes>,
    Vec<String>,
    Option<Instant>,
    crate::error::Result<RespValue>,
);

fn parse_command_parts(frame: &RespValue) -> Option<(String, Vec<Bytes>)> {
    let RespValue::Array(Some(items)) = frame else {
        return None;
    };
    if items.is_empty() {
        return None;
    }
    let cmd_bytes = super::extract_bulk(&items[0])?;
    let cmd = String::from_utf8_lossy(&cmd_bytes).to_ascii_uppercase();
    let mut args = Vec::with_capacity(items.len() - 1);
    for item in &items[1..] {
        args.push(super::extract_bulk(item)?);
    }
    Some((cmd, args))
}

fn is_safe_concurrent_command(cmd: &str) -> bool {
    match cmd {
        "MULTI" | "ATOM.MULTI" | "EXEC" | "ATOM.EXEC" | "DISCARD" | "ATOM.DISCARD" | "WATCH"
        | "ATOM.WATCH" | "UNWATCH" | "ATOM.UNWATCH" => false,
        "PING" | "ECHO" | "HELLO" | "QUIT" | "MONITOR" | "SHUTDOWN" => false,
        "SELECT" | "ASKING" | "READONLY" | "READWRITE" => false,
        _ => !crate::command::lookup(cmd)
            .map(|info| info.flags.contains(&"blocking"))
            .unwrap_or(false),
    }
}

struct ConnBatchSampler {
    batches: AtomicU64,
    commands: AtomicU64,
    b_1: AtomicU64,
    b_2_4: AtomicU64,
    b_5_16: AtomicU64,
    b_17_32: AtomicU64,
    b_33_64: AtomicU64,
    b_gt_64: AtomicU64,
}

impl ConnBatchSampler {
    const fn new() -> Self {
        Self {
            batches: AtomicU64::new(0),
            commands: AtomicU64::new(0),
            b_1: AtomicU64::new(0),
            b_2_4: AtomicU64::new(0),
            b_5_16: AtomicU64::new(0),
            b_17_32: AtomicU64::new(0),
            b_33_64: AtomicU64::new(0),
            b_gt_64: AtomicU64::new(0),
        }
    }

    fn record(&self, len: usize) {
        let b = self.batches.fetch_add(1, Ordering::Relaxed) + 1;
        let c = self.commands.fetch_add(len as u64, Ordering::Relaxed) + len as u64;
        match len {
            1 => {
                self.b_1.fetch_add(1, Ordering::Relaxed);
            }
            2..=4 => {
                self.b_2_4.fetch_add(1, Ordering::Relaxed);
            }
            5..=16 => {
                self.b_5_16.fetch_add(1, Ordering::Relaxed);
            }
            17..=32 => {
                self.b_17_32.fetch_add(1, Ordering::Relaxed);
            }
            33..=64 => {
                self.b_33_64.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.b_gt_64.fetch_add(1, Ordering::Relaxed);
            }
        }
        if b.is_multiple_of(20) || len > 10 {
            let avg_len = c as f64 / b as f64;
            let b1 = self.b_1.load(Ordering::Relaxed);
            let b2_4 = self.b_2_4.load(Ordering::Relaxed);
            let b5_16 = self.b_5_16.load(Ordering::Relaxed);
            let b17_32 = self.b_17_32.load(Ordering::Relaxed);
            let b33_64 = self.b_33_64.load(Ordering::Relaxed);
            let b_gt = self.b_gt_64.load(Ordering::Relaxed);
            tracing::info!(
                target: "perf",
                "[CONN_BATCH_PERF] curr_len={} batches={} cmds={} avg_len={:.2} (1: {}, 2-4: {}, 5-16: {}, 17-32: {}, 33-64: {}, >64: {})",
                len, b, c, avg_len, b1, b2_4, b5_16, b17_32, b33_64, b_gt
            );
        }
    }
}

static CONN_BATCH_SAMPLER: ConnBatchSampler = ConnBatchSampler::new();

pub(super) fn record_batch_len(len: usize) {
    CONN_BATCH_SAMPLER.record(len);
}

pub struct BatchTracker {
    pub total: usize,
    pub enqueued: AtomicUsize,
    pub t_last_enq: Mutex<Option<Instant>>,
}

impl BatchTracker {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            enqueued: AtomicUsize::new(0),
            t_last_enq: Mutex::new(None),
        }
    }

    pub fn on_enqueued(&self) {
        let prev = self.enqueued.fetch_add(1, Ordering::SeqCst);
        if prev + 1 == self.total {
            *self.t_last_enq.lock() = Some(Instant::now());
        }
    }
}

tokio::task_local! {
    pub(crate) static CURRENT_BATCH: Arc<BatchTracker>;
}

pub(crate) fn notify_enqueued() {
    let _ = CURRENT_BATCH.try_with(|t| {
        t.on_enqueued();
    });
}

pub(super) struct PendingBatchTiming {
    pub cmds: usize,
    pub t_enter: Instant,
    pub t_last_enq: Option<Instant>,
    pub t_join: Instant,
    pub t_flushed: Instant,
}

impl PendingBatchTiming {
    pub fn report_read_next(self, bytes: usize) {
        let t_read_next = Instant::now();
        let enq_span_ms = self
            .t_last_enq
            .map(|t| t.duration_since(self.t_enter).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let tail_join_ms = self
            .t_last_enq
            .map(|t| self.t_join.duration_since(t).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let server_total_ms = self.t_flushed.duration_since(self.t_enter).as_secs_f64() * 1000.0;
        let wait_read_ms = t_read_next.duration_since(self.t_flushed).as_secs_f64() * 1000.0;
        let cycle_total_ms = t_read_next.duration_since(self.t_enter).as_secs_f64() * 1000.0;

        tracing::info!(
            target: "perf",
            "[PIPELINE_TIMING] cmds={} enq_span_ms={:.2} tail_join_ms={:.2} server_total_ms={:.2} wait_read_ms={:.2} cycle_total_ms={:.2} read_bytes={}",
            self.cmds, enq_span_ms, tail_join_ms, server_total_ms, wait_read_ms, cycle_total_ms, bytes
        );
    }

    pub fn report_from_buffer(self) {
        let now = Instant::now();
        let enq_span_ms = self
            .t_last_enq
            .map(|t| t.duration_since(self.t_enter).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let tail_join_ms = self
            .t_last_enq
            .map(|t| self.t_join.duration_since(t).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let server_total_ms = self.t_flushed.duration_since(self.t_enter).as_secs_f64() * 1000.0;
        let wait_read_ms = now.duration_since(self.t_flushed).as_secs_f64() * 1000.0;
        let cycle_total_ms = now.duration_since(self.t_enter).as_secs_f64() * 1000.0;

        tracing::info!(
            target: "perf",
            "[PIPELINE_TIMING] cmds={} enq_span_ms={:.2} tail_join_ms={:.2} server_total_ms={:.2} wait_read_ms={:.2} cycle_total_ms={:.2} buffered=true",
            self.cmds, enq_span_ms, tail_join_ms, server_total_ms, wait_read_ms, cycle_total_ms
        );
    }
}
