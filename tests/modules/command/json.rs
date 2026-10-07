//! JSON 数据类型扩展命令族测试 (JSON.SET / JSON.GET / JSON.ARRAPPEND 等)
//! @component aikv-command

use aikv::protocol::RespValue;
use serde_json::Value as JsonValue;

use super::helpers::{
    assert_err_contains, assert_int, assert_nil, assert_ok, b, router, router_with_shared,
};

fn bulk_json(resp: RespValue) -> JsonValue {
    let RespValue::BulkString(Some(data)) = resp else {
        panic!("expected bulk string, got {resp:?}");
    };
    serde_json::from_slice(&data).expect("valid json")
}

/// 验证 JSON.SET 根节点设置与 JSON.GET 根节点获取
#[tokio::test]
async fn test_json_set_get_root() {
    let r = router();
    let mut db = 0;
    let doc = r#"{"name":"John","age":30}"#;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(doc)], &mut db)
            .await
            .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got["name"], "John");
    assert_eq!(got["age"], 30);
}

#[tokio::test]
async fn test_json_set_get_nested() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"user":{"name":"Alice"}}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$.user.age"), b("25")], &mut db)
            .await
            .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$.user")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got["name"], "Alice");
    assert_eq!(got["age"], 25);
}

#[tokio::test]
async fn test_json_arr_index() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("arr"), b("$"), b("[10,20,30]")], &mut db)
            .await
            .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("arr"), b("$[1]")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got, 20);
}

#[tokio::test]
async fn test_json_set_nx_xx() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"v":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    assert_nil(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"v":2}"#), b("NX")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        bulk_json(r.execute("JSON.GET", &[b("k")], &mut db).await.unwrap())["v"],
        1
    );

    assert_nil(
        r.execute(
            "JSON.SET",
            &[b("newk"), b("$"), b(r#"{"v":1}"#), b("XX")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_nil(r.execute("JSON.GET", &[b("newk")], &mut db).await.unwrap());
}

#[tokio::test]
async fn test_json_set_with_expire() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"t":1}"#), b("60")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let ttl = r.execute("TTL", &[b("k")], &mut db).await.unwrap();
    let RespValue::Integer(n) = ttl else {
        panic!("expected integer ttl");
    };
    assert!(n > 0 && n <= 60);
}

#[tokio::test]
async fn test_json_del_root() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"a":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    assert_int(r.execute("JSON.DEL", &[b("k")], &mut db).await.unwrap(), 1);
    assert_nil(r.execute("JSON.GET", &[b("k")], &mut db).await.unwrap());
}

#[tokio::test]
async fn test_json_del_path() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"a":1,"b":2}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_int(
        r.execute("JSON.DEL", &[b("k"), b("$.a")], &mut db)
            .await
            .unwrap(),
        1,
    );
    let got = bulk_json(r.execute("JSON.GET", &[b("k")], &mut db).await.unwrap());
    assert_eq!(got["b"], 2);
    assert!(got.get("a").is_none());
}

#[tokio::test]
async fn test_json_del_filter_count() {
    let r = router();
    let mut db = 0;
    let doc = r#"[{"age":10},{"age":25},{"age":30}]"#;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(doc)], &mut db)
            .await
            .unwrap(),
    );
    assert_int(
        r.execute("JSON.DEL", &[b("k"), b("$[?(@.age > 20)]")], &mut db)
            .await
            .unwrap(),
        2,
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$")], &mut db)
            .await
            .unwrap(),
    );
    let arr = got.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["age"], 10);
}

#[tokio::test]
async fn test_json_type() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[
                b("k"),
                b("$"),
                b(r#"{"name":"x","age":1,"ok":true,"items":[]}"#),
            ],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        r.execute("JSON.TYPE", &[b("k"), b("$.name")], &mut db)
            .await
            .unwrap(),
        RespValue::SimpleString("string".into())
    );
    assert_eq!(
        r.execute("JSON.TYPE", &[b("k"), b("$.age")], &mut db)
            .await
            .unwrap(),
        RespValue::SimpleString("number".into())
    );
    assert_eq!(
        r.execute("JSON.TYPE", &[b("k"), b("$.ok")], &mut db)
            .await
            .unwrap(),
        RespValue::SimpleString("boolean".into())
    );
    assert_eq!(
        r.execute("JSON.TYPE", &[b("k"), b("$.items")], &mut db)
            .await
            .unwrap(),
        RespValue::SimpleString("array".into())
    );
}

