//! Hash 哈希结构命令族测试
//! @component aikv-command

use aikv::protocol::RespValue;

use super::helpers::{assert_int, assert_nil, b, router, router_with_shared};

/// 验证 HMSET 返回 OK 字符串而非整数
#[tokio::test]
async fn test_hmset_returns_ok_not_integer() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute(
            "HMSET",
            &[b("h"), b("f1"), b("v1"), b("f2"), b("v2")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(resp, RespValue::SimpleString("OK".into()));
    assert_int(
        r.execute("HSET", &[b("h"), b("f3"), b("v3")], &mut db)
            .await
            .unwrap(),
        1,
    );
}

#[tokio::test]
async fn test_hash_hset_hget() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("HSET", &[b("h"), b("f1"), b("v1")], &mut db)
            .await
            .unwrap(),
        1,
    );
    let resp = r
        .execute("HGET", &[b("h"), b("f1")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("v1"))));
}

#[tokio::test]
async fn test_hash_hset_existing_field() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v1")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("HSET", &[b("h"), b("f"), b("v2")], &mut db)
            .await
            .unwrap(),
        0,
    );
}

#[tokio::test]
async fn test_hash_hdel() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f1"), b("v1")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("HDEL", &[b("h"), b("f1")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_nil(
        r.execute("HGET", &[b("h"), b("f1")], &mut db)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn test_hash_wrong_type_hget() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("s"), b("v")], &mut db).await.unwrap();
    assert!(r
        .execute("HGET", &[b("s"), b("f")], &mut db)
        .await
        .unwrap_err()
        .to_string()
        .contains("WRONGTYPE"));
}

#[tokio::test]
async fn test_hash_hgetall() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("a"), b("1"), b("b"), b("2")], &mut db)
        .await
        .unwrap();
    let resp = r.execute("HGETALL", &[b("h")], &mut db).await.unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 4);
}

#[tokio::test]
async fn test_hash_hsetnx() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("HSETNX", &[b("h"), b("f"), b("v1")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("HSETNX", &[b("h"), b("f"), b("v2")], &mut db)
            .await
            .unwrap(),
        0,
    );
    let resp = r.execute("HGET", &[b("h"), b("f")], &mut db).await.unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("v1"))));
}

#[tokio::test]
async fn test_hash_hincrby() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("HINCRBY", &[b("h"), b("f"), b("5")], &mut db)
            .await
            .unwrap(),
        5,
    );
    assert_int(
        r.execute("HINCRBY", &[b("h"), b("f"), b("3")], &mut db)
            .await
            .unwrap(),
        8,
    );
}

#[tokio::test]
async fn test_hash_hexists_hlen_hkeys_hvals_hmget() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("a"), b("1"), b("b"), b("2")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("HEXISTS", &[b("h"), b("a")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("HEXISTS", &[b("h"), b("z")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_int(r.execute("HLEN", &[b("h")], &mut db).await.unwrap(), 2);
    let RespValue::Array(Some(keys)) = r.execute("HKEYS", &[b("h")], &mut db).await.unwrap() else {
        panic!("expected array");
    };
    assert_eq!(keys.len(), 2);
    let RespValue::Array(Some(vals)) = r.execute("HVALS", &[b("h")], &mut db).await.unwrap() else {
        panic!("expected array");
    };
    assert_eq!(vals.len(), 2);
    let RespValue::Array(Some(m)) = r
        .execute("HMGET", &[b("h"), b("a"), b("z")], &mut db)
        .await
        .unwrap()
    else {
        panic!("expected array");
    };
    assert_eq!(m[0], RespValue::BulkString(Some(b("1"))));
    assert_eq!(m[1], RespValue::BulkString(None));
}

#[tokio::test]
async fn test_hash_wrong_type_all_commands() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("s"), b("v")], &mut db).await.unwrap();
    let cases: &[(&str, Vec<bytes::Bytes>)] = &[
        ("HSET", vec![b("s"), b("f"), b("v")]),
        ("HDEL", vec![b("s"), b("f")]),
        ("HEXISTS", vec![b("s"), b("f")]),
        ("HLEN", vec![b("s")]),
        ("HKEYS", vec![b("s")]),
        ("HVALS", vec![b("s")]),
        ("HGETALL", vec![b("s")]),
        ("HMGET", vec![b("s"), b("f")]),
        ("HSETNX", vec![b("s"), b("f"), b("v")]),
        ("HINCRBY", vec![b("s"), b("f"), b("1")]),
    ];
    for (cmd, args) in cases {
        let err = r.execute(cmd, args, &mut db).await.unwrap_err();
        assert!(
            err.to_string().contains("WRONGTYPE"),
            "{cmd} should return WRONGTYPE"
        );
    }
}

