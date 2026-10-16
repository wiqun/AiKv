use super::helpers::{b, router_with_shared};

#[tokio::test]
async fn test_info_server() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("INFO", &[b("server")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
        panic!("expected bulk string");
    };
    let s = String::from_utf8_lossy(&text);
    assert!(s.contains("# Server"));
    assert!(s.contains("tcp_port:"));
    assert!(s.contains("run_id:"));
}

#[tokio::test]
async fn test_time() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router.execute("TIME", &[], &mut db).await.unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_config_get_set() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    let resp = router
        .execute("CONFIG", &[b("GET"), b("port")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);

    let resp = router
        .execute("CONFIG", &[b("SET"), b("port"), b("6380")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::SimpleString("OK".into()));

    let map = shared.config_map.read();
    assert_eq!(map.get("port").map(String::as_str), Some("6380"));
}

#[tokio::test]
async fn test_config_get_unknown() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("CONFIG", &[b("GET"), b("nosuch")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_info_keyspace() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    router
        .execute("SET", &[b("k1"), b("v1")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("k2"), b("v2")], &mut db)
        .await
        .unwrap();
    router
        .execute("EXPIRE", &[b("k1"), b("3600")], &mut db)
        .await
        .unwrap();

    let resp = router
        .execute("INFO", &[b("keyspace")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
        panic!("expected bulk string");
    };
    let s = String::from_utf8_lossy(&text);
    assert!(s.contains("# Keyspace"));
    assert!(s.contains("keys_estimate:"));
    assert!(s.contains("db0:expires=1"));
    assert!(!s.contains("keys="));
    assert!(!s.contains("avg_ttl"));
}

#[tokio::test]
async fn test_info_stats_keyspace_hits_misses() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    router
        .execute("GET", &[b("missing")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    router.execute("GET", &[b("k")], &mut db).await.unwrap();

    let resp = router
        .execute("INFO", &[b("stats")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
        panic!("expected bulk string");
    };
    let s = String::from_utf8_lossy(&text);
    assert!(s.contains("keyspace_hits:1"));
    assert!(s.contains("keyspace_misses:1"));
}

#[tokio::test]
async fn test_config_slowlog_get_set() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    let resp = router
        .execute("CONFIG", &[b("GET"), b("slowlog-log-slower-than")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(shared.slow_query_log.threshold_us(), 100_000);

    router
        .execute(
            "CONFIG",
            &[b("SET"), b("slowlog-log-slower-than"), b("5000")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(shared.slow_query_log.threshold_us(), 5000);

    let resp = router
        .execute("CONFIG", &[b("GET"), b("slowlog-max-len")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(shared.slow_query_log.max_entries(), 128);

    router
        .execute(
            "CONFIG",
            &[b("SET"), b("slowlog-max-len"), b("64")],
            &mut db,
        )
        .await
        .unwrap();
    assert_eq!(shared.slow_query_log.max_entries(), 64);
}

#[tokio::test]
async fn test_slowlog_get_len_reset() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    shared.slow_query_log.set_threshold_us(0);

    shared.slow_query_log.record(
        "SET",
        &["k1".into(), "v1".into()],
        5000,
        "127.0.0.1:1234",
        0,
    );
    shared
        .slow_query_log
        .record("GET", &["k1".into()], 6000, "127.0.0.1:1234", 1);

    let resp = router
        .execute("SLOWLOG", &[b("LEN")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::Integer(2));

    let resp = router
        .execute("SLOWLOG", &[b("GET")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(entries)) = resp else {
        panic!("expected array");
    };
    assert_eq!(entries.len(), 2);
    let aikv::protocol::RespValue::Array(Some(fields)) = &entries[0] else {
        panic!("expected entry array");
    };
    assert_eq!(fields.len(), 6);
    assert_eq!(fields[0], aikv::protocol::RespValue::Integer(2));
    assert_eq!(fields[2], aikv::protocol::RespValue::Integer(6000));

    let resp = router
        .execute("SLOWLOG", &[b("GET"), b("0")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(entries)) = resp else {
        panic!("expected array");
    };
    assert!(entries.is_empty());

    router
        .execute("SLOWLOG", &[b("RESET")], &mut db)
        .await
        .unwrap();
    let resp = router
        .execute("SLOWLOG", &[b("LEN")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::Integer(0));
}

#[tokio::test]
async fn test_slowlog_threshold() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    router
        .execute(
            "CONFIG",
            &[b("SET"), b("slowlog-log-slower-than"), b("1000")],
            &mut db,
        )
        .await
        .unwrap();

    shared
        .slow_query_log
        .record("SET", &["k".into()], 500, "127.0.0.1:1", 0);
    shared
        .slow_query_log
        .record("SET", &["k".into()], 1500, "127.0.0.1:1", 0);

    let resp = router
        .execute("SLOWLOG", &[b("LEN")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::Integer(1));

    let resp = router
        .execute("SLOWLOG", &[b("GET"), b("1")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(entries)) = resp else {
        panic!("expected array");
    };
    assert_eq!(entries.len(), 1);
    let aikv::protocol::RespValue::Array(Some(fields)) = &entries[0] else {
        panic!("expected entry array");
    };
    assert_eq!(fields[2], aikv::protocol::RespValue::Integer(1500));
}

#[tokio::test]
async fn test_command_count() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("COMMAND", &[b("COUNT")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Integer(n) = resp else {
        panic!("expected integer");
    };
    assert!(n >= 126);
    assert_eq!(n as usize, aikv::command::command_count());
}

#[tokio::test]
async fn test_command_info() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    let resp = router
        .execute("COMMAND", &[b("INFO"), b("GET"), b("NOSUCH")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
    let aikv::protocol::RespValue::Array(Some(get_info)) = &items[0] else {
        panic!("expected GET info array");
    };
    assert_eq!(
        get_info[0],
        aikv::protocol::RespValue::BulkString(Some(b("GET")))
    );
    assert_eq!(get_info[1], aikv::protocol::RespValue::Integer(2));
    assert_eq!(items[1], aikv::protocol::RespValue::Null);
}

#[tokio::test]
async fn test_command_docs() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    let resp = router
        .execute("COMMAND", &[b("DOCS")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::Array(Some(vec![])));

    let resp = router
        .execute("COMMAND", &[b("DOCS"), b("GET"), b("SET")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::Array(Some(vec![])));
}

#[tokio::test]
async fn test_command_getkeys() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    let resp = router
        .execute(
            "COMMAND",
            &[b("GETKEYS"), b("MSET"), b("k1"), b("v1"), b("k2"), b("v2")],
            &mut db,
        )
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(keys)) = resp else {
        panic!("expected array");
    };
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0],
        aikv::protocol::RespValue::BulkString(Some(b("k1")))
    );
    assert_eq!(
        keys[1],
        aikv::protocol::RespValue::BulkString(Some(b("k2")))
    );

    let resp = router
        .execute("COMMAND", &[b("GETKEYS"), b("GET"), b("mykey")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(keys)) = resp else {
        panic!("expected array");
    };
    assert_eq!(keys.len(), 1);
}

#[tokio::test]
async fn test_latency_histogram() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    shared
        .metrics
        .on_command_done(aikv::command::canonical_command("GET"), true, 10);
    shared
        .metrics
        .on_command_done(aikv::command::canonical_command("GET"), true, 100);
    shared
        .metrics
        .on_command_done(aikv::command::canonical_command("SET"), true, 50);

    let resp = router
        .execute("LATENCY", &[b("HISTOGRAM")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert!(!items.is_empty());
    assert_eq!(items.len() % 2, 0);

    let resp = router
        .execute("LATENCY", &[b("HISTOGRAM"), b("GET")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_latency_latest() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("LATENCY", &[b("LATEST")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_latency_reset() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    shared
        .metrics
        .on_command_done(aikv::command::canonical_command("GET"), true, 10);
    shared
        .metrics
        .on_command_done(aikv::command::canonical_command("SET"), true, 20);

    let resp = router
        .execute("LATENCY", &[b("RESET")], &mut db)
        .await
        .unwrap();
    assert_eq!(resp, aikv::protocol::RespValue::Integer(2));

    let resp = router
        .execute("LATENCY", &[b("HISTOGRAM")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn test_latency_help() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("LATENCY", &[b("HELP")], &mut db)
        .await
        .unwrap();
    let aikv::protocol::RespValue::Array(Some(items)) = resp else {
        panic!("expected array");
    };
    assert!(!items.is_empty());
}