#[tokio::test]
async fn test_json_strlen() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"s":"hello"}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_int(
        r.execute("JSON.STRLEN", &[b("k"), b("$.s")], &mut db)
            .await
            .unwrap(),
        5,
    );
    assert_nil(
        r.execute("JSON.STRLEN", &[b("k"), b("$.nope")], &mut db)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn test_json_arrlen() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b("[1,2,3]")], &mut db)
            .await
            .unwrap(),
    );
    assert_int(
        r.execute("JSON.ARRLEN", &[b("k")], &mut db).await.unwrap(),
        3,
    );
}

#[tokio::test]
async fn test_json_objlen() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"a":1,"b":2}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_int(
        r.execute("JSON.OBJLEN", &[b("k")], &mut db).await.unwrap(),
        2,
    );
}

#[tokio::test]
async fn test_json_key_not_found() {
    let r = router();
    let mut db = 0;
    assert_nil(
        r.execute("JSON.GET", &[b("missing")], &mut db)
            .await
            .unwrap(),
    );
    assert_nil(
        r.execute("JSON.TYPE", &[b("missing")], &mut db)
            .await
            .unwrap(),
    );
    assert_int(
        r.execute("JSON.DEL", &[b("missing"), b("$.x")], &mut db)
            .await
            .unwrap(),
        0,
    );
}

#[tokio::test]
async fn test_json_wrongtype() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert_err_contains(
        r.execute("JSON.GET", &[b("h")], &mut db).await.unwrap_err(),
        "WRONGTYPE",
    );
}

#[tokio::test]
async fn test_json_overwrite_hash() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert_ok(
        r.execute("JSON.SET", &[b("h"), b("$"), b(r#"{"x":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    let got = bulk_json(r.execute("JSON.GET", &[b("h")], &mut db).await.unwrap());
    assert_eq!(got["x"], 1);
}

#[tokio::test]
async fn test_json_wrong_arg_count() {
    let r = router();
    let mut db = 0;
    assert_err_contains(
        r.execute("JSON.SET", &[b("k")], &mut db).await.unwrap_err(),
        "wrong number of arguments",
    );
}

#[tokio::test]
async fn test_json_invalid_json() {
    let r = router();
    let mut db = 0;
    assert_err_contains(
        r.execute("JSON.SET", &[b("k"), b("$"), b("{bad")], &mut db)
            .await
            .unwrap_err(),
        "ERR",
    );
}

#[tokio::test]
async fn test_json_path_not_found() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"a":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    assert_err_contains(
        r.execute("JSON.GET", &[b("k"), b("$.missing")], &mut db)
            .await
            .unwrap_err(),
        "ERR",
    );
}

#[tokio::test]
async fn test_json_filter_equal() {
    let r = router();
    let mut db = 0;
    let doc = r#"{"items":[{"name":"a","v":1},{"name":"b","v":2}]}"#;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(doc)], &mut db)
            .await
            .unwrap(),
    );
    let got = bulk_json(
        r.execute(
            "JSON.GET",
            &[b("k"), b("$.items[?(@.name == 'b')]")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let arr = got.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "b");
}

#[tokio::test]
async fn test_json_get_filter_primitive_string_gt() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"List":["1","2","3"]}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$.List[?(@ > '1')]")], &mut db)
            .await
            .unwrap(),
    );
    let arr = got.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|v| v == "2" || v == "3"));
}

