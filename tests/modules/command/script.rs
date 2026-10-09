use aikv::protocol::RespValue;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;

use super::helpers::{assert_err_contains, assert_int, assert_nil, b, router, router_with_shared};
use aikv::command::KeyLock;

#[tokio::test]
async fn test_script_eval_basic() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute("EVAL", &[b("return 1+1"), b("0")], &mut db)
        .await
        .unwrap();
    assert_int(resp, 2);
}

#[tokio::test]
async fn test_script_globals_do_not_leak() {
    let r = router();
    let mut db = 0;
    r.execute(
        "EVAL",
        &[b("_G.leaked = KEYS[1]"), b("1"), b("secret")],
        &mut db,
    )
    .await
    .unwrap();
    let resp = r
        .execute("EVAL", &[b("return leaked"), b("0")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::Null);
}

#[tokio::test]
async fn test_script_load_stays_sealed_after_another_script() {
    let r = router();
    let mut db = 0;
    r.execute("EVAL", &[b("return 1"), b("0")], &mut db)
        .await
        .unwrap();
    let err = r
        .execute("EVAL", &[b("return load('return 1')"), b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "ERR");
}

#[tokio::test]
async fn test_script_syntax_error() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("EVAL", &[b("return +"), b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "ERR");
}

#[tokio::test]
async fn test_script_sandbox_no_os() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("EVAL", &[b("return os.getenv('PATH')"), b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "ERR");
}

#[tokio::test]
async fn test_script_eval_keys_argv() {
    let r = router();
    let mut db = 0;
    let resp = r
    .execute(
      "EVAL",
      &[
        b("return tostring(#KEYS) .. ':' .. tostring(#ARGV) .. ':' .. KEYS[1] .. ':' .. ARGV[1]"),
        b("1"),
        b("mykey"),
        b("a1"),
      ],
      &mut db,
    )
    .await
    .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("1:1:mykey:a1"))));
}

#[tokio::test]
async fn test_script_numkeys_mismatch() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("EVAL", &[b("return 1"), b("2"), b("k1")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "ERR");
}

#[tokio::test]
async fn test_script_eval_get_set() {
    let r = router();
    let mut db = 0;
    let script = r#"
    redis.call('SET', KEYS[1], ARGV[1])
    return redis.call('GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("k"), b("v")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("v"))));
}

#[tokio::test]
async fn test_script_read_own_writes() {
    let r = router();
    let mut db = 0;
    let script = r#"
    redis.call('SET', KEYS[1], 'a')
    redis.call('SET', KEYS[1], 'b')
    return redis.call('GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("k")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("b"))));
}

#[tokio::test]
async fn test_script_del_then_get() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("x")], &mut db).await.unwrap();
    let script = r#"
    redis.call('DEL', KEYS[1])
    return redis.call('GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("k")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
}

#[tokio::test]
async fn test_script_wrongtype_get() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    let script = r#"return redis.call('GET', KEYS[1])"#;
    let err = r
        .execute("EVAL", &[b(script), b("1"), b("h")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "WRONGTYPE");
}

#[tokio::test]
async fn test_script_redis_pcall() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    let script = r#"
    local r = redis.pcall('GET', KEYS[1])
    if type(r) == 'table' and r.err then
      return r.err
    end
    return 'unexpected'
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("h")], &mut db)
        .await
        .unwrap();
    let RespValue::BulkString(Some(msg)) = resp else {
        panic!("expected bulk error message, got {resp:?}");
    };
    assert!(String::from_utf8_lossy(&msg).contains("WRONGTYPE"));
}

#[tokio::test]
async fn test_script_constructed_key_is_allowed() {
    let r = router();
    let mut db = 0;
    let script = r#"
        redis.call('SET', KEYS[1] .. 'id', 'v')
        return redis.call('GET', KEYS[1] .. 'id')
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("{tag}:")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("v"))));
}

#[cfg(feature = "cluster")]
#[tokio::test]
async fn test_script_cross_slot_rejected() {
    use aidb::cluster::router::key_to_slot;
    let declared = b"{slot-a}:p".as_slice();
    let other = b"{slot-b}:p".as_slice();
    assert_ne!(key_to_slot(declared), key_to_slot(other));
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('GET', ARGV[1])"#;
    let err = r
        .execute(
            "EVAL",
            &[b(script), b("1"), b("{slot-a}:p"), b("{slot-b}:p")],
            &mut db,
        )
        .await
        .unwrap_err();
    assert_err_contains(err, "same slot");
}

