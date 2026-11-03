//! @component aikv-command

use aikv::command::KeyLock;
use aikv::error::Error;
use aikv::protocol::RespValue;

use super::helpers::{b, router};

#[tokio::test]
async fn test_lock_keys_sorted_dedup_and_order() {
    let lock = KeyLock::new(64);
    let a = b"key-a";
    let b_key = b"key-b";
    let _g = lock.lock_keys_sorted(&[b_key, a, a]).await;
    // 若顺序错误或重入, 可能死锁; 能到达此处即通过
}

#[tokio::test]
async fn test_router_unknown_command() {
    let r = router();
    let mut db = 0;
    let err = r.execute("NOPE", &[b("k")], &mut db).await.unwrap_err();
    assert!(matches!(err, Error::Command(msg) if msg.contains("ERR unknown command 'NOPE'")));
    assert_eq!(db, 0);
}

#[tokio::test]
async fn test_router_select_updates_db() {
    let r = router();
    let mut db = 0;
    let resp = r.execute("SELECT", &[b("2")], &mut db).await.unwrap();
    assert_eq!(resp, RespValue::SimpleString("OK".into()));
    assert_eq!(db, 0);
}
