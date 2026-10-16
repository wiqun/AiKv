//! 懒过期 mock 回归: 清理 delete 失败或禁止时, 过期读仍须 Ok(None).
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aikv::error::{Error, Result};
use aikv::storage::adapter::{AdapterWriteOp, KvStorageAdapter, StorageAdapter};
use aikv::storage::types::{KvStorage, StoredValue};

struct MockAdapter {
    data: Mutex<std::collections::HashMap<Vec<u8>, Vec<u8>>>,
    delete_calls: AtomicUsize,
    allow_lazy_expire: AtomicBool,
    fail_delete: AtomicBool,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            data: Mutex::new(std::collections::HashMap::new()),
            delete_calls: AtomicUsize::new(0),
            allow_lazy_expire: AtomicBool::new(true),
            fail_delete: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl StorageAdapter for MockAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        Ok(self.data.lock().unwrap().get(&key).cloned())
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.data.lock().unwrap().insert(key, value);
        Ok(())
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
        self.delete_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(Error::Storage("不是 Leader".into()));
        }
        Ok(self.data.lock().unwrap().remove(&key).is_some())
    }

    async fn exists(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().contains_key(&key))
    }

    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        for op in batch {
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

    fn allow_lazy_expire_delete(&self, _key: &[u8]) -> bool {
        self.allow_lazy_expire.load(Ordering::SeqCst)
    }
}

fn expired_value_bytes() -> Vec<u8> {
    let stored = StoredValue {
        value: aikv::storage::types::ValueType::String(b"v".to_vec()),
        expires_at: Some(1), // 1ms since epoch: always in the past.
    };
    postcard::to_allocvec(&stored).unwrap()
}

#[tokio::test]
async fn expired_key_skips_delete_when_not_allowed() {
    let mock = Arc::new(MockAdapter::new());
    mock.allow_lazy_expire.store(false, Ordering::SeqCst);
    let encoded_key = aikv::storage::AiDbEngine::encode_key(0, b"k");
    mock.set(encoded_key.clone(), expired_value_bytes())
        .await
        .unwrap();

    let kv = KvStorageAdapter::new(mock.clone());
    let result = kv.get(0, b"k").await;

    assert!(matches!(result, Ok(None)), "got {:?}", result);
    assert_eq!(
        mock.delete_calls.load(Ordering::SeqCst),
        0,
        "delete must not be attempted when allow_lazy_expire_delete() is false"
    );
}

#[tokio::test]
async fn expired_key_read_never_errors_even_if_delete_fails() {
    let mock = Arc::new(MockAdapter::new());
    mock.fail_delete.store(true, Ordering::SeqCst);
    let encoded_key = aikv::storage::AiDbEngine::encode_key(0, b"k");
    mock.set(encoded_key.clone(), expired_value_bytes())
        .await
        .unwrap();

    let kv = KvStorageAdapter::new(mock.clone());
    let result = kv.get(0, b"k").await;

    assert!(
        matches!(result, Ok(None)),
        "expired-key read must return Ok(None) even if the cleanup delete errors, got {:?}",
        result
    );
    assert_eq!(mock.delete_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_key_deleted_when_allowed() {
    let mock = Arc::new(MockAdapter::new());
    let encoded_key = aikv::storage::AiDbEngine::encode_key(0, b"k");
    mock.set(encoded_key.clone(), expired_value_bytes())
        .await
        .unwrap();

    let kv = KvStorageAdapter::new(mock.clone());
    let result = kv.get(0, b"k").await;

    assert!(matches!(result, Ok(None)), "got {:?}", result);
    assert_eq!(
        mock.delete_calls.load(Ordering::SeqCst),
        1,
        "leader/local-engine path should still perform the cleanup delete"
    );
}
