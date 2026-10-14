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
//!   ├─ eval_async → lua_to_resp
//!   └─ txn.commit: 脚本内写操作单次落盘
//! ```
//!
//! # Invariant
//!
//! - 原子性: 脚本内写操作进 `ScriptTransaction` 缓冲; 成功结束单次 `commit`, 失败 drop.
//! - 仅 `SCRIPT LOAD` 写入 LRU 缓存 (max 256); **EVAL 不自动缓存**; EVALSHA 未命中 → NOSCRIPT.
//! - 超时与内存上限由 mlua hook 与 memory limit 强制; SCRIPT KILL 恒 NOTBUSY (stub).

mod cache;
mod convert;
mod execute;
pub(crate) mod in_flight;
pub(crate) mod json_exec;
mod sandbox;
mod transaction;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use mlua::{HookTriggers, MultiValue, Value as LuaValue, VmState};
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

use self::cache::ScriptCache;
use self::convert::lua_to_resp;
use self::execute::redis_call_async;
use self::in_flight::InFlightBuffer;
use self::sandbox::new_sandbox_lua;
use self::transaction::ScriptTransaction;
use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::ServerMetrics;
use crate::storage::KvStorage;

const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_SCRIPT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MEMORY_LIMIT: usize = 128 * 1024 * 1024;
const MAX_IDLE_VMS: usize = 64;

struct ScriptExecutionContext {
    txn: Arc<Mutex<ScriptTransaction>>,
    declared: Arc<HashSet<Vec<u8>>>,
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
        }
    }

    fn record(&self, lock_us: u64, lua_us: u64, prep_us: u64, raft_us: u64, overall_us: u64) {
        self.total_lock_us.fetch_add(lock_us, Ordering::Relaxed);
        self.total_lua_us.fetch_add(lua_us, Ordering::Relaxed);
        self.total_prepare_us.fetch_add(prep_us, Ordering::Relaxed);
        self.total_raft_us.fetch_add(raft_us, Ordering::Relaxed);
        self.total_overall_us.fetch_add(overall_us, Ordering::Relaxed);
        let n = self.samples.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(1000) {
            let lock_avg = self.total_lock_us.load(Ordering::Relaxed) / n;
            let lua_avg = self.total_lua_us.load(Ordering::Relaxed) / n;
            let prep_avg = self.total_prepare_us.load(Ordering::Relaxed) / n;
            let raft_avg = self.total_raft_us.load(Ordering::Relaxed) / n;
            let overall_avg = self.total_overall_us.load(Ordering::Relaxed) / n;
            tracing::info!(
                target: "perf",
                "[SCRIPT_PERF] samples={} avg_lock_us={} avg_lua_us={} avg_prepare_us={} avg_raft_us={} avg_overall_us={}",
                n, lock_avg, lua_avg, prep_avg, raft_avg, overall_avg
            );
        }
    }
}

static PERF_SAMPLER: ScriptPerfSampler = ScriptPerfSampler::new();

pub struct ScriptCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    cache: ScriptCache,
    script_timeout: Duration,
    script_memory_limit: usize,
    metrics: Option<Arc<ServerMetrics>>,
    vm_pool: parking_lot::Mutex<Vec<mlua::Lua>>,
    in_flight: Arc<InFlightBuffer>,
}

