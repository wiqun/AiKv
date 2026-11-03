//! Database 数据库选库与清理命令族测试
//! @component aikv-command

use super::helpers::{assert_int, assert_ok, b, router};

/// 验证 SELECT 切换到合法数据库 ID (单库模式下始终归一为 0)
#[tokio::test]
async fn test_select_valid_range() {
    let r = router();
    let mut db = 0;
    assert_ok(r.execute("SELECT", &[b("15")], &mut db).await.unwrap());
    assert_eq!(db, 0);
}

#[tokio::test]
async fn test_select_invalid_range() {
    let r = router();
    let mut db = 0;
    assert!(r
        .execute("SELECT", &[b("16")], &mut db)
        .await
        .unwrap_err()
        .to_string()
        .contains("out of range"));
}

#[tokio::test]
async fn test_dbsize() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("v")], &mut db).await.unwrap();
    assert_int(r.execute("DBSIZE", &[], &mut db).await.unwrap(), 1);
}

#[tokio::test]
async fn test_flushdb_dbsize() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("v")], &mut db).await.unwrap();
    assert_ok(r.execute("FLUSHDB", &[], &mut db).await.unwrap());
    assert_int(r.execute("DBSIZE", &[], &mut db).await.unwrap(), 0);
}

#[tokio::test]
async fn test_swapdb() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("SWAPDB", &[b("1"), b("2")], &mut db)
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("ERR SWAPDB is not supported in single-db mode"));
}

#[tokio::test]
async fn test_flushall() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k0"), b("v")], &mut db).await.unwrap();
    r.execute("SELECT", &[b("1")], &mut db).await.unwrap();
    r.execute("SET", &[b("k1"), b("v")], &mut db).await.unwrap();
    assert_ok(r.execute("FLUSHALL", &[], &mut db).await.unwrap());
    assert_int(r.execute("DBSIZE", &[], &mut db).await.unwrap(), 0);
    r.execute("SELECT", &[b("0")], &mut db).await.unwrap();
    assert_int(r.execute("DBSIZE", &[], &mut db).await.unwrap(), 0);
}

#[tokio::test]
async fn test_move_target_exists_returns_zero() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("src")], &mut db)
        .await
        .unwrap();
    // 单库模式下 MOVE 始终返回 0
    assert_int(
        r.execute("MOVE", &[b("k"), b("1")], &mut db).await.unwrap(),
        0,
    );
    let v = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    assert_eq!(v, aikv::protocol::RespValue::BulkString(Some(b("src"))));
}

#[tokio::test]
async fn test_move_success() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("v")], &mut db).await.unwrap();
    // 单库模式下 MOVE 无法跨库，返回 0 且数据保留在当前库
    assert_int(
        r.execute("MOVE", &[b("k"), b("2")], &mut db).await.unwrap(),
        0,
    );
    let v = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    assert_eq!(v, aikv::protocol::RespValue::BulkString(Some(b("v"))));
}