#[tokio::test]
async fn test_script_unknown_command() {
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('UNKNOWNCMD', KEYS[1])"#;
    let err = r
        .execute("EVAL", &[b(script), b("1"), b("k")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "not supported");
}

#[tokio::test]
async fn test_script_expire_ttl_preserved_on_hset() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("h"), b("f"), b("1")], &mut db)
        .await
        .unwrap();
    r.execute("EXPIRE", &[b("h"), b("3600")], &mut db)
        .await
        .unwrap();
    let script = r#"
    redis.call('HSET', KEYS[1], 'f2', '2')
    return 1
  "#;
    r.execute("EVAL", &[b(script), b("1"), b("h")], &mut db)
        .await
        .unwrap();
    let ttl = r.execute("TTL", &[b("h")], &mut db).await.unwrap();
    match ttl {
        RespValue::Integer(n) => assert!(n > 0),
        other => panic!("expected positive TTL, got {other:?}"),
    }
}

#[tokio::test]
async fn test_evalsha_loaded() {
    let r = router();
    let mut db = 0;
    let script = "return 'hello'";
    let sha_resp = r
        .execute("SCRIPT", &[b("LOAD"), b(script)], &mut db)
        .await
        .unwrap();
    let RespValue::BulkString(Some(sha)) = sha_resp else {
        panic!("expected sha1 bulk");
    };
    let resp = r
        .execute("EVALSHA", &[sha.clone(), b("0")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("hello"))));
}

#[tokio::test]
async fn test_evalsha_not_found() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute(
            "EVALSHA",
            &[b("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"), b("0")],
            &mut db,
        )
        .await
        .unwrap_err();
    assert_err_contains(err, "NOSCRIPT");
}

#[tokio::test]
async fn test_script_load() {
    let r = router();
    let mut db = 0;
    let resp = r
        .execute("SCRIPT", &[b("LOAD"), b("return 42")], &mut db)
        .await
        .unwrap();
    assert!(matches!(resp, RespValue::BulkString(Some(_))));
}

#[tokio::test]
async fn test_script_exists() {
    let r = router();
    let mut db = 0;
    let sha_resp = r
        .execute("SCRIPT", &[b("LOAD"), b("return 1")], &mut db)
        .await
        .unwrap();
    let RespValue::BulkString(Some(sha)) = sha_resp else {
        panic!("expected sha");
    };
    let resp = r
        .execute("SCRIPT", &[b("EXISTS"), sha.clone()], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::Array(Some(vec![RespValue::Integer(1)])));
}

