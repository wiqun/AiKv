//! 声明 key 完全相同的 EVAL / EVALSHA 合成一次 `apply_stored_batch`。
//!
//! 第一条脚本成为 owner 并持有键锁。后来的相同 key 集合只排队，不再加锁。
//! owner 在内存里顺序执行，后一条能读到本轮前面的写。达到 512 笔或队列暂时空了
//! 就提交一次，然后放开键锁，把 owner 交给队首。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use bytes::Bytes;
use tokio::sync::oneshot;

use super::transaction::{BurstOverlay, ScriptTransaction};
use super::ScriptCommands;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::storage::StoredBatchOp;

/// 与 `storage::cluster_batcher::SET_BATCH_MAX_OPS` 相同。
/// 脚本路径在没有 cluster feature 时也使用这个上限。
const SCRIPT_BURST_MAX_OPS: usize = 512;

type ChainId = (usize, Vec<Vec<u8>>);

pub(super) struct ScriptJob {
    pub(super) script: String,
    pub(super) keys: Vec<Bytes>,
    pub(super) argv: Vec<Bytes>,
    pub(super) db: usize,
    pub(super) with_redis: bool,
    pub(super) sha: Option<String>,
}

enum Wake {
    Reply(Result<RespValue>),
    Lead(ScriptJob),
}

struct Waiter {
    job: ScriptJob,
    reply: oneshot::Sender<Wake>,
}

#[derive(Default)]
struct ChainQueue {
    owner: bool,
    waiters: VecDeque<Waiter>,
}

struct Outcome {
    reply: Option<oneshot::Sender<Wake>>,
    result: Result<RespValue>,
}

pub(super) struct CommitChains {
    inner: Mutex<HashMap<ChainId, ChainQueue>>,
}

impl CommitChains {
    pub(super) fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

struct OwnerGuard<'a> {
    chains: &'a CommitChains,
    id: ChainId,
    armed: bool,
}

impl Drop for OwnerGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            handoff_or_clear(self.chains, &self.id);
        }
    }
}

impl ScriptCommands {
    pub(super) async fn enter_chain(
        &self,
        script: &str,
        keys: &[Bytes],
        argv: &[Bytes],
        db: usize,
        with_redis: bool,
        sha: Option<&str>,
    ) -> Result<RespValue> {
        let job = ScriptJob {
            script: script.to_owned(),
            keys: keys.to_vec(),
            argv: argv.to_vec(),
            db,
            with_redis,
            sha: sha.map(str::to_owned),
        };
        let id = (db, self.key_lock.canonical_key_set(keys));
        let (lead_job, queued) = {
            let mut map = self.chains.inner.lock().unwrap();
            let queue = map.entry(id.clone()).or_default();
            if queue.owner {
                let (tx, rx) = oneshot::channel();
                queue.waiters.push_back(Waiter { job, reply: tx });
                (None, Some(rx))
            } else {
                queue.owner = true;
                (Some(job), None)
            }
        };
        match queued {
            None => self.lead(id, lead_job.expect("owner keeps its job")).await,
            Some(rx) => match rx.await {
                Ok(Wake::Reply(result)) => result,
                Ok(Wake::Lead(job)) => self.lead(id, job).await,
                Err(_) => Err(Error::Command("ERR script chain closed".into())),
            },
        }
    }

    /// 同一条连接上已经解析出来的脚本。先入队再执行第一条，这样一轮提交能看见它们。
    pub(super) async fn enter_pipeline(&self, jobs: Vec<ScriptJob>) -> Vec<Result<RespValue>> {
        let mut jobs = jobs.into_iter();
        let Some(first) = jobs.next() else {
            return Vec::new();
        };
        let id = (first.db, self.key_lock.canonical_key_set(&first.keys));
        let mut receivers = Vec::new();
        let lead_job = {
            let mut map = self.chains.inner.lock().unwrap();
            let queue = map.entry(id.clone()).or_default();
            if queue.owner {
                let (tx, rx) = oneshot::channel();
                queue.waiters.push_back(Waiter {
                    job: first,
                    reply: tx,
                });
                receivers.push(rx);
                for job in jobs {
                    let (tx, rx) = oneshot::channel();
                    queue.waiters.push_back(Waiter { job, reply: tx });
                    receivers.push(rx);
                }
                None
            } else {
                queue.owner = true;
                for job in jobs {
                    let (tx, rx) = oneshot::channel();
                    queue.waiters.push_back(Waiter { job, reply: tx });
                    receivers.push(rx);
                }
                Some(first)
            }
        };
        let mut results = Vec::new();
        if let Some(job) = lead_job {
            results.push(self.lead(id.clone(), job).await);
        }
        results.extend(self.collect_wakes(&id, receivers).await);
        results
    }