#[tokio::test]
async fn test_json_filter_compare() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"items":[{"v":1},{"v":5},{"v":3}]}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$.items[?(@.v > 2)]")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_json_filter_any_contains_on_string_list() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[
                b("k"),
                b("$"),
                b(r#"{"StrList":["test1","test11","test111"]}"#),
            ],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(
        r.execute(
            "JSON.GET",
            &[b("k"), b("$.StrList[?(@ =~ /.*test.*/i)]")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let arr = got.as_array().expect("array");
    assert_eq!(arr.len(), 3);
}

#[tokio::test]
async fn test_json_filter_regex() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[
                b("k"),
                b("$"),
                b(r#"{"items":[{"name":"John"},{"name":"Jane"}]}"#),
            ],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(
        r.execute(
            "JSON.GET",
            &[b("k"), b("$.items[?(@.name =~ /Jo.*/)]")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_eq!(got.as_array().unwrap().len(), 1);
    assert_eq!(got[0]["name"], "John");
}

#[tokio::test]
async fn test_json_arr_wildcard() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"[{"n":1},{"n":2}]"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$[*].n")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got, serde_json::json!([1, 2]));
}

#[tokio::test]
async fn test_json_multi_field() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"name":"A","age":10,"city":"X"}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$['name','age']")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got, serde_json::json!(["A", 10]));
}

#[tokio::test]
async fn test_json_numincrby() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"count":10}"#)], &mut db)
            .await
            .unwrap(),
    );
    let got = bulk_json(
        r.execute("JSON.NUMINCRBY", &[b("k"), b("$.count"), b("5")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got, 15);
}

#[tokio::test]
async fn test_json_numincrby_errors() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"count":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    assert_err_contains(
        r.execute("JSON.NUMINCRBY", &[b("k"), b("$"), b("1")], &mut db)
            .await
            .unwrap_err(),
        "Cannot increment root",
    );
    assert_err_contains(
        r.execute("JSON.NUMINCRBY", &[b("k"), b("$.count"), b("NaN")], &mut db)
            .await
            .unwrap_err(),
        "ERR",
    );
}

#[tokio::test]
async fn test_json_ttl_preserved() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"n":1}"#), b("120")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let ttl_before = match r.execute("TTL", &[b("k")], &mut db).await.unwrap() {
        RespValue::Integer(n) => n,
        _ => panic!("ttl"),
    };
    assert!(ttl_before > 0);

    r.execute("JSON.NUMINCRBY", &[b("k"), b("$.n"), b("1")], &mut db)
        .await
        .unwrap();
    let ttl_after = match r.execute("TTL", &[b("k")], &mut db).await.unwrap() {
        RespValue::Integer(n) => n,
        _ => panic!("ttl"),
    };
    assert!(ttl_after > 0);
    assert!(ttl_after <= ttl_before);
}

#[tokio::test]
async fn test_json_arrappend() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"a":[1,2]}"#)], &mut db)
            .await
            .unwrap(),
    );
    assert_int(
        r.execute(
            "JSON.ARRAPPEND",
            &[b("k"), b("$.a"), b("3"), b("4")],
            &mut db,
        )
        .await
        .unwrap(),
        4,
    );
    let got = bulk_json(
        r.execute("JSON.GET", &[b("k"), b("$.a")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got, serde_json::json!([1, 2, 3, 4]));
}

#[tokio::test]
async fn test_json_set_xe_nn() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("k"), b("$"), b(r#"{"v":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    assert_err_contains(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"v":2}"#), b("XE")],
            &mut db,
        )
        .await
        .unwrap_err(),
        "XE",
    );
    assert_nil(
        r.execute(
            "JSON.SET",
            &[b("new"), b("$"), b(r#"{"v":1}"#), b("NN")],
            &mut db,
        )
        .await
        .unwrap(),
    );
}

#[tokio::test]
async fn test_json_set_filter_precheck() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"items":[{"v":1}]}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_err_contains(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$.items[?(@.v == 99)]"), b("99")],
            &mut db,
        )
        .await
        .unwrap_err(),
        "No elements match",
    );
}