#[tokio::test]
async fn test_script_flush() {
    let r = router();
    let mut db = 0;
    let sha_resp = r
        .execute("SCRIPT", &[b("LOAD"), b("return 1")], &mut db)
        .await
        .unwrap();
    let RespValue::BulkString(Some(sha)) = sha_resp else {
        panic!("expected sha");
    };
    assert_ok(r.execute("SCRIPT", &[b("FLUSH")], &mut db).await.unwrap());
    let err = r
        .execute("EVALSHA", &[sha, b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "NOSCRIPT");
}

#[tokio::test]
async fn test_script_kill() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute("SCRIPT", &[b("KILL")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "NOTBUSY");
}

#[tokio::test]
async fn test_script_del_then_hget() {
    let r = router();
    let mut db = 0;
    r.execute("HSET", &[b("hk"), b("f"), b("v")], &mut db)
        .await
        .unwrap();
    let script = r#"
    redis.call('DEL', KEYS[1])
    return redis.call('HGET', KEYS[1], 'f')
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("hk")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
}

#[tokio::test]
async fn test_key_lock_parallel_different_keys() {
    let (r, _shared) = router_with_shared();
    let script = r#"
    redis.call('SET', KEYS[1], ARGV[1])
    return redis.call('GET', KEYS[1])
  "#;
    let h1 = tokio::spawn({
        let r = r.clone();
        async move {
            let mut db = 0;
            r.execute("EVAL", &[b(script), b("1"), b("k1"), b("v1")], &mut db)
                .await
        }
    });
    let h2 = tokio::spawn({
        let r = r.clone();
        async move {
            let mut db = 0;
            r.execute("EVAL", &[b(script), b("1"), b("k2"), b("v2")], &mut db)
                .await
        }
    });
    let r1 = h1.await.unwrap().unwrap();
    let r2 = h2.await.unwrap().unwrap();
    assert_eq!(r1, RespValue::BulkString(Some(b("v1"))));
    assert_eq!(r2, RespValue::BulkString(Some(b("v2"))));
}

#[tokio::test]
async fn test_script_eval_same_key_serialized() {
    let (r, _shared) = router_with_shared();
    let script = r#"
    redis.call('SET', KEYS[1], ARGV[1])
    return redis.call('GET', KEYS[1])
  "#;
    let h1 = tokio::spawn({
        let r = r.clone();
        async move {
            let mut db = 0;
            r.execute("EVAL", &[b(script), b("1"), b("hotkey"), b("v1")], &mut db)
                .await
        }
    });
    let h2 = tokio::spawn({
        let r = r.clone();
        async move {
            let mut db = 0;
            r.execute("EVAL", &[b(script), b("1"), b("hotkey"), b("v2")], &mut db)
                .await
        }
    });
    let r1 = h1.await.unwrap().unwrap();
    let r2 = h2.await.unwrap().unwrap();
    assert_eq!(r1, RespValue::BulkString(Some(b("v1"))));
    assert_eq!(r2, RespValue::BulkString(Some(b("v2"))));
}

#[tokio::test]
async fn test_script_lock_timeout_returns_err() {
    let key_lock = Arc::new(KeyLock::new(1024));
    let key = b"lock-timeout-key";

    let holder = key_lock.clone();
    let holder_task = tokio::spawn(async move {
        let _guard = holder.lock(key).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let err = match key_lock
        .lock_keys_sorted_with_timeout(&[key.as_slice()], Duration::from_millis(100))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("expected lock acquisition timeout"),
    };
    assert_err_contains(err, "Lock acquisition timeout");

    holder_task.await.unwrap();
}

#[tokio::test]
async fn test_script_lock_timeout_releases_partial_locks() {
    let key_lock = Arc::new(KeyLock::new(1024));
    let k_free = b"partial-lock-a-free";
    let k_held = b"partial-lock-b-held";

    let holder = key_lock.clone();
    let holder_task = tokio::spawn(async move {
        let _guard = holder.lock(k_held).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let err = match key_lock
        .lock_keys_sorted_with_timeout(
            &[k_free.as_slice(), k_held.as_slice()],
            Duration::from_millis(100),
        )
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("expected lock acquisition timeout"),
    };
    assert_err_contains(err, "Lock acquisition timeout");

    let acquired = tokio::time::timeout(Duration::from_millis(200), key_lock.lock(k_free)).await;
    assert!(
        acquired.is_ok(),
        "k_free should be released after timed multi-key lock attempt"
    );

    holder_task.await.unwrap();
}

fn assert_ok(resp: RespValue) {
    assert_eq!(resp, RespValue::SimpleString("OK".into()));
}

// ---- JSON in Lua script tests ----

#[tokio::test]
async fn test_script_json_set_get() {
    let r = router();
    let mut db = 0;
    let script = r#"
    redis.call('JSON.SET', KEYS[1], '$', '{"a":1}')
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b(r#"{"a":1}"#))));
}

#[tokio::test]
async fn test_script_json_get_with_path() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"x":{"y":42}}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"return redis.call('JSON.GET', KEYS[1], '$.x')"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(Bytes::from_static(br#"{"y":42}"#)))
    );
}

#[tokio::test]
async fn test_script_json_get_nonexistent() {
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('JSON.GET', KEYS[1])"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("noexist")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
}

#[tokio::test]
async fn test_script_json_del_root() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"a":1}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    redis.call('JSON.DEL', KEYS[1])
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
}

#[tokio::test]
async fn test_script_json_del_path() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"a":1,"b":2}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    redis.call('JSON.DEL', KEYS[1], '$.a')
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(Bytes::from_static(br#"{"b":2}"#)))
    );
}

