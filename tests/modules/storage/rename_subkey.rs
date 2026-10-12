//! 大 Hash 的 RENAME 必须带走 subkey。含 subkey 分隔符的父 key 不能写入。

use std::sync::Arc;

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::storage::adapter::KvStorageAdapter;
use aikv::storage::types::KvStorage;
use bytes::Bytes;

use super::map_adapter::MapAdapter;

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
