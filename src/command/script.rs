//! Lua 脚本命令入口: EVAL / EVALSHA / SCRIPT (LOAD/EXISTS/FLUSH/KILL). 子模块:
//! `sandbox` (mlua 沙箱) / `execute` (redis.call) / `json_exec` (JSON 子集) /
//! `transaction` (写缓冲 + commit) / `convert` (Lua ↔ RESP) / `cache` (SCRIPT LOAD LRU).
//!
//! # 执行流程
//!
//! ```text
//! EVAL script numkeys key… arg…
//!   ├─ parse_keys_argv: 按 numkeys 切分 KEYS / ARGV
//!   ├─ key_lock.lock_keys_sorted_with_timeout(KEYS, 30s)
//!   ├─ new_sandbox_lua: StdLib = TABLE|STRING|MATH|UTF8; 封印 load/require/rawget/rawset…
//!   ├─ set_memory_limit (128MB) + 指令 hook 超时 (默认 5s)
//!   ├─ populate_keys_argv (全局 KEYS/ARGV) + install_redis_api (redis.call/pcall)
//!   ├─ eval: 多线程运行时整段脚本在同一条阻塞线程上执行, 存储调用不再逐次换线程
//!   ├─ 单机写入也在这条线程上完成; 集群的 Raft 确认留在线程外, 在途缓冲保持可见
//!   └─ lua_to_resp
//! ```
//!
//! # Invariant
//!
//! - 原子性: 脚本内写操作进 `ScriptTransaction` 缓冲; 成功结束单次 `commit`, 失败 drop.
//! - 仅 `SCRIPT LOAD` 写入 LRU 缓存 (max 256); **EVAL 不自动缓存**; EVALSHA 未命中 → NOSCRIPT.
//! - 超时与内存上限由 mlua hook 与 memory limit 强制; SCRIPT KILL 恒 NOTBUSY (stub).

mod cache;
mod call_stats;
mod convert;
mod drive;
mod execute;
pub(crate) mod in_flight;
pub(crate) mod json_exec;
mod phase_stats;
mod pool;
mod sandbox;
mod transaction;
mod zset_edit;

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use mlua::{HookTriggers, MultiValue, Value as LuaValue, VmState};
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

use self::cache::ScriptCache;
use self::convert::lua_to_resp;
use self::execute::{redis_call, DeclaredKeys};
use self::in_flight::InFlightBuffer;
use self::sandbox::new_sandbox_lua;
use self::transaction::ScriptTransaction;
use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::ServerMetrics;
use crate::storage::ScriptIo;
use crate::storage::{KvStorage, WriteOp};

const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_SCRIPT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MEMORY_LIMIT: usize = 128 * 1024 * 1024;
const MAX_IDLE_VMS: usize = 64;

struct ScriptExecutionContext {
    txn: RefCell<ScriptTransaction>,
    declared: Arc<DeclaredKeys>,
}

struct ScriptArgs<'a> {
    sha1: &'a str,
    script: &'a str,
    keys: &'a [Bytes],
    argv: &'a [Bytes],
}

struct ScriptPerfSampler {
    samples: AtomicU64,
    total_lock_us: AtomicU64,
    total_lua_us: AtomicU64,
    total_prepare_us: AtomicU64,
    total_raft_us: AtomicU64,
    total_overall_us: AtomicU64,
    total_lock_hold_us: AtomicU64,
}

impl ScriptPerfSampler {
    const fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            total_lock_us: AtomicU64::new(0),
            total_lua_us: AtomicU64::new(0),
            total_prepare_us: AtomicU64::new(0),
            total_raft_us: AtomicU64::new(0),
            total_overall_us: AtomicU64::new(0),
            total_lock_hold_us: AtomicU64::new(0),
        }
    }

    fn record(
        &self,
        lock_us: u64,
        lua_us: u64,
        prep_us: u64,
        raft_us: u64,
        overall_us: u64,
        lock_hold_us: u64,
    ) {
        self.total_lock_us.fetch_add(lock_us, Ordering::Relaxed);
        self.total_lua_us.fetch_add(lua_us, Ordering::Relaxed);
        self.total_prepare_us.fetch_add(prep_us, Ordering::Relaxed);
        self.total_raft_us.fetch_add(raft_us, Ordering::Relaxed);
        self.total_overall_us
            .fetch_add(overall_us, Ordering::Relaxed);
        self.total_lock_hold_us
            .fetch_add(lock_hold_us, Ordering::Relaxed);
        let n = self.samples.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(1000) {
            let lock_avg = self.total_lock_us.load(Ordering::Relaxed) / n;
            let lua_avg = self.total_lua_us.load(Ordering::Relaxed) / n;
            let prep_avg = self.total_prepare_us.load(Ordering::Relaxed) / n;
            let raft_avg = self.total_raft_us.load(Ordering::Relaxed) / n;
            let overall_avg = self.total_overall_us.load(Ordering::Relaxed) / n;
            let hold_avg = self.total_lock_hold_us.load(Ordering::Relaxed) / n;
            tracing::info!(
                target: "perf",
                "[SCRIPT_PERF] samples={} avg_lock_us={} avg_lua_us={} avg_prepare_us={} avg_raft_us={} avg_overall_us={} avg_lock_hold_us={}",
                n, lock_avg, lua_avg, prep_avg, raft_avg, overall_avg, hold_avg
            );
        }
    }
}

