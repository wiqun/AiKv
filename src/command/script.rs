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
pub(crate) mod json_exec;
mod sandbox;
mod transaction;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use mlua::{HookTriggers, MultiValue, Value as LuaValue, VmState};
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

use self::cache::ScriptCache;
use self::convert::lua_to_resp;
use self::execute::redis_call_async;
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

pub struct ScriptCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    cache: ScriptCache,
    script_timeout: Duration,
    script_memory_limit: usize,
    metrics: Option<Arc<ServerMetrics>>,
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
        let (keys, argv) = parse_keys_argv(args)?;
        let result = self.execute_script(&script, &keys, &argv, db, true).await;
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
        let result = self.execute_script(&script, &keys, &argv, db, true).await;
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
        script: &str,
        keys: &[Bytes],
        argv: &[Bytes],
        db: usize,
        with_redis: bool,
    ) -> Result<RespValue> {
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_ref()).collect();
        let _guard = if keys.is_empty() {
            None
        } else {
            Some(
                self.key_lock
                    .lock_keys_sorted_with_timeout(&key_refs, DEFAULT_SCRIPT_LOCK_TIMEOUT)
                    .await?,
            )
        };

        let declared: HashSet<Vec<u8>> = keys.iter().map(|k| k.to_vec()).collect();
        let txn = Arc::new(Mutex::new(ScriptTransaction::new(db)));
        let lua = new_sandbox_lua()?;
        lua.set_memory_limit(self.script_memory_limit)
            .map_err(|e| Error::Command(format!("ERR {e}")))?;

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

        populate_keys_argv(&lua, keys, argv)?;

        if with_redis {
            install_redis_api(&lua, self.storage.clone(), Arc::clone(&txn), declared)?;
        }

        let result: LuaValue = lua
            .load(script)
            .eval_async()
            .await
            .map_err(|e| Error::Command(format!("ERR Error running script: {e}")))?;

        let duration_us = started.elapsed().as_micros() as u64;
        tracing::info!(
          target: "cmd.lua",
          name = "cmd_lua_exec",
          duration_us,
          "script execution complete"
        );
        if let Some(m) = &self.metrics {
            m.on_lua_execution(duration_us);
        }

        let resp = lua_to_resp(result)?;

        drop(lua);

        let txn =
            Arc::try_unwrap(txn).map_err(|_| Error::Command("ERR script still running".into()))?;
        let txn = txn.into_inner();
        txn.commit(self.storage.as_ref()).await?;

        Ok(resp)
    }
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

fn install_redis_api(
    lua: &mlua::Lua,
    storage: Arc<dyn KvStorage>,
    txn: Arc<Mutex<ScriptTransaction>>,
    declared: HashSet<Vec<u8>>,
) -> Result<()> {
    let redis = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let storage_call = storage.clone();
    let txn_call = Arc::clone(&txn);
    let declared_call = declared.clone();
    let call_fn = lua
        .create_async_function(move |lua, args: MultiValue| {
            let storage = storage_call.clone();
            let txn = Arc::clone(&txn_call);
            let declared = declared_call.clone();
            async move {
                let mut guard = txn.lock().await;
                redis_call_async(&lua, storage.as_ref(), &mut guard, &declared, args, true).await
            }
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let storage_pcall = storage;
    let txn_pcall = txn;
    let declared_pcall = declared;
    let pcall_fn = lua
        .create_async_function(move |lua, args: MultiValue| {
            let storage = storage_pcall.clone();
            let txn = Arc::clone(&txn_pcall);
            let declared = declared_pcall.clone();
            async move {
                let mut guard = txn.lock().await;
                redis_call_async(&lua, storage.as_ref(), &mut guard, &declared, args, false).await
            }
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    redis
        .set("call", call_fn)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    redis
        .set("pcall", pcall_fn)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    lua.globals()
        .set("redis", redis)
        .map_err(|e| Error::Command(format!("ERR {e}")))?;
    Ok(())
}
