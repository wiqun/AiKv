//! String 字符串命令族测试
//! @component aikv-command

use aikv::protocol::RespValue;

use super::helpers::{assert_int, assert_nil, assert_ok, b, router, router_with_shared};

/// 验证 SET 命令 EX/NX/XX 参数组合
#[tokio::test]
async fn test_set_with_ex_nx_xx() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("SET", &[b("k"), b("v"), b("EX"), b("60")], &mut db)
            .await
            .unwrap(),
    );
    assert_nil(
        r.execute("SET", &[b("k"), b("v2"), b("NX")], &mut db)
            .await
            .unwrap(),
    );
    assert_nil(
        r.execute("SET", &[b("k2"), b("v"), b("XX")], &mut db)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn test_set_nx_xx_conflict() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("SET", &[b("k"), b("v"), b("NX"), b("XX")], &mut db)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("syntax error"));
}

#[tokio::test]
async fn test_get_missing_key() {
    let r = router();
    let mut db = 0;
    assert_nil(r.execute("GET", &[b("missing")], &mut db).await.unwrap());
}

#[tokio::test]
async fn test_mget_mset() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("MSET", &[b("a"), b("1"), b("b"), b("2")], &mut db)
            .await
            .unwrap(),
    );
    let resp = r
        .execute("MGET", &[b("a"), b("b"), b("c")], &mut db)
        .await
        .unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 3);
}

#[tokio::test]
async fn test_mget_wrongtype_returns_nil() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("s"), b("hello")], &mut db)
        .await
        .unwrap();
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("MGET", &[b("s"), b("h"), b("missing")], &mut db)
        .await
        .unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0], RespValue::BulkString(Some(b("hello"))));
    assert_eq!(items[1], RespValue::BulkString(None));
    assert_eq!(items[2], RespValue::BulkString(None));
}

#[tokio::test]
async fn test_mget_mset_param_validation() {
    let r = router();
    let mut db = 0;
    assert!(r.execute("MSET", &[b("a")], &mut db).await.is_err());
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    let resp = r.execute("MGET", &[b("h")], &mut db).await.unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items[0], RespValue::BulkString(None));
}

#[tokio::test]
async fn test_del_multiple_keys() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("a"), b("1")], &mut db).await.unwrap();
    r.execute("SET", &[b("b"), b("2")], &mut db).await.unwrap();
    assert_int(
        r.execute("DEL", &[b("a"), b("b"), b("c")], &mut db)
            .await
            .unwrap(),
        2,
    );
}

#[tokio::test]
async fn test_append_strlen() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("APPEND", &[b("k"), b("hi")], &mut db)
            .await
            .unwrap(),
        2,
    );
    assert_int(r.execute("STRLEN", &[b("k")], &mut db).await.unwrap(), 2);
}

#[tokio::test]
async fn test_append_wrong_type() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert!(r
        .execute("APPEND", &[b("h"), b("x")], &mut db)
        .await
        .unwrap_err()
        .to_string()
        .contains("WRONGTYPE"));
}

#[tokio::test]
async fn test_strlen_wrong_type() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert!(r
        .execute("STRLEN", &[b("h")], &mut db)
        .await
        .unwrap_err()
        .to_string()
        .contains("WRONGTYPE"));
}

#[tokio::test]
async fn test_incr_decr_incrby() {
    let r = router();
    let mut db = 0;
    assert_int(r.execute("INCR", &[b("n")], &mut db).await.unwrap(), 1);
    assert_int(r.execute("INCR", &[b("n")], &mut db).await.unwrap(), 2);
    assert_int(r.execute("DECR", &[b("n")], &mut db).await.unwrap(), 1);
    assert_int(
        r.execute("INCRBY", &[b("n"), b("10")], &mut db)
            .await
            .unwrap(),
        11,
    );
    assert_int(
        r.execute("DECRBY", &[b("n"), b("5")], &mut db)
            .await
            .unwrap(),
        6,
    );
}

#[tokio::test]
async fn test_incrbyfloat() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute("INCRBYFLOAT", &[b("f"), b("2.5")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("2.5"))));
    let resp = r
        .execute("INCRBYFLOAT", &[b("f"), b("0.5")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("3"))));
}

#[tokio::test]
async fn test_exists_expired_key() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("v")], &mut db).await.unwrap();
    r.execute("PEXPIRE", &[b("k"), b("1")], &mut db)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_int(r.execute("EXISTS", &[b("k")], &mut db).await.unwrap(), 0);
}

