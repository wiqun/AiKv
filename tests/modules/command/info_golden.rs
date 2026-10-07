//! Redis 7 INFO 字段存在性 golden 测试 (Phase 4).
//!
//! 注意: 本模块所在的 `tests/commands.rs` binary 内禁止任何测试调用
//! `CLUSTER_STATE_MGR.set(...)` (OnceLock 一次性, 会永久污染同进程其他测试).
//! 集群状态相关 golden 测试须放独立 binary: `tests/cluster_info_golden.rs`.

use super::helpers::{b, router_with_shared};

const INFO_P0_FIELDS: &str = include_str!("../../fixtures/redis88_info_p0_fields.txt");

fn info_text(resp: aikv::protocol::RespValue) -> String {
    let aikv::protocol::RespValue::BulkString(Some(text)) = resp else {
        panic!("expected bulk string");
    };
    String::from_utf8_lossy(&text).into_owned()
}

fn parse_field_list(raw: &str) -> Vec<String> {
    raw.lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| line.trim().to_string())
        .collect()
}

fn info_has_field(text: &str, field: &str) -> bool {
    let prefix = format!("{field}:");
    text.lines().any(|line| line.starts_with(&prefix))
}

#[tokio::test]
async fn info_redis88_all_p0_fields_present() {
    let (router, shared) = router_with_shared();
    let mut db = 0;

    router
        .execute("SET", &[b("golden"), b("v")], &mut db)
        .await
        .unwrap();
    router
        .execute("GET", &[b("golden")], &mut db)
        .await
        .unwrap();
    shared.refresh_runtime_metrics().await;

    let text = info_text(router.execute("INFO", &[b("all")], &mut db).await.unwrap());
    let fields = parse_field_list(INFO_P0_FIELDS);
    let missing: Vec<_> = fields
        .iter()
        .filter(|field| !info_has_field(&text, field))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "INFO all missing Redis 8.8 P0 fields: {}",
        missing.join(", ")
    );
    assert!(text.contains("keys_estimate:"), "keyspace estimate missing");
    assert!(!text.contains("keys="));
}

#[tokio::test]
async fn info_all_extra_sections_present() {
    let (router, _shared) = router_with_shared();
    let mut db = 0;
    router
        .execute("SET", &[b("k"), b("v")], &mut db)
        .await
        .unwrap();

    let text = info_text(router.execute("INFO", &[b("all")], &mut db).await.unwrap());
    for section in ["Commandstats", "Errorstats", "Threads", "Latencystats"] {
        assert!(
            text.contains(&format!("# {section}")),
            "missing section header: {section}"
        );
    }
}

#[tokio::test]
async fn info_everything_includes_modules_section() {
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

const INFO_FULL_FIELDS: &str = include_str!("../../fixtures/redis88_info_full_fields.txt");

#[tokio::test]
async fn info_full_fields_present_in_everything() {
    let (router, shared) = router_with_shared();
    let mut db = 0;
    router
        .execute("SET", &[b("golden"), b("v")], &mut db)
        .await
        .unwrap();
    shared.refresh_runtime_metrics().await;

    let text = info_text(
        router
            .execute("INFO", &[b("everything")], &mut db)
            .await
            .unwrap(),
    );
    let fields = parse_field_list(INFO_FULL_FIELDS);
    let missing: Vec<_> = fields
        .iter()
        .filter(|field| !info_has_field(&text, field))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "INFO everything missing Redis 8.8 fields: {}",
        missing.join(", ")
    );
}