    async fn collect_wakes(
        &self,
        id: &ChainId,
        receivers: Vec<oneshot::Receiver<Wake>>,
    ) -> Vec<Result<RespValue>> {
        let mut results = Vec::with_capacity(receivers.len());
        for rx in receivers {
            match rx.await {
                Ok(Wake::Reply(result)) => results.push(result),
                Ok(Wake::Lead(job)) => results.push(self.lead(id.clone(), job).await),
                Err(_) => results.push(Err(Error::Command("ERR script chain closed".into()))),
            }
        }
        results
    }

    async fn lead(&self, id: ChainId, job: ScriptJob) -> Result<RespValue> {
        let _owner = OwnerGuard {
            chains: &self.chains,
            id: id.clone(),
            armed: true,
        };
        let keys: Vec<Vec<u8>> = job.keys.iter().map(|key| key.to_vec()).collect();
        let key_refs: Vec<&[u8]> = keys.iter().map(|key| key.as_slice()).collect();
        let guard = self
            .key_lock
            .lock_keys_sorted_with_timeout(&key_refs, super::DEFAULT_SCRIPT_LOCK_TIMEOUT)
            .await?;
        let result = self.execute_burst(&id, job).await;
        drop(guard);
        result
    }

    async fn execute_burst(&self, id: &ChainId, job: ScriptJob) -> Result<RespValue> {
        let db = job.db;
        let overlay = std::sync::Arc::new(Mutex::new(BurstOverlay::new()));
        let mut release_ttl = 0u64;
        let mut outcomes = Vec::new();

        match self
            .run_script(&job, Some(std::sync::Arc::clone(&overlay)))
            .await
        {
            Ok(run) => {
                release_ttl += run.release_ttl;
                {
                    let mut guard = overlay.lock().unwrap();
                    guard.apply_script(run.ops);
                }
                outcomes.push(Outcome {
                    reply: None,
                    result: Ok(run.resp),
                });
            }
            Err(err) => outcomes.push(Outcome {
                reply: None,
                result: Err(err),
            }),
        }

        tokio::task::yield_now().await;
        let mut stopped_for_cap = false;
        loop {
            let mut pending: VecDeque<Waiter> = self.drain_waiters(id).into();
            if pending.is_empty() {
                break;
            }
            while let Some(waiter) = pending.pop_front() {
                let full = {
                    let guard = overlay.lock().unwrap();
                    !guard.is_empty() && guard.len() >= SCRIPT_BURST_MAX_OPS
                };
                if full {
                    let mut rest = vec![waiter];
                    rest.extend(pending.drain(..));
                    self.requeue_front(id, rest);
                    stopped_for_cap = true;
                    break;
                }
                match self
                    .run_script(&waiter.job, Some(std::sync::Arc::clone(&overlay)))
                    .await
                {
                    Ok(run) => {
                        release_ttl += run.release_ttl;
                        {
                            let mut guard = overlay.lock().unwrap();
                            guard.apply_script(run.ops);
                        }
                        outcomes.push(Outcome {
                            reply: Some(waiter.reply),
                            result: Ok(run.resp),
                        });
                    }
                    Err(err) => outcomes.push(Outcome {
                        reply: Some(waiter.reply),
                        result: Err(err),
                    }),
                }
            }
            if stopped_for_cap {
                break;
            }
        }

        let batch = overlay.lock().unwrap().drain_batch();
        if !batch.is_empty() {
            let failed = match self.storage.apply_stored_batch(db, batch).await {
                Ok(()) => {
                    let mut release_err = None;
                    for _ in 0..release_ttl {
                        if let Err(err) = self.storage.release_seen_ttl(db).await {
                            release_err = Some(err);
                            break;
                        }
                    }
                    release_err
                }
                Err(err) => Some(err),
            };
            if let Some(err) = failed {
                for outcome in &mut outcomes {
                    outcome.result = Err(share_error(&err));
                }
            }
        }

        let mut leader = Err(Error::Command("ERR script chain closed".into()));
        for outcome in outcomes {
            if let Some(reply) = outcome.reply {
                let _ = reply.send(Wake::Reply(outcome.result));
            } else {
                leader = outcome.result;
            }
        }
        leader
    }

