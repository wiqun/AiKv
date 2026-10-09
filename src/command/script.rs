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
//!   ├─ 声明 key 相同、且已经排在这条连接缓冲区里的脚本合成一轮
//!   ├─ checkout LuaPool: 复用沙箱，执行前清掉上一份脚本的全局表
//!   ├─ set_memory_limit (128MB) + 指令 hook 超时 (默认 5s)
//!   ├─ populate_keys_argv (全局 KEYS/ARGV) + install_redis_api (redis.call/pcall)
//!   ├─ eval_async → lua_to_resp
//!   └─ 本轮写操作一次 apply_stored_batch；客户端在这次提交成功后才收到成功回复
//! ```
//!
//! # Invariant
//!
//! - 原子性: 相同声明 key 的脚本可以排进同一轮。一轮成功时一次 `apply_stored_batch`，失败则这一轮都不算成功。Lua 失败只丢掉这一条的写缓冲。
//! - 集群下 `redis.call` 的 key 必须与 `KEYS[1]` 同 slot; 未声明的、同 slot 的 key 允许访问。
//! - 仅 `SCRIPT LOAD` 写入 LRU 缓存 (max 256); **EVAL 不自动缓存**; EVALSHA 未命中 → NOSCRIPT.
//! - 超时与内存上限由 mlua hook 与 memory limit 强制; SCRIPT KILL 恒 NOTBUSY (stub).

mod cache;
mod commit_chain;
mod convert;
mod execute;
pub(crate) mod json_exec;
mod sandbox;
mod transaction;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use mlua::MultiValue;
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

use self::cache::ScriptCache;
use self::commit_chain::{CommitChains, ScriptJob};
use self::execute::redis_call_async;
use self::sandbox::LuaPool;
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
    lua_pool: LuaPool,
    chains: CommitChains,
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
            lua_pool: LuaPool::with_available_parallelism(),
            chains: CommitChains::new(),
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
            lua_pool: LuaPool::with_available_parallelism(),
            chains: CommitChains::new(),
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
        let result = self
            .execute_script(&script, &keys, &argv, db, true, None)
            .await;
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
        let result = self
            .execute_script(&script, &keys, &argv, db, true, Some(sha1.as_str()))
            .await;
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
        sha: Option<&str>,
    ) -> Result<RespValue> {
        if keys.is_empty() {
            let job = commit_chain::ScriptJob {
                script: script.to_owned(),
                keys: keys.to_vec(),
                argv: argv.to_vec(),
                db,
                with_redis,
                sha: sha.map(str::to_owned),
            };
            return self.run_script(&job, None).await.map(|run| run.resp);
        }
        self.enter_chain(script, keys, argv, db, with_redis, sha)
            .await
    }

    /// 按到达顺序执行已经解析出的 EVAL / EVALSHA。
    /// 相邻且声明 key 集合相同的脚本合成一轮；key 一变就先提交再继续。
    pub(crate) async fn eval_many(
        &self,
        db: usize,
        calls: Vec<(String, Vec<Bytes>)>,
    ) -> Vec<Result<RespValue>> {
        let mut out = Vec::with_capacity(calls.len());
        let mut index = 0;
        while index < calls.len() {
            let (cmd, args) = &calls[index];
            let job = match self.prepare_call(db, cmd, args) {
                Ok(job) => job,
                Err(err) => {
                    self.record(lua_stat_name(cmd), false);
                    out.push(Err(err));
                    index += 1;
                    continue;
                }
            };
            let mut names = vec![lua_stat_name(cmd)];
            let group_id = self.chain_id(db, &job);
            let mut jobs = vec![job];
            index += 1;
            while index < calls.len() {
                let (cmd, args) = &calls[index];
                let Ok(next) = self.prepare_call(db, cmd, args) else {
                    break;
                };
                if self.chain_id(db, &next) != group_id {
                    break;
                }
                names.push(lua_stat_name(cmd));
                jobs.push(next);
                index += 1;
            }
            let results = self.run_pipeline(jobs).await;
            for (name, result) in names.into_iter().zip(&results) {
                self.record(name, result.is_ok());
            }
            out.extend(results);
        }
        out
    }

    fn prepare_call(&self, db: usize, cmd: &str, args: &[Bytes]) -> Result<ScriptJob> {
        match cmd.to_ascii_uppercase().as_str() {
            "EVAL" => {
                router::require_min_args("EVAL", args, 2)?;
                let script = String::from_utf8_lossy(&args[0]).into_owned();
                let (keys, argv) = parse_keys_argv(args)?;
                Ok(ScriptJob {
                    script,
                    keys,
                    argv,
                    db,
                    with_redis: true,
                    sha: None,
                })
            }
            "EVALSHA" => {
                router::require_min_args("EVALSHA", args, 2)?;
                let sha1 = String::from_utf8_lossy(&args[0]).to_string();
                let script = self.cache.get(&sha1).ok_or_else(|| {
                    Error::Command("NOSCRIPT No matching script. Use EVAL.".into())
                })?;
                let (keys, argv) = parse_keys_argv(args)?;
                Ok(ScriptJob {
                    script,
                    keys,
                    argv,
                    db,
                    with_redis: true,
                    sha: Some(sha1),
                })
            }
            _ => Err(Error::Command(format!("ERR unknown command '{cmd}'"))),
        }
    }

    fn chain_id(&self, db: usize, job: &ScriptJob) -> (usize, Vec<Vec<u8>>) {
        (db, self.key_lock.canonical_key_set(&job.keys))
    }

    async fn run_pipeline(&self, jobs: Vec<ScriptJob>) -> Vec<Result<RespValue>> {
        if jobs.is_empty() {
            return Vec::new();
        }
        if jobs[0].keys.is_empty() {
            let mut out = Vec::with_capacity(jobs.len());
            for job in jobs {
                out.push(self.run_script(&job, None).await.map(|run| run.resp));
            }
            return out;
        }
        self.enter_pipeline(jobs).await
    }
}

fn lua_stat_name(cmd: &str) -> &'static str {
    if cmd.eq_ignore_ascii_case("evalsha") {
        "evalsha"
    } else {
        "eval"
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
    access: self::execute::ScriptAccess,
) -> Result<()> {
    let redis = lua
        .create_table()
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let storage_call = storage.clone();
    let txn_call = Arc::clone(&txn);
    let access_call = access.clone();
    let call_fn = lua
        .create_async_function(move |lua, args: MultiValue| {
            let storage = storage_call.clone();
            let txn = Arc::clone(&txn_call);
            let access = access_call.clone();
            async move {
                let mut guard = txn.lock().await;
                redis_call_async(&lua, storage.as_ref(), &mut guard, &access, args, true).await
            }
        })
        .map_err(|e| Error::Command(format!("ERR {e}")))?;

    let storage_pcall = storage;
    let txn_pcall = txn;
    let access_pcall = access;
    let pcall_fn = lua
        .create_async_function(move |lua, args: MultiValue| {
            let storage = storage_pcall.clone();
            let txn = Arc::clone(&txn_pcall);
            let access = access_pcall.clone();
            async move {
                let mut guard = txn.lock().await;
                redis_call_async(&lua, storage.as_ref(), &mut guard, &access, args, false).await
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
