//! Issue #83: SET 须一次 put_many(用户 key + watch meta), 不得串行两次 set.
//! @component aikv-storage

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aikv::error::Result;
use aikv::storage::adapter::{AdapterWriteOp, KvStorageAdapter, StorageAdapter};
use aikv::storage::types::KvStorage;
use aikv::storage::watch_version::is_watch_meta_user_key;
use aikv::storage::AiDbEngine;

struct CountingAdapter {
    data: Mutex<std::collections::HashMap<Vec<u8>, Vec<u8>>>,
    set_calls: AtomicUsize,
    put_many_calls: AtomicUsize,
    last_put_many: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl CountingAdapter {
    fn new() -> Self {
        Self {
            data: Mutex::new(std::collections::HashMap::new()),
            set_calls: AtomicUsize::new(0),
            put_many_calls: AtomicUsize::new(0),
            last_put_many: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl StorageAdapter for CountingAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        Ok(self.data.lock().unwrap().get(&key).cloned())
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        self.data.lock().unwrap().insert(key, value);
        Ok(())
    }

    async fn put_many(&self, ops: Vec<(Vec<u8>, Vec<u8>)>) -> Result<()> {
        self.put_many_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_put_many.lock().unwrap() = ops.clone();
        for (key, value) in ops {
            self.data.lock().unwrap().insert(key, value);
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
        for op in batch {
            match op {
                AdapterWriteOp::Put { key, value } => self.set(key, value).await?,
                AdapterWriteOp::Delete { key } => {
                    self.delete(key).await?;
                }
            }
        }
        Ok(())
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let to_remove: Vec<Vec<u8>> = data
            .keys()
            .filter(|k| k.as_slice() >= start.as_slice() && k.as_slice() < end.as_slice())
            .cloned()
            .collect();
        for k in to_remove {
            data.remove(&k);
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
}

/// 压测热路径无人 WATCH 时不应为每个 SET 多写一笔 watch meta.
#[tokio::test]
async fn issue_83_set_without_watch_skips_meta_put() {
    let mock = Arc::new(CountingAdapter::new());
    let kv = KvStorageAdapter::new(mock.clone());
    kv.set(0, b"k", b"v").await.unwrap();

    assert_eq!(mock.put_many_calls.load(Ordering::SeqCst), 1);
    assert_eq!(mock.set_calls.load(Ordering::SeqCst), 0);
    let ops = mock.last_put_many.lock().unwrap().clone();
    assert_eq!(
        ops.len(),
        1,
        "SET without WATCH must not write watch meta, got {} ops",
        ops.len()
    );
    assert_eq!(ops[0].0, AiDbEngine::encode_key(0, b"k"));
}

/// Issue #83: 二次串行 set_raw 导致集群 SET 腰斩; SET 必须一次 put_many 写入用户值与 watch meta.
#[tokio::test]
async fn issue_83_set_puts_user_and_watch_meta_in_one_put_many() {
    let mock = Arc::new(CountingAdapter::new());
    let kv = KvStorageAdapter::new(mock.clone());
    kv.watch_registry().watch(0, b"k");
    kv.set(0, b"k", b"v").await.unwrap();

    assert_eq!(
        mock.put_many_calls.load(Ordering::SeqCst),
        1,
        "SET must submit user key and watch meta in one put_many, not two serial set() calls"
    );
    assert_eq!(
        mock.set_calls.load(Ordering::SeqCst),
        0,
        "SET hot path must not call StorageAdapter::set"
    );

    let ops = mock.last_put_many.lock().unwrap().clone();
    assert_eq!(ops.len(), 2, "user Put + watch meta Put");
    assert_eq!(ops[0].0, AiDbEngine::encode_key(0, b"k"));
    let meta_user = AiDbEngine::decode_key(&ops[1].0)
        .map(|(_, k)| k)
        .expect("watch meta physical key must decode");
    assert!(
        is_watch_meta_user_key(&meta_user),
        "second op must be watch meta, got {:?}",
        meta_user
    );
}