/// Mirrors `TL.Redis` `BaseRelationTests.创建bits` — bytes [1, 128, 3].
#[tokio::test]
async fn test_setbit_base_relation_bits_pattern() {
    let r = router();
    let mut db = 0;
    // TL.Redis: byte 1 → bit j=7; 128 → j=0; 3 → j=6,7
    let bits: &[(u64, u8)] = &[(7, 1), (8, 1), (22, 1), (23, 1)];
    for &(offset, value) in bits {
        let off = offset.to_string();
        r.execute(
            "SETBIT",
            &[b("k"), b(&off), b(if value == 1 { "1" } else { "0" })],
            &mut db,
        )
        .await
        .unwrap();
    }
    let got = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    match got {
        RespValue::BulkString(Some(bytes)) => {
            assert_eq!(bytes.len(), 3);
            assert_eq!(bytes[0], 1);
            assert_eq!(bytes[1], 128);
            assert_eq!(bytes[2], 3);
        }
        other => panic!("expected bulk string, got {:?}", other),
    }
    assert_int(
        r.execute("GETBIT", &[b("k"), b("7")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("GETBIT", &[b("k"), b("99")], &mut db)
            .await
            .unwrap(),
        0,
    );
}

#[tokio::test]
async fn test_setbit_returns_previous_and_clears() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("SETBIT", &[b("k"), b("0"), b("1")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_int(
        r.execute("SETBIT", &[b("k"), b("0"), b("0")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("GETBIT", &[b("k"), b("0")], &mut db)
            .await
            .unwrap(),
        0,
    );
}

#[tokio::test]
async fn test_getrange_setrange_basic() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("SET", &[b("mykey"), b("Hello World")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(
        r.execute("GETRANGE", &[b("mykey"), b("0"), b("4")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(b("Hello")))
    );
    assert_eq!(
        r.execute("GETRANGE", &[b("mykey"), b("-5"), b("-1")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(b("World")))
    );
    assert_int(
        r.execute("SETRANGE", &[b("mykey"), b("6"), b("Redis")], &mut db)
            .await
            .unwrap(),
        11,
    );
    assert_eq!(
        r.execute("GET", &[b("mykey")], &mut db).await.unwrap(),
        RespValue::BulkString(Some(b("Hello Redis")))
    );
}

#[tokio::test]
async fn test_getrange_missing_key_returns_empty_string() {
    let r = router();
    let mut db = 0;
    assert_eq!(
        r.execute("GETRANGE", &[b("missing"), b("0"), b("4")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(b("")))
    );
}

#[tokio::test]
async fn test_getrange_inverted_range_returns_empty() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("abc")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        r.execute("GETRANGE", &[b("k"), b("5"), b("3")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(b("")))
    );
}

#[tokio::test]
async fn test_getrange_full_string_with_negative_end() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("foo")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        r.execute("GETRANGE", &[b("k"), b("0"), b("-1")], &mut db)
            .await
            .unwrap(),
        RespValue::BulkString(Some(b("foo")))
    );
}

#[tokio::test]
async fn test_getrange_wrong_type() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert!(r
        .execute("GETRANGE", &[b("h"), b("0"), b("0")], &mut db)
        .await
        .unwrap_err()
        .to_string()
        .contains("WRONGTYPE"));
}

#[tokio::test]
async fn test_setrange_creates_key_with_null_padding() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("SETRANGE", &[b("new"), b("2"), b("hi")], &mut db)
            .await
            .unwrap(),
        4,
    );
    let resp = r.execute("GET", &[b("new")], &mut db).await.unwrap();
    let RespValue::BulkString(Some(bytes)) = resp else {
        panic!("expected bulk string");
    };
    assert_eq!(bytes, vec![0, 0, b'h', b'i']);
}

#[tokio::test]
async fn test_setrange_negative_offset_errors() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("SETRANGE", &[b("k"), b("-1"), b("x")], &mut db)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("offset is out of range"));
}

#[tokio::test]
async fn test_setrange_wrong_type() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert!(r
        .execute("SETRANGE", &[b("h"), b("0"), b("x")], &mut db)
        .await
        .unwrap_err()
        .to_string()
        .contains("WRONGTYPE"));
}

