//! 脚本线程上的同步存储接口. 普通命令仍走 `KvStorage` 的异步方法.

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
}
