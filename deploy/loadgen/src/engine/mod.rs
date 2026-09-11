//! 加压引擎: 期望状态对账 + worker 生命周期 + 连接可用性探活.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::Serialize;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{TargetMode, WorkloadConfig};
use crate::conn::{needs_reconnect, pause_reason, Conn};
use crate::ratelimit::TokenBucket;
use crate::workload::{build_pipeline, plan_batch};

/// 对账周期.
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);
/// 探活周期 (UI 绿灯); 测试工具不需要高频握手.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(30);
/// 暂停派发时 worker 空转间隔.
const PAUSE_WAIT: Duration = Duration::from_millis(500);
/// 同时只允许一条建连; 失败时持有许可退避, 避免打满本机临时端口.
const MAX_CONCURRENT_CONNECTS: usize = 1;
const CONNECT_BACKOFF_START: Duration = Duration::from_millis(500);
const CONNECT_BACKOFF_CAP: Duration = Duration::from_secs(10);

pub(crate) fn next_connect_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(CONNECT_BACKOFF_CAP)
}

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
    pub spec: ConnSpec,
    pub epoch: u64,
    /// 本次启动时的 worker 数; 补员只补到这个数, 不跟表单差值缩放.
    pub started: u32,
}

/// supervisor 决策动作 (纯数据, 便于单测).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    AddWorkers(u32),
    RemoveWorkers(u32),
    Restart { workers: u32 },
}

/// 纯函数对账: 点启动抬 epoch 才整批重建; 运行中不按差值加减 worker、不热改速率.
pub fn plan_actions(
    current: &EngineSnapshot,
    desired: &WorkloadConfig,
    run_epoch: u64,
) -> Vec<Action> {
    if !desired.running {
        return if current.workers > 0 {
            vec![Action::RemoveWorkers(current.workers)]
        } else {
            Vec::new()
        };
    }

    if current.epoch != run_epoch {
        return vec![Action::Restart {
            workers: desired.connections,
        }];
    }

    if current.workers < current.started {
        vec![Action::AddWorkers(current.started - current.workers)]
    } else {
        Vec::new()
    }
}

/// 对外可见的运行时状态 (UI 只读; 不含任何压测结果统计).
#[derive(Debug, Default)]
pub struct RuntimeState {
    pub workers: AtomicU64,
    run_epoch: AtomicU64,
    dispatch_paused: AtomicBool,
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

    pub fn is_paused(&self) -> bool {
        self.dispatch_paused.load(Ordering::Relaxed)
    }

    pub fn resume_dispatch(&self) {
        self.dispatch_paused.store(false, Ordering::Relaxed);
    }

    pub fn bump_run_epoch(&self) -> u64 {
        self.run_epoch.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn run_epoch(&self) -> u64 {
        self.run_epoch.load(Ordering::Relaxed)
    }

    async fn set_paused(&self, paused: bool, reason: Option<String>) {
        let was = self.dispatch_paused.swap(paused, Ordering::Relaxed);
        if paused && !was {
            if let Some(reason) = reason {
                self.record_error(reason).await;
            }
        }
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
    connect_limit: Arc<Semaphore>,
}

static NEXT_WORKER_ID: AtomicU32 = AtomicU32::new(1);

impl Engine {
    pub fn new(cfg: Arc<ArcSwap<WorkloadConfig>>, state: Arc<RuntimeState>) -> Self {
        let rate = cfg.load().target_ops as f64;
        Self {
            cfg,
            state,
            bucket: Arc::new(Mutex::new(TokenBucket::new(rate))),
            connect_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTS)),
        }
    }

    pub fn connect_limit(&self) -> Arc<Semaphore> {
        self.connect_limit.clone()
    }

