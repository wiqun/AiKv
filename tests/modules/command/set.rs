//! Set 集合结构命令族测试
//! @component aikv-command

use aikv::protocol::RespValue;

use super::helpers::{assert_int, b, router};

fn members(resp: RespValue) -> Vec<RespValue> {
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    items
}

fn bulk_str(resp: &RespValue) -> Option<bytes::Bytes> {
    let RespValue::BulkString(v) = resp else {
        panic!("expected bulk string");
    };
    v.clone()
}

/// 验证 SADD 元素添加与 SMEMBERS 获取全量集合成员
#[tokio::test]
async fn test_sadd_smembers() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("SADD", &[b("s"), b("a"), b("b"), b("c")], &mut db)
            .await
            .unwrap(),
        3,
    );
    let items = members(r.execute("SMEMBERS", &[b("s")], &mut db).await.unwrap());
    assert_eq!(items.len(), 3);
}

#[tokio::test]
async fn test_sinter() {
    let r = router();
    let mut db = 0;
    r.execute("SADD", &[b("s1"), b("a"), b("b"), b("c")], &mut db)
        .await
        .unwrap();
    r.execute("SADD", &[b("s2"), b("b"), b("c"), b("d")], &mut db)
        .await
        .unwrap();
    let items = members(
        r.execute("SINTER", &[b("s1"), b("s2")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_sunionstore() {
    let r = router();
    let mut db = 0;
    r.execute("SADD", &[b("s1"), b("a"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("SADD", &[b("s2"), b("b"), b("c")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("SUNIONSTORE", &[b("dest"), b("s1"), b("s2")], &mut db)
            .await
            .unwrap(),
        3,
    );
    let items = members(r.execute("SMEMBERS", &[b("dest")], &mut db).await.unwrap());
    assert_eq!(items.len(), 3);
}

#[tokio::test]
async fn test_smove() {
    let r = router();
    let mut db = 0;
    r.execute("SADD", &[b("src"), b("a"), b("b"), b("c")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("SMOVE", &[b("src"), b("dst"), b("b")], &mut db)
            .await
            .unwrap(),
        1,
    );
    let src = members(r.execute("SMEMBERS", &[b("src")], &mut db).await.unwrap());
    assert_eq!(src.len(), 2);
    let dst = members(r.execute("SMEMBERS", &[b("dst")], &mut db).await.unwrap());
    assert_eq!(dst.len(), 1);
    assert_eq!(dst[0], RespValue::BulkString(Some(b("b"))));
    assert_int(
        r.execute("SMOVE", &[b("src"), b("dst"), b("missing")], &mut db)
            .await
            .unwrap(),
        0,
    );
}

#[tokio::test]
async fn test_smove_same_key() {
    let r = router();
    let mut db = 0;
    r.execute("SADD", &[b("s"), b("a"), b("b")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("SMOVE", &[b("s"), b("s"), b("a")], &mut db)
            .await
            .unwrap(),
        1,
    );
    let items = members(r.execute("SMEMBERS", &[b("s")], &mut db).await.unwrap());
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_sscan() {
    let r = router();
    let mut db = 0;
    for ch in ["a", "b", "c", "d", "e"] {
        r.execute("SADD", &[b("s"), b(ch)], &mut db).await.unwrap();
    }
    let mut cursor = b("0");
    let mut seen = Vec::new();
    loop {
        let resp = r
            .execute("SSCAN", &[b("s"), cursor, b("COUNT"), b("2")], &mut db)
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
        for entry in entries {
            if let Some(m) = bulk_str(entry) {
                seen.push(m);
            }
        }
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(seen.len(), 5);
}

#[tokio::test]
async fn test_sscan_pattern() {
    let r = router();
    let mut db = 0;
    for name in ["foo", "bar", "baz", "qux"] {
        r.execute("SADD", &[b("s"), b(name)], &mut db)
            .await
            .unwrap();
    }
    let resp = r
        .execute(
            "SSCAN",
            &[b("s"), b("0"), b("MATCH"), b("ba*"), b("COUNT"), b("10")],
            &mut db,
        )
        .await
        .unwrap();
    let RespValue::Array(Some(parts)) = resp else {
        panic!("expected array");
    };
    let RespValue::Array(Some(entries)) = &parts[1] else {
        panic!("expected entries");
    };
    assert_eq!(entries.len(), 2);
    let mut matched: Vec<_> = entries
        .iter()
        .filter_map(|e| bulk_str(e).map(|b| String::from_utf8_lossy(&b).into_owned()))
        .collect();
    matched.sort();
    assert_eq!(matched, vec!["bar", "baz"]);
}

#[tokio::test]
async fn test_sscan_large_subkey_streaming_and_invalid_cursor() {
    use aikv::command::CommandRouter;
    use aikv::storage::{AiDbEngine, KvStorageAdapter};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let engine = AiDbEngine::open_for_testing(dir.path()).unwrap();
    let storage = KvStorageAdapter::new(engine);
    let r = CommandRouter::new(storage);
    let mut db = 0;

    // 写入 100 个成员, 触发转 Subkey 格式存储
    for i in 0..100 {
        r.execute("SADD", &[b("largeset"), b(&format!("mem_{i:03}"))], &mut db)
            .await
            .unwrap();
    }

    let mut seen = std::collections::HashSet::new();
    let mut cursor = b("0");
    let mut iterations = 0;
    loop {
        iterations += 1;
        assert!(iterations <= 25, "SSCAN 流式迭代陷入死循环");
        let resp = r
            .execute(
                "SSCAN",
                &[b("largeset"), cursor, b("COUNT"), b("10")],
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
        for entry in entries {
            if let Some(m) = bulk_str(entry) {
                seen.insert(m);
            }
        }
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(seen.len(), 100, "必须不漏数据收集齐全部 100 个成员");

    // 验证非法游标报错
    let err = r
        .execute("SSCAN", &[b("largeset"), b("999999")], &mut db)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ERR invalid cursor"));
}