#[tokio::test]
async fn test_script_json_type() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[
            b("jkey"),
            b("$"),
            Bytes::from_static(br#"{"a":1,"b":"s","c":[]}"#),
        ],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    local t1 = redis.call('JSON.TYPE', KEYS[1], '$.a')
    local t2 = redis.call('JSON.TYPE', KEYS[1], '$.b')
    local t3 = redis.call('JSON.TYPE', KEYS[1], '$.c')
    return t1 .. ':' .. t2 .. ':' .. t3
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("number:string:array"))));
}

#[tokio::test]
async fn test_script_json_strlen() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"s":"hello"}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"return redis.call('JSON.STRLEN', KEYS[1], '$.s')"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_int(resp, 5);
}

#[tokio::test]
async fn test_script_json_arrlen() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"[1,2,3]"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"return redis.call('JSON.ARRLEN', KEYS[1])"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_int(resp, 3);
}

#[tokio::test]
async fn test_script_json_objlen() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"a":1,"b":2}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"return redis.call('JSON.OBJLEN', KEYS[1])"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_int(resp, 2);
}

#[tokio::test]
async fn test_script_json_numincrby() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"n":10}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"return redis.call('JSON.NUMINCRBY', KEYS[1], '$.n', 5)"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("15"))));
}

#[tokio::test]
async fn test_script_json_numincrby_verify() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"n":10}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    redis.call('JSON.NUMINCRBY', KEYS[1], '$.n', 5)
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(Bytes::from_static(br#"{"n":15}"#)))
    );
}

#[tokio::test]
async fn test_script_json_arrappend() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"a":[1,2]}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"return redis.call('JSON.ARRAPPEND', KEYS[1], '$.a', '3', '4')"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_int(resp, 4);
}

#[tokio::test]
async fn test_script_json_arrappend_verify() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"a":[1,2]}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    redis.call('JSON.ARRAPPEND', KEYS[1], '$.a', '3')
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(Bytes::from_static(br#"{"a":[1,2,3]}"#)))
    );
}

