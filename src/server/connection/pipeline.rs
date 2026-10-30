//! Connection Pipeline 批处理优化: 将流水线批次中的命令就地批量流式执行并聚合网络写回.
//! 依据 ADR-001: 消除针对微秒级命令的逐个 Tokio Task 衍生与乱序重排开销.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use crate::error::Result;
use crate::protocol::RespValue;
use crate::storage::types::BatchWriteHandle;

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

        self.execute_pipeline(parsed).await
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

    /// 等待挂起的写全部确认, 按原顺序输出各自的响应 (确认失败则该条响应变为错误).
    async fn flush_pending(&mut self, pending: &mut Vec<PendingWrite>) -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        for r in resolve_pending(std::mem::take(pending)).await {
            self.finish_immediate_command(&r.cmd, &r.args, r.started, r.result)
                .await?;
        }
        Ok(())
    }

    /// 按到达顺序逐条执行流水线命令, 响应直接追加至当前连接响应缓冲, 批末统一一次性刷网.
    ///
    /// 单机与集群共用: Redis 语义要求同一连接流水线内的命令按序生效 (`SET k v` 之后的 `GET k`
    /// 必须读到 `v`). 曾为集群并发 `spawn` 每条命令以合并 Raft 提交, 但这会破坏读后写与写写顺序.
    pub async fn execute_pipeline(&mut self, commands: Vec<(String, Vec<Bytes>)>) -> Result<()> {
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

        #[cfg(feature = "cluster")]
        let defer_enabled = crate::server::info::is_cluster_initialized();
        #[cfg(not(feature = "cluster"))]
        let defer_enabled = false;
        let mut pending: Vec<PendingWrite> = Vec::new();

        for (cmd, args) in commands {
            let track = super::should_track_observability(&cmd);
            let started = track.then(Instant::now);

            // 盲写允许先入队后确认 (保持按序); 其余命令可能读到前序写的结果, 必须先等挂起的写全部确认.
            let defer = defer_enabled && is_blind_write(&cmd, &args);
            if !defer {
                self.flush_pending(&mut pending).await?;
            }

            let mut db = self.current_db;
            let router = self.state.router();
            let exec = router.execute_with_client(
                &cmd,
                &args,
                &mut db,
                Some(client_id),
                Some(remote),
                proto,
                #[cfg(feature = "cluster")]
                Some(&self.cluster_state),
            );
            let (result, handles) = if defer {
                crate::storage::deferred::scope(exec).await
            } else {
                (exec.await, Vec::new())
            };
            self.current_db = db;

            match result {
                Ok(resp) if !handles.is_empty() => {
                    pending.push(PendingWrite {
                        cmd,
                        args,
                        started,
                        resp,
                        handles,
                    });
                    if pending.len() >= MAX_PENDING_WRITES {
                        self.flush_pending(&mut pending).await?;
                    }
                }
                result => {
                    // 保序: 先输出之前挂起的响应, 再输出当前命令的响应.
                    self.flush_pending(&mut pending).await?;
                    self.finish_immediate_command(&cmd, &args, started, result)
                        .await?;
                }
            }
        }
        self.flush_pending(&mut pending).await?;

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

/// 单条流水线内最多挂起多少条写后强制确认一次, 限制内存与首条响应的等待时间.
const MAX_PENDING_WRITES: usize = 256;

/// 已执行 (写已入队) 但尚未确认的盲写命令, 响应暂存到确认之后再输出.
struct PendingWrite {
    cmd: String,
    args: Vec<Bytes>,
    started: Option<Instant>,
    resp: RespValue,
    handles: Vec<Box<dyn BatchWriteHandle>>,
}

struct ResolvedWrite {
    cmd: String,
    args: Vec<Bytes>,
    started: Option<Instant>,
    result: Result<RespValue>,
}

/// 只写不读旧值的命令. 纯 `SET key value` 不读旧值, 可以先入队后确认; 带选项的 SET (NX/XX/GET/KEEPTTL 等)
/// 与其他写命令 (INCR, DEL, HSET 等) 要读旧值, 必须先等前序写确认.
fn is_blind_write(cmd: &str, args: &[Bytes]) -> bool {
    cmd == "SET" && args.len() == 2
}

/// 按原顺序等待每条命令的写确认; 任一 handle 失败则该条命令的结果为该错误, 其余命令不受影响.
async fn resolve_pending(pending: Vec<PendingWrite>) -> Vec<ResolvedWrite> {
    let mut out = Vec::with_capacity(pending.len());
    for p in pending {
        let mut failure = None;
        for handle in p.handles {
            if let Err(e) = handle.wait().await {
                failure = Some(e);
                break;
            }
        }
        out.push(ResolvedWrite {
            cmd: p.cmd,
            args: p.args,
            started: p.started,
            result: match failure {
                Some(e) => Err(e),
                None => Ok(p.resp),
            },
        });
    }
    out
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

#[cfg(test)]
mod defer_tests {
    use super::*;
    use crate::error::Error;

    struct Fake(bool);

    #[async_trait::async_trait]
    impl BatchWriteHandle for Fake {
        async fn wait(self: Box<Self>) -> Result<()> {
            if self.0 {
                Err(Error::Storage("commit failed".into()))
            } else {
                Ok(())
            }
        }
    }

    fn pending(cmd: &str, fail: &[bool]) -> PendingWrite {
        PendingWrite {
            cmd: cmd.to_string(),
            args: vec![Bytes::from(cmd.to_string())],
            started: None,
            resp: RespValue::SimpleString("OK".into()),
            handles: fail
                .iter()
                .map(|&f| Box::new(Fake(f)) as Box<dyn BatchWriteHandle>)
                .collect(),
        }
    }

    #[test]
    fn only_plain_set_is_a_blind_write() {
        let b = |s: &str| Bytes::from(s.to_string());
        assert!(is_blind_write("SET", &[b("k"), b("v")]));
        assert!(!is_blind_write("SET", &[b("k"), b("v"), b("NX")]));
        assert!(!is_blind_write("SET", &[b("k"), b("v"), b("EX"), b("10")]));
        assert!(!is_blind_write("SET", &[b("k")]));
        for cmd in ["GET", "INCR", "DEL", "HSET", "MSET", "EVAL", "SETNX"] {
            assert!(!is_blind_write(cmd, &[b("k"), b("v")]), "{cmd} 不是盲写");
        }
    }

    #[tokio::test]
    async fn pending_writes_resolve_in_order_and_failure_only_affects_that_command() {
        let resolved = resolve_pending(vec![
            pending("a", &[false]),
            pending("b", &[false, true]),
            pending("c", &[false]),
        ])
        .await;
        let cmds: Vec<_> = resolved.iter().map(|r| r.cmd.as_str()).collect();
        assert_eq!(cmds, ["a", "b", "c"], "必须保持原顺序");
        assert!(resolved[0].result.is_ok());
        assert!(
            resolved[1].result.is_err(),
            "任一 handle 失败则该命令变为错误"
        );
        assert!(resolved[2].result.is_ok(), "失败不应波及其他命令");
    }
}
