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
    pub(super) io: crate::storage::script_io::ScriptIoStats,
}

pub(super) struct ScriptDrive {
    pub(super) lua: mlua::Lua,
    pub(super) result: Result<ScriptBody>,
    pub(super) io: crate::storage::script_io::ScriptIoStats,
    pub(super) calls: super::call_stats::ScriptCallStats,
    pub(super) t_entered: Instant,
    pub(super) setup_us: u64,
    pub(super) call_wall_us: u64,
    pub(super) teardown_us: u64,
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
    pub(super) fn run(self) -> ScriptDrive {
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
    }
}

pub(super) fn script_runs_off_worker() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

fn drive_script_body(
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
    let t_entered = Instant::now();
    crate::storage::script_io::begin_script_io_sample();
    super::call_stats::begin_script_call_sample();
    super::phase_stats::begin_eval_span();
    let prepared = prepare_script(
        &lua, &storage, &in_flight, timeout, db, &sha1, &script, &keys, &argv,
    );
    let (value, t_lua_done, t_prep_done, batch_ops) = match prepared {
        Ok(parts) => parts,
        Err(err) => return failed_drive(lua, err, t_entered),
    };
    let pending = if local_commit {
        if let Err(err) = commit_local_batch(&storage, &in_flight, db, batch_ops, seg_id, slot_idx)
        {
            return failed_drive(lua, err, t_entered);
        }
        None
    } else {
        Some(batch_ops)
    };
    let io = crate::storage::script_io::take_script_io_stats();
    let calls = super::call_stats::take_script_call_stats();
    let (setup_us, call_wall_us, teardown_us) = super::phase_stats::take_eval_span();
    ScriptDrive {
        lua,
        result: Ok(ScriptBody {
            value,
            pending,
            t_lua_done,
            t_prep_done,
            t_local_done: Instant::now(),
            io,
        }),
        io,
        calls,
        t_entered,
        setup_us,
        call_wall_us,
        teardown_us,
    }
}

fn prepare_script(
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
    let (value, txn) = ScriptCommands::eval_lua_func(lua, timeout, &args, txn, declared)?;
    let t_lua_done = Instant::now();
    let batch_ops = txn.prepare_batch_ops_owned(storage.script_io())?;
    Ok((value, t_lua_done, Instant::now(), batch_ops))
}

fn failed_drive(lua: mlua::Lua, err: Error, t_entered: Instant) -> ScriptDrive {
    let io = crate::storage::script_io::take_script_io_stats();
    let calls = super::call_stats::take_script_call_stats();
    let (setup_us, call_wall_us, teardown_us) = super::phase_stats::take_eval_span();
    ScriptDrive {
        lua,
        result: Err(err),
        io,
        calls,
        t_entered,
        setup_us,
        call_wall_us,
        teardown_us,
    }
}

fn commit_local_batch(
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
    if let Err(err) = storage.script_io().write_batch(db, batch_ops) {
        rollback_inserted(in_flight, Some((seq, encoded)));
        return Err(err);
    }
    in_flight.remove_encoded(&encoded, seq);
    SEG_TRACKER.on_enqueued(slot_idx);
    SEG_TRACKER.on_committed(seg_id, slot_idx);
    Ok(())
}

pub(super) fn rollback_inserted(in_flight: &InFlightBuffer, inserted: Option<(u64, Vec<Vec<u8>>)>) {
    if let Some((seq, keys)) = inserted {
        in_flight.remove_encoded(&keys, seq);
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::rollback_inserted;
    use crate::command::script::in_flight::InFlightBuffer;
    use crate::storage::WriteOp;

    #[test]
    fn rollback_inserted_removes_only_that_batch() {
        let buffer = InFlightBuffer::new();
        let ops = vec![(b"k".to_vec(), WriteOp::Put(b"v".to_vec()))];
        let (seq, keys) = buffer.insert_batch(0, &ops);
        assert!(buffer.get(0, b"k").is_some());
        rollback_inserted(&buffer, Some((seq, keys)));
        assert!(buffer.get(0, b"k").is_none());
    }
}