static PERF_SAMPLER: ScriptPerfSampler = ScriptPerfSampler::new();

struct ScriptIoSampler {
    samples: AtomicU64,
    get_us: AtomicU64,
    range_us: AtomicU64,
    prefix_us: AtomicU64,
    scan_us: AtomicU64,
    write_us: AtomicU64,
}

impl ScriptIoSampler {
    const fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            get_us: AtomicU64::new(0),
            range_us: AtomicU64::new(0),
            prefix_us: AtomicU64::new(0),
            scan_us: AtomicU64::new(0),
            write_us: AtomicU64::new(0),
        }
    }

    fn record(&self, stats: crate::storage::script_io::ScriptIoStats) {
        self.get_us.fetch_add(stats.get_us, Ordering::Relaxed);
        self.range_us.fetch_add(stats.range_us, Ordering::Relaxed);
        self.prefix_us.fetch_add(stats.prefix_us, Ordering::Relaxed);
        self.scan_us.fetch_add(stats.scan_us, Ordering::Relaxed);
        self.write_us.fetch_add(stats.write_us, Ordering::Relaxed);
        let n = self.samples.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(1000) {
            tracing::info!(
                target: "perf",
                "[SCRIPT_IO_PERF] samples={} avg_get_us={} avg_range_us={} avg_prefix_us={} avg_scan_us={} avg_write_us={}",
                n,
                self.get_us.load(Ordering::Relaxed) / n,
                self.range_us.load(Ordering::Relaxed) / n,
                self.prefix_us.load(Ordering::Relaxed) / n,
                self.scan_us.load(Ordering::Relaxed) / n,
                self.write_us.load(Ordering::Relaxed) / n
            );
        }
    }
}

static IO_SAMPLER: ScriptIoSampler = ScriptIoSampler::new();

struct Seg50Slot {
    first_start: parking_lot::Mutex<Option<Instant>>,
    first_enq: parking_lot::Mutex<Option<Instant>>,
    last_enq: parking_lot::Mutex<Option<Instant>>,
    last_commit: parking_lot::Mutex<Option<Instant>>,
    enq_count: AtomicUsize,
    commit_count: AtomicUsize,
}

impl Seg50Slot {
    const fn new() -> Self {
        Self {
            first_start: parking_lot::Mutex::new(None),
            first_enq: parking_lot::Mutex::new(None),
            last_enq: parking_lot::Mutex::new(None),
            last_commit: parking_lot::Mutex::new(None),
            enq_count: AtomicUsize::new(0),
            commit_count: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        *self.first_start.lock() = None;
        *self.first_enq.lock() = None;
        *self.last_enq.lock() = None;
        *self.last_commit.lock() = None;
        self.enq_count.store(0, Ordering::Relaxed);
        self.commit_count.store(0, Ordering::Relaxed);
    }
}

struct Seg50Tracker {
    req_counter: AtomicUsize,
    slots: [Seg50Slot; 64],
}

impl Seg50Tracker {
    const fn new() -> Self {
        Self {
            req_counter: AtomicUsize::new(0),
            slots: [const { Seg50Slot::new() }; 64],
        }
    }

    fn on_start(&self) -> (usize, usize) {
        let req_id = self.req_counter.fetch_add(1, Ordering::Relaxed);
        let seg_id = req_id / 50;
        let slot_idx = seg_id % 64;
        let slot = &self.slots[slot_idx];
        if req_id.is_multiple_of(50) {
            slot.reset();
            *slot.first_start.lock() = Some(Instant::now());
        }
        (seg_id, slot_idx)
    }

