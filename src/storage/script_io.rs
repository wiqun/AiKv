//! 脚本线程上的同步存储接口. 普通命令仍走 `KvStorage` 的异步方法.

use std::cell::Cell;
use std::time::Instant;

use crate::error::{Error, Result};
use crate::storage::types::{StorageEngineKind, StoredValue, WriteOp};

pub trait ScriptIo: Send + Sync {
    fn engine_kind(&self) -> StorageEngineKind;
    fn get_typed(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>>;
    fn ttl(&self, db: usize, key: &[u8]) -> Result<Option<i64>>;
    fn write_batch(&self, db: usize, ops: Vec<(Vec<u8>, WriteOp)>) -> Result<()>;
    fn raw_prefix_last(&self, prefix: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>>;
    fn raw_range_last(&self, start: Vec<u8>, end: Vec<u8>) -> Result<Option<(Vec<u8>, Vec<u8>)>>;
    fn raw_subkey_get(&self, db: usize, encoded_key: Vec<u8>) -> Result<Option<Vec<u8>>>;
    fn raw_subkey_for_each(
        &self,
        db: usize,
        prefix: Vec<u8>,
        f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<()> + Send>,
    ) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptIoStats {
    pub get_n: u64,
    pub get_us: u64,
    pub range_n: u64,
    pub range_us: u64,
    pub prefix_n: u64,
    pub prefix_us: u64,
    pub scan_n: u64,
    pub scan_us: u64,
    pub write_n: u64,
    pub write_us: u64,
}

#[derive(Clone, Copy)]
pub enum ScriptIoOp {
    Get,
    Range,
    Prefix,
    Scan,
    Write,
}

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static STATS: Cell<ScriptIoStats> = const { Cell::new(ScriptIoStats {
        get_n: 0,
        get_us: 0,
        range_n: 0,
        range_us: 0,
        prefix_n: 0,
        prefix_us: 0,
        scan_n: 0,
        scan_us: 0,
        write_n: 0,
        write_us: 0,
    }) };
}

pub fn time_script_io<T>(op: ScriptIoOp, f: impl FnOnce() -> T) -> T {
    let nested = DEPTH.with(|depth| {
        let current = depth.get();
        depth.set(current + 1);
        current > 0
    });
    let started = Instant::now();
    let out = f();
    let us = started.elapsed().as_micros() as u64;
    DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    if !nested {
        STATS.with(|cell| {
            let mut stats = cell.get();
            match op {
                ScriptIoOp::Get => {
                    stats.get_n += 1;
                    stats.get_us += us;
                }
                ScriptIoOp::Range => {
                    stats.range_n += 1;
                    stats.range_us += us;
                }
                ScriptIoOp::Prefix => {
                    stats.prefix_n += 1;
                    stats.prefix_us += us;
                }
                ScriptIoOp::Scan => {
                    stats.scan_n += 1;
                    stats.scan_us += us;
                }
                ScriptIoOp::Write => {
                    stats.write_n += 1;
                    stats.write_us += us;
                }
            }
            cell.set(stats);
        });
    }
    out
}

pub fn take_script_io_stats() -> ScriptIoStats {
    STATS.with(|cell| cell.replace(ScriptIoStats::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::storage::{KvStorage, MemoryEngine};

    #[test]
    fn memory_script_io_roundtrip() {
        let engine = MemoryEngine::new(4);
        let io = engine.script_io();
        io.write_batch(0, vec![(b"k".to_vec(), WriteOp::Put(b"v".to_vec()))])
            .unwrap();
        let stored = io.get_typed(0, b"k").unwrap().expect("value");
        match stored.value {
            crate::storage::ValueType::String(bytes) => assert_eq!(bytes, b"v"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn memory_script_io_raw_range_unsupported() {
        let engine = MemoryEngine::new(4);
        let err = engine.script_io().raw_range_last(vec![1], vec![2]);
        match err {
            Err(Error::Storage(text)) => {
                assert!(text.contains("raw subkey access not supported"), "{text}")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn aidb_script_delete_removes_zset_subkeys() {
        let dir = tempfile::tempdir().unwrap();
        let engine = crate::storage::AiDbEngine::open_with_options(
            dir.path(),
            crate::storage::testing_db_options(),
        )
        .unwrap();
        let adapter = crate::storage::KvStorageAdapter::new(engine);
        let io = adapter.script_io();
        let header = crate::storage::zset_subkey::header(1, None);
        io.write_batch(0, vec![(b"z".to_vec(), WriteOp::PutStored(header))])
            .unwrap();
        let encoded = crate::storage::AiDbEngine::encode_key(0, b"z");
        let score_key = crate::storage::zset_subkey::score_key(&encoded, 1.0, b"m");
        io.write_batch(
            0,
            vec![(
                score_key.clone(),
                WriteOp::PutEngine {
                    value: crate::storage::zset_subkey::encode_score(1.0),
                },
            )],
        )
        .unwrap();
        io.write_batch(0, vec![(b"z".to_vec(), WriteOp::Delete)])
            .unwrap();
        assert!(io.raw_subkey_get(0, score_key).unwrap().is_none());
    }

    #[test]
    fn aidb_script_get_deletes_expired_key() {
        let dir = tempfile::tempdir().unwrap();
        let engine = crate::storage::AiDbEngine::open_with_options(
            dir.path(),
            crate::storage::testing_db_options(),
        )
        .unwrap();
        let adapter = crate::storage::KvStorageAdapter::new(engine);
        let io = adapter.script_io();
        let mut stored = StoredValue::string(b"old".to_vec());
        stored.expires_at = Some(1);
        io.write_batch(0, vec![(b"k".to_vec(), WriteOp::PutStored(stored))])
            .unwrap();
        assert!(io.get_typed(0, b"k").unwrap().is_none());
        let encoded = crate::storage::AiDbEngine::encode_key(0, b"k");
        assert!(io.raw_subkey_get(0, encoded).unwrap().is_none());
    }

    #[test]
    fn script_io_stats_counts_get_and_write() {
        let _ = take_script_io_stats();
        let engine = MemoryEngine::new(4);
        let io = engine.script_io();
        io.write_batch(0, vec![(b"k".to_vec(), WriteOp::Put(b"v".to_vec()))])
            .unwrap();
        io.get_typed(0, b"k").unwrap();
        let stats = take_script_io_stats();
        assert!(stats.write_n >= 1, "{stats:?}");
        assert!(stats.get_n >= 1, "{stats:?}");
    }
}
