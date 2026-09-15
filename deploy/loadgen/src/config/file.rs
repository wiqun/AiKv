//! 从本机 loadgen.toml 加载表单默认值与侧栏预设; 没有该文件则用内置 example.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{ConfigError, Mix, TargetMode, WorkloadConfig};

const SHIPPED_TOML: &str = include_str!("../../loadgen.example.toml");

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Preset {
    pub id: String,
    pub title: String,
    #[serde(alias = "desc")]
    pub description: String,
    pub config: WorkloadConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedUi {
    pub config: WorkloadConfig,
    pub presets: Vec<Preset>,
}

#[derive(Debug)]
pub enum ConfigSource {
    Missing,
    Path(PathBuf),
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileRoot {
    server: FileServer,
    stress: FileStress,
    keyspace: FileKeyspace,
    commands: Option<Mix>,
    /// None = 继承内置 6 套; Some = 用文件列表 (可为空).
    presets: Option<Vec<FilePreset>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileServer {
    endpoint: Option<String>,
    mode: Option<TargetMode>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileStress {
    #[serde(alias = "ops")]
    target_ops: Option<u64>,
    #[serde(alias = "client")]
    connections: Option<u32>,
    pipeline: Option<u32>,
    #[serde(alias = "timeout_ms")]
    timeout: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileKeyspace {
    #[serde(alias = "key_prefix")]
    prefix: Option<String>,
    #[serde(alias = "keyspace")]
    size: Option<u64>,
    use_hashtag: Option<bool>,
    target_slot: Option<u16>,
    value_size_min: Option<u32>,
    value_size_max: Option<u32>,
    ttl_ratio: Option<f64>,
    ttl_seconds: Option<u64>,
    miss_ratio: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePreset {
    id: String,
    title: String,
    #[serde(default, alias = "desc")]
    description: String,
    #[serde(default)]
    server: FileServer,
    #[serde(default)]
    stress: FileStress,
    #[serde(default)]
    keyspace: FileKeyspace,
    #[serde(default)]
    commands: Option<Mix>,
}

impl FileServer {
    fn apply_to(&self, next: &mut WorkloadConfig) {
        if let Some(v) = &self.endpoint {
            next.endpoint = v.clone();
        }
        if let Some(v) = self.mode {
            next.mode = v;
        }
    }
}

impl FileStress {
    fn apply_to(&self, next: &mut WorkloadConfig) {
        if let Some(v) = self.target_ops {
            next.target_ops = v;
        }
        if let Some(v) = self.connections {
            next.connections = v;
        }
        if let Some(v) = self.pipeline {
            next.pipeline = v;
        }
        if let Some(v) = self.timeout {
            next.timeout_ms = v;
        }
    }
}

impl FileKeyspace {
    fn apply_to(&self, next: &mut WorkloadConfig) {
        if let Some(v) = &self.prefix {
            next.key_prefix = v.clone();
        }
        if let Some(v) = self.size {
            next.keyspace = v;
        }
        if let Some(v) = self.use_hashtag {
            next.use_hashtag = v;
        }
        if let Some(v) = self.target_slot {
            next.target_slot = Some(v);
        }
        if let Some(v) = self.value_size_min {
            next.value_size_min = v;
        }
        if let Some(v) = self.value_size_max {
            next.value_size_max = v;
        }
        if let Some(v) = self.ttl_ratio {
            next.ttl_ratio = v;
        }
        if let Some(v) = self.ttl_seconds {
            next.ttl_seconds = v;
        }
        if let Some(v) = self.miss_ratio {
            next.miss_ratio = v;
        }
    }
}

impl FileRoot {
    fn apply(&self, base: &WorkloadConfig) -> WorkloadConfig {
        let mut next = base.clone();
        self.server.apply_to(&mut next);
        self.stress.apply_to(&mut next);
        self.keyspace.apply_to(&mut next);
        if let Some(mix) = &self.commands {
            next.mix = mix.clone();
        }
        next.running = false;
        if next.target_slot.is_some() {
            next.use_hashtag = true;
        }
        next
    }
}

impl FilePreset {
    fn apply(&self, base: &WorkloadConfig) -> WorkloadConfig {
        let mut next = base.clone();
        self.server.apply_to(&mut next);
        self.stress.apply_to(&mut next);
        self.keyspace.apply_to(&mut next);
        if let Some(mix) = &self.commands {
            next.mix = mix.clone();
        }
        next.running = false;
        if next.target_slot.is_some() {
            next.use_hashtag = true;
        }
        next
    }
}

fn parse_root(text: &str) -> Result<FileRoot, ConfigError> {
    toml::from_str(text).map_err(|err| ConfigError::new(format!("配置文件解析失败: {err}")))
}

fn builtin_root() -> FileRoot {
    parse_root(SHIPPED_TOML).expect("仓库 loadgen.example.toml 必须能解析")
}

fn build_loaded(root: FileRoot) -> Result<LoadedUi, ConfigError> {
    let config = root.apply(&WorkloadConfig::default());
    config.validate()?;
    let specs = match root.presets {
        Some(list) => list,
        None => builtin_root().presets.unwrap_or_default(),
    };
    let mut seen = HashSet::new();
    let mut presets = Vec::with_capacity(specs.len());
    for spec in specs {
        if spec.id.trim().is_empty() {
            return Err(ConfigError::new("预设 id 不能为空"));
        }
        if spec.title.trim().is_empty() {
            return Err(ConfigError::new(format!(
                "预设 {} 的 title 不能为空",
                spec.id
            )));
        }
        if !seen.insert(spec.id.clone()) {
            return Err(ConfigError::new(format!("预设 id 重复: {}", spec.id)));
        }
        let mut preset_cfg = spec.apply(&config);
        preset_cfg.running = false;
        preset_cfg.validate()?;
        presets.push(Preset {
            id: spec.id,
            title: spec.title,
            description: spec.description,
            config: preset_cfg,
        });
    }
    Ok(LoadedUi { config, presets })
}

/// 编译进二进制的默认值 (与 loadgen.example.toml 同步).
pub fn code_defaults() -> LoadedUi {
    load_from_str(SHIPPED_TOML).expect("仓库 loadgen.example.toml 必须通过校验")
}

pub fn load_from_str(text: &str) -> Result<LoadedUi, ConfigError> {
    build_loaded(parse_root(text)?)
}

pub fn load_from_path(path: &Path) -> Result<LoadedUi, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| ConfigError::new(format!("无法读取 {}: {err}", path.display())))?;
    load_from_str(&text).map_err(|err| ConfigError::new(format!("{}: {err}", path.display())))
}

pub fn resolve_config_path(
    explicit: Option<PathBuf>,
    cwd: &Path,
) -> Result<ConfigSource, ConfigError> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(ConfigError::new(format!(
                "配置文件不存在: {}",
                path.display()
            )));
        }
        return Ok(ConfigSource::Path(path));
    }
    let cwd_file = cwd.join("loadgen.toml");
    if cwd_file.is_file() {
        return Ok(ConfigSource::Path(cwd_file));
    }
    Ok(ConfigSource::Missing)
}

pub fn load_ui(explicit: Option<PathBuf>, cwd: &Path) -> Result<LoadedUi, ConfigError> {
    match resolve_config_path(explicit, cwd)? {
        ConfigSource::Missing => Ok(code_defaults()),
        ConfigSource::Path(path) => load_from_path(&path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_uses_code_defaults() {
        let loaded = load_ui(None, Path::new("/tmp/no-such-loadgen-dir")).unwrap();
        assert_eq!(loaded.config, WorkloadConfig::default());
        assert_eq!(loaded.presets.len(), 6);
        let ids: Vec<_> = loaded.presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "balanced",
                "read_heavy",
                "write_heavy",
                "hotspot",
                "cache_miss",
                "max_throughput"
            ]
        );
    }

    #[test]
    fn partial_defaults_override_only_ops() {
        let text = "[stress]\ntarget_ops = 1234\n";
        let loaded = load_from_str(text).unwrap();
        assert_eq!(loaded.config.target_ops, 1234);
        assert_eq!(loaded.config.connections, 8);
        assert_eq!(loaded.config.mix, Mix::default());
        assert_eq!(loaded.presets.len(), 6);
        assert_eq!(loaded.presets[0].config.target_ops, 8000);
    }

    #[test]
    fn preset_mix_replaces_whole_table() {
        let text = r#"
[[presets]]
id = "onlykv"
title = "只要 KV"
commands.get = 50
commands.set = 50
"#;
        let loaded = load_from_str(text).unwrap();
        assert_eq!(loaded.presets.len(), 1);
        let preset = loaded.presets.iter().find(|p| p.id == "onlykv").unwrap();
        assert_eq!(preset.config.mix.get, 50);
        assert_eq!(preset.config.mix.set, 50);
        assert_eq!(preset.config.mix.expire, 0);
        assert_eq!(preset.config.mix.del, 0);
    }

    #[test]
    fn unknown_mix_command_fails() {
        let text = "[commands]\nkeys = 1\nget = 1\n";
        let err = load_from_str(text).unwrap_err().to_string();
        assert!(err.contains("keys") || err.contains("未知"), "{err}");
    }

    #[test]
    fn duplicate_preset_id_fails() {
        let text = r#"
[[presets]]
id = "a"
title = "A"
[[presets]]
id = "a"
title = "B"
"#;
        assert!(load_from_str(text).is_err());
    }

    #[test]
    fn unknown_field_fails() {
        let text = "[stress]\nnope = 1\n";
        assert!(load_from_str(text).is_err());
    }

    #[test]
    fn explicit_missing_path_fails() {
        let err = resolve_config_path(
            Some(PathBuf::from("/tmp/definitely-missing-loadgen.toml")),
            Path::new("/tmp"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("不存在") || err.contains("找不到"), "{err}");
    }

    #[test]
    fn hotspot_sets_hashtag_from_slot() {
        let loaded = code_defaults();
        let hot = loaded.presets.iter().find(|p| p.id == "hotspot").unwrap();
        assert_eq!(hot.config.target_slot, Some(10438));
        assert!(hot.config.use_hashtag);
    }

    #[test]
    fn shipped_toml_matches_code_defaults() {
        let from_file = load_from_str(SHIPPED_TOML).unwrap();
        let from_code = code_defaults();
        assert_eq!(from_file, from_code);
    }

    #[test]
    fn empty_presets_array_is_allowed() {
        let loaded = load_from_str("presets = []\n").unwrap();
        assert!(loaded.presets.is_empty());
        assert_eq!(loaded.config, WorkloadConfig::default());
    }
}