    fn on_enqueued(&self, slot_idx: usize) {
        let slot = &self.slots[slot_idx];
        let idx = slot.enq_count.fetch_add(1, Ordering::SeqCst);
        let now = Instant::now();
        if idx == 0 {
            *slot.first_enq.lock() = Some(now);
        } else if idx == 49 {
            *slot.last_enq.lock() = Some(now);
        }
    }

    fn on_committed(&self, seg_id: usize, slot_idx: usize) {
        let slot = &self.slots[slot_idx];
        *slot.last_commit.lock() = Some(Instant::now());
        let idx = slot.commit_count.fetch_add(1, Ordering::SeqCst);
        if idx == 49 {
            self.report_seg(seg_id, slot);
        }
    }

    fn report_seg(&self, seg_id: usize, slot: &Seg50Slot) {
        let t_commit = slot.last_commit.lock().unwrap_or_else(Instant::now);
        let t_start = *slot.first_start.lock();
        let t_first = *slot.first_enq.lock();
        let t_last = *slot.last_enq.lock();

        if let (Some(t_first), Some(t_last)) = (t_first, t_last) {
            let enq_span_ms = t_last.duration_since(t_first).as_secs_f64() * 1000.0;
            let tail_ms = t_commit.duration_since(t_last).as_secs_f64() * 1000.0;
            let total_ms = match t_start {
                Some(ts) => t_commit.duration_since(ts).as_secs_f64() * 1000.0,
                None => t_commit.duration_since(t_first).as_secs_f64() * 1000.0,
            };
            tracing::info!(
                target: "perf",
                "[SEG50_TIMING] seg={} enq_span_ms={:.2} tail_commit_ms={:.2} total_seg_ms={:.2}",
                seg_id, enq_span_ms, tail_ms, total_ms
            );
        }
    }
}

static SEG_TRACKER: Seg50Tracker = Seg50Tracker::new();

pub struct ScriptCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    cache: ScriptCache,
    script_timeout: Duration,
    script_memory_limit: usize,
    metrics: Option<Arc<ServerMetrics>>,
    vm_pool: parking_lot::Mutex<Vec<mlua::Lua>>,
    in_flight: Arc<InFlightBuffer>,
    pool: pool::ScriptWorkerPool,
}

