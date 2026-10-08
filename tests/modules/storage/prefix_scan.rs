//! KEYS / SCAN 只扫模式能收成的字节区间，不把同库里别的前缀也读出来。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aikv::error::Result;
use aikv::storage::adapter::{AdapterWriteOp, KvStorageAdapter, StorageAdapter};
use aikv::storage::types::KvStorage;

type RangeCall = (Vec<u8>, Option<Vec<u8>>);

struct RecordingAdapter {
    data: Mutex<std::collections::BTreeMap<Vec<u8>, Vec<u8>>>,
    prefixes: Mutex<Vec<Vec<u8>>>,
    ranges: Mutex<Vec<RangeCall>>,
    range_visits: Mutex<Vec<usize>>,
}

impl RecordingAdapter {
    fn new() -> Self {
        Self {
            data: Mutex::new(std::collections::BTreeMap::new()),
            prefixes: Mutex::new(Vec::new()),
            ranges: Mutex::new(Vec::new()),
            range_visits: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl StorageAdapter for RecordingAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        Ok(self.data.lock().unwrap().get(&key).cloned())
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.data.lock().unwrap().insert(key, value);
        Ok(())
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
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
        self.prefixes.lock().unwrap().push(prefix.to_vec());
        Ok(self
            .data
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    async fn visit_range(
        &self,
        start: Vec<u8>,
        end: Option<Vec<u8>>,
        mut f: Box<dyn FnMut(Vec<u8>, Vec<u8>) -> Result<bool> + Send>,
    ) -> Result<()> {
        self.ranges
            .lock()
            .unwrap()
            .push((start.clone(), end.clone()));
        let rows: Vec<(Vec<u8>, Vec<u8>)> = self
            .data
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| {
                key.as_slice() >= start.as_slice()
                    && end
                        .as_ref()
                        .is_none_or(|end| key.as_slice() < end.as_slice())
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut visits = 0usize;
        for (key, value) in rows {
            visits += 1;
            if !f(key, value)? {
                break;
            }
        }
        self.range_visits.lock().unwrap().push(visits);
        Ok(())
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let doomed: Vec<Vec<u8>> = data
            .keys()
            .filter(|k| k.as_slice() >= start.as_slice() && k.as_slice() < end.as_slice())
            .cloned()
            .collect();
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
}

#[tokio::test]
async fn keys_prefix_glob_scans_that_prefix_only() {
    let raw = Arc::new(RecordingAdapter::new());
    let storage = KvStorageAdapter::new(raw.clone());
    storage.set(0, b"key:1", b"v").await.unwrap();
    storage.set(0, b"KvDoc:a", b"v").await.unwrap();

    let matched = storage.keys(0, b"KvDoc:*").await.unwrap();
    assert_eq!(matched, vec![b"KvDoc:a".to_vec()]);
    assert_eq!(
        raw.prefixes.lock().unwrap().as_slice(),
        &[b"0:KvDoc:".to_vec()]
    );
}

#[tokio::test]
async fn keys_unbounded_glob_still_scans_the_whole_db() {
    let raw = Arc::new(RecordingAdapter::new());
    let storage = KvStorageAdapter::new(raw.clone());
    storage.set(0, b"key:1", b"v").await.unwrap();
    storage.set(0, b"KvDoc:a", b"v").await.unwrap();

    let mut matched = storage.keys(0, b"*:*").await.unwrap();
    matched.sort();
    assert_eq!(matched, vec![b"KvDoc:a".to_vec(), b"key:1".to_vec()]);
    assert_eq!(raw.prefixes.lock().unwrap().as_slice(), &[b"0:".to_vec()]);
}

#[tokio::test]
async fn scan_prefix_resumes_from_the_next_key() {
    let raw = Arc::new(RecordingAdapter::new());
    let storage = KvStorageAdapter::new(raw.clone());
    for key in [b"p:a".as_slice(), b"p:b", b"p:c", b"other"] {
        storage.set(0, key, b"v").await.unwrap();
    }

    let first = storage.scan(0, 0, b"p*", 1).await.unwrap();
    assert_eq!(first.keys, vec![b"p:a".to_vec()]);
    assert_ne!(first.cursor, 0);

    let second = storage.scan(0, first.cursor, b"p*", 1).await.unwrap();
    assert_eq!(second.keys, vec![b"p:b".to_vec()]);

    let ranges = raw.ranges.lock().unwrap().clone();
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0].0, b"0:p");
    assert_eq!(ranges[0].1.as_deref(), Some(b"0:q".as_slice()));
    assert_eq!(ranges[1].0, b"0:p:b");
    assert_eq!(ranges[1].1.as_deref(), Some(b"0:q".as_slice()));
    assert_eq!(raw.range_visits.lock().unwrap().as_slice(), &[2, 2]);
}
