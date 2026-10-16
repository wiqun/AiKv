//! Connection Pipeline 并发优化: 将多笔安全命令并行执行并聚合写凑批.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::task::JoinSet;

use crate::command::CommandRouter;
use crate::error::Result;
use crate::protocol::{ProtocolVersion, RespValue};

use super::{Connection, ConnectionMode};

const PIPELINE_MAX_IN_FLIGHT: usize = 512;

impl Connection {
    /// 尝试对同一批中的命令进行流水线并发执行.
    /// 若包含事务、阻塞或状态敏感命令, 则回退到顺序执行.
    pub(super) async fn process_pipeline_batch(
        &mut self,
        batch: Vec<RespValue>,
        buf: &mut [u8],
    ) -> Result<()> {
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

        self.execute_concurrent_pipeline(parsed, buf).await
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
        buf: &mut [u8],
    ) -> Result<()> {
        let launch = self.pipeline_launch();
        let gate = Arc::clone(&self.state.transaction_gate);
        let _guard = gate.read_owned().await;
        let t_enter = Instant::now();
        let mut join_set = JoinSet::new();
        let mut next_idx = 0usize;
        let initial = commands.len();
        self.spawn_pipeline(&mut join_set, &mut next_idx, commands, &launch);
        let mut results = Vec::with_capacity(initial);
        self.collect_pipeline(&mut join_set, &mut results, &mut next_idx, &launch, buf)
            .await?;
        let t_join = Instant::now();
        drop(_guard);
        results.sort_unstable_by_key(|(idx, ..)| *idx);
        self.finish_pipeline_results(results).await?;
        let t_flushed = Instant::now();
        record_batch_len(next_idx);
        self.pending_timing = Some(PendingBatchTiming {
            cmds: next_idx,
            t_enter,
            t_last_enq: None,
            t_join,
            t_flushed,
        });
        Ok(())
    }

    fn pipeline_launch(&self) -> PipelineLaunch {
        PipelineLaunch {
            router: self.state.router().clone(),
            client_id: self.client_id,
            remote: self.remote,
            proto: self.protocol_version,
            db: self.current_db,
            #[cfg(feature = "cluster")]
            cluster_state: self.cluster_state.clone(),
        }
    }

    fn spawn_pipeline(
        &self,
        join_set: &mut JoinSet<PipelineTaskResult>,
        next_idx: &mut usize,
        commands: Vec<(String, Vec<Bytes>)>,
        launch: &PipelineLaunch,
    ) {
        for (cmd, args) in commands {
            launch_pipeline_task(join_set, *next_idx, cmd, args, launch);
            *next_idx += 1;
        }
    }

    async fn collect_pipeline(
        &mut self,
        join_set: &mut JoinSet<PipelineTaskResult>,
        results: &mut Vec<PipelineTaskResult>,
        next_idx: &mut usize,
        launch: &PipelineLaunch,
        buf: &mut [u8],
    ) -> Result<()> {
        let mut accept = true;
        loop {
            if self.spawn_buffered(join_set, next_idx, launch, &mut accept)? {
                continue;
            }
            if join_set.is_empty() {
                if !self.drain_for_more(buf, &mut accept)? {
                    break;
                }
                continue;
            }
            self.wait_pipeline(results, join_set, buf, &mut accept)
                .await?;
        }
        Ok(())
    }

    fn spawn_buffered(
        &mut self,
        join_set: &mut JoinSet<PipelineTaskResult>,
        next_idx: &mut usize,
        launch: &PipelineLaunch,
        accept: &mut bool,
    ) -> Result<bool> {
        let room = PIPELINE_MAX_IN_FLIGHT.saturating_sub(*next_idx);
        if !*accept || room == 0 {
            *accept = room > 0 && *accept;
            return Ok(false);
        }
        let parsed = self.parse_ready_commands(room)?;
        if parsed.stop {
            *accept = false;
        }
        if parsed.commands.is_empty() {
            return Ok(false);
        }
        self.spawn_pipeline(join_set, next_idx, parsed.commands, launch);
        Ok(true)
    }

