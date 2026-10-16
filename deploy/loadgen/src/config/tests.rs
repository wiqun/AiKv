//! config 模块单测.

use super::*;

#[test]
fn defaults_are_valid() {
    WorkloadConfig::default().validate().unwrap();
}

#[test]
fn rejects_out_of_range_connections() {
    for bad in [0u32, MAX_CONNECTIONS + 1] {
        let cfg = WorkloadConfig {
            connections: bad,
            ..Default::default()
        };
        assert!(cfg.validate().is_err(), "connections={bad} 应被拒绝");
    }
}

#[test]
fn rejects_out_of_range_pipeline_and_timeout() {
    let cfg = WorkloadConfig {
        pipeline: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        pipeline: MAX_PIPELINE + 1,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        timeout_ms: 0,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn rejects_value_size_crossing() {
    let cfg = WorkloadConfig {
        value_size_min: 200,
        value_size_max: 100,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn rejects_invalid_ratio() {
    let cfg = WorkloadConfig {
        ttl_ratio: 1.5,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        miss_ratio: -0.1,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        ttl_ratio: f64::NAN,
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn rejects_all_zero_mix() {
    let cfg = WorkloadConfig::default();
    let zero = Mix {
        set: 0,
        get: 0,
        del: 0,
        mget: 0,
        incr: 0,
        expire: 0,
        extra: Default::default(),
    };
    assert!(WorkloadConfig { mix: zero, ..cfg }.validate().is_err());
}

#[test]
fn rejects_bad_prefix() {
    let cfg = WorkloadConfig {
        key_prefix: String::new(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        key_prefix: "a b".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        key_prefix: "x{y".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn rejects_bad_endpoint() {
    let cfg = WorkloadConfig {
        endpoint: "".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        endpoint: "127.0.0.1".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        endpoint: "127.0.0.1:0".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
    let cfg = WorkloadConfig {
        endpoint: "127.0.0.1:70000".to_string(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}

#[test]
fn patch_only_touches_given_fields() {
    let base = WorkloadConfig::default();
    let patch: ConfigPatch = serde_json::from_str(r#"{"target_ops": 5000}"#).unwrap();
    let next = base.patched(&patch).unwrap();
    assert_eq!(next.target_ops, 5000);
    assert_eq!(next.connections, base.connections);
    assert_eq!(next.mix, base.mix);
    assert_eq!(next.running, base.running);
}

#[test]
fn patch_rejects_invalid_value_and_keeps_base() {
    let base = WorkloadConfig::default();
    let patch: ConfigPatch = serde_json::from_str(r#"{"connections": 0}"#).unwrap();
    assert!(base.patched(&patch).is_err());
    assert_eq!(base.connections, 8);
}

#[test]
fn patch_rejects_unknown_field() {
    let parsed = serde_json::from_str::<ConfigPatch>(r#"{"target_ops_typo": 1}"#);
    assert!(parsed.is_err(), "未知字段必须被拒绝");
}

#[test]
fn endpoint_parsing() {
    assert_eq!(
        parse_endpoint("127.0.0.1:6379").unwrap(),
        ("127.0.0.1".to_string(), 6379)
    );
    assert!(parse_endpoint("127.0.0.1").is_err());
    assert!(parse_endpoint(":6379").is_err());
    assert!(parse_endpoint("127.0.0.1:0").is_err());
    assert!(parse_endpoint("127.0.0.1:70000").is_err());
}

#[test]
fn rejects_invalid_target_slot_and_ttl_seconds() {
    let bad_slot = WorkloadConfig {
        target_slot: Some(16_384),
        ..Default::default()
    };
    assert!(bad_slot.validate().is_err());

    let good_slot = WorkloadConfig {
        target_slot: Some(16_383),
        ..Default::default()
    };
    assert!(good_slot.validate().is_ok());

    let zero_ttl = WorkloadConfig {
        ttl_seconds: 0,
        ..Default::default()
    };
    assert!(zero_ttl.validate().is_err());

    let good_ttl = WorkloadConfig {
        ttl_seconds: 300,
        ..Default::default()
    };
    assert!(good_ttl.validate().is_ok());
}

#[test]
fn patch_target_slot_and_ttl_seconds() {
    let base = WorkloadConfig::default();
    let patch: ConfigPatch =
        serde_json::from_str(r#"{"target_slot": 1024, "ttl_seconds": 120}"#).unwrap();
    let next = base.patched(&patch).unwrap();
    assert_eq!(next.target_slot, Some(1024));
    assert_eq!(next.ttl_seconds, 120);

    let clear_patch: ConfigPatch = serde_json::from_str(r#"{"target_slot": null}"#).unwrap();
    let restored = next.patched(&clear_patch).unwrap();
    assert_eq!(restored.target_slot, None);
}

#[test]
fn mix_accepts_extended_commands() {
    let patch: ConfigPatch = serde_json::from_str(
        r#"{"mix":{"hget":40,"hset":60,"json_get":10,"set":0,"get":0,"del":0,"mget":0,"incr":0,"expire":0}}"#,
    )
    .unwrap();
    let next = WorkloadConfig::default().patched(&patch).unwrap();
    assert_eq!(next.mix.weight("hget"), 40);
    assert_eq!(next.mix.weight("json_get"), 10);
    assert_eq!(next.mix.set, 0);
}

#[test]
fn mix_rejects_destructive_commands() {
    let patch: ConfigPatch = serde_json::from_str(r#"{"mix":{"flushall":1}}"#).unwrap();
    let err = WorkloadConfig::default().patched(&patch).unwrap_err();
    assert!(err.to_string().contains("flushall"));
}
