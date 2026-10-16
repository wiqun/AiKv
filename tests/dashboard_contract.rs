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
    assert!(latency
        .iter()
        .any(|expr| expr.contains("histogram_quantile(0.50")));
    assert!(latency
        .iter()
        .any(|expr| expr.contains("histogram_quantile(0.99")));
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
            assert!(
                !text.contains(forbidden),
                "{} has {forbidden}",
                path.display()
            );
        }
    }
    for name in [
        "storage-engine.json",
        "process-system.json",
        "cold-recovery.json",
    ] {
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
    assert!(
        read_amp.contains("aidb_block_read_bytes_total"),
        "{read_amp}"
    );
    assert!(read_amp.contains("$rate_window"), "{read_amp}");
    assert!(read_amp.contains("> 0"), "{read_amp}");

    let cache_hit = queries
        .iter()
        .find(|expr| expr.contains("aidb_block_cache_hits_total"))
        .expect("cache hit");
    assert!(cache_hit.contains("rate("), "{cache_hit}");
    assert!(cache_hit.contains("$rate_window"), "{cache_hit}");
    assert!(
        cache_hit.contains("aidb_block_cache_misses_total"),
        "{cache_hit}"
    );

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

#[test]
fn capacity_titles_say_estimate_and_process_local_ttl() {
    let dash = dashboard("aikv-capacity.json");
    let estimate = panel_by_title(&dash, "引擎条目估值");
    assert_eq!(estimate["targets"][0]["expr"], "aikv_keys_estimate");
    let estimate_note = estimate["description"].as_str().unwrap_or("");
    for needle in ["旧版本", "删除标记", "subkey", "Raft 日志", "compaction"] {
        assert!(
            estimate_note.contains(needle),
            "{estimate_note} missing {needle}"
        );
    }
    let expires = panel_by_title(&dash, "本进程登记的 TTL");
    assert_eq!(expires["targets"][0]["expr"], "sum by (db) (aikv_expires)");
    assert!(expires["description"]
        .as_str()
        .unwrap_or("")
        .contains("重启后不代表磁盘存量"));
    let queries = exprs(&dash);
    assert!(queries
        .iter()
        .any(|expr| expr == "rate(aikv_expired_keys_total[$rate_window])"));
    assert!(queries
        .iter()
        .any(|expr| expr == "sum(aidb_sstable_size_bytes)"));
    assert!(queries.iter().any(|expr| expr == "aikv_rss_bytes"));
    let rss = queries
        .iter()
        .position(|expr| expr == "aikv_rss_bytes")
        .unwrap();
    let _ = rss;
    let rss_panel = find_panel_with_expr(&dash, "aikv_rss_bytes");
    assert!(rss_panel["description"]
        .as_str()
        .unwrap_or("")
        .contains("RSS 不是数据集大小"));
    let raw = std::fs::read_to_string(dashboards_dir().join("aikv-capacity.json")).unwrap();
    assert!(!raw.contains("avg_ttl"));
    assert!(!raw.contains("DBSIZE"));
}

fn find_panel_with_expr<'a>(value: &'a Value, expr: &str) -> &'a Value {
    fn walk<'a>(value: &'a Value, expr: &str) -> Option<&'a Value> {
        match value {
            Value::Object(map) => {
                if map.get("targets").is_some() {
                    let mut queries = Vec::new();
                    collect_exprs(value, &mut queries);
                    if queries.iter().any(|item| item == expr) {
                        return Some(value);
                    }
                }
                for child in map.values() {
                    if let Some(found) = walk(child, expr) {
                        return Some(found);
                    }
                }
                None
            }
            Value::Array(items) => items.iter().find_map(|child| walk(child, expr)),
            _ => None,
        }
    }
    walk(value, expr).unwrap_or_else(|| panic!("no panel with {expr}"))
}

#[test]
fn cluster_status_is_one_row_per_group() {
    let dash = dashboard("aikv-cluster.json");
    let table = panel_by_title(&dash, "每个 group");
    assert_eq!(table["type"], "table");
    for target in table["targets"].as_array().expect("targets") {
        assert_eq!(target["instant"], true);
    }
    let mut table_exprs = Vec::new();
    collect_exprs(table, &mut table_exprs);
    let mut got = table_exprs.clone();
    got.sort();
    let mut expected = vec![
        "aidb_raft_role".to_string(),
        "aidb_raft_apply_lag_entries".to_string(),
        "aidb_raft_replication_lag_entries".to_string(),
        "aidb_raft_leader_contact_age_seconds".to_string(),
    ];
    expected.sort();
    assert_eq!(got, expected);

    let queries = exprs(&dash);
    for name in [
        "aidb_l0_files",
        "aidb_memtable_active_bytes",
        "aidb_write_stall_window_max_us",
    ] {
        assert!(queries.iter().any(|expr| expr == name), "missing {name}");
    }
    assert!(panel_descriptions(&dash)
        .iter()
        .any(|note| note.contains("没有 group 标签")));

    assert!(queries.iter().any(|expr| {
        expr == "histogram_quantile(0.99, sum by (le, group) (rate(aidb_propose_duration_seconds_bucket[$rate_window])))"
    }));
    assert!(queries
        .iter()
        .any(|expr| { expr == "sum(rate(aikv_redirects_total{type=\"moved\"}[$rate_window]))" }));
    assert!(queries
        .iter()
        .any(|expr| { expr == "sum(rate(aikv_redirects_total{type=\"ask\"}[$rate_window]))" }));
    let raw = std::fs::read_to_string(dashboards_dir().join("aikv-cluster.json")).unwrap();
    assert!(!raw.contains("type=\"MOVED\""));
    assert!(!raw.contains("type=\"ASK\""));
}