impl ScriptCommands {
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.script_timeout = timeout;
        self
    }

    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self::with_worker_threads(
            storage,
            key_lock,
            crate::config::settings::default_script_worker_threads(),
        )
    }

    pub fn with_worker_threads(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        threads: usize,
    ) -> Self {
        Self {
            storage,
            key_lock,
            cache: ScriptCache::default(),
            script_timeout: DEFAULT_SCRIPT_TIMEOUT,
            script_memory_limit: DEFAULT_MEMORY_LIMIT,
            metrics: None,
            vm_pool: parking_lot::Mutex::new(Vec::new()),
            in_flight: Arc::new(InFlightBuffer::new()),
            pool: pool::ScriptWorkerPool::new(threads),
        }
    }

    pub fn with_metrics(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        metrics: Arc<ServerMetrics>,
        threads: usize,
    ) -> Self {
        let mut commands = Self::with_worker_threads(storage, key_lock, threads);
        commands.metrics = Some(metrics);
        commands
    }

    fn acquire_vm(&self) -> Result<mlua::Lua> {
        if let Some(lua) = self.vm_pool.lock().pop() {
            return Ok(lua);
        }
        create_sandbox_vm(self.storage.clone(), self.script_memory_limit)
    }

    fn release_vm(&self, lua: mlua::Lua) {
        if lua.used_memory() > 16 * 1024 * 1024 {
            return;
        }
        let mut pool = self.vm_pool.lock();
        if pool.len() < MAX_IDLE_VMS {
            pool.push(lua);
        }
    }

    fn record(&self, cmd: &str, ok: bool) {
        if let Some(m) = &self.metrics {
            m.on_lua_command(cmd, ok);
        }
    }

    fn sha1(script: &str) -> String {
        let mut hasher = Sha1::new();
        hasher.update(script.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub async fn eval(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("EVAL", args, 2)?;
        let script = String::from_utf8_lossy(&args[0]).into_owned();
        let sha1 = Self::sha1(&script);
        self.cache.insert(sha1.clone(), script.clone());
        let (keys, argv) = parse_keys_argv(args)?;
        let result = self.execute_script(&sha1, &script, &keys, &argv, db).await;
        self.record("eval", result.is_ok());
        result
    }

    pub async fn evalsha(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("EVALSHA", args, 2)?;
        let sha1 = String::from_utf8_lossy(&args[0]).to_string();
        let script = self
            .cache
            .get(&sha1)
            .ok_or_else(|| Error::Command("NOSCRIPT No matching script. Use EVAL.".into()))?;
        let (keys, argv) = parse_keys_argv(args)?;
        let result = self.execute_script(&sha1, &script, &keys, &argv, db).await;
        self.record("evalsha", result.is_ok());
        result
    }

    pub async fn script(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SCRIPT", args, 1)?;
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "LOAD" => self.script_load(&args[1..]).await,
            "EXISTS" => self.script_exists(&args[1..]).await,
            "FLUSH" => self.script_flush(),
            "KILL" => self.script_kill(),
            _ => Err(Error::Command(format!(
                "ERR unknown SCRIPT subcommand '{sub}'"
            ))),
        }
    }

    async fn script_load(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("SCRIPT LOAD", args, 1)?;
        let script = String::from_utf8_lossy(&args[0]).into_owned();
        let sha1 = Self::sha1(&script);
        self.cache.insert(sha1.clone(), script);
        self.record("script_load", true);
        Ok(router::bulk(sha1.into_bytes()))
    }

    async fn script_exists(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(router::wrong_args("SCRIPT EXISTS", ""));
        }
        let out: Vec<RespValue> = args
            .iter()
            .map(|a| {
                let sha1 = String::from_utf8_lossy(a);
                router::integer(i64::from(self.cache.exists(&sha1)))
            })
            .collect();
        Ok(RespValue::Array(Some(out)))
    }

    fn script_flush(&self) -> Result<RespValue> {
        self.cache.flush();
        Ok(RespValue::SimpleString("OK".into()))
    }

    fn script_kill(&self) -> Result<RespValue> {
        Err(Error::Command(
            "NOTBUSY No scripts in execution right now.".into(),
        ))
    }

    async fn execute_script(
        &self,
        sha1: &str,
        script: &str,
        keys: &[Bytes],
        argv: &[Bytes],
        db: usize,
    ) -> Result<RespValue> {
        let (seg_id, slot_idx) = SEG_TRACKER.on_start();
        let t_start = Instant::now();
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_ref()).collect();
        let _guard = if !keys.is_empty() {
            Some(
                self.key_lock
                    .lock_keys_sorted_with_timeout(&key_refs, DEFAULT_SCRIPT_LOCK_TIMEOUT)
                    .await?,
            )
        } else {
            None
        };
        let t_locked = Instant::now();

        let lua = self.acquire_vm()?;
        let drive = self
            .drive_script(lua, db, sha1, script, keys, argv, seg_id, slot_idx)
            .await?;
        self.release_vm(drive.lua);
        IO_SAMPLER.record(drive.io);
        call_stats::record_script_calls(drive.calls);
        let prepare_us = drive.result.as_ref().map_or(0, |body| {
            phase_stats::micros_between(body.t_lua_done, body.t_prep_done)
        });
        phase_stats::record_script_phase(phase_stats::ScriptPhaseStats {
            hop_us: phase_stats::micros_between(t_locked, drive.t_entered),
            setup_us: drive.setup_us,
            gap_us: phase_stats::gap_us(drive.call_wall_us, drive.calls),
            teardown_us: drive.teardown_us,
            prepare_us,
        });
        let body = drive.result?;
        let (commit_done, unlocked) = self
            .finish_pending_commit(body.pending, db, seg_id, slot_idx, _guard)
            .await?;
        let t_done = commit_done.unwrap_or(body.t_local_done);

        PERF_SAMPLER.record(
            t_locked.duration_since(t_start).as_micros() as u64,
            body.t_lua_done.duration_since(t_locked).as_micros() as u64,
            body.t_prep_done.duration_since(body.t_lua_done).as_micros() as u64,
            t_done.duration_since(body.t_prep_done).as_micros() as u64,
            t_done.duration_since(t_start).as_micros() as u64,
            unlocked.duration_since(t_locked).as_micros() as u64,
        );
        lua_to_resp(body.value)
    }

    async fn drive_script(
        &self,
        lua: mlua::Lua,
        db: usize,
        sha1: &str,
        script: &str,
        keys: &[Bytes],
        argv: &[Bytes],
        seg_id: usize,
        slot_idx: usize,
    ) -> Result<drive::ScriptDrive> {
        let job = drive::ScriptJob {
            lua,
            storage: Arc::clone(&self.storage),
            in_flight: Arc::clone(&self.in_flight),
            timeout: self.script_timeout,
            db,
            sha1: sha1.to_string(),
            script: script.to_string(),
            keys: keys.to_vec(),
            argv: argv.to_vec(),
            local_commit: self.storage.local_script_commit(),
            seg_id,
            slot_idx,
        };
        if !drive::script_runs_off_worker() {
            return Ok(job.run());
        }
        self.pool.run(move || job.run()).await
    }

    async fn finish_pending_commit<G>(
        &self,
        pending: Option<Vec<(Vec<u8>, WriteOp)>>,
        db: usize,
        seg_id: usize,
        slot_idx: usize,
        guard: G,
    ) -> Result<(Option<Instant>, Instant)> {
        let Some(batch_ops) = pending else {
            drop(guard);
            return Ok((None, Instant::now()));
        };
        if batch_ops.is_empty() {
            SEG_TRACKER.on_enqueued(slot_idx);
            drop(guard);
            let unlocked = Instant::now();
            SEG_TRACKER.on_committed(seg_id, slot_idx);
            return Ok((Some(unlocked), unlocked));
        }
        let inserted = self.in_flight.insert_batch(db, &batch_ops);
        let handle = match self.storage.enqueue_batch(db, batch_ops).await {
            Ok(handle) => handle,
            Err(err) => {
                drive::rollback_inserted(&self.in_flight, Some(inserted));
                drop(guard);
                return Err(err);
            }
        };
        SEG_TRACKER.on_enqueued(slot_idx);
        drop(guard);
        let unlocked = Instant::now();
        let write_res = handle.wait().await;
        self.in_flight.remove_encoded(&inserted.1, inserted.0);
        SEG_TRACKER.on_committed(seg_id, slot_idx);
        write_res?;
        Ok((Some(Instant::now()), unlocked))
    }

    fn eval_lua_func(
        lua: &mlua::Lua,
        timeout: Duration,
        args: &ScriptArgs<'_>,
        txn: ScriptTransaction,
        declared: Arc<DeclaredKeys>,
    ) -> Result<(LuaValue, ScriptTransaction)> {
        let t_setup = Instant::now();
        let func = prepare_lua_call(lua, timeout, args, txn, declared)?;
        let t_call = Instant::now();
        let eval_result: mlua::Result<LuaValue> = func.call(());
        let t_back = Instant::now();
        let txn = finish_lua_call(lua)?;
        let t_done = Instant::now();
        phase_stats::store_eval_span(
            phase_stats::micros_between(t_setup, t_call),
            phase_stats::micros_between(t_call, t_back),
            phase_stats::micros_between(t_back, t_done),
        );
        eval_result
            .map(|value| (value, txn))
            .map_err(|e| Error::Command(format!("ERR Error running script: {e}")))
    }
}