    fn drain_waiters(&self, id: &ChainId) -> Vec<Waiter> {
        let mut map = self.chains.inner.lock().unwrap();
        let Some(queue) = map.get_mut(id) else {
            return Vec::new();
        };
        queue.waiters.drain(..).collect()
    }

    fn requeue_front(&self, id: &ChainId, waiters: Vec<Waiter>) {
        let mut map = self.chains.inner.lock().unwrap();
        let queue = map.entry(id.clone()).or_default();
        for waiter in waiters.into_iter().rev() {
            queue.waiters.push_front(waiter);
        }
    }
}

fn handoff_or_clear(chains: &CommitChains, id: &ChainId) {
    loop {
        let next = {
            let mut map = chains.inner.lock().unwrap();
            let Some(queue) = map.get_mut(id) else {
                return;
            };
            queue.waiters.pop_front()
        };
        let Some(waiter) = next else {
            let mut map = chains.inner.lock().unwrap();
            let Some(queue) = map.get_mut(id) else {
                return;
            };
            if queue.waiters.is_empty() {
                map.remove(id);
            } else {
                continue;
            }
            return;
        };
        if waiter.reply.send(Wake::Lead(waiter.job)).is_ok() {
            return;
        }
    }
}

fn share_error(err: &Error) -> Error {
    match err {
        Error::Io(err) => Error::Io(std::io::Error::new(err.kind(), err.to_string())),
        Error::Protocol(text) => Error::Protocol(text.clone()),
        Error::Command(text) => Error::Command(text.clone()),
        Error::Storage(text) => Error::Storage(text.clone()),
        Error::Config(text) => Error::Config(text.clone()),
        #[cfg(feature = "cluster")]
        Error::Cluster(err) => Error::Command(err.to_string()),
    }
}

impl ScriptCommands {
    pub(super) async fn run_script(
        &self,
        job: &ScriptJob,
        overlay: Option<std::sync::Arc<Mutex<BurstOverlay>>>,
    ) -> Result<ScriptRun> {
        let access = super::execute::script_access(&job.keys);
        let txn = std::sync::Arc::new(tokio::sync::Mutex::new(match &overlay {
            Some(overlay) => {
                ScriptTransaction::with_overlay(job.db, Some(std::sync::Arc::clone(overlay)))
            }
            None => ScriptTransaction::new(job.db),
        }));
        let lua = self.lua_pool.checkout(self.cache.generation())?;
        lua.set_memory_limit(self.script_memory_limit)
            .map_err(|err| Error::Command(format!("ERR {err}")))?;

        let started = std::time::Instant::now();

        super::populate_keys_argv(&lua, &job.keys, &job.argv)?;
        if job.with_redis {
            super::install_redis_api(
                &lua,
                self.storage.clone(),
                std::sync::Arc::clone(&txn),
                access,
            )?;
        }

        let result =
            super::sandbox::run_chunk(&lua, job.sha.as_deref(), &job.script, self.script_timeout)
                .await?;
        let duration_us = started.elapsed().as_micros() as u64;
        tracing::info!(
          target: "cmd.lua",
          name = "cmd_lua_exec",
          duration_us,
          "script execution complete"
        );
        if let Some(metrics) = &self.metrics {
            metrics.on_lua_execution(duration_us);
        }

        let resp = super::convert::lua_to_resp(result)?;
        drop(lua);
        let txn = std::sync::Arc::try_unwrap(txn)
            .map_err(|_| Error::Command("ERR script still running".into()))?;
        let mut txn = txn.into_inner();
        if overlay.is_some() {
            let (ops, release_ttl) = txn.collect_ops(self.storage.as_ref()).await?;
            Ok(ScriptRun {
                resp,
                ops,
                release_ttl,
            })
        } else {
            txn.commit(self.storage.as_ref()).await?;
            Ok(ScriptRun {
                resp,
                ops: Vec::new(),
                release_ttl: 0,
            })
        }
    }
}

pub(super) struct ScriptRun {
    pub(super) resp: RespValue,
    pub(super) ops: Vec<(Vec<u8>, StoredBatchOp)>,
    pub(super) release_ttl: u64,
}
