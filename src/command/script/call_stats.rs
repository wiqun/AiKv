//! 按命令族累计 `redis.call` 耗时. 平均值是微秒总和除以脚本条数.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ScriptCallStats {
    pub(super) prep_n: u64,
    pub(super) prep_us: u64,
    pub(super) string_n: u64,
    pub(super) string_us: u64,
    pub(super) hash_n: u64,
    pub(super) hash_us: u64,
    pub(super) list_n: u64,
    pub(super) list_us: u64,
    pub(super) set_n: u64,
    pub(super) set_us: u64,
    pub(super) zadd_n: u64,
    pub(super) zadd_us: u64,
    pub(super) zrev_n: u64,
    pub(super) zrev_us: u64,
    pub(super) zother_n: u64,
    pub(super) zother_us: u64,
    pub(super) json_n: u64,
    pub(super) json_us: u64,
    pub(super) other_n: u64,
    pub(super) other_us: u64,
}

impl ScriptCallStats {
    pub(super) fn accounted_us(self) -> u64 {
        self.prep_us
            + self.string_us
            + self.hash_us
            + self.list_us
            + self.set_us
            + self.zadd_us
            + self.zrev_us
            + self.zother_us
            + self.json_us
            + self.other_us
    }
}

#[derive(Clone, Copy)]
pub(super) enum ScriptCallOp {
    Prep,
    String,
    Hash,
    List,
    Set,
    Zadd,
    Zrev,
    Zother,
    Json,
    Other,
}

pub(super) struct CallTimer {
    op: ScriptCallOp,
    started: Instant,
    live: bool,
}

impl CallTimer {
    pub(super) fn start(op: ScriptCallOp) -> Self {
        Self {
            op,
            started: Instant::now(),
            live: true,
        }
    }

    pub(super) fn finish(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        add_stat(self.op, self.started.elapsed().as_micros() as u64);
    }
}

impl Drop for CallTimer {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(super) fn script_call_family(cmd: &str) -> ScriptCallOp {
    if cmd.starts_with("JSON.") {
        return ScriptCallOp::Json;
    }
    match cmd {
        "GET" | "SET" | "DEL" | "EXISTS" | "INCR" | "DECR" | "INCRBY" | "DECRBY"
        | "INCRBYFLOAT" | "APPEND" | "STRLEN" => ScriptCallOp::String,
        "HGET" | "HSET" | "HMSET" | "HSETNX" | "HDEL" | "HGETALL" | "HMGET" | "HINCRBY"
        | "HEXISTS" | "HLEN" => ScriptCallOp::Hash,
        "LPUSH" | "RPUSH" | "LPOP" | "RPOP" | "LLEN" | "LRANGE" | "LINDEX" => ScriptCallOp::List,
        "SADD" | "SREM" | "SMEMBERS" | "SISMEMBER" | "SCARD" | "SSCAN" => ScriptCallOp::Set,
        "ZADD" => ScriptCallOp::Zadd,
        "ZREVRANGEBYSCORE" => ScriptCallOp::Zrev,
        "ZREM" | "ZREMRANGEBYLEX" | "ZREMRANGEBYRANK" | "ZREMRANGEBYSCORE" | "ZSCORE" | "ZRANK"
        | "ZRANGE" | "ZREVRANGE" | "ZRANGEBYSCORE" | "ZCARD" => ScriptCallOp::Zother,
        _ => ScriptCallOp::Other,
    }
}

thread_local! {
    static STATS: Cell<ScriptCallStats> = const { Cell::new(ScriptCallStats {
        prep_n: 0,
        prep_us: 0,
        string_n: 0,
        string_us: 0,
        hash_n: 0,
        hash_us: 0,
        list_n: 0,
        list_us: 0,
        set_n: 0,
        set_us: 0,
        zadd_n: 0,
        zadd_us: 0,
        zrev_n: 0,
        zrev_us: 0,
        zother_n: 0,
        zother_us: 0,
        json_n: 0,
        json_us: 0,
        other_n: 0,
        other_us: 0,
    }) };
}

fn bump(n: &mut u64, total: &mut u64, us: u64) {
    *n += 1;
    *total += us;
}

fn add_stat(op: ScriptCallOp, us: u64) {
    STATS.with(|cell| {
        let mut stats = cell.get();
        match op {
            ScriptCallOp::Prep => bump(&mut stats.prep_n, &mut stats.prep_us, us),
            ScriptCallOp::String => bump(&mut stats.string_n, &mut stats.string_us, us),
            ScriptCallOp::Hash => bump(&mut stats.hash_n, &mut stats.hash_us, us),
            ScriptCallOp::List => bump(&mut stats.list_n, &mut stats.list_us, us),
            ScriptCallOp::Set => bump(&mut stats.set_n, &mut stats.set_us, us),
            ScriptCallOp::Zadd => bump(&mut stats.zadd_n, &mut stats.zadd_us, us),
            ScriptCallOp::Zrev => bump(&mut stats.zrev_n, &mut stats.zrev_us, us),
            ScriptCallOp::Zother => bump(&mut stats.zother_n, &mut stats.zother_us, us),
            ScriptCallOp::Json => bump(&mut stats.json_n, &mut stats.json_us, us),
            ScriptCallOp::Other => bump(&mut stats.other_n, &mut stats.other_us, us),
        }
        cell.set(stats);
    });
}

pub(super) fn take_script_call_stats() -> ScriptCallStats {
    STATS.with(|cell| cell.replace(ScriptCallStats::default()))
}

pub(super) fn begin_script_call_sample() {
    let _ = take_script_call_stats();
}

struct ScriptCallSampler {
    samples: AtomicU64,
    prep_us: AtomicU64,
    string_us: AtomicU64,
    hash_us: AtomicU64,
    list_us: AtomicU64,
    set_us: AtomicU64,
    zadd_us: AtomicU64,
    zrev_us: AtomicU64,
    zother_us: AtomicU64,
    json_us: AtomicU64,
    other_us: AtomicU64,
}

impl ScriptCallSampler {
    const fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            prep_us: AtomicU64::new(0),
            string_us: AtomicU64::new(0),
            hash_us: AtomicU64::new(0),
            list_us: AtomicU64::new(0),
            set_us: AtomicU64::new(0),
            zadd_us: AtomicU64::new(0),
            zrev_us: AtomicU64::new(0),
            zother_us: AtomicU64::new(0),
            json_us: AtomicU64::new(0),
            other_us: AtomicU64::new(0),
        }
    }

    fn record(&self, stats: ScriptCallStats) {
        self.prep_us.fetch_add(stats.prep_us, Ordering::Relaxed);
        self.string_us.fetch_add(stats.string_us, Ordering::Relaxed);
        self.hash_us.fetch_add(stats.hash_us, Ordering::Relaxed);
        self.list_us.fetch_add(stats.list_us, Ordering::Relaxed);
        self.set_us.fetch_add(stats.set_us, Ordering::Relaxed);
        self.zadd_us.fetch_add(stats.zadd_us, Ordering::Relaxed);
        self.zrev_us.fetch_add(stats.zrev_us, Ordering::Relaxed);
        self.zother_us.fetch_add(stats.zother_us, Ordering::Relaxed);
        self.json_us.fetch_add(stats.json_us, Ordering::Relaxed);
        self.other_us.fetch_add(stats.other_us, Ordering::Relaxed);
        let n = self.samples.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(1000) {
            self.log(n);
        }
    }

    fn log(&self, n: u64) {
        tracing::info!(
            target: "perf",
            "[SCRIPT_CALL_PERF] samples={} avg_prep_us={} avg_string_us={} avg_hash_us={} avg_list_us={} avg_set_us={} avg_zadd_us={} avg_zrev_us={} avg_zother_us={} avg_json_us={} avg_other_us={}",
            n,
            self.prep_us.load(Ordering::Relaxed) / n,
            self.string_us.load(Ordering::Relaxed) / n,
            self.hash_us.load(Ordering::Relaxed) / n,
            self.list_us.load(Ordering::Relaxed) / n,
            self.set_us.load(Ordering::Relaxed) / n,
            self.zadd_us.load(Ordering::Relaxed) / n,
            self.zrev_us.load(Ordering::Relaxed) / n,
            self.zother_us.load(Ordering::Relaxed) / n,
            self.json_us.load(Ordering::Relaxed) / n,
            self.other_us.load(Ordering::Relaxed) / n
        );
    }
}

