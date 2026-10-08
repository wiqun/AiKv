//! INFO 与 Redis 语义对齐 (Phase 1–2)

use super::helpers::{b, router_with_shared};

fn info_text(resp: aikv::protocol::RespValue) -> String {
    let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
        panic!("expected bulk string");
    };
    String::from_utf8_lossy(&text).into_owned()
}

fn info_field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k == key).then(|| v.parse().ok())?
    })
}

fn info_has_keyspace_contract(text: &str) -> bool {
    text.contains("keys_estimate:")
        && text.contains("db0:expires=")
        && !text.contains("avg_ttl")
        && !text.contains("keys=")
}

#[tokio::test]
async fn info_unknown_section_returns_empty() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let resp = router
        .execute("INFO", &[b("nosuch")], &mut db)
        .await
        .unwrap();
    assert_eq!(
        resp,
        aikv::protocol::RespValue::BulkString(Some(bytes::Bytes::new()))
    );
}

#[tokio::test]
async fn info_memory_uses_storage_not_placeholder() {
    let (router, shared) = router_with_shared();
    let mut db = 0;
    router
        .execute("SET", &[b("k1"), b("value-one")], &mut db)
        .await
        .unwrap();
    router
        .execute("SET", &[b("k2"), b("value-two")], &mut db)
        .await
        .unwrap();
    shared.refresh_runtime_metrics().await;

    let text = info_text(
        router
            .execute("INFO", &[b("memory")], &mut db)
            .await
            .unwrap(),
    );
    let used = info_field(&text, "used_memory").expect("used_memory");
    let connected = shared.metrics().connected_clients() as u64;
    let placeholder = 1_048_576 + connected * 16_384;
    assert_ne!(
        used, placeholder,
        "INFO memory must not use placeholder formula"
    );
    assert!(used > 0, "used_memory should reflect stored keys");
}

#[tokio::test]
async fn info_stats_includes_expired_and_rejected() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    shared.metrics().on_rejected_connection();
    shared.metrics().on_expired_key();

    let text = info_text(
        router
            .execute("INFO", &[b("stats")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(info_field(&text, "expired_keys"), Some(1));
    assert_eq!(info_field(&text, "rejected_connections"), Some(1));
    assert!(text.contains("instantaneous_ops_per_sec:"));
}

#[tokio::test]
async fn info_default_section_order_and_fields() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let text = info_text(router.execute("INFO", &[], &mut db).await.unwrap());

    let headers: Vec<_> = text
        .lines()
        .filter(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim())
        .collect();
    let expected = vec![
        "Server",
        "Clients",
        "Memory",
        "Persistence",
        "Stats",
        "Replication",
        "CPU",
        "Cluster",
        "Keyspace",
        "Storage",
    ];
    assert_eq!(headers, expected);
    assert!(text.contains("redis_version:8.8"));
    assert!(text.contains("redis_compatible_version:8.8"));
    assert!(text.contains("redis_mode:standalone"));
}

#[tokio::test]
async fn info_stats_matches_metrics_counters() {
    let (router, shared) = router_with_shared();
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

    let expected_total = shared.metrics().total_commands_processed();
    let text = info_text(
        router
            .execute("INFO", &[b("stats")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(
        info_field(&text, "keyspace_hits"),
        Some(shared.metrics().keyspace_hits())
    );
    assert_eq!(
        info_field(&text, "keyspace_misses"),
        Some(shared.metrics().keyspace_misses())
    );
    assert_eq!(
        info_field(&text, "total_commands_processed"),
        Some(expected_total)
    );
}

#[tokio::test]
async fn info_commandstats_reflects_client_commands() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    router.execute("GET", &[b("k")], &mut db).await.unwrap();

    let text = info_text(
        router
            .execute("INFO", &[b("commandstats")], &mut db)
            .await
            .unwrap(),
    );
    assert!(text.contains("# Commandstats"));
    assert!(text.contains("cmdstat_set:calls=1,usec="));
    assert!(text.contains("cmdstat_get:calls=1,usec="));
    assert!(text.contains("usec_per_call="));
    assert!(text.contains("rejected_calls=0,failed_calls=0"));
    assert!(text.contains("slowlog_count=0,slowlog_time_ms_sum=0,slowlog_time_ms_max=0"));
}

#[tokio::test]
async fn info_errorstats_after_command_error() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;

    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();
    assert!(router
        .execute("LPUSH", &[b("k"), b("x")], &mut db)
        .await
        .is_err());

    let text = info_text(
        router
            .execute("INFO", &[b("errorstats")], &mut db)
            .await
            .unwrap(),
    );
    assert!(text.contains("# Errorstats"));
    assert!(text.contains("errorstat_WRONGTYPE:count=1"));
}

#[tokio::test]
async fn info_multi_section_stats_and_memory() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let text = info_text(
        router
            .execute("INFO", &[b("stats"), b("memory")], &mut db)
            .await
            .unwrap(),
    );
    let headers: Vec<_> = text
        .lines()
        .filter(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim())
        .collect();
    assert_eq!(headers, vec!["Stats", "Memory"]);
    assert!(text.contains("total_commands_processed:"));
    assert!(text.contains("used_memory:"));
}

#[tokio::test]
async fn info_default_alias_matches_empty() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let default_text = info_text(router.execute("INFO", &[], &mut db).await.unwrap());
    let explicit = info_text(
        router
            .execute("INFO", &[b("default")], &mut db)
            .await
            .unwrap(),
    );
    fn headers(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").trim())
            .collect()
    }
    assert_eq!(headers(&default_text), headers(&explicit));
}

