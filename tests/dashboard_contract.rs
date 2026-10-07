//! @component aikv-observability
//! 看板 JSON 契约: 查询, 窗口和禁止序列.

use serde_json::Value;

fn dashboard(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("deploy/observability/config/grafana/dashboards")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}"));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {path:?}: {err}"))
}

fn exprs(dashboard: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_exprs(dashboard, &mut out);
    out
}

fn collect_exprs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(expr) = map.get("expr").and_then(Value::as_str) {
                out.push(expr.to_string());
            }
            for child in map.values() {
                collect_exprs(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_exprs(child, out);
            }
        }
        _ => {}
    }
}

fn panel_by_title<'a>(dashboard: &'a Value, title: &str) -> &'a Value {
    find_panel(dashboard, title).unwrap_or_else(|| panic!("missing panel {title}"))
}

fn find_panel<'a>(value: &'a Value, title: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if map.get("title").and_then(Value::as_str) == Some(title)
                && map.get("targets").is_some()
            {
                return Some(value);
            }
            for child in map.values() {
                if let Some(found) = find_panel(child, title) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|child| find_panel(child, title)),
        _ => None,
    }
}

const FORBIDDEN: &[&str] = &[
    "aidb_backup",
    "evicted_keys",
    "used_memory_lua",
    "instantaneous_ops",
    "aidb_write_stall_max",
    "avg_ttl",
    "pprof",
    "flamegraph",
];

fn dashboards_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("deploy/observability/config/grafana/dashboards")
}

#[test]
fn overview_latency_is_one_line_and_freshness_is_sample_age() {
    let dash = dashboard("aikv-overview.json");
    let latency: Vec<_> = exprs(&dash)
        .into_iter()
        .filter(|expr| expr.contains("aikv_command_duration_seconds"))
        .collect();
    assert!(latency.iter().any(|expr| expr.contains("histogram_quantile(0.50")));
    assert!(latency.iter().any(|expr| expr.contains("histogram_quantile(0.99")));
    for expr in &latency {
        assert!(expr.contains("sum by (le)"), "{expr}");
        assert!(expr.contains("$rate_window"), "{expr}");
        assert!(!expr.contains("by (command)"), "{expr}");
        assert!(!expr.contains("command="), "{expr}");
        assert!(!expr.contains("by (group)"), "{expr}");
    }

    let freshness = panel_by_title(&dash, "导出新鲜度");
    let freshness_expr = freshness["targets"][0]["expr"].as_str().unwrap();
    assert_eq!(freshness_expr, "time() - timestamp(aikv_process_up)");
    let thresholds = freshness.to_string();
    assert!(thresholds.contains("30"), "{thresholds}");
    let mut without_description = freshness.clone();
    if let Some(map) = without_description.as_object_mut() {
        map.remove("description");
    }
    assert!(
        !without_description.to_string().contains("health"),
        "{without_description}"
    );

    let raw = exprs(&dash).join("\n");
    for needle in [
        "sum by (status) (rate(aikv_commands_total[$rate_window]))",
        "status=\"error\"",
        "aikv_slowlog_total",
        "result=\"hit\"",
        "aikv_connections_current",
        "aikv_connections_rejected_total",
        "aidb_l0_files",
        "aidb_l0_compaction_trigger",
        "aidb_l0_slowdown_writes_trigger",
        "aidb_l0_stop_writes_trigger",
        "sum by (cause, kind)",
        "max(aidb_write_stall_window_max_us)",
    ] {
        assert!(raw.contains(needle), "missing {needle}");
    }
    let error_ratio = panel_by_title(&dash, "错误比例");
    assert!(error_ratio["description"]
        .as_str()
        .unwrap_or("")
        .contains("看连接看板"));

    let window = dash["templating"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "rate_window")
        .expect("rate_window");
    assert_eq!(window["current"]["value"], "5m");

    for entry in std::fs::read_dir(dashboards_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for forbidden in FORBIDDEN {
            assert!(!text.contains(forbidden), "{} has {forbidden}", path.display());
        }
    }
    for name in ["storage-engine.json", "process-system.json", "cold-recovery.json"] {
        assert!(!dashboards_dir().join(name).exists(), "{name} still exists");
    }
}

#[test]
fn troubleshoot_read_amp_skips_zero_and_cache_hit_uses_the_window() {
    let dash = dashboard("aikv-troubleshoot.json");
    let queries = exprs(&dash);
    let read_amp = queries
        .iter()
        .find(|expr| expr.contains("aidb_logical_read_bytes_total"))
        .expect("read amp");
    assert!(read_amp.contains("aidb_block_read_bytes_total"), "{read_amp}");
    assert!(read_amp.contains("$rate_window"), "{read_amp}");
    assert!(read_amp.contains("> 0"), "{read_amp}");

    let cache_hit = queries
        .iter()
        .find(|expr| expr.contains("aidb_block_cache_hits_total"))
        .expect("cache hit");
    assert!(cache_hit.contains("rate("), "{cache_hit}");
    assert!(cache_hit.contains("$rate_window"), "{cache_hit}");
    assert!(cache_hit.contains("aidb_block_cache_misses_total"), "{cache_hit}");

    let pending = panel_by_title(&dash, "compaction 待处理字节");
    let note = pending["description"].as_str().unwrap_or("");
    assert!(note.contains("L0 超出 trigger 的最老文件"), "{note}");
    assert!(note.contains("最底层不计入"), "{note}");

    let raw = serde_json::to_string(&dash).unwrap();
    assert!(raw.contains("aidb_flush_last_duration_seconds"));
    assert!(raw.contains("aidb_compaction_last_duration_seconds"));
    assert!(!raw.contains("aidb_flush_total"));
    assert!(!raw.contains("aidb_compaction_total"));
    for expr in &queries {
        if expr.contains("histogram_quantile") {
            assert!(!expr.contains("aidb_flush"), "{expr}");
            assert!(!expr.contains("aidb_compaction"), "{expr}");
        }
    }
    assert!(queries
        .iter()
        .any(|expr| expr.contains("histogram_quantile(0.99") && expr.contains("by (le, command)")));
}