fn prepare_lua_call(
    lua: &mlua::Lua,
    timeout: Duration,
    args: &ScriptArgs<'_>,
    txn: ScriptTransaction,
    declared: Arc<DeclaredKeys>,
) -> Result<mlua::Function> {
    let started = Instant::now();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(10_000),
        move |_lua, _debug| {
            if started.elapsed() >= timeout {
                return Err(mlua::Error::RuntimeError("Script timed out".to_string()));
            }
            Ok(VmState::Continue)
        },
    );

    let func_name = format!("__aikv_script_{}", args.sha1);
    let globals = lua.globals();
    let func: mlua::Function = match globals.get(func_name.as_str()) {
        Ok(f) => f,
        Err(_) => {
            let f: mlua::Function = lua
                .load(args.script)
                .into_function()
                .map_err(|e| Error::Command(format!("ERR Error compiling script: {e}")))?;
            globals
                .set(func_name.as_str(), f.clone())
                .map_err(|e| Error::Command(format!("ERR Error saving script: {e}")))?;
            f
        }
    };

    populate_keys_argv(lua, args.keys, args.argv)?;
    lua.set_app_data(ScriptExecutionContext {
        txn: RefCell::new(txn),
        declared,
    });
    Ok(func)
}

fn finish_lua_call(lua: &mlua::Lua) -> Result<ScriptTransaction> {
    lua.remove_hook();
    let ctx = lua.remove_app_data::<ScriptExecutionContext>();
    let _ = clear_keys_argv(lua);
    Ok(ctx
        .ok_or_else(|| Error::Command("ERR missing script execution context".into()))?
        .txn
        .into_inner())
}