#[tokio::test]
async fn info_all_includes_commandstats_errorstats_threads_latencystats() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();

    let text = info_text(router.execute("INFO", &[b("all")], &mut db).await.unwrap());
    let headers: Vec<_> = text
        .lines()
        .filter(|l| l.starts_with("# "))
        .map(|l| l.trim_start_matches("# ").trim())
        .collect();
    for section in ["Commandstats", "Errorstats", "Threads", "Latencystats"] {
        assert!(
            headers.contains(&section),
            "INFO all missing section: {section}"
        );
    }
    assert!(
        !headers.contains(&"Modules"),
        "INFO all must not include Modules"
    );
}

#[tokio::test]
async fn info_everything_includes_modules() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let text = info_text(
        router
            .execute("INFO", &[b("everything")], &mut db)
            .await
            .unwrap(),
    );
    assert!(text.contains("# Modules"));
}

#[tokio::test]
async fn info_memory_includes_rss_and_maxmemory() {
    let (router, shared) = router_with_shared();
    let mut db = 0;
    shared.refresh_runtime_metrics().await;
    let text = info_text(
        router
            .execute("INFO", &[b("memory")], &mut db)
            .await
            .unwrap(),
    );
    assert!(text.contains("used_memory_rss:"));
    assert!(text.contains("maxmemory:"));
    assert!(text.contains("mem_fragmentation_ratio:"));
}

#[tokio::test]
async fn info_stats_includes_redis88_slowlog_globals() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let text = info_text(
        router
            .execute("INFO", &[b("stats")], &mut db)
            .await
            .unwrap(),
    );
    assert!(text.contains("slowlog_commands_count:"));
    assert!(text.contains("slowlog_commands_time_ms_sum:"));
    assert!(text.contains("slowlog_commands_time_ms_max:"));
    assert!(text.contains("instantaneous_input_kbps:"));
    assert!(text.contains("instantaneous_output_kbps:"));
}

#[tokio::test]
async fn info_persistence_uses_redis_bgsave_time_sec_key() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    let text = info_text(
        router
            .execute("INFO", &[b("persistence")], &mut db)
            .await
            .unwrap(),
    );
    assert!(text.contains("rdb_last_bgsave_time_sec:"));
    assert!(!text.contains("rdb_last_bgsave_time:"));
}

#[tokio::test]
async fn info_clients_includes_maxclients() {
    let (router, shared) = router_with_shared();
    let mut db = 0;
    let text = info_text(
        router
            .execute("INFO", &[b("clients")], &mut db)
            .await
            .unwrap(),
    );
    assert_eq!(
        info_field(&text, "maxclients"),
        Some(shared.connection_config.max_clients as u64)
    );
    assert!(text.contains("blocked_clients:0"));
}

#[tokio::test]
async fn info_keyspace_reports_estimate_and_expires() {
    let (router, shared) = router_with_shared();
    let mut db = 0;
    router
        .execute("SET", &[b("ttlkey"), b("v")], &mut db)
        .await
        .unwrap();
    router
        .execute("PEXPIRE", &[b("ttlkey"), b("60000")], &mut db)
        .await
        .unwrap();

    let text = info_text(
        router
            .execute("INFO", &[b("keyspace")], &mut db)
            .await
            .unwrap(),
    );
    assert!(info_has_keyspace_contract(&text), "keyspace text: {text}");

    let sample = shared
        .storage
        .sample_avg_ttl(0)
        .await
        .expect("sample_avg_ttl");
    assert!(sample > 50_000 && sample <= 60_000, "sample={sample}");
    assert!(!text.contains(&sample.to_string()));
}