    fn drain_for_more(&mut self, buf: &mut [u8], accept: &mut bool) -> Result<bool> {
        if !*accept {
            return Ok(false);
        }
        let before = self.parser.buffer_len();
        let drained = self.drain_ready(buf)?;
        if drained != ReadyDrain::Open {
            self.ready_after = drained;
            *accept = false;
        }
        Ok(self.parser.buffer_len() > before)
    }

    async fn wait_pipeline(
        &mut self,
        results: &mut Vec<PipelineTaskResult>,
        join_set: &mut JoinSet<PipelineTaskResult>,
        buf: &mut [u8],
        accept: &mut bool,
    ) -> Result<()> {
        let wake = tokio::select! {
            biased;
            ready = self.stream.readable(), if *accept => PipelineWake::Socket(ready),
            joined = join_set.join_next() => PipelineWake::Joined(joined),
        };
        match wake {
            PipelineWake::Socket(ready) => {
                ready?;
                self.drain_for_more(buf, accept)?;
            }
            PipelineWake::Joined(Some(Ok(item))) => results.push(item),
            PipelineWake::Joined(Some(Err(err))) => {
                return Err(crate::error::Error::Command(format!(
                    "pipeline task failed: {err}"
                )));
            }
            PipelineWake::Joined(None) => {}
        }
        Ok(())
    }

    fn parse_ready_commands(&mut self, room: usize) -> Result<ReadyCommands> {
        let mut commands = Vec::new();
        while commands.len() < room {
            let frame = match self.parser.parse()? {
                Some(frame) => frame,
                None => break,
            };
            if !push_ready_command(&mut commands, frame, &mut self.held_frame) {
                return Ok(ReadyCommands {
                    commands,
                    stop: true,
                });
            }
        }
        Ok(ReadyCommands {
            commands,
            stop: false,
        })
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

struct PipelineLaunch {
    router: Arc<CommandRouter>,
    client_id: usize,
    remote: SocketAddr,
    proto: ProtocolVersion,
    db: usize,
    #[cfg(feature = "cluster")]
    cluster_state: crate::cluster::connection::ClusterConnectionState,
}

struct ReadyCommands {
    commands: Vec<(String, Vec<Bytes>)>,
    stop: bool,
}

enum PipelineWake {
    Socket(std::io::Result<()>),
    Joined(Option<std::result::Result<PipelineTaskResult, tokio::task::JoinError>>),
}

fn push_ready_command(
    commands: &mut Vec<(String, Vec<Bytes>)>,
    frame: RespValue,
    held: &mut Option<RespValue>,
) -> bool {
    if super::should_flush_immediately(&frame) || super::is_blocking_command(&frame) {
        *held = Some(frame);
        return false;
    }
    let Some((cmd, args)) = parse_command_parts(&frame) else {
        *held = Some(frame);
        return false;
    };
    if !is_safe_concurrent_command(&cmd) {
        *held = Some(frame);
        return false;
    }
    commands.push((cmd, args));
    true
}

fn launch_pipeline_task(
    join_set: &mut JoinSet<PipelineTaskResult>,
    idx: usize,
    cmd: String,
    args: Vec<Bytes>,
    launch: &PipelineLaunch,
) {
    let router = launch.router.clone();
    let client_id = launch.client_id;
    let remote = launch.remote;
    let proto = launch.proto;
    let db = launch.db;
    #[cfg(feature = "cluster")]
    let cluster_state = launch.cluster_state.clone();
    let track = super::should_track_observability(&cmd);
    let arg_strings = tracked_args(track, &args);
    let started = track.then(Instant::now);
    join_set.spawn(async move {
        let mut db = db;
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
    });
}

fn tracked_args(track: bool, args: &[Bytes]) -> Vec<String> {
    if !track {
        return Vec::new();
    }
    args.iter()
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
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
