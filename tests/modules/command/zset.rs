//! ZSet 有序集合命令族测试
//! @component aikv-command

use aikv::protocol::RespValue;

use super::helpers::{assert_int, assert_nil, b, router};

fn members(resp: RespValue) -> Vec<RespValue> {
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    items
}

/// 验证 ZADD 成员添加与 ZRANGE 范围查询
#[tokio::test]
async fn test_zadd_zrange() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("ZADD", &[b("z"), b("1"), b("a"), b("2"), b("b")], &mut db)
            .await
            .unwrap(),
        2,
    );
    let items = members(
        r.execute("ZRANGE", &[b("z"), b("0"), b("-1")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_zadd_float_score() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute(
            "ZADD",
            &[b("z"), b("1.5"), b("a"), b("2.75"), b("b")],
            &mut db,
        )
        .await
        .unwrap(),
        2,
    );
    assert_int(r.execute("ZCARD", &[b("z")], &mut db).await.unwrap(), 2);
}

#[tokio::test]
async fn test_zadd_update_existing() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("ZADD", &[b("z"), b("1.0"), b("a")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("ZADD", &[b("z"), b("2.0"), b("a")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_int(r.execute("ZCARD", &[b("z")], &mut db).await.unwrap(), 1);
}

#[tokio::test]
async fn test_zcard() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("ZCARD", &[b("nonexistent")], &mut db)
            .await
            .unwrap(),
        0,
    );
    r.execute("ZADD", &[b("z"), b("1.0"), b("a")], &mut db)
        .await
        .unwrap();
    assert_int(r.execute("ZCARD", &[b("z")], &mut db).await.unwrap(), 1);
}

#[tokio::test]
async fn test_zscore() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z"), b("3.5"), b("a")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("ZSCORE", &[b("z"), b("a")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::BulkString(Some(val)) => {
            let s = String::from_utf8(val.to_vec()).unwrap();
            assert!(s.starts_with("3.5"), "expected ~3.5, got {}", s);
        }
        _ => panic!("expected BulkString, got {:?}", resp),
    }
    assert_nil(
        r.execute("ZSCORE", &[b("z"), b("nonexistent")], &mut db)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn test_zrem() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    assert_int(
        r.execute("ZREM", &[b("z"), b("a")], &mut db).await.unwrap(),
        1,
    );
    assert_int(r.execute("ZCARD", &[b("z")], &mut db).await.unwrap(), 2);
    assert_int(
        r.execute("ZREM", &[b("z"), b("nonexistent")], &mut db)
            .await
            .unwrap(),
        0,
    );
}

