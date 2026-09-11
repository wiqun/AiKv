//! 加压引擎: 期望状态对账 + worker 生命周期 + 连接可用性探活.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{TargetMode, WorkloadConfig};
use crate::conn::Conn;
use crate::ratelimit::TokenBucket;
use crate::workload::{build_pipeline, plan_batch};

/// 对账周期.
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);
/// 探活周期.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// 连接签名: 变化即需要重建全部 worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnSpec {
    pub mode: TargetMode,
    pub endpoints: Vec<String>,
    pub timeout_ms: u64,
}

impl ConnSpec {
    pub fn from_config(cfg: &WorkloadConfig) -> Self {
        Self {
            mode: cfg.mode,
            endpoints: cfg.endpoints.clone(),
            timeout_ms: cfg.timeout_ms,
        }
    }
}

/// 当前运行时快照 (对账输入).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSnapshot {
    pub workers: u32,
    pub rate: u64,
    pub spec: ConnSpec,
}

/// supervisor 决策动作 (纯数据, 便于单测).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    AddWorkers(u32),
    RemoveWorkers(u32),
    SetRate(u64),
    Restart { workers: u32 },
}

/// 纯函数对账: 依据当前快照与期望配置, 给出需要执行的动作.
pub fn plan_actions(current: &EngineSnapshot, desired: &WorkloadConfig) -> Vec<Action> {
    if !desired.running {
        return if current.workers > 0 {
            vec![Action::RemoveWorkers(current.workers)]
        } else {
            Vec::new()
        };
    }

    let desired_spec = ConnSpec::from_config(desired);
    if current.spec != desired_spec {
        return vec![Action::Restart {
            workers: desired.connections,
        }];
    }

    let mut actions = Vec::new();
    if current.rate != desired.target_ops {
        actions.push(Action::SetRate(desired.target_ops));
    }
    match desired.connections.cmp(&current.workers) {
        std::cmp::Ordering::Greater => {
            actions.push(Action::AddWorkers(desired.connections - current.workers));
        }
        std::cmp::Ordering::Less => {
            actions.push(Action::RemoveWorkers(current.workers - desired.connections));
        }
        std::cmp::Ordering::Equal => {}
    }
    actions
}

/// 对外可见的运行时状态 (UI 只读; 不含任何压测结果统计).
#[derive(Debug, Default)]
pub struct RuntimeState {
    pub workers: AtomicU64,
    endpoint_status: Mutex<Vec<EndpointStatus>>,
    last_error: Mutex<Option<ErrorInfo>>,
}

/// 单个 endpoint 的可达性.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointStatus {
    pub addr: String,
    pub reachable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    pub message: String,
    pub at_unix: u64,
}

impl RuntimeState {
    pub async fn endpoints(&self) -> Vec<EndpointStatus> {
        self.endpoint_status.lock().await.clone()
    }

    pub async fn last_error(&self) -> Option<ErrorInfo> {
        self.last_error.lock().await.clone()
    }

    pub async fn set_endpoints(&self, statuses: Vec<EndpointStatus>) {
        *self.endpoint_status.lock().await = statuses;
    }

    pub async fn record_error(&self, message: String) {
        let info = ErrorInfo {
            message,
            at_unix: now_unix(),
        };
        *self.last_error.lock().await = Some(info);
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

struct WorkerHandle {
    stop: CancellationToken,
    handle: JoinHandle<()>,
}

impl WorkerHandle {
    fn stop(&self) {
        self.stop.cancel();
    }
}

pub struct Engine {
    pub cfg: Arc<ArcSwap<WorkloadConfig>>,
    pub state: Arc<RuntimeState>,
    bucket: Arc<Mutex<TokenBucket>>,
}

static NEXT_WORKER_ID: AtomicU32 = AtomicU32::new(1);

impl Engine {
    pub fn new(cfg: Arc<ArcSwap<WorkloadConfig>>, state: Arc<RuntimeState>) -> Self {
        let rate = cfg.load().target_ops as f64;
        Self {
            cfg,
            state,
            bucket: Arc::new(Mutex::new(TokenBucket::new(rate))),
        }
    }

    /// supervisor: 按周期对账, 直到收到停止信号; 退出前优雅停掉全部 worker.
    pub async fn run(self: Arc<Self>, stop: CancellationToken) {
        let mut workers: Vec<WorkerHandle> = Vec::new();
        let mut snapshot = EngineSnapshot {
            workers: 0,
            rate: self.cfg.load().target_ops,
            spec: ConnSpec::from_config(&self.cfg.load()),
        };
        let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = ticker.tick() => {
                    self.reconcile(&mut workers, &mut snapshot, &stop).await;
                }
            }
        }
        stop_workers(&mut workers);
        self.state.workers.store(0, Ordering::Relaxed);
    }

    async fn reconcile(
        &self,
        workers: &mut Vec<WorkerHandle>,
        snapshot: &mut EngineSnapshot,
        stop: &CancellationToken,
    ) {
        workers.retain(|worker| !worker.handle.is_finished());
        snapshot.workers = workers.len() as u32;
        let desired = self.cfg.load_full();
        for action in plan_actions(snapshot, &desired) {
            self.apply_action(action, workers, snapshot, &desired, stop)
                .await;
        }
        self.state
            .workers
            .store(u64::from(snapshot.workers), Ordering::Relaxed);
    }