#[tokio::test]
async fn hset_on_ttl_hash_does_not_grow_expires() {
    let (r, shared) = router_with_shared();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    r.execute("EXPIRE", &[b("h"), b("60")], &mut db)
        .await
        .unwrap();
    assert_eq!(shared.storage.expires(0).await.unwrap(), 1);
    r.execute("HSET", &[b("h"), b("f2"), b("v2")], &mut db)
        .await
        .unwrap();
    assert_eq!(shared.storage.expires(0).await.unwrap(), 1);
}

fn bulk_str(resp: &RespValue) -> Option<bytes::Bytes> {
    match resp {
        RespValue::BulkString(Some(b)) => Some(b.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn test_hscan_inline_and_invalid_cursor() {
    let r = router();
    let mut db = 0;
    for i in 0..5 {
        r.execute(
            "HSET",
            &[b("myhash"), b(&format!("f{i}")), b(&format!("v{i}"))],
            &mut db,
        )
        .await
        .unwrap();
    }

    let mut seen = std::collections::HashMap::new();
    let mut cursor = b("0");
    loop {
        let resp = r
            .execute("HSCAN", &[b("myhash"), cursor, b("COUNT"), b("2")], &mut db)
            .await
            .unwrap();
        let RespValue::Array(Some(parts)) = resp else {
            panic!("expected array");
        };
        let RespValue::BulkString(Some(next)) = &parts[0] else {
            panic!("expected cursor");
        };
        cursor = next.clone();
        let RespValue::Array(Some(entries)) = &parts[1] else {
            panic!("expected entries");
        };
        assert_eq!(entries.len() % 2, 0);
        for chunk in entries.chunks(2) {
            let k = bulk_str(&chunk[0]).unwrap();
            let v = bulk_str(&chunk[1]).unwrap();
            seen.insert(k, v);
        }
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(seen.len(), 5);

    // 验证内联集合游标越界优雅返回 ["0", []] (兼容边扫边删工作流)
    let res = r
        .execute("HSCAN", &[b("myhash"), b("999999")], &mut db)
        .await
        .unwrap();
    let RespValue::Array(Some(arr)) = res else {
        panic!("expected array response");
    };
    assert_eq!(bulk_str(&arr[0]).unwrap(), "0");
    let RespValue::Array(Some(items)) = &arr[1] else {
        panic!("expected items array");
    };
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_hscan_large_subkey_streaming() {
    use aikv::command::CommandRouter;
    use aikv::storage::{AiDbEngine, KvStorageAdapter};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let storage = KvStorageAdapter::new(engine);
    let r = CommandRouter::new(storage);
    let mut db = 0;

    // 写入 100 个字段, 超过 HASH_MAX_INLINE_FIELDS(64) 触发转 Subkey
    for i in 0..100 {
        r.execute(
            "HSET",
            &[
                b("largehash"),
                b(&format!("field_{i:03}")),
                b(&format!("val_{i:03}")),
            ],
            &mut db,
        )
        .await
        .unwrap();
    }

    let mut seen = std::collections::HashMap::new();
    let mut cursor = b("0");
    let mut iterations = 0;
    loop {
        iterations += 1;
        assert!(iterations <= 25, "HSCAN 流式迭代陷入死循环");
        let resp = r
            .execute(
                "HSCAN",
                &[b("largehash"), cursor, b("COUNT"), b("10")],
                &mut db,
            )
            .await
            .unwrap();
        let RespValue::Array(Some(parts)) = resp else {
            panic!("expected array");
        };
        let RespValue::BulkString(Some(next)) = &parts[0] else {
            panic!("expected cursor");
        };
        cursor = next.clone();
        let RespValue::Array(Some(entries)) = &parts[1] else {
            panic!("expected entries");
        };
        for chunk in entries.chunks(2) {
            let k = bulk_str(&chunk[0]).unwrap();
            let v = bulk_str(&chunk[1]).unwrap();
            seen.insert(k, v);
        }
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(seen.len(), 100, "必须不漏数据收集齐 100 个字段");

    // 验证非法游标报错
    let err = r
        .execute("HSCAN", &[b("largehash"), b("888888")], &mut db)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ERR invalid cursor"));
}
