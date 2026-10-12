//! 测试用内存 `StorageAdapter`: 有序存储 (`scan_prefix` 有序), 统计 `apply_writes` 调用次数.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use aikv::error::Result;
use aikv::storage::adapter::{AdapterWriteOp, StorageAdapter};
use async_trait::async_trait;

#[derive(Default)]
pub struct MapAdapter {
    data: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
    apply_calls: AtomicUsize,
}

impl MapAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_calls(&self) -> usize {
        self.apply_calls.load(Ordering::SeqCst)
    }

    pub fn keys_with_prefix(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.data
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl StorageAdapter for MapAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        Ok(self.data.lock().unwrap().get(&key).cloned())
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.data.lock().unwrap().insert(key, value);
        Ok(())
    }

    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<()> {
        self.apply_calls.fetch_add(1, Ordering::SeqCst);
        let mut data = self.data.lock().unwrap();
        for op in ops {
            match op {
                AdapterWriteOp::Put { key, value } => {
                    data.insert(key, value);
                }
                AdapterWriteOp::Delete { key } => {
                    data.remove(&key);
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().remove(&key).is_some())
    }

    async fn exists(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().contains_key(&key))
    }

    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        self.apply_writes(batch).await
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let doomed: Vec<Vec<u8>> = data.range(start..end).map(|(k, _)| k.clone()).collect();
        for key in doomed {
            data.remove(&key);
        }
        Ok(())
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.data.lock().unwrap().len())
    }

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.data.lock().unwrap().is_empty())
    }

    async fn clear(&self) -> Result<()> {
        self.data.lock().unwrap().clear();
        Ok(())
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }
}