fn lua_redis_entry(
    lua: &mlua::Lua,
    storage: Arc<dyn KvStorage>,
    throw_error: bool,
) -> Result<mlua::Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let declared = {
            let ctx = lua
                .app_data_ref::<ScriptExecutionContext>()
                .ok_or_else(|| {
                    mlua::Error::RuntimeError("ERR missing script execution context".into())
                })?;
            Arc::clone(&ctx.declared)
        };
        let mut ctx = lua
            .app_data_mut::<ScriptExecutionContext>()
            .ok_or_else(|| {
                mlua::Error::RuntimeError("ERR missing script execution context".into())
            })?;
        let mut txn = ctx.txn.borrow_mut();
        redis_call(
            lua,
            storage.script_io(),
            &mut txn,
            declared.as_ref(),
            args,
            throw_error,
        )
    })
    .map_err(|e| Error::Command(format!("ERR {e}")))
}

fn create_sandbox_vm(storage: Arc<dyn KvStorage>, memory_limit: usize) -> Result<mlua::Lua> {
    let lua = new_sandbox_lua()?;
    lua.set_memory_limit(memory_limit)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    install_redis_api(&lua, storage)?;
    Ok(lua)
}

fn clear_keys_argv(lua: &mlua::Lua) -> Result<()> {
    let globals = lua.globals();
    globals
        .set("KEYS", mlua::Value::Nil)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    globals
        .set("ARGV", mlua::Value::Nil)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    Ok(())
}

fn parse_keys_argv(args: &[Bytes]) -> Result<(Vec<Bytes>, Vec<Bytes>)> {
    let numkeys: usize = String::from_utf8_lossy(&args[1])
        .parse()
        .map_err(|_| Error::Command("ERR Number of keys is not an integer".into()))?;
    if args.len() < 2 + numkeys {
        return Err(Error::Command(
            "ERR Number of keys can't be greater than number of args".into(),
        ));
    }
    let keys = args[2..2 + numkeys].to_vec();
    let argv = args[2 + numkeys..].to_vec();
    Ok((keys, argv))
}

fn populate_keys_argv(lua: &mlua::Lua, keys: &[Bytes], argv: &[Bytes]) -> Result<()> {
    let globals = lua.globals();
    let keys_table = lua
        .create_table_with_capacity(keys.len(), 0)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    for (i, k) in keys.iter().enumerate() {
        let s = lua
            .create_string(k.as_ref())
            .map_err(|e| Error::Command(format!("ERR {e}")))?;
        keys_table
            .set(i + 1, s)
            .map_err(|e| Error::Command(format!("ERR {e}")))?;
    }
    globals
        .set("KEYS", keys_table)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let argv_table = lua
        .create_table_with_capacity(argv.len(), 0)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    for (i, a) in argv.iter().enumerate() {
        let s = lua
            .create_string(a.as_ref())
            .map_err(|e| Error::Command(format!("ERR {e}")))?;
        argv_table
            .set(i + 1, s)
            .map_err(|e| Error::Command(format!("ERR {e}")))?;
    }
    globals
        .set("ARGV", argv_table)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    Ok(())
}

fn install_redis_api(lua: &mlua::Lua, storage: Arc<dyn KvStorage>) -> Result<()> {
    let redis = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let call_fn = lua_redis_entry(lua, Arc::clone(&storage), true)?;
    let pcall_fn = lua_redis_entry(lua, storage, false)?;

    let error_reply = lua
        .create_function(|lua, msg: mlua::String| {
            let table = lua.create_table()?;
            table.set("err", msg)?;
            Ok(mlua::Value::Table(table))
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let status_reply = lua
        .create_function(|lua, msg: mlua::String| {
            let table = lua.create_table()?;
            table.set("ok", msg)?;
            Ok(mlua::Value::Table(table))
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let log_fn = lua
        .create_function(|_lua, _args: MultiValue| Ok(()))
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let sha1hex_fn = lua
        .create_function(|lua, s: mlua::String| {
            let mut hasher = Sha1::new();
            hasher.update(s.as_bytes());
            let hex = format!("{:x}", hasher.finalize());
            lua.create_string(&hex)
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    redis
        .set("call", call_fn)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    redis
        .set("pcall", pcall_fn)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    redis
        .set("error_reply", error_reply)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    redis
        .set("status_reply", status_reply)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    redis
        .set("log", log_fn)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    redis
        .set("sha1hex", sha1hex_fn)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    lua.globals()
        .set("redis", redis)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    Ok(())
}