static CALL_SAMPLER: ScriptCallSampler = ScriptCallSampler::new();

pub(super) fn record_script_calls(stats: ScriptCallStats) {
    CALL_SAMPLER.record(stats);
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::take_script_call_stats;
    use crate::command::script::execute::{redis_call, DeclaredKeys};
    use crate::command::script::transaction::ScriptTransaction;
    use crate::storage::{KvStorage, MemoryEngine};

    fn call_args(lua: &mlua::Lua, parts: &[&str]) -> mlua::MultiValue {
        parts
            .iter()
            .map(|part| mlua::Value::String(lua.create_string(*part).unwrap()))
            .collect()
    }

    #[test]
    fn script_call_stats_split_string_and_zset() {
        let _ = take_script_call_stats();
        let lua = mlua::Lua::new();
        let engine = MemoryEngine::new(4);
        let io = engine.script_io();
        let mut txn = ScriptTransaction::new(0);
        let keys = [Bytes::from_static(b"k"), Bytes::from_static(b"z")];
        let declared = DeclaredKeys::new(&keys);
        redis_call(
            &lua,
            io,
            &mut txn,
            &declared,
            call_args(&lua, &["SET", "k", "v"]),
            true,
        )
        .unwrap();
        redis_call(
            &lua,
            io,
            &mut txn,
            &declared,
            call_args(&lua, &["GET", "k"]),
            true,
        )
        .unwrap();
        redis_call(
            &lua,
            io,
            &mut txn,
            &declared,
            call_args(&lua, &["ZADD", "z", "1", "m"]),
            true,
        )
        .unwrap();
        redis_call(
            &lua,
            io,
            &mut txn,
            &declared,
            call_args(&lua, &["ZREVRANGEBYSCORE", "z", "+inf", "-inf"]),
            true,
        )
        .unwrap();
        redis_call(
            &lua,
            io,
            &mut txn,
            &declared,
            call_args(&lua, &["ZCARD", "z"]),
            true,
        )
        .unwrap();
        let failed = redis_call(
            &lua,
            io,
            &mut txn,
            &declared,
            call_args(&lua, &["HGET"]),
            true,
        );
        assert!(failed.is_err());
        let stats = take_script_call_stats();
        assert!(stats.string_n >= 2, "{stats:?}");
        assert_eq!(stats.zadd_n, 1, "{stats:?}");
        assert_eq!(stats.zrev_n, 1, "{stats:?}");
        assert_eq!(stats.zother_n, 1, "{stats:?}");
        assert_eq!(stats.hash_n, 1, "{stats:?}");
        assert!(stats.prep_n >= 6, "{stats:?}");
    }
}
