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
async fn test_script_timeout() {
    let r = router().with_script_timeout(Duration::from_millis(200));
    let mut db = 0;
    let err = r
        .execute("EVAL", &[b("while true do end"), b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "Script timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_script_eval_get_set_on_worker_thread() {
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
async fn test_script_keys_validation() {
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('GET', 'undeclared')"#;
    let err = r
        .execute("EVAL", &[b(script), b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "undeclared");
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
async fn test_eval_caches_script_for_evalsha() {
    let r = router();
    let mut db = 0;
    let eval_resp = r
        .execute("EVAL", &[b("return 'cached_by_eval'"), b("0")], &mut db)
        .await
        .unwrap();
    assert_eq!(eval_resp, RespValue::BulkString(Some(b("cached_by_eval"))));

    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(b"return 'cached_by_eval'");
    let sha = format!("{:x}", hasher.finalize());

    let resp = r
        .execute("EVALSHA", &[Bytes::from(sha), b("0")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, RespValue::BulkString(Some(b("cached_by_eval"))));
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
        &[b("j1"), b("$"), b(r#"{"name":"Alice"}"#)],
        &mut db,
    )
    .await
    .unwrap();
    r.execute(
        "JSON.SET",
        &[b("j2"), b("$"), b(r#"{"name":"Bob"}"#)],
        &mut db,
    )
    .await
    .unwrap();
    let script = r#"
    return redis.call('JSON.MGET', KEYS[1], KEYS[2], '$.name')
  "#;
    let resp = r
        .execute("EVAL", &[b(script), b("2"), b("j1"), b("j2")], &mut db)
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
    r.execute("JSON.SET", &[b("j1"), b("$"), b(r#"{"a":1}"#)], &mut db)
        .await
        .unwrap();
    let script = r#"return redis.call('JSON.MGET', KEYS[1], 'undeclared', '$.a')"#;
    let err = r
        .execute("EVAL", &[b(script), b("1"), b("j1")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "undeclared");
}

#[tokio::test]
async fn test_script_json_keys_validation() {
    let r = router();
    let mut db = 0;
    let script = r#"return redis.call('JSON.GET', 'undeclared')"#;
    let err = r
        .execute("EVAL", &[b(script), b("0")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "undeclared");
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
async fn test_script_hash_tag_dynamic_keys_and_new_commands() {
    let r = router();
    let mut db = 0;
    let script = r#"
        local tag_prefix = KEYS[1]
        local zkey = tag_prefix .. ':zset'
        local hkey = tag_prefix .. ':hash'
        redis.call('ZADD', zkey, 10, 'm10', 20, 'm20', 30, 'm30')
        local rev = redis.call('ZREVRANGE', zkey, 0, 1)
        local range_score = redis.call('ZRANGEBYSCORE', zkey, 15, 35)
        local rev_score = redis.call('ZREVRANGEBYSCORE', zkey, 25, 5, 'LIMIT', 0, 1)
        redis.call('HSETNX', hkey, 'f1', 'v1')
        local nx2 = redis.call('HSETNX', hkey, 'f1', 'v2')
        return {rev, range_score, rev_score, nx2}
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("prefix:{user1}")], &mut db)
        .await
        .unwrap();
    if let RespValue::Array(Some(arr)) = resp {
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[3], RespValue::Integer(0));
    } else {
        panic!("expected array response");
    }
}

#[tokio::test]
async fn test_script_zrevrangebyscore_limit_0_1_precision() {
    let r = router();
    let mut db = 0;
    let script = r#"
        local k = KEYS[1]
        redis.call('ZADD', k, 100, 'seg100', 200, 'seg200', 300, 'seg300')
        local r1 = redis.call('ZREVRANGEBYSCORE', k, 250, 0, 'LIMIT', 0, 1)
        local r2 = redis.call('ZREVRANGEBYSCORE', k, 300, 0, 'LIMIT', 0, 1)
        local r3 = redis.call('ZREVRANGEBYSCORE', k, 99, 0, 'LIMIT', 0, 1)
        local r4 = redis.call('ZREVRANGEBYSCORE', k, '(200', 0, 'LIMIT', 0, 1)
        return {r1[1], r2[1], #r3, r4[1]}
    "#;
    let resp = r
        .execute("EVAL", &[b(script), b("1"), b("test:{zset}")], &mut db)
        .await
        .unwrap();
    if let RespValue::Array(Some(arr)) = resp {
        assert_eq!(arr[0], RespValue::BulkString(Some(b("seg200"))));
        assert_eq!(arr[1], RespValue::BulkString(Some(b("seg300"))));
        assert_eq!(arr[2], RespValue::Integer(0));
        assert_eq!(arr[3], RespValue::BulkString(Some(b("seg100"))));
    } else {
        panic!("expected array response: {resp:?}");
    }
}
