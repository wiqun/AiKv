//! Connection Pipeline 批处理优化: 将流水线批次中的命令就地批量流式执行并聚合网络写回.
//! 依据 ADR-001: 消除针对微秒级命令的逐个 Tokio Task 衍生与乱序重排开销.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use crate::error::Result;
use crate::protocol::RespValue;

use super::{Connection, ConnectionMode};

impl Connection {
    /// 对同一批中的命令进行流水线批处理执行.
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

        #[cfg(feature = "cluster")]
        if crate::server::info::is_cluster_initialized() {
            return self.execute_cluster_concurrent_pipeline(parsed).await;
        }

        self.execute_in_place_pipeline(parsed).await
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

    /// 集群模式并发分发流水线命令, 响应严格按照输入下标保序刷网.
    #[cfg(feature = "cluster")]
    pub async fn execute_cluster_concurrent_pipeline(
        &mut self,
        commands: Vec<(String, Vec<Bytes>)>,
    ) -> Result<()> {
        struct ExecutedPipelineItem {
            idx: usize,
            cmd: String,
            args: Vec<Bytes>,
            started: Option<Instant>,
            result: Result<RespValue>,
        }

        let total = commands.len();
        if total == 0 {
            return Ok(());
        }
        let t_enter = Instant::now();

        let gate = Arc::clone(&self.state.transaction_gate);
        let _guard = gate.read_owned().await;

        let client_id = self.client_id;
        let remote = self.remote;
        let proto = self.protocol_version;
        let current_db = self.current_db;
        let cluster_state = self.cluster_state.clone();

        const MAX_CONCURRENT_PIPELINE: usize = 256;
        let mut join_set = tokio::task::JoinSet::<ExecutedPipelineItem>::new();
        let mut results: Vec<Option<ExecutedPipelineItem>> = (0..total).map(|_| None).collect();

        for (idx, (cmd, args)) in commands.into_iter().enumerate() {
            if join_set.len() >= MAX_CONCURRENT_PIPELINE {
                if let Some(res) = join_set.join_next().await {
                    let item = res.map_err(|e| crate::error::Error::Command(e.to_string()))?;
                    let i = item.idx;
                    results[i] = Some(item);
                }
            }

            let state = Arc::clone(&self.state);
            let conn_cluster_state = cluster_state.clone();
            let track = super::should_track_observability(&cmd);
            let started = track.then(Instant::now);

            join_set.spawn(async move {
                let mut db = current_db;
                let result = state
                    .router()
                    .execute_with_client(
                        &cmd,
                        &args,
                        &mut db,
                        Some(client_id),
                        Some(remote),
                        proto,
                        Some(&conn_cluster_state),
                    )
                    .await;

                ExecutedPipelineItem {
                    idx,
                    cmd,
                    args,
                    started,
                    result,
                }
            });
        }

        while let Some(res) = join_set.join_next().await {
            let item = res.map_err(|e| crate::error::Error::Command(e.to_string()))?;
            let i = item.idx;
            results[i] = Some(item);
        }

        drop(_guard);

        for item in results.into_iter().flatten() {
            self.finish_immediate_command(&item.cmd, &item.args, item.started, item.result)
                .await?;
        }

        self.flush_responses().await?;

        let t_flushed = Instant::now();
        self.pending_timing = Some(PendingBatchTiming {
            cmds: total,
            t_enter,
            t_last_enq: None,
            t_join: t_flushed,
            t_flushed,
        });
        Ok(())
    }

    /// 就地流式顺序批量执行流水线命令, 响应直接追加至当前连接响应缓冲, 批末统一一次性刷网.
    async fn execute_in_place_pipeline(
        &mut self,
        commands: Vec<(String, Vec<Bytes>)>,
    ) -> Result<()> {
        let total = commands.len();
        if total == 0 {
            return Ok(());
        }
        let t_enter = Instant::now();

        let gate = Arc::clone(&self.state.transaction_gate);
        let _guard = gate.read_owned().await;

        let client_id = self.client_id;
        let remote = self.remote;
        let proto = self.protocol_version;

        for (cmd, args) in commands {
            let track = super::should_track_observability(&cmd);
            let started = track.then(Instant::now);

            let mut db = self.current_db;
            let result = self
                .state
                .router()
                .execute_with_client(
                    &cmd,
                    &args,
                    &mut db,
                    Some(client_id),
                    Some(remote),
                    proto,
                    #[cfg(feature = "cluster")]
                    Some(&self.cluster_state),
                )
                .await;
            self.current_db = db;

            self.finish_immediate_command(&cmd, &args, started, result)
                .await?;
        }

        drop(_guard);
        self.flush_responses().await?;

        let t_flushed = Instant::now();
        self.pending_timing = Some(PendingBatchTiming {
            cmds: total,
            t_enter,
            t_last_enq: None,
            t_join: t_flushed,
            t_flushed,
        });
        Ok(())
    }
}

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
