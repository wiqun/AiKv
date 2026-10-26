//! 已有数据类型上的 Redis 别名和写回命令.
//! @component aikv-command

use aikv::protocol::RespValue;

use super::helpers::{assert_err_contains, assert_int, assert_nil, b, router};

fn bulk(resp: RespValue) -> String {
    let RespValue::BulkString(Some(bytes)) = resp else {
        panic!("expected bulk, got {resp:?}");
    };
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn int_array(resp: RespValue) -> Vec<i64> {
    let RespValue::Array(Some(items)) = resp else {
        panic!("expected array, got {resp:?}");
    };
    items
        .into_iter()
        .map(|item| {
            let RespValue::Integer(n) = item else {
                panic!("expected integer, got {item:?}");
            };
            n
        })
        .collect()
}

#[tokio::test]
async fn test_string_and_key_aliases() {
    let r = router();
    let mut db = 0;
    r.execute("SET", &[b("k"), b("hello")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        bulk(
            r.execute("GETSET", &[b("k"), b("world")], &mut db)
                .await
                .unwrap()
        ),
        "hello"
    );
    assert_eq!(
        bulk(r.execute("GET", &[b("k")], &mut db).await.unwrap()),
        "world"
    );
    assert_nil(
        r.execute("GETSET", &[b("missing"), b("x")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(
        bulk(
            r.execute("SUBSTR", &[b("k"), b("1"), b("3")], &mut db)
                .await
                .unwrap()
        ),
        "orl"
    );

    r.execute("SET", &[b("a"), b("1")], &mut db).await.unwrap();
    r.execute("SET", &[b("b"), b("2")], &mut db).await.unwrap();
    assert_int(
        r.execute("UNLINK", &[b("a"), b("b"), b("gone")], &mut db)
            .await
            .unwrap(),
        2,
    );
    assert_nil(r.execute("GET", &[b("a")], &mut db).await.unwrap());

    assert_int(
        r.execute("MSETNX", &[b("n1"), b("v1"), b("n2"), b("v2")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute("MSETNX", &[b("n1"), b("other"), b("n3"), b("v3")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_eq!(
        bulk(r.execute("GET", &[b("n1")], &mut db).await.unwrap()),
        "v1"
    );
    assert_nil(r.execute("GET", &[b("n3")], &mut db).await.unwrap());
}

#[tokio::test]
async fn test_list_pushx_and_rpoplpush() {
    let r = router();
    let mut db = 0;
    assert_int(
        r.execute("LPUSHX", &[b("missing"), b("x")], &mut db)
            .await
            .unwrap(),
        0,
    );
    assert_int(
        r.execute("EXISTS", &[b("missing")], &mut db).await.unwrap(),
        0,
    );
    r.execute("SET", &[b("str"), b("x")], &mut db)
        .await
        .unwrap();
    let err = r
        .execute("RPUSHX", &[b("str"), b("y")], &mut db)
        .await
        .unwrap_err();
    assert_err_contains(err, "WRONGTYPE");

    r.execute("LPUSH", &[b("k"), b("a")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("LPUSHX", &[b("k"), b("b")], &mut db)
            .await
            .unwrap(),
        2,
    );
    assert_int(
        r.execute("RPUSHX", &[b("k"), b("c")], &mut db)
            .await
            .unwrap(),
        3,
    );

    r.execute("RPUSH", &[b("src"), b("a"), b("b")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        bulk(
            r.execute("RPOPLPUSH", &[b("src"), b("dst")], &mut db)
                .await
                .unwrap()
        ),
        "b"
    );
    assert_int(r.execute("LLEN", &[b("src")], &mut db).await.unwrap(), 1);
    assert_int(r.execute("LLEN", &[b("dst")], &mut db).await.unwrap(), 1);
}

#[tokio::test]
async fn test_brpoplpush_moves_existing_element() {
    let r = router();
    let mut db = 0;
    r.execute("RPUSH", &[b("src"), b("a"), b("b")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        bulk(
            r.execute("BRPOPLPUSH", &[b("src"), b("dst"), b("1")], &mut db)
                .await
                .unwrap()
        ),
        "b"
    );
    assert_int(r.execute("LLEN", &[b("src")], &mut db).await.unwrap(), 1);
    assert_int(r.execute("LLEN", &[b("dst")], &mut db).await.unwrap(), 1);
}

#[tokio::test]
async fn test_membership_and_store_variants() {
    let r = router();
    let mut db = 0;
    r.execute("SADD", &[b("s"), b("a"), b("b")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        int_array(
            r.execute("SMISMEMBER", &[b("s"), b("a"), b("c")], &mut db)
                .await
                .unwrap()
        ),
        vec![1, 0]
    );
    r.execute("SADD", &[b("s2"), b("b"), b("d")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("SINTERCARD", &[b("2"), b("s"), b("s2")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_int(
        r.execute(
            "SINTERCARD",
            &[b("2"), b("s"), b("s2"), b("LIMIT"), b("0")],
            &mut db,
        )
        .await
        .unwrap(),
        1,
    );

    r.execute("ZADD", &[b("z"), b("1"), b("a"), b("2"), b("b")], &mut db)
        .await
        .unwrap();
    let scores = r
        .execute("ZMSCORE", &[b("z"), b("a"), b("c")], &mut db)
        .await
        .unwrap();
    let RespValue::Array(Some(items)) = scores else {
        panic!("expected array");
    };
    assert_eq!(bulk(items[0].clone()), "1");
    assert_nil(items[1].clone());

    r.execute("ZADD", &[b("z2"), b("3"), b("a")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("ZUNIONSTORE", &[b("out"), b("2"), b("z"), b("z2")], &mut db)
            .await
            .unwrap(),
        2,
    );
    assert_eq!(
        bulk(
            r.execute("ZSCORE", &[b("out"), b("a")], &mut db)
                .await
                .unwrap()
        ),
        "4"
    );
    assert_int(
        r.execute(
            "ZINTERSTORE",
            &[b("inter"), b("2"), b("z"), b("z2")],
            &mut db,
        )
        .await
        .unwrap(),
        1,
    );
    r.execute("ZADD", &[b("z3"), b("1"), b("a"), b("1"), b("c")], &mut db)
        .await
        .unwrap();
    assert_int(
        r.execute("ZDIFFSTORE", &[b("diff"), b("2"), b("z3"), b("z")], &mut db)
            .await
            .unwrap(),
        1,
    );
    assert_eq!(
        bulk(
            r.execute("ZSCORE", &[b("diff"), b("c")], &mut db)
                .await
                .unwrap()
        ),
        "1"
    );
    assert_nil(
        r.execute("ZSCORE", &[b("diff"), b("a")], &mut db)
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn test_script_alias_commands_see_own_writes() {
    let r = router();
    let mut db = 0;
    let script = r#"
        redis.call('SET', KEYS[1], 'hello')
        local old = redis.call('GETSET', KEYS[1], 'world')
        local part = redis.call('SUBSTR', KEYS[1], 1, 3)
        redis.call('SET', KEYS[2], '1')
        local removed = redis.call('UNLINK', KEYS[2], KEYS[3])
        local created = redis.call('LPUSHX', KEYS[4], 'x')
        redis.call('LPUSH', KEYS[4], 'a')
        local pushed = redis.call('LPUSHX', KEYS[4], 'b')
        redis.call('RPUSH', KEYS[5], 'a', 'b')
        local moved = redis.call('RPOPLPUSH', KEYS[5], KEYS[6])
        local nx = redis.call('MSETNX', KEYS[7], 'v', KEYS[8], 'w')
        local nx2 = redis.call('MSETNX', KEYS[7], 'no', KEYS[9], 'z')
        redis.call('SADD', KEYS[10], 'a', 'b')
        redis.call('SADD', KEYS[11], 'b')
        local mem = redis.call('SMISMEMBER', KEYS[10], 'a', 'c')
        local card = redis.call('SINTERCARD', 2, KEYS[10], KEYS[11])
        redis.call('ZADD', KEYS[12], 1, 'a', 2, 'b')
        redis.call('ZADD', KEYS[13], 3, 'a')
        local scores = redis.call('ZMSCORE', KEYS[12], 'a', 'c')
        local stored = redis.call('ZUNIONSTORE', KEYS[14], 2, KEYS[12], KEYS[13])
        local score = redis.call('ZSCORE', KEYS[14], 'a')
        return table.concat({
            old, part, removed, created, pushed, moved, nx, nx2,
            mem[1], mem[2], card, scores[1], scores[2] == false and 'nil' or scores[2],
            stored, score
        }, ',')
    "#;
    let resp = r
        .execute(
            "EVAL",
            &[
                b(script),
                b("14"),
                b("k"),
                b("a"),
                b("gone"),
                b("list"),
                b("src"),
                b("dst"),
                b("n1"),
                b("n2"),
                b("n3"),
                b("s"),
                b("s2"),
                b("z"),
                b("z2"),
                b("out"),
            ],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(
        resp,
        RespValue::BulkString(Some(b("hello,orl,1,0,2,b,1,0,1,0,1,1,nil,2,4")))
    );
}

#[tokio::test]
async fn test_script_brpoplpush_rejected() {
    let r = router();
    let mut db = 0;
    let err = r
        .execute(
            "EVAL",
            &[
                b("return redis.call('BRPOPLPUSH', KEYS[1], KEYS[2], 1)"),
                b("2"),
                b("src"),
                b("dst"),
            ],
            &mut db,
        )
        .await
        .unwrap_err();
    assert_err_contains(err, "not supported");
}
