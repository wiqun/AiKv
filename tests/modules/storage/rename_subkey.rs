//! 大 Hash 的 RENAME 必须带走 subkey。含 subkey 分隔符的父 key 不能写入。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aikv::command::CommandRouter;
use aikv::error::Result;
use aikv::protocol::RespValue;
use aikv::storage::adapter::{AdapterWriteOp, KvStorageAdapter, StorageAdapter};
use aikv::storage::types::KvStorage;
use async_trait::async_trait;
use bytes::Bytes;

struct MapAdapter {
    data: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
}

impl MapAdapter {
    fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }

    fn contains_separator(&self) -> bool {
        self.data
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.contains(&0x01))
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
        let keys: Vec<Vec<u8>> = data
            .keys()
            .filter(|key| key.as_slice() >= start.as_slice() && key.as_slice() < end.as_slice())
            .cloned()
            .collect();
        for key in keys {
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
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }
}

fn router(storage: Arc<dyn KvStorage>) -> CommandRouter {
    CommandRouter::new(storage)
}

fn bulk(value: &RespValue) -> Option<Vec<u8>> {
    let RespValue::BulkString(bytes) = value else {
        panic!("bulk");
    };
    bytes.as_ref().map(|item| item.to_vec())
}

#[tokio::test]
async fn rename_moves_hash_subkeys() {
    let map = Arc::new(MapAdapter::new());
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(map);
    let router = router(storage);
    let mut db = 0usize;
    let mut args = vec![Bytes::from_static(b"old")];
    for index in 0..65 {
        args.push(Bytes::from(format!("f{index}")));
        args.push(Bytes::from_static(b"v"));
    }
    router.execute("HSET", &args, &mut db).await.unwrap();
    router
        .execute(
            "RENAME",
            &[Bytes::from_static(b"old"), Bytes::from_static(b"new")],
            &mut db,
        )
        .await
        .unwrap();
    let got = router
        .execute(
            "HGET",
            &[Bytes::from_static(b"new"), Bytes::from_static(b"f0")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(bulk(&got).as_deref(), Some(b"v".as_ref()));
    let old = router
        .execute(
            "HGET",
            &[Bytes::from_static(b"old"), Bytes::from_static(b"f1")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(bulk(&old), None);
}

#[tokio::test]
async fn copy_keeps_hash_subkeys_on_the_source() {
    let map = Arc::new(MapAdapter::new());
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(map);
    let router = router(storage);
    let mut db = 0usize;
    let mut args = vec![Bytes::from_static(b"old")];
    for index in 0..65 {
        args.push(Bytes::from(format!("f{index}")));
        args.push(Bytes::from_static(b"v"));
    }
    router.execute("HSET", &args, &mut db).await.unwrap();
    router
        .execute(
            "COPY",
            &[Bytes::from_static(b"old"), Bytes::from_static(b"copied")],
            &mut db,
        )
        .await
        .unwrap();
    for key in [b"old".as_ref(), b"copied".as_ref()] {
        let got = router
            .execute(
                "HGET",
                &[Bytes::copy_from_slice(key), Bytes::from_static(b"f0")],
                &mut db,
            )
            .await
            .unwrap();
        assert_eq!(bulk(&got).as_deref(), Some(b"v".as_ref()));
    }
}

#[tokio::test]
async fn hset_rejects_a_parent_key_containing_the_separator() {
    let map = Arc::new(MapAdapter::new());
    let storage: Arc<dyn KvStorage> =
        KvStorageAdapter::new(Arc::clone(&map) as Arc<dyn StorageAdapter>);
    let router = router(storage);
    let mut db = 0usize;
    let err = router
        .execute(
            "HSET",
            &[
                Bytes::from(vec![b'a', 0x01, b'b']),
                Bytes::from_static(b"field"),
                Bytes::from_static(b"v"),
            ],
            &mut db,
        )
        .await
        .expect_err("separator");
    assert!(err.to_string().contains("subkey separator"));
    assert!(!map.contains_separator());
}

#[tokio::test]
async fn rename_inline_hash_keeps_its_fields() {
    let map = Arc::new(MapAdapter::new());
    let storage: Arc<dyn KvStorage> = KvStorageAdapter::new(map);
    let router = router(storage);
    let mut db = 0usize;
    router
        .execute(
            "HSET",
            &[
                Bytes::from_static(b"old"),
                Bytes::from_static(b"f"),
                Bytes::from_static(b"v"),
            ],
            &mut db,
        )
        .await
        .unwrap();
    router
        .execute(
            "RENAME",
            &[Bytes::from_static(b"old"), Bytes::from_static(b"new")],
            &mut db,
        )
        .await
        .unwrap();
    let got = router
        .execute(
            "HGET",
            &[Bytes::from_static(b"new"), Bytes::from_static(b"f")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(bulk(&got).as_deref(), Some(b"v".as_ref()));
}
