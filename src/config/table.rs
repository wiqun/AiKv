//! 通用 TOML 表深度合并、点分路径覆盖、字节单位解析与配置扁平化工具.

use std::collections::{BTreeMap, HashMap};

use super::settings::ConfigError;

/// 需要支持带单位字节字符串 (如 `"128MiB"`, `"64K"`) 的配置字段名.
const BYTE_SIZE_FIELDS: &[&str] = &[
    "memtable_size",
    "block_size",
    "block_cache_size",
    "row_cache_size",
    "max_wal_size",
    "max_bytes_for_level_base",
    "subcompaction_min_size",
    "max_manifest_size",
    "proto_max_bulk_len",
    "proto_max_buffer_size",
    "memory_limit_bytes",
    "grpc_max_message_size",
    "max_entry_size",
    "snapshot_size_threshold",
    "max_bytes",
];

/// 递归深度合并 `overlay` 到 `base` (`Table` 递归合并, 其余标量/数组直接替换).
pub fn deep_merge(base: &mut toml::Table, overlay: toml::Table) {
    for (k, v) in overlay {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(base_sub)), toml::Value::Table(overlay_sub)) => {
                deep_merge(base_sub, overlay_sub);
            }
            (_, val) => {
                base.insert(k, val);
            }
        }
    }
}

/// 按点分路径 (`a.b.c`) 写入 `toml::Table`, 自动创建缺失的中间子表.
pub fn set_dotted_path(
    root: &mut toml::Table,
    path: &str,
    value: toml::Value,
    layer: &'static str,
) -> Result<(), ConfigError> {
    let parts: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(ConfigError::Field {
            layer,
            field: "option",
            message: format!("empty config path: '{path}'"),
        });
    }
    let mut cur = root;
    for &seg in &parts[..parts.len() - 1] {
        let entry = cur
            .entry(seg.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        cur = entry.as_table_mut().expect("table checked above");
    }
    cur.insert(parts[parts.len() - 1].to_string(), value);
    Ok(())
}

/// 解析带 IEC/SI 单位的字节大小字符串 (如 `"128MiB"`, `"64M"`, `"16KiB"`, `"16K"`, `"2G"`, `"1048576"`).
pub fn parse_byte_size(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(v) = s.parse::<u64>() {
        return Some(v);
    }
    let lower = s.to_ascii_lowercase();
    let suffixes: &[(&str, u64)] = &[
        ("kib", 1024),
        ("mib", 1024 * 1024),
        ("gib", 1024 * 1024 * 1024),
        ("tib", 1024 * 1024 * 1024 * 1024),
        ("kb", 1024),
        ("mb", 1024 * 1024),
        ("gb", 1024 * 1024 * 1024),
        ("tb", 1024 * 1024 * 1024 * 1024),
        ("k", 1024),
        ("m", 1024 * 1024),
        ("g", 1024 * 1024 * 1024),
        ("t", 1024 * 1024 * 1024 * 1024),
        ("b", 1),
    ];
    for &(suf, mult) in suffixes {
        if let Some(num_str) = lower.strip_suffix(suf) {
            let num = num_str.trim().parse::<u64>().ok()?;
            return num.checked_mul(mult);
        }
    }
    None
}

/// 将 `table` 中已知大小字段的字符串值归一化为 `toml::Value::Integer`.
pub fn normalize_byte_sizes(table: &mut toml::Table) -> Result<(), ConfigError> {
    for (k, v) in table.iter_mut() {
        match v {
            toml::Value::Table(sub) => normalize_byte_sizes(sub)?,
            toml::Value::String(s) if BYTE_SIZE_FIELDS.contains(&k.as_str()) => {
                let bytes = parse_byte_size(s).ok_or_else(|| ConfigError::Field {
                    layer: "validation",
                    field: "byte_size",
                    message: format!("invalid byte size for '{k}': '{s}'"),
                })?;
                let i = i64::try_from(bytes).map_err(|_| ConfigError::Field {
                    layer: "validation",
                    field: "byte_size",
                    message: format!("byte size overflow for '{k}': '{s}'"),
                })?;
                *v = toml::Value::Integer(i);
            }
            _ => {}
        }
    }
    Ok(())
}

/// 智能解析 CLI `-o key=value` 或通用双下划线 env 的值.
pub fn parse_scalar_value(raw: &str) -> toml::Value {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("true") {
        return toml::Value::Boolean(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return toml::Value::Boolean(false);
    }
    if let Ok(i) = s.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() {
            return toml::Value::Float(f);
        }
    }
    toml::Value::String(s.to_string())
}

/// 将 `AIKV_<SECTION>__<FIELD>` 形式 (含双下划线 `__`) 的环境变量写入配置表.
pub fn apply_generic_env(root: &mut toml::Table, vars: &HashMap<String, String>) {
    for (k, v) in vars {
        if v.is_empty() {
            continue;
        }
        let Some(rest) = k.strip_prefix("AIKV_") else {
            continue;
        };
        if !rest.contains("__") {
            continue;
        }
        let path = rest.to_ascii_lowercase().replace("__", ".");
        let val = parse_scalar_value(v);
        let _ = set_dotted_path(root, &path, val, "env");
    }
}

/// 将 `ResolvedSettings` 序列化后的 `toml::Table` 扁平化为点分路径键值映射.
pub fn flatten_table(prefix: &str, table: &toml::Table, out: &mut BTreeMap<String, String>) {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(sub) => flatten_table(&key, sub, out),
            toml::Value::Array(arr) => {
                let joined: Vec<String> = arr.iter().map(format_toml_scalar).collect();
                out.insert(key, joined.join(","));
            }
            other => {
                out.insert(key, format_toml_scalar(other));
            }
        }
    }
}

fn format_toml_scalar(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        other => other.to_string(),
    }
}
