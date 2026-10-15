//! 把整段 Lua 和单机提交放在同一条阻塞线程上执行.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use mlua::Value as LuaValue;
use tokio::sync::Mutex;

use super::execute::DeclaredKeys;
use super::in_flight::InFlightBuffer;
use super::transaction::ScriptTransaction;
use super::{ScriptArgs, ScriptCommands, SEG_TRACKER};
use crate::error::{Error, Result};
use crate::storage::{KvStorage, WriteOp};

pub(super) struct ScriptBody {
    pub(super) value: LuaValue,
    pub(super) pending: Option<Vec<(Vec<u8>, WriteOp)>>,
    pub(super) t_lua_done: Instant,
    pub(super) t_prep_done: Instant,
    pub(super) t_local_done: Instant,
}

pub(super) struct ScriptDrive {
    pub(super) lua: mlua::Lua,
    pub(super) result: Result<ScriptBody>,
}

pub(super) struct ScriptJob {
    pub(super) lua: mlua::Lua,
    pub(super) storage: Arc<dyn KvStorage>,
    pub(super) in_flight: Arc<InFlightBuffer>,
    pub(super) timeout: Duration,
    pub(super) db: usize,
    pub(super) sha1: String,
    pub(super) script: String,
    pub(super) keys: Vec<Bytes>,
    pub(super) argv: Vec<Bytes>,
    pub(super) local_commit: bool,
    pub(super) seg_id: usize,
    pub(super) slot_idx: usize,
}

impl ScriptJob {
    pub(super) async fn run(self) -> ScriptDrive {
        drive_script_body(
            self.lua,
            self.storage,
            self.in_flight,
            self.timeout,
            self.db,
            self.sha1,
            self.script,
            self.keys,
            self.argv,
            self.local_commit,
            self.seg_id,
            self.slot_idx,
        )
        .await
    }
}

pub(super) async fn run_on_blocking_thread(job: ScriptJob) -> Result<ScriptDrive> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _direct = crate::storage::direct::DirectStorageIo::enter();
        handle.block_on(job.run())
    })
    .await
    .map_err(|err| Error::Command(format!("ERR script thread failed: {err}")))
}

pub(super) fn script_runs_off_worker() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

async fn drive_script_body(
    lua: mlua::Lua,
    storage: Arc<dyn KvStorage>,
    in_flight: Arc<InFlightBuffer>,
    timeout: Duration,
    db: usize,
    sha1: String,
    script: String,
    keys: Vec<Bytes>,
    argv: Vec<Bytes>,
    local_commit: bool,
    seg_id: usize,
    slot_idx: usize,
) -> ScriptDrive {
    let prepared = prepare_script(
        &lua, &storage, &in_flight, timeout, db, &sha1, &script, &keys, &argv,
    )
    .await;
    let (value, t_lua_done, t_prep_done, batch_ops) = match prepared {
        Ok(parts) => parts,
        Err(err) => return failed_drive(lua, err),
    };
    let pending = if local_commit {
        if let Err(err) =
            commit_local_batch(&storage, &in_flight, db, batch_ops, seg_id, slot_idx).await
        {
            return failed_drive(lua, err);
        }
        None
    } else {
        Some(batch_ops)
    };
    ScriptDrive {
        lua,
        result: Ok(ScriptBody {
            value,
            pending,
            t_lua_done,
            t_prep_done,
            t_local_done: Instant::now(),
        }),
    }
}

async fn prepare_script(
    lua: &mlua::Lua,
    storage: &Arc<dyn KvStorage>,
    in_flight: &Arc<InFlightBuffer>,
    timeout: Duration,
    db: usize,
    sha1: &str,
    script: &str,
    keys: &[Bytes],
    argv: &[Bytes],
) -> Result<(LuaValue, Instant, Instant, Vec<(Vec<u8>, WriteOp)>)> {
    let declared = Arc::new(DeclaredKeys::new(keys));
    let txn = ScriptTransaction::with_in_flight(db, Some(Arc::clone(in_flight)));
    let args = ScriptArgs {
        sha1,
        script,
        keys,
        argv,
    };
    let (value, txn) = ScriptCommands::eval_lua_func(lua, timeout, &args, txn, declared).await?;
    let t_lua_done = Instant::now();
    let batch_ops = txn.prepare_batch_ops_owned(storage.script_io())?;
    Ok((value, t_lua_done, Instant::now(), batch_ops))
}

fn failed_drive(lua: mlua::Lua, err: Error) -> ScriptDrive {
    ScriptDrive {
        lua,
        result: Err(err),
    }
}

async fn commit_local_batch(
    storage: &Arc<dyn KvStorage>,
    in_flight: &InFlightBuffer,
    db: usize,
    batch_ops: Vec<(Vec<u8>, WriteOp)>,
    seg_id: usize,
    slot_idx: usize,
) -> Result<()> {
    if batch_ops.is_empty() {
        SEG_TRACKER.on_enqueued(slot_idx);
        SEG_TRACKER.on_committed(seg_id, slot_idx);
        return Ok(());
    }
    let (seq, encoded) = in_flight.insert_batch(db, &batch_ops);
    let handle = storage.enqueue_batch(db, batch_ops).await?;
    SEG_TRACKER.on_enqueued(slot_idx);
    let write_res = handle.wait().await;
    in_flight.remove_encoded(&encoded, seq);
    SEG_TRACKER.on_committed(seg_id, slot_idx);
    write_res
}