/// SET … GET 在 key 已存在时返回旧值并写入新值.
#[tokio::test]
async fn test_set_get_returns_old_value() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("SET", &[b("k"), b("old")], &mut db)
            .await
            .unwrap(),
    );
    let resp = r
        .execute("SET", &[b("k"), b("new"), b("GET")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("old"))));
    let got = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    assert_eq!(got, RespValue::BulkString(Some(b("new"))));
}

/// SET … GET 在 key 不存在时返回 nil 并创建.
#[tokio::test]
async fn test_set_get_missing_returns_nil() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute("SET", &[b("k"), b("v"), b("GET")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
    let got = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    assert_eq!(got, RespValue::BulkString(Some(b("v"))));
}

/// SET … KEEPTTL 保留原有过期时间 (短睡后 PTTL 仍为正且不回升).
#[tokio::test]
async fn test_set_keepttl_preserves_ttl() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("SET", &[b("k"), b("v"), b("EX"), b("60")], &mut db)
            .await
            .unwrap(),
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let pttl_before = r.execute("PTTL", &[b("k")], &mut db).await.unwrap();
    assert_ok(
        r.execute("SET", &[b("k"), b("v2"), b("KEEPTTL")], &mut db)
            .await
            .unwrap(),
    );
    let pttl_after = r.execute("PTTL", &[b("k")], &mut db).await.unwrap();
    let RespValue::Integer(before) = pttl_before else {
        panic!("expected Integer PTTL before")
    };
    let RespValue::Integer(after) = pttl_after else {
        panic!("expected Integer PTTL after")
    };
    assert!(
        before > 0 && after > 0,
        "ttl should remain, before={before} after={after}"
    );
    // 短睡后 before 已明显 < 60000; 误刷新 EX 60 会使 after ≈ 60000 > before
    assert!(
        after <= before + 5,
        "KEEPTTL must not refresh TTL, before={before} after={after}"
    );
    let got = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    assert_eq!(got, RespValue::BulkString(Some(b("v2"))));
}

/// SET … GET NX 在 key 存在时返回旧值且不覆盖.
#[tokio::test]
async fn test_set_get_nx_existing_returns_old_without_overwrite() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("SET", &[b("k"), b("old")], &mut db)
            .await
            .unwrap(),
    );
    let resp = r
        .execute("SET", &[b("k"), b("new"), b("NX"), b("GET")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("old"))));
    let got = r.execute("GET", &[b("k")], &mut db).await.unwrap();
    assert_eq!(got, RespValue::BulkString(Some(b("old"))));
}

/// SET … GET XX 在 key 不存在时返回 nil 且不创建.
#[tokio::test]
async fn test_set_get_xx_missing_returns_nil() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute("SET", &[b("k"), b("v"), b("XX"), b("GET")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
    assert_nil(r.execute("GET", &[b("k")], &mut db).await.unwrap());
}

#[tokio::test]
async fn set_keepttl_does_not_grow_expires() {
    let (r, shared) = router_with_shared();
    let mut db = 0;
    assert_ok(
        r.execute("SET", &[b("k"), b("v"), b("EX"), b("60")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(shared.storage.expires(0).await.unwrap(), 1);
    assert_ok(
        r.execute("SET", &[b("k"), b("v2"), b("KEEPTTL")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(shared.storage.expires(0).await.unwrap(), 1);
}

/// 业务客户端会发 BITFIELD / BITFIELD_RO / BITCOUNT. SET 的旧值、只读拒绝写、非字符串 WRONGTYPE 与 Redis 一致.
#[tokio::test]
async fn bitfield_set_get_readonly_and_wrongtype() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute(
            "BITFIELD",
            &[b("k"), b("SET"), b("u8"), b("0"), b("128")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(resp, RespValue::Array(Some(vec![RespValue::Integer(0)])));
    assert_int(
        r.execute("GETBIT", &[b("k"), b("0")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("BITCOUNT", &[b("k"), b("0"), b("-1")], &mut db)
            .await
            .unwrap(),
        1,
    );
    let ro = r
        .execute("BITFIELD_RO", &[b("k"), b("GET"), b("u8"), b("0")], &mut db)
        .await
        .unwrap();
    assert_eq!(ro, RespValue::Array(Some(vec![RespValue::Integer(128)])));
    let err = r
        .execute(
            "BITFIELD_RO",
            &[b("k"), b("SET"), b("u8"), b("0"), b("1")],
            &mut db,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("BITFIELD_RO"));
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    let err = r
        .execute("BITFIELD", &[b("h"), b("GET"), b("u8"), b("0")], &mut db)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("WRONGTYPE"));
}