#[tokio::test]
async fn test_zincrby() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z"), b("1.0"), b("a")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("ZINCRBY", &[b("z"), b("2.5"), b("a")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::BulkString(Some(val)) => {
            let s = String::from_utf8(val.to_vec()).unwrap();
            assert!(s.starts_with("3.5"), "expected ~3.5, got {}", s);
        }
        _ => panic!("expected BulkString, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zrank_revrank() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    assert_int(
        r.execute("ZRANK", &[b("z"), b("a")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_int(
        r.execute("ZRANK", &[b("z"), b("b")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("ZRANK", &[b("z"), b("c")], &mut db)
            .await
            .unwrap(),
        2,
    );
    assert_int(
        r.execute("ZREVRANK", &[b("z"), b("c")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_int(
        r.execute("ZREVRANK", &[b("z"), b("a")], &mut db)
            .await
            .unwrap(),
        2,
    );
    assert_nil(
        r.execute("ZRANK", &[b("z"), b("nonexistent")], &mut db)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn test_zrange_by_index() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r
        .execute("ZRANGE", &[b("z"), b("0"), b("-1")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 3);
            let vals: Vec<String> = items
                .iter()
                .map(|v| {
                    if let RespValue::BulkString(Some(b)) = v {
                        String::from_utf8(b.to_vec()).unwrap()
                    } else {
                        "?".to_string()
                    }
                })
                .collect();
            assert_eq!(vals, vec!["a", "b", "c"]);
        }
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zrange_withscores() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r
        .execute(
            "ZRANGE",
            &[b("z"), b("0"), b("-1"), b("WITHSCORES")],
            &mut db,
        )
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => assert_eq!(items.len(), 4),
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zrevrange() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r
        .execute("ZREVRANGE", &[b("z"), b("0"), b("-1")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => {
            let vals: Vec<String> = items
                .iter()
                .map(|v| {
                    if let RespValue::BulkString(Some(b)) = v {
                        String::from_utf8(b.to_vec()).unwrap()
                    } else {
                        "?".to_string()
                    }
                })
                .collect();
            assert_eq!(vals, vec!["c", "b", "a"]);
        }
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zcount() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    assert_int(
        r.execute("ZCOUNT", &[b("z"), b("-inf"), b("+inf")], &mut db)
            .await
            .unwrap(),
        3,
    );
    assert_int(
        r.execute("ZCOUNT", &[b("z"), b("2.0"), b("3.0")], &mut db)
            .await
            .unwrap(),
        2,
    );
}

#[tokio::test]
async fn test_zpopmin() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r.execute("ZPOPMIN", &[b("z")], &mut db).await.unwrap();
    match resp {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 2);
            assert_int(r.execute("ZCARD", &[b("z")], &mut db).await.unwrap(), 2);
        }
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zpopmax() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r.execute("ZPOPMAX", &[b("z")], &mut db).await.unwrap();
    match resp {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 2);
            assert_int(r.execute("ZCARD", &[b("z")], &mut db).await.unwrap(), 2);
        }
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zrangebyscore_limit_negative_count_unlimited() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[
            b("z"),
            b("1"),
            b("a"),
            b("7"),
            b("b"),
            b("3"),
            b("c"),
            b("5"),
            b("d"),
            b("10"),
            b("e"),
        ],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r
        .execute(
            "ZRANGEBYSCORE",
            &[
                b("z"),
                b("0"),
                b("100"),
                b("WITHSCORES"),
                b("LIMIT"),
                b("3"),
                b("-1"),
            ],
            &mut db,
        )
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => {
            assert_eq!(items.len(), 4);
            if let RespValue::BulkString(Some(v)) = &items[0] {
                assert_eq!(v.as_ref(), b"b");
            } else {
                panic!("expected member b");
            }
            if let RespValue::BulkString(Some(v)) = &items[2] {
                assert_eq!(v.as_ref(), b"e");
            } else {
                panic!("expected member e");
            }
        }
        other => panic!("expected array, got {:?}", other),
    }
}

#[tokio::test]
async fn test_zrangebyscore() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1.0"), b("a"), b("2.0"), b("b"), b("3.0"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r
        .execute("ZRANGEBYSCORE", &[b("z"), b("2.0"), b("3.0")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => assert_eq!(items.len(), 2),
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zset_wrongtype() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("str"), b("value")], &mut db)
        .await
        .unwrap();
    let err = r
        .execute("ZADD", &[b("str"), b("1.0"), b("a")], &mut db)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("WRONGTYPE"),
        "expected WRONGTYPE, got {}",
        err
    );
}

// ---- ZINTER / ZUNION / ZDIFF tests ----

#[tokio::test]
async fn test_zinter_basic() {
    let r = router();
    let mut db = 0;
    // Set up: z1={a:1, b:2, c:3}, z2={b:4, c:5, d:6}
    r.execute(
        "ZADD",
        &[b("z1"), b("1"), b("a"), b("2"), b("b"), b("3"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    r.execute(
        "ZADD",
        &[b("z2"), b("4"), b("b"), b("5"), b("c"), b("6"), b("d")],
        &mut db,
    )
    .await
    .unwrap();
    // ZINTER 2 z1 z2 → [b, c] (intersection, scores summed: b=2+4=6, c=3+5=8)
    let resp = r
        .execute("ZINTER", &[b("2"), b("z1"), b("z2")], &mut db)
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 2, "expected 2 members, got {}", items.len());
    // Should be sorted by score: b (6), c (8)
    let vals: Vec<String> = items
        .iter()
        .map(|v| {
            if let RespValue::BulkString(Some(b)) = v {
                String::from_utf8(b.to_vec()).unwrap()
            } else {
                "?".to_string()
            }
        })
        .collect();
    // b score=6, c score=8 → sorted b then c
    assert_eq!(
        vals,
        vec!["b", "c"],
        "expected members b,c (without scores)"
    );
}

#[tokio::test]
async fn test_zinter_withscores() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b"), b("4"), b("c")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute(
            "ZINTER",
            &[b("2"), b("z1"), b("z2"), b("WITHSCORES")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(
        items.len(),
        2,
        "expected 2 items (member+score), got {}",
        items.len()
    );
    // member + score = 2 items for 1 result
}

#[tokio::test]
async fn test_zinter_weights() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b"), b("4"), b("c")], &mut db)
        .await
        .unwrap();
    // WEIGHTS 2 3 → z1*2, z2*3, b=2*2+3*3=4+9=13
    let resp = r
        .execute(
            "ZINTER",
            &[b("2"), b("z1"), b("z2"), b("WEIGHTS"), b("2"), b("3")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 1);
    let val = if let RespValue::BulkString(Some(b)) = &items[0] {
        String::from_utf8(b.to_vec()).unwrap()
    } else {
        "?".to_string()
    };
    assert_eq!(val, "b");
}

#[tokio::test]
async fn test_zinter_aggregate_min() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("5"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    // AGGREGATE MIN → a=min(1,3)=1, b=min(5,2)=2
    let resp = r
        .execute(
            "ZINTER",
            &[b("2"), b("z1"), b("z2"), b("AGGREGATE"), b("MIN")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 2, "expected 2 members");
}

#[tokio::test]
async fn test_zinter_aggregate_max() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("5"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    // AGGREGATE MAX → a=max(1,3)=3, b=max(5,2)=5
    let resp = r
        .execute(
            "ZINTER",
            &[b("2"), b("z1"), b("z2"), b("AGGREGATE"), b("MAX")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 2, "expected 2 members");
}

#[tokio::test]
async fn test_zinter_no_intersection() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("ZINTER", &[b("2"), b("z1"), b("z2")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => assert_eq!(items.len(), 0, "expected empty array"),
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zinter_single_key() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("ZINTER", &[b("1"), b("z1")], &mut db)
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(
        items.len(),
        2,
        "expected 2 members for single-key intersection"
    );
}

#[tokio::test]
async fn test_zunion_basic() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b"), b("4"), b("c")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("ZUNION", &[b("2"), b("z1"), b("z2")], &mut db)
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    // Union of {a:1,b:2} + {b:3,c:4} → {a:1, b:5, c:4} → a, c, b (sorted by score)
    assert_eq!(items.len(), 3, "expected 3 members");
    let vals: Vec<String> = items
        .iter()
        .map(|v| {
            if let RespValue::BulkString(Some(b)) = v {
                String::from_utf8(b.to_vec()).unwrap()
            } else {
                "?".to_string()
            }
        })
        .collect();
    assert_eq!(
        vals,
        vec!["a", "c", "b"],
        "expected members a,c,b sorted by score"
    );
}

#[tokio::test]
async fn test_zunion_weights() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b"), b("4"), b("c")], &mut db)
        .await
        .unwrap();
    // WEIGHTS 2 3 → a=1*2=2, b=2*2+3*3=13, c=4*3=12
    let resp = r
        .execute(
            "ZUNION",
            &[b("2"), b("z1"), b("z2"), b("WEIGHTS"), b("2"), b("3")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 3);
}

#[tokio::test]
async fn test_zunion_aggregate_min() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("5"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    // AGGREGATE MIN → a=min(1,3)=1, b=min(5,2)=2
    let resp = r
        .execute(
            "ZUNION",
            &[b("2"), b("z1"), b("z2"), b("AGGREGATE"), b("MIN")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_zunion_withscores() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute(
            "ZUNION",
            &[b("2"), b("z1"), b("z2"), b("WITHSCORES")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    // 2 members × 2 (member+score) = 4
    assert_eq!(items.len(), 4, "expected 4 items (2 member+score pairs)");
}

#[tokio::test]
async fn test_zdiff_basic() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z1"), b("1"), b("a"), b("2"), b("b"), b("3"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    r.execute("ZADD", &[b("z2"), b("4"), b("b"), b("5"), b("c")], &mut db)
        .await
        .unwrap();
    // ZDIFF 2 z1 z2 → {a} only (in z1, not in z2)
    let resp = r
        .execute("ZDIFF", &[b("2"), b("z1"), b("z2")], &mut db)
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    assert_eq!(items.len(), 1, "expected 1 member");
    let val = if let RespValue::BulkString(Some(b)) = &items[0] {
        String::from_utf8(b.to_vec()).unwrap()
    } else {
        "?".to_string()
    };
    assert_eq!(val, "a");
}

#[tokio::test]
async fn test_zdiff_withscores() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute(
            "ZDIFF",
            &[b("2"), b("z1"), b("z2"), b("WITHSCORES")],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    // a (member) + 1 (score) = 2 items
    assert_eq!(items.len(), 2, "expected 2 items (member+score)");
}

#[tokio::test]
async fn test_zdiff_no_difference() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("a"), b("4"), b("b")], &mut db)
        .await
        .unwrap();
    let resp = r
        .execute("ZDIFF", &[b("2"), b("z1"), b("z2")], &mut db)
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => assert_eq!(items.len(), 0, "expected empty array"),
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zinter_zunion_non_existent_keys() {
    let r = router();
    let mut db = 0;
    // Non-existent keys should be treated as empty zsets
    let resp = r
        .execute(
            "ZINTER",
            &[b("2"), b("nonexistent1"), b("nonexistent2")],
            &mut db,
        )
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => assert_eq!(items.len(), 0),
        _ => panic!("expected Array, got {:?}", resp),
    }
    let resp = r
        .execute(
            "ZUNION",
            &[b("2"), b("nonexistent1"), b("nonexistent2")],
            &mut db,
        )
        .await
        .unwrap();
    match resp {
        RespValue::Array(Some(items)) => assert_eq!(items.len(), 0),
        _ => panic!("expected Array, got {:?}", resp),
    }
}

#[tokio::test]
async fn test_zinter_wrongtype() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("str"), b("value")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z1"), b("1"), b("a")], &mut db)
        .await
        .unwrap();
    let err = r
        .execute("ZINTER", &[b("2"), b("z1"), b("str")], &mut db)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("WRONGTYPE"),
        "expected WRONGTYPE, got {}",
        err
    );
}

#[tokio::test]
async fn test_zinter_syntax_error() {
    let r = router();
    let mut db = 0;
    // wrong args: ZINTER without enough args
    let err = r.execute("ZINTER", &[b("1")], &mut db).await.unwrap_err();
    assert!(
        err.to_string().contains("wrong number of arguments"),
        "expected wrong number of arguments, got {}",
        err
    );
    // negative numkeys
    let err = r
        .execute("ZINTER", &[b("-1"), b("z1")], &mut db)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not an integer or out of range"),
        "expected range error, got {}",
        err
    );
}

#[tokio::test]
async fn test_zinter_parse_errors() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a")], &mut db)
        .await
        .unwrap();
    // numkeys not a valid integer
    let err = r
        .execute("ZINTER", &[b("abc"), b("z1")], &mut db)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not an integer"), "got {}", err);
}

// ZDIFF 语法错误
#[tokio::test]
async fn test_zdiff_syntax_error() {
    let r = router();
    let mut db = 0;
    let err = r.execute("ZDIFF", &[b("1")], &mut db).await.unwrap_err();
    assert!(
        err.to_string().contains("wrong number of arguments"),
        "got {}",
        err
    );
}

// ZUNION 多余选项
#[tokio::test]
async fn test_zunion_weights_aggregate() {
    let r = router();
    let mut db = 0;
    r.execute("ZADD", &[b("z1"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    r.execute("ZADD", &[b("z2"), b("3"), b("b"), b("4"), b("c")], &mut db)
        .await
        .unwrap();
    // WEIGHTS 2 3 AGGREGATE MAX
    let resp = r
        .execute(
            "ZUNION",
            &[
                b("2"),
                b("z1"),
                b("z2"),
                b("WEIGHTS"),
                b("2"),
                b("3"),
                b("AGGREGATE"),
                b("MAX"),
            ],
            &mut db,
        )
        .await
        .unwrap();
    let items = match resp {
        RespValue::Array(Some(items)) => items,
        _ => panic!("expected Array, got {:?}", resp),
    };
    // a=1*2=2, b=max(2*2=4, 3*3=9)=9, c=4*3=12
    assert_eq!(items.len(), 3);
}

#[tokio::test]
async fn test_zscan() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[
            b("z"),
            b("1"),
            b("a"),
            b("2"),
            b("b"),
            b("3"),
            b("c"),
            b("4"),
            b("d"),
            b("5"),
            b("e"),
        ],
        &mut db,
    )
    .await
    .unwrap();
    let mut cursor = "0".to_string();
    let mut seen = Vec::new();
    loop {
        let resp = r
            .execute("ZSCAN", &[b("z"), b(&cursor)], &mut db)
            .await
            .unwrap();
        let RespValue::Array(Some(chunk)) = resp else {
            panic!("expected array")
        };
        assert_eq!(chunk.len(), 2);
        if let RespValue::BulkString(Some(c)) = &chunk[0] {
            cursor = String::from_utf8(c.to_vec()).unwrap();
        }
        if let RespValue::Array(Some(items)) = &chunk[1] {
            for i in (0..items.len()).step_by(2) {
                if let RespValue::BulkString(Some(m)) = &items[i] {
                    seen.push(m.clone());
                }
            }
        }
        if cursor == "0" {
            break;
        }
    }
    assert_eq!(seen.len(), 5);
}

#[tokio::test]
async fn test_zrangebyscore_equal_scores_and_limit() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[
            b("z"),
            b("1"),
            b("c"),
            b("1"),
            b("a"),
            b("1"),
            b("b"),
            b("2"),
            b("d"),
        ],
        &mut db,
    )
    .await
    .unwrap();
    let items = members(
        r.execute(
            "ZRANGEBYSCORE",
            &[b("z"), b("-inf"), b("+inf"), b("LIMIT"), b("0"), b("2")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], RespValue::BulkString(Some(b("a"))));
    assert_eq!(items[1], RespValue::BulkString(Some(b("b"))));
}

#[tokio::test]
async fn test_zrangebyscore_exclusive_and_rescore() {
    let r = router();
    let mut db = 0;
    r.execute(
        "ZADD",
        &[b("z"), b("1"), b("a"), b("2"), b("b"), b("3"), b("c")],
        &mut db,
    )
    .await
    .unwrap();
    let items = members(
        r.execute(
            "ZRANGEBYSCORE",
            &[b("z"), b("(1"), b("3"), b("WITHSCORES")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_eq!(items.len(), 4);
    assert_eq!(items[0], RespValue::BulkString(Some(b("b"))));
    assert_eq!(items[2], RespValue::BulkString(Some(b("c"))));

    let rev = members(
        r.execute("ZREVRANGEBYSCORE", &[b("z"), b("3"), b("(1")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(rev[0], RespValue::BulkString(Some(b("c"))));
    assert_eq!(rev[1], RespValue::BulkString(Some(b("b"))));

    assert_int(
        r.execute("ZRANK", &[b("z"), b("b")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("ZREVRANK", &[b("z"), b("b")], &mut db)
            .await
            .unwrap(),
        1,
    );

    r.execute("ZADD", &[b("z"), b("9"), b("b")], &mut db)
        .await
        .unwrap();
    let after = members(
        r.execute("ZRANGEBYSCORE", &[b("z"), b("2"), b("2")], &mut db)
            .await
            .unwrap(),
    );
    assert!(after.is_empty());
    assert_int(
        r.execute("ZRANK", &[b("z"), b("b")], &mut db)
            .await
            .unwrap(),
        2,
    );
}

#[tokio::test]
async fn test_zset_dump_bytes_stay_member_map() {
    use std::collections::BTreeMap;

    use aikv::storage::{dump_decode, dump_encode, StoredValue};

    let mut map = BTreeMap::new();
    map.insert(b"a".to_vec(), 1.0);
    map.insert(b"b".to_vec(), 1.0);
    map.insert(b"c".to_vec(), 2.5);
    let stored = StoredValue::new_zset(map.clone());
    let bytes = dump_encode(&stored).unwrap();
    let back = dump_decode(&bytes).unwrap();
    assert_eq!(stored, back);
    let again = dump_encode(&StoredValue::new_zset(map)).unwrap();
    assert_eq!(bytes, again);
}