fn panel_descriptions(value: &Value) -> Vec<String> {
    let mut notes = Vec::new();
    fn walk(value: &Value, notes: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if map.get("targets").is_some() {
                    if let Some(note) = map.get("description").and_then(|item| item.as_str()) {
                        notes.push(note.to_string());
                    }
                }
                for child in map.values() {
                    walk(child, notes);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, notes);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut notes);
    notes
}

#[test]
fn connections_split_accept_errors_and_close_reasons() {
    let dash = dashboard("aikv-connections.json");
    let queries = exprs(&dash);
    for needle in [
        "rate(aikv_net_input_bytes_total[$rate_window])",
        "rate(aikv_net_output_bytes_total[$rate_window])",
        "sum by (errno) (rate(aikv_accept_errors_total[$rate_window]))",
        "sum by (reason) (rate(aikv_connection_closes_total[$rate_window]))",
        "rate(aikv_scan_entries_total[$rate_window])",
    ] {
        assert!(
            queries.iter().any(|expr| expr == needle),
            "missing {needle}"
        );
    }

    let mut names: Vec<String> = std::fs::read_dir(dashboards_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "aikv-capacity.json".to_string(),
            "aikv-cluster.json".to_string(),
            "aikv-connections.json".to_string(),
            "aikv-overview.json".to_string(),
            "aikv-troubleshoot.json".to_string(),
        ]
    );

    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/observability/README.md"),
    )
    .unwrap();
    let section = readme
        .split("## 6.")
        .nth(1)
        .unwrap()
        .split("## 7.")
        .next()
        .unwrap();
    for uid in [
        "aikv-overview",
        "aikv-troubleshoot",
        "aikv-capacity",
        "aikv-cluster",
        "aikv-connections",
    ] {
        assert!(section.contains(uid), "section 6 missing {uid}");
    }
    assert!(!section.contains("4 张"), "{section}");
}

#[test]
fn review_names_lag_columns_and_keeps_stall_and_duration_readable() {
    let cluster = dashboard("aikv-cluster.json");
    let table = panel_by_title(&cluster, "每个 group");
    let renames = organize_rename_targets(table);
    for name in ["apply lag", "replication lag", "leader contact age"] {
        assert!(renames.iter().any(|item| item == name), "missing {name}");
    }
    assert!(
        organize_excludes_value_one(table),
        "role gauge value 1 is still a column"
    );

    for name in ["aikv-overview.json", "aikv-cluster.json"] {
        let dash = dashboard(name);
        let stall = panel_by_title(&dash, "本周期最大停顿");
        assert_eq!(stall["fieldConfig"]["defaults"]["unit"], "µs", "{name}");
        let note = stall["description"].as_str().unwrap_or("");
        assert!(
            note.contains("0 表示这个导出周期没有停顿"),
            "{name}: {note}"
        );
    }

    let trouble = dashboard("aikv-troubleshoot.json");
    let bytes = find_panel_with_expr(
        &trouble,
        "rate(aidb_flush_written_bytes_total[$rate_window])",
    );
    assert!(panel_exprs(bytes)
        .iter()
        .all(|expr| !expr.contains("last_duration")));
    let durations = find_panel_with_expr(&trouble, "aidb_flush_last_duration_seconds");
    let duration_exprs = panel_exprs(durations);
    assert!(duration_exprs
        .iter()
        .any(|expr| expr == "aidb_compaction_last_duration_seconds"));
    assert!(duration_exprs.iter().all(|expr| !expr.contains("bytes")));
    assert_eq!(durations["fieldConfig"]["defaults"]["unit"], "s");
}

fn panel_exprs(panel: &Value) -> Vec<String> {
    let mut queries = Vec::new();
    collect_exprs(panel, &mut queries);
    queries
}

fn organize_rename_targets(panel: &Value) -> Vec<String> {
    let mut names = Vec::new();
    for step in panel["transformations"].as_array().into_iter().flatten() {
        if step["id"] != "organize" {
            continue;
        }
        if let Some(map) = step["options"]["renameByName"].as_object() {
            names.extend(
                map.values()
                    .filter_map(|item| item.as_str().map(str::to_string)),
            );
        }
    }
    names
}

fn organize_excludes_value_one(panel: &Value) -> bool {
    panel["transformations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|step| step["id"] == "organize")
        .any(|step| step["options"]["excludeByName"]["Value"] == true)
}