    async fn apply_action(
        &self,
        action: Action,
        workers: &mut Vec<WorkerHandle>,
        snapshot: &mut EngineSnapshot,
        desired: &WorkloadConfig,
        stop: &CancellationToken,
    ) {
        match action {
            Action::SetRate(rate) => {
                self.bucket.lock().await.set_rate(rate as f64);
                snapshot.rate = rate;
            }
            Action::AddWorkers(count) => {
                for _ in 0..count {
                    workers.push(self.spawn_worker(stop));
                }
                snapshot.workers = workers.len() as u32;
            }
            Action::RemoveWorkers(count) => {
                let keep = workers.len().saturating_sub(count as usize);
                for worker in workers.drain(keep..) {
                    worker.stop();
                }
                snapshot.workers = workers.len() as u32;
            }
            Action::Restart { workers: count } => {
                restart_workers(self, workers, snapshot, desired, stop, count).await;
            }
        }
    }

    fn spawn_worker(&self, stop: &CancellationToken) -> WorkerHandle {
        let worker_stop = stop.child_token();
        let task_stop = worker_stop.clone();
        let cfg = self.cfg.clone();
        let bucket = self.bucket.clone();
        let state = self.state.clone();
        let id = NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed);
        let handle = tokio::spawn(async move {
            worker_loop(id, cfg, bucket, state, task_stop).await;
        });
        WorkerHandle {
            stop: worker_stop,
            handle,
        }
    }
}

fn stop_workers(workers: &mut Vec<WorkerHandle>) {
    for worker in workers.drain(..) {
        worker.stop();
    }
}

async fn restart_workers(
    engine: &Engine,
    workers: &mut Vec<WorkerHandle>,
    snapshot: &mut EngineSnapshot,
    desired: &WorkloadConfig,
    stop: &CancellationToken,
    count: u32,
) {
    stop_workers(workers);
    snapshot.spec = ConnSpec::from_config(desired);
    snapshot.rate = desired.target_ops;
    engine
        .bucket
        .lock()
        .await
        .set_rate(desired.target_ops as f64);
    for _ in 0..count {
        workers.push(engine.spawn_worker(stop));
    }
    snapshot.workers = workers.len() as u32;
}

enum Tick {
    Stop,
    Retry,
    Go,
}

async fn worker_loop(
    id: u32,
    cfg: Arc<ArcSwap<WorkloadConfig>>,
    bucket: Arc<Mutex<TokenBucket>>,
    state: Arc<RuntimeState>,
    stop: CancellationToken,
) {
    let initial = cfg.load_full();
    let mut conn = match Conn::connect(&initial).await {
        Ok(conn) => conn,
        Err(err) => {
            state
                .record_error(format!("worker {id} 连接失败: {err}"))
                .await;
            return;
        }
    };
    let mut rng = StdRng::seed_from_u64(u64::from(id).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut value = vec![b'x'; initial.value_size_max as usize];
    run_worker_ticks(
        &cfg, &bucket, &state, &stop, &mut conn, &mut rng, &mut value,
    )
    .await;
}

async fn run_worker_ticks(
    cfg: &Arc<ArcSwap<WorkloadConfig>>,
    bucket: &Arc<Mutex<TokenBucket>>,
    state: &Arc<RuntimeState>,
    stop: &CancellationToken,
    conn: &mut Conn,
    rng: &mut StdRng,
    value: &mut Vec<u8>,
) {
    loop {
        if stop.is_cancelled() {
            break;
        }
        let current = cfg.load_full();
        if !current.running {
            break;
        }
        if value.len() < current.value_size_max as usize {
            value.resize(current.value_size_max as usize, b'x');
        }
        match wait_for_tokens(&current, bucket, stop).await {
            Tick::Stop => break,
            Tick::Retry => continue,
            Tick::Go => {}
        }
        let plan = plan_batch(&current, rng);
        let pipe = build_pipeline(&plan, value);
        if let Err(err) = conn.exec(&pipe).await {
            state.record_error(format!("执行失败: {err}")).await;
            if !reconnect_or_wait(&current, state, stop, conn).await {
                break;
            }
        }
    }
}

async fn wait_for_tokens(
    current: &WorkloadConfig,
    bucket: &Arc<Mutex<TokenBucket>>,
    stop: &CancellationToken,
) -> Tick {
    if current.target_ops == 0 {
        return Tick::Go;
    }
    let wait = {
        let permit = f64::from(current.pipeline);
        bucket
            .lock()
            .await
            .try_take(permit, tokio::time::Instant::now())
            .err()
    };
    let Some(wait) = wait else {
        return Tick::Go;
    };
    tokio::select! {
        _ = stop.cancelled() => Tick::Stop,
        _ = tokio::time::sleep(wait) => Tick::Retry,
    }
}

async fn reconnect_or_wait(
    cfg: &WorkloadConfig,
    state: &RuntimeState,
    stop: &CancellationToken,
    conn: &mut Conn,
) -> bool {
    match Conn::connect(cfg).await {
        Ok(new_conn) => {
            *conn = new_conn;
            true
        }
        Err(err) => {
            state.record_error(format!("重连失败: {err}")).await;
            tokio::select! {
                _ = stop.cancelled() => false,
                _ = tokio::time::sleep(Duration::from_millis(200)) => true,
            }
        }
    }
}

/// 连接可用性探活 (每 2s; 只反映 PING 可达性, 不是压测结果).
pub async fn probe_loop(
    cfg: Arc<ArcSwap<WorkloadConfig>>,
    state: Arc<RuntimeState>,
    stop: CancellationToken,
) {
    let mut ticker = tokio::time::interval(PROBE_INTERVAL);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = ticker.tick() => {
                let current = cfg.load_full();
                let timeout = Duration::from_millis(current.timeout_ms);
                let mut statuses = Vec::with_capacity(current.endpoints.len());
                for endpoint in &current.endpoints {
                    let reachable = crate::conn::ping(endpoint, timeout).await;
                    statuses.push(EndpointStatus {
                        addr: endpoint.clone(),
                        reachable,
                    });
                }
                state.set_endpoints(statuses).await;
            }
        }
    }
}

#[cfg(test)]
mod tests;