    /// supervisor: 按周期对账, 直到收到停止信号; 退出前优雅停掉全部 worker.
    pub async fn run(self: Arc<Self>, stop: CancellationToken) {
        let mut workers: Vec<WorkerHandle> = Vec::new();
        let mut snapshot = EngineSnapshot {
            workers: 0,
            spec: ConnSpec::from_config(&self.cfg.load()),
            epoch: 0,
            started: 0,
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
        let run_epoch = self.state.run_epoch();
        for action in plan_actions(snapshot, &desired, run_epoch) {
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
        let frozen = cfg.load_full();
        let bucket = self.bucket.clone();
        let state = self.state.clone();
        let connect_limit = self.connect_limit.clone();
        let id = NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed);
        let handle = tokio::spawn(async move {
            worker_loop(id, frozen, cfg, bucket, state, connect_limit, task_stop).await;
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
    snapshot.epoch = engine.state.run_epoch();
    snapshot.started = count;
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
    frozen: Arc<WorkloadConfig>,
    cfg: Arc<ArcSwap<WorkloadConfig>>,
    bucket: Arc<Mutex<TokenBucket>>,
    state: Arc<RuntimeState>,
    connect_limit: Arc<Semaphore>,
    stop: CancellationToken,
) {
    let mut rng = StdRng::seed_from_u64(u64::from(id).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut value = vec![b'x'; frozen.value_size_max as usize];
    loop {
        if stop.is_cancelled() || !cfg.load().running {
            return;
        }
        let mut conn = match connect_limited(&frozen, &cfg, &connect_limit, &stop, &state, id).await
        {
            ConnectOutcome::Ok(conn) => conn,
            ConnectOutcome::Stopped => return,
        };
        let reconnect = run_worker_ticks(WorkerTick {
            frozen: &frozen,
            cfg: &cfg,
            bucket: &bucket,
            state: &state,
            stop: &stop,
            conn: &mut conn,
            rng: &mut rng,
            value: &mut value,
        })
        .await;
        if !reconnect {
            return;
        }
    }
}

struct WorkerTick<'a> {
    frozen: &'a Arc<WorkloadConfig>,
    cfg: &'a Arc<ArcSwap<WorkloadConfig>>,
    bucket: &'a Arc<Mutex<TokenBucket>>,
    state: &'a Arc<RuntimeState>,
    stop: &'a CancellationToken,
    conn: &'a mut Conn,
    rng: &'a mut StdRng,
    value: &'a mut Vec<u8>,
}

/// 返回 true 表示传输失败, 需要重建连接; false 表示停止.
async fn run_worker_ticks(tick: WorkerTick<'_>) -> bool {
    loop {
        if tick.stop.is_cancelled() {
            return false;
        }
        if tick.state.is_paused() {
            tokio::select! {
                _ = tick.stop.cancelled() => return false,
                _ = tokio::time::sleep(PAUSE_WAIT) => continue,
            }
        }
        if !tick.cfg.load().running {
            return false;
        }
        if tick.value.len() < tick.frozen.value_size_max as usize {
            tick.value.resize(tick.frozen.value_size_max as usize, b'x');
        }
        match wait_for_tokens(tick.frozen, tick.bucket, tick.stop).await {
            Tick::Stop => return false,
            Tick::Retry => continue,
            Tick::Go => {}
        }
        let plan = plan_batch(tick.frozen, tick.rng);
        let pipe = build_pipeline(&plan, tick.value);
        if let Err(err) = tick.conn.exec(&pipe).await {
            tick.state.record_error(format!("执行失败: {err}")).await;
            if needs_reconnect(&err) {
                return true;
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

enum ConnectOutcome {
    Ok(Conn),
    Stopped,
}

async fn connect_limited(
    frozen: &WorkloadConfig,
    cfg: &Arc<ArcSwap<WorkloadConfig>>,
    limit: &Semaphore,
    stop: &CancellationToken,
    state: &RuntimeState,
    worker_id: u32,
) -> ConnectOutcome {
    let mut backoff = CONNECT_BACKOFF_START;
    loop {
        if stop.is_cancelled() {
            return ConnectOutcome::Stopped;
        }
        if !cfg.load().running {
            return ConnectOutcome::Stopped;
        }
        if state.is_paused() {
            tokio::select! {
                _ = stop.cancelled() => return ConnectOutcome::Stopped,
                _ = tokio::time::sleep(PAUSE_WAIT) => continue,
            }
        }
        let _permit = tokio::select! {
            _ = stop.cancelled() => return ConnectOutcome::Stopped,
            result = limit.acquire() => match result {
                Ok(permit) => permit,
                Err(_) => return ConnectOutcome::Stopped,
            },
        };
        match Conn::connect(frozen).await {
            Ok(conn) => return ConnectOutcome::Ok(conn),
            Err(err) => {
                drop(_permit);
                state
                    .record_error(format!("worker {worker_id} 连接失败: {err}"))
                    .await;
                tokio::select! {
                    _ = stop.cancelled() => return ConnectOutcome::Stopped,
                    _ = tokio::time::sleep(backoff) => {
                        backoff = next_connect_backoff(backoff);
                    }
                }
            }
        }
    }
}

/// 连接可用性探活: 刷新绿灯, 并在全挂 / CLUSTERDOWN 时暂停派发.
pub async fn probe_loop(
    cfg: Arc<ArcSwap<WorkloadConfig>>,
    state: Arc<RuntimeState>,
    connect_limit: Arc<Semaphore>,
    stop: CancellationToken,
) {
    let mut ticker = tokio::time::interval(PROBE_INTERVAL);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = ticker.tick() => {
                probe_once(&cfg, &state, &connect_limit).await;
            }
        }
    }
}

async fn probe_once(
    cfg: &Arc<ArcSwap<WorkloadConfig>>,
    state: &RuntimeState,
    connect_limit: &Semaphore,
) {
    let Ok(_permit) = connect_limit.try_acquire() else {
        return;
    };
    let current = cfg.load_full();
    let timeout = Duration::from_millis(current.timeout_ms.max(1));
    let mut statuses = Vec::with_capacity(current.endpoints.len());
    for endpoint in &current.endpoints {
        let reachable = crate::conn::ping(endpoint, timeout).await;
        statuses.push(EndpointStatus {
            addr: endpoint.clone(),
            reachable,
        });
    }
    let any_up = statuses.iter().any(|item| item.reachable);
    let cluster_view = if current.mode == TargetMode::Cluster {
        let seed = current.endpoints.first().cloned().unwrap_or_default();
        let seed_up = statuses
            .iter()
            .any(|item| item.addr == seed && item.reachable);
        match crate::conn::cluster_state(&seed, timeout).await {
            Ok(status) => Some(status),
            Err(_) if seed_up => Some("unreachable".to_string()),
            Err(_) => None,
        }
    } else {
        None
    };
    let reason = pause_reason(!any_up, cluster_view.as_deref());
    state.set_paused(reason.is_some(), reason).await;
    state.set_endpoints(statuses).await;
}

#[cfg(test)]
mod tests;
