//! SCAN 游标记住上一把已经返回的 key，插入更小的 key 不会把它再扫出来。

use std::sync::Arc;

use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use aikv::storage::{KvStorage, MemoryEngine};
use bytes::Bytes;

fn router() -> CommandRouter {
    let storage: Arc<dyn KvStorage> = MemoryEngine::new(16);
    CommandRouter::new(storage)
}

fn scan_page(value: RespValue) -> (String, Vec<Vec<u8>>) {
    let RespValue::Array(Some(items)) = value else {
        panic!("scan result");
    };
    let RespValue::BulkString(Some(cursor)) = &items[0] else {
        panic!("cursor");
    };
    let RespValue::Array(Some(keys)) = &items[1] else {
        panic!("keys");
    };
    let keys = keys
        .iter()
        .map(|item| {
            let RespValue::BulkString(Some(key)) = item else {
                panic!("key");
            };
            key.to_vec()
        })
        .collect();
    (String::from_utf8(cursor.to_vec()).unwrap(), keys)
}

#[tokio::test]
async fn scan_continues_after_the_last_returned_key() {
    let router = router();
    let mut db = 0usize;
    router
        .execute(
            "SET",
            &[Bytes::from_static(b"a"), Bytes::from_static(b"1")],
            &mut db,
        )
        .await
        .unwrap();
    router
        .execute(
            "SET",
            &[Bytes::from_static(b"c"), Bytes::from_static(b"1")],
            &mut db,
        )
        .await
        .unwrap();
    let (cursor, keys) = scan_page(
        router
            .execute(
                "SCAN",
                &[
                    Bytes::from_static(b"0"),
                    Bytes::from_static(b"COUNT"),
                    Bytes::from_static(b"1"),
                ],
                &mut db,
            )
            .await
            .unwrap(),
    );
    assert_eq!(keys, vec![b"a".to_vec()]);
    assert_ne!(cursor, "0");
    router
        .execute(
            "SET",
            &[Bytes::from_static(b"b"), Bytes::from_static(b"1")],
            &mut db,
        )
        .await
        .unwrap();
    let (_, keys) = scan_page(
        router
            .execute(
                "SCAN",
                &[
                    Bytes::from(cursor),
                    Bytes::from_static(b"COUNT"),
                    Bytes::from_static(b"1"),
                ],
                &mut db,
            )
            .await
            .unwrap(),
    );
    assert_eq!(keys, vec![b"c".to_vec()]);
}