impl ScriptCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self {
            storage,
            key_lock,
            cache: ScriptCache::default(),
            script_timeout: DEFAULT_SCRIPT_TIMEOUT,
            script_memory_limit: DEFAULT_MEMORY_LIMIT,
            metrics: None,
            vm_pool: parking_lot::Mutex::new(Vec::new()),
            in_flight: Arc::new(InFlightBuffer::new()),
        }
    }

    pub fn with_metrics(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        metrics: Arc<ServerMetrics>,
    ) -> Self {
        Self {
            storage,
            key_lock,
            cache: ScriptCache::default(),
            script_timeout: DEFAULT_SCRIPT_TIMEOUT,
            script_memory_limit: DEFAULT_MEMORY_LIMIT,
            metrics: Some(metrics),
            vm_pool: parking_lot::Mutex::new(Vec::new()),
            in_flight: Arc::new(InFlightBuffer::new()),
        }
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

        let declared = Arc::new(keys.iter().map(|k| k.to_vec()).collect());
        let txn = Arc::new(Mutex::new(ScriptTransaction::with_in_flight(
            db,
            Some(Arc::clone(&self.in_flight)),
        )));
        let lua = self.acquire_vm()?;

        let script_args = ScriptArgs {
            sha1,
            script,
            keys,
            argv,
        };
        let eval_res = self
            .eval_lua_func(&lua, &script_args, Arc::clone(&txn), declared)
            .await;
        self.release_vm(lua);
        let result = eval_res?;
        let t_lua_done = Instant::now();

        let txn =
            Arc::try_unwrap(txn).map_err(|_| Error::Command("ERR script still running".into()))?;
        let txn = txn.into_inner();
        let batch_ops = txn.prepare_batch_ops_owned(self.storage.as_ref()).await?;

        let (seq, keys) = {
            let keys: Vec<Vec<u8>> = batch_ops.iter().map(|(k, _)| k.clone()).collect();
            let seq = self.in_flight.insert_batch(db, &batch_ops);
            (seq, keys)
        };
        let wait_handle = if !batch_ops.is_empty() {
            Some(self.storage.enqueue_batch(db, batch_ops).await?)
        } else {
            None
        };
        drop(_guard);
        let t_prep_done = Instant::now();

        let resp = lua_to_resp(result)?;
        let write_res = if let Some(handle) = wait_handle {
            handle.wait().await
        } else {
            Ok(())
        };
        self.in_flight.remove_batch_keys(db, &keys, seq);
        write_res?;
        let t_done = Instant::now();

        PERF_SAMPLER.record(
            t_locked.duration_since(t_start).as_micros() as u64,
            t_lua_done.duration_since(t_locked).as_micros() as u64,
            t_prep_done.duration_since(t_lua_done).as_micros() as u64,
            t_done.duration_since(t_prep_done).as_micros() as u64,
            t_done.duration_since(t_start).as_micros() as u64,
        );

        Ok(resp)
    }

    async fn eval_lua_func(
        &self,
        lua: &mlua::Lua,
        args: &ScriptArgs<'_>,
        txn: Arc<Mutex<ScriptTransaction>>,
        declared: Arc<HashSet<Vec<u8>>>,
    ) -> Result<LuaValue> {
        let started = Instant::now();
        let timeout = self.script_timeout;
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
        lua.set_app_data(ScriptExecutionContext { txn, declared });

        let eval_result: mlua::Result<LuaValue> = func.call_async(()).await;

        lua.remove_hook();
        let _ = lua.remove_app_data::<ScriptExecutionContext>();
        let _ = clear_keys_argv(lua);

        eval_result.map_err(|e| Error::Command(format!("ERR Error running script: {e}")))
    }
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
        .create_table()
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
        .create_table()
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

    let storage_call = storage.clone();
    let call_fn = lua
        .create_async_function(move |lua, args: MultiValue| {
            let storage = storage_call.clone();
            async move {
                let (txn, declared) = {
                    let ctx = lua.app_data_ref::<ScriptExecutionContext>().ok_or_else(|| {
                        mlua::Error::RuntimeError("ERR missing script execution context".into())
                    })?;
                    (Arc::clone(&ctx.txn), Arc::clone(&ctx.declared))
                };
                let mut guard = txn.lock().await;
                redis_call_async(
                    &lua,
                    storage.as_ref(),
                    &mut guard,
                    declared.as_ref(),
                    args,
                    true,
                )
                .await
            }
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let storage_pcall = storage;
    let pcall_fn = lua
        .create_async_function(move |lua, args: MultiValue| {
            let storage = storage_pcall.clone();
            async move {
                let (txn, declared) = {
                    let ctx = lua.app_data_ref::<ScriptExecutionContext>().ok_or_else(|| {
                        mlua::Error::RuntimeError("ERR missing script execution context".into())
                    })?;
                    (Arc::clone(&ctx.txn), Arc::clone(&ctx.declared))
                };
                let mut guard = txn.lock().await;
                redis_call_async(
                    &lua,
                    storage.as_ref(),
                    &mut guard,
                    declared.as_ref(),
                    args,
                    false,
                )
                .await
            }
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

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