#[tokio::test]
async fn test_json_update() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"items":[{"v":1},{"v":2}]}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_ok(
        r.execute(
            "JSON.UPDATE",
            &[b("k"), b("$"), b("$.items[0].v"), b("10"), b("")],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let got = bulk_json(r.execute("JSON.GET", &[b("k")], &mut db).await.unwrap());
    assert_eq!(got["items"][0]["v"], 10);
}

#[tokio::test]
async fn test_json_mset() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.MSET",
            &[
                b("k1"),
                b("$"),
                b(r#"{"a":1}"#),
                b("k2"),
                b("$"),
                b(r#"{"b":2}"#),
            ],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        bulk_json(r.execute("JSON.GET", &[b("k1")], &mut db).await.unwrap())["a"],
        1
    );
    assert_eq!(
        bulk_json(r.execute("JSON.GET", &[b("k2")], &mut db).await.unwrap())["b"],
        2
    );
}

#[tokio::test]
async fn test_json_concurrent_incr() {
    use std::sync::Arc;

    let (router, _shared) = router_with_shared();
    let router = Arc::clone(&router);
    let mut db = 0;

    router
        .execute("JSON.SET", &[b("k"), b("$"), b(r#"{"n":0}"#)], &mut db)
        .await
        .unwrap();

    let mut handles = Vec::new();
    for _ in 0..20 {
        let r = Arc::clone(&router);
        handles.push(tokio::spawn(async move {
            let mut db = 0;
            r.execute("JSON.NUMINCRBY", &[b("k"), b("$.n"), b("1")], &mut db)
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let got = bulk_json(
        router
            .execute("JSON.GET", &[b("k"), b("$.n")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(got, 20);
}

#[tokio::test]
async fn test_json_metrics_recorded() {
    let (router, shared) = router_with_shared();
    let mut db = 0;
    assert_ok(
        router
            .execute("JSON.SET", &[b("mk"), b("$"), b(r#"{"x":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    router
        .execute("JSON.GET", &[b("mk")], &mut db)
        .await
        .unwrap();
    assert!(shared.metrics.command_ok_count("JSON.SET") >= 1);
    assert!(shared.metrics.command_ok_count("JSON.GET") >= 1);
}

#[tokio::test]
async fn test_json_mget_basic() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("d1"), b("$"), b(r#"{"a":1,"nested":{"a":3}}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("d2"), b("$"), b(r#"{"a":4,"nested":{"a":6}}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );

    let RespValue::Array(Some(items)) = r
        .execute(
            "JSON.MGET",
            &[b("d1"), b("d2"), b("missing"), b("$.a")],
            &mut db,
        )
        .await
        .unwrap()
    else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(bulk_json(items[0].clone()), serde_json::json!(1));
    assert_eq!(bulk_json(items[1].clone()), serde_json::json!(4));
    assert_eq!(items[2], RespValue::Null);
}

#[tokio::test]
async fn test_json_mget_missing_path() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute(
            "JSON.SET",
            &[b("k"), b("$"), b(r#"{"name":"Alice"}"#)],
            &mut db,
        )
        .await
        .unwrap(),
    );
    let RespValue::Array(Some(items)) = r
        .execute("JSON.MGET", &[b("k"), b("$.age")], &mut db)
        .await
        .unwrap()
    else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0], RespValue::Null);
}

#[tokio::test]
async fn test_json_mget_wrongtype() {
    let r = router();
    let mut db = 0;
    assert_ok(
        r.execute("JSON.SET", &[b("j"), b("$"), b(r#"{"x":1}"#)], &mut db)
            .await
            .unwrap(),
    );
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    assert_err_contains(
        r.execute("JSON.MGET", &[b("j"), b("h"), b("$")], &mut db)
            .await
            .unwrap_err(),
        "WRONGTYPE",
    );
}

#[tokio::test]
async fn test_json_mget_arg_validation() {
    let r = router();
    let mut db = 0;
    assert_err_contains(
        r.execute("JSON.MGET", &[b("k")], &mut db)
            .await
            .unwrap_err(),
        "wrong number of arguments",
    );
}
