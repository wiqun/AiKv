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
}