#[tokio::test]
async fn test_script_json_mget() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("{mget}:j1"), b("$"), b(r#"{"name":"Alice"}"#)],
        &mut db,
    )
    .await
    .unwrap();
    r.execute(
        "JSON.SET",
        &[b("{mget}:j2"), b("$"), b(r#"{"name":"Bob"}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    return redis.call('JSON.MGET', KEYS[1], KEYS[2], '$.name')
  "#;
    let resp = r
        .execute(
            "EVAL",
            &[b(script), b("2"), b("{mget}:j1"), b("{mget}:j2")],
            &mut db,
        )
        .await
        .unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array, got {resp:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        RespValue::BulkString(Some(Bytes::from_static(br#""Alice""#)))
    );
    assert_eq!(
        items[1],
        RespValue::BulkString(Some(Bytes::from_static(br#""Bob""#)))
    );
}

#[tokio::test]
async fn test_script_json_mget_keys_validation() {
    let r = router();
    let mut db = 0;
    r.execute("JSON.SET", &[b("{t}:j1"), b("$"), b(r#"{"a":1}"#)], &mut db)
        .await
        .unwrap();
    let script = r#"return redis.call('JSON.MGET', KEYS[1], '{t}:missing', '$.a')"#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("{t}:j1")], &mut db)
        .await
        .unwrap();
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array, got {resp:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1], RespValue::BulkString(None));
}

#[tokio::test]
async fn test_script_json_keys_validation() {
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('JSON.GET', 'missing')"#;
    let resp = r
        .execute("EVAL", &[b(script), b("0")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
}

#[tokio::test]
async fn test_script_json_not_supported_without_dispatch() {
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('JSON.OBJKEYS', KEYS[1])"#;
    let err = r
        .execute("EVAL", &[b(script), b("1"), b("k")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "not supported");
}

#[tokio::test]
async fn test_script_json_set_nx_xx() {
    let r = router();
    let mut db = 0;
    r.execute(
        "JSON.SET",
        &[b("jkey"), b("$"), Bytes::from_static(br#"{"v":1}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script_nx = r#"return redis.call('JSON.SET', KEYS[1], '$', '{"v":2}', 'NX')"#;
    let resp = r
        .execute("EVAL", &[b(script_nx), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);
    let script_xx = r#"
    redis.call('JSON.SET', KEYS[1], '$', '{"v":2}', 'XX')
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script_xx), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(Bytes::from_static(br#"{"v":2}"#)))
    );
}

#[tokio::test]
async fn test_script_json_pcall() {
    let r = router();
    let mut db = 0;
    let script = r#"
    local ok, err = redis.pcall('JSON.GET', KEYS[1])
    if ok then return 'unexpected' end
    return tostring(err)
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("noexist")], &mut db)
        .await
        .unwrap();
    assert!(matches!(resp, RespValue::BulkString(Some(_))));
}

#[tokio::test]
async fn test_script_json_read_own_writes() {
    let r = router();
    let mut db = 0;
    let script = r#"
    redis.call('JSON.SET', KEYS[1], '$', '{"a":1}')
    redis.call('JSON.SET', KEYS[1], '$.a', '2')
    return redis.call('JSON.GET', KEYS[1])
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("jkey")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(Bytes::from_static(br#"{"a":2}"#)))
    );
}

#[tokio::test]
async fn test_script_set_ex_ttl() {
    let r = router();
    let mut db = 0;
    let script = r#"
        redis.call('SET', KEYS[1], 'v', 'EX', 60)
        return redis.call('TTL', KEYS[1])
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("k")], &mut db)
        .await
        .unwrap();
    let RespValue::Integer(ttl) = resp else {
        panic!("expected ttl integer, got {resp:?}");
    };
    assert!((1..=60).contains(&ttl), "ttl {ttl}");
}

#[tokio::test]
async fn test_script_set_xx_ex() {
    let r = router();
    let mut db = 0;
    let missing = r#"return redis.call('SET', KEYS[1], 'v', 'XX')"#;
    let resp = r
        .execute("EVAL", &[b(missing), b("1"), b("k")], &mut db)
        .await
        .unwrap();
    assert_nil(resp);

    let update = r#"
        redis.call('SET', KEYS[1], 'old')
        redis.call('SET', KEYS[1], 'new', 'XX', 'EX', 30)
        return redis.call('GET', KEYS[1])
    "#;
    let resp = r
        .execute("EVAL", &[b(update), b("1"), b("k")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("new"))));
    let ttl = r.execute("TTL", &[b("k")], &mut db).await.unwrap();
    let RespValue::Integer(ttl) = ttl else {
        panic!("expected ttl integer, got {ttl:?}");
    };
    assert!((1..=30).contains(&ttl), "ttl {ttl}");
}

#[tokio::test]
async fn test_script_hsetnx() {
    let r = router();
    let mut db = 0;
    let script = r#"
        local a = redis.call('HSETNX', KEYS[1], 'f', '1')
        local b = redis.call('HSETNX', KEYS[1], 'f', '2')
        return {a, b, redis.call('HGET', KEYS[1], 'f')}
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("h")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::Array(Some(vec![
            RespValue::Integer(1),
            RespValue::Integer(0),
            RespValue::BulkString(Some(b("1"))),
        ]))
    );
}

#[tokio::test]
async fn test_script_zset_score_ranges() {
    let r = router();
    let mut db = 0;
    let script = r#"
        redis.call('ZADD', KEYS[1], 1, 'a', 2, 'b', 3, 'c')
        local page = redis.call('ZRANGEBYSCORE', KEYS[1], 1, 2, 'LIMIT', 0, 1)
        local rev = redis.call('ZREVRANGE', KEYS[1], 0, 0)
        local scored = redis.call('ZREVRANGEBYSCORE', KEYS[1], 3, 2, 'WITHSCORES')
        return page[1] .. ':' .. rev[1] .. ':' .. scored[1] .. ':' .. scored[2]
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("z")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("a:c:c:3"))));
}

#[tokio::test]
async fn test_script_zset_repeated_add_rem_keeps_tail() {
    let r = router();
    let mut db = 0;
    let script = r#"
        for i = 1, 400 do
            redis.call('ZADD', KEYS[1], i, 'm' .. i)
        end
        for i = 1, 399 do
            redis.call('ZREM', KEYS[1], 'm' .. i)
        end
        local card = redis.call('ZCARD', KEYS[1])
        local score = redis.call('ZSCORE', KEYS[1], 'm400')
        redis.call('ZREM', KEYS[1], 'm400')
        local gone = redis.call('ZCARD', KEYS[1])
        return card .. ':' .. score .. ':' .. gone
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("{z}:seg")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("1:400:0"))));
}
