//! 运行时配置模型: 不可变快照 + 局部更新补丁 + 严格校验.

use std::collections::BTreeMap;
use std::fmt;

use clap::ValueEnum;
use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// 目标拓扑: 单机直连或集群路由.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum TargetMode {
    Single,
    Cluster,
}

/// 命令混合权重 (自动归一化, 无需凑成 100).
/// 核心六字段保持兼容; 其余 aikv 客户端命令走 `extra` (如 `hget` / `json_get`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mix {
    #[serde(default)]
    pub set: u32,
    #[serde(default)]
    pub get: u32,
    #[serde(default)]
    pub del: u32,
    #[serde(default)]
    pub mget: u32,
    #[serde(default)]
    pub incr: u32,
    #[serde(default)]
    pub expire: u32,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, u32>,
}

impl Default for Mix {
    fn default() -> Self {
        Self {
            set: 40,
            get: 40,
            del: 5,
            mget: 10,
            incr: 3,
            expire: 2,
            extra: BTreeMap::new(),
        }
    }
}

impl Mix {
    pub fn zeros() -> Self {
        Self {
            set: 0,
            get: 0,
            del: 0,
            mget: 0,
            incr: 0,
            expire: 0,
            extra: BTreeMap::new(),
        }
    }

    pub fn weight(&self, mix_key: &str) -> u32 {
        match mix_key {
            "set" => self.set,
            "get" => self.get,
            "del" => self.del,
            "mget" => self.mget,
            "incr" => self.incr,
            "expire" => self.expire,
            other => self.extra.get(other).copied().unwrap_or(0),
        }
    }

    pub fn total(&self) -> u32 {
        self.set
            + self.get
            + self.del
            + self.mget
            + self.incr
            + self.expire
            + self.extra.values().copied().sum::<u32>()
    }
}

pub const MAX_CONNECTIONS: u32 = 1024;
pub const MAX_PIPELINE: u32 = 256;
pub const MAX_KEYSPACE: u64 = 100_000_000;
pub const MAX_VALUE_SIZE: u32 = 1_048_576;
pub const MAX_TIMEOUT_MS: u64 = 60_000;
pub const MAX_SLOT: u16 = 16_383;
pub const MAX_TTL_SECONDS: u64 = 86_400 * 365;

/// 校验错误: 直接携带面向用户的中文原因.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigError(String);

impl ConfigError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

mod file;
pub use file::{code_defaults, load_ui, LoadedUi, Preset};

/// 加压参数快照 (整体原子替换).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkloadConfig {
    pub mode: TargetMode,
    /// 目标地址 (`host:port`)
    pub endpoint: String,
    /// 目标总速率 (ops/s); 0 = 不限速尽力压
    pub target_ops: u64,
    /// worker 数 (每 worker 一条连接)
    pub connections: u32,
    /// 每轮提交的在途命令数
    pub pipeline: u32,
    pub keyspace: u64,
    pub key_prefix: String,
    /// true: key 包 `{}`, 全部压到同一 slot (定向单节点)
    pub use_hashtag: bool,
    /// 集中单槽时的目标槽位 (0..=16383); None 表示由 key_prefix 哈希决定
    pub target_slot: Option<u16>,
    pub value_size_min: u32,
    pub value_size_max: u32,
    /// SET 带 TTL 的比例
    pub ttl_ratio: f64,
    /// SET/EXPIRE 的 TTL 过期秒数
    pub ttl_seconds: u64,
    /// 读不存在 key 的比例
    pub miss_ratio: f64,
    pub mix: Mix,
    pub timeout_ms: u64,
    /// 引擎开关
    pub running: bool,
}

impl Default for WorkloadConfig {
    fn default() -> Self {
        Self {
            mode: TargetMode::Cluster,
            endpoint: "127.0.0.1:6379".to_string(),
            target_ops: 5_000,
            connections: 8,
            pipeline: 8,
            keyspace: 100_000,
            key_prefix: "loadgen".to_string(),
            use_hashtag: false,
            target_slot: None,
            value_size_min: 64,
            value_size_max: 256,
            ttl_ratio: 0.0,
            ttl_seconds: 60,
            miss_ratio: 0.1,
            mix: Mix::default(),
            timeout_ms: 1_000,
            running: false,
        }
    }
}

/// 局部更新补丁 (所有字段可选; 未知字段拒绝, 避免拼写错误静默失效).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigPatch {
    pub mode: Option<TargetMode>,
    pub endpoint: Option<String>,
    pub target_ops: Option<u64>,
    pub connections: Option<u32>,
    pub pipeline: Option<u32>,
    pub keyspace: Option<u64>,
    pub key_prefix: Option<String>,
    pub use_hashtag: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub target_slot: Option<Option<u16>>,
    pub value_size_min: Option<u32>,
    pub value_size_max: Option<u32>,
    pub ttl_ratio: Option<f64>,
    pub ttl_seconds: Option<u64>,
    pub miss_ratio: Option<f64>,
    pub mix: Option<Mix>,
    pub timeout_ms: Option<u64>,
    pub running: Option<bool>,
    /// 仅运行时有效: true = 用户暂停派发 (不停 worker).
    pub paused: Option<bool>,
}

impl ConfigPatch {
    fn apply_to(&self, cfg: &mut WorkloadConfig) {
        if let Some(v) = self.mode {
            cfg.mode = v;
        }
        if let Some(v) = &self.endpoint {
            cfg.endpoint = v.clone();
        }
        if let Some(v) = self.target_ops {
            cfg.target_ops = v;
        }
        if let Some(v) = self.connections {
            cfg.connections = v;
        }
        if let Some(v) = self.pipeline {
            cfg.pipeline = v;
        }
        if let Some(v) = self.keyspace {
            cfg.keyspace = v;
        }
        if let Some(v) = &self.key_prefix {
            cfg.key_prefix = v.clone();
        }
        if let Some(v) = self.use_hashtag {
            cfg.use_hashtag = v;
        }
        if let Some(v) = self.target_slot {
            cfg.target_slot = v;
        }
        if let Some(v) = self.value_size_min {
            cfg.value_size_min = v;
        }
        if let Some(v) = self.value_size_max {
            cfg.value_size_max = v;
        }
        if let Some(v) = self.ttl_ratio {
            cfg.ttl_ratio = v;
        }
        if let Some(v) = self.ttl_seconds {
            cfg.ttl_seconds = v;
        }
        if let Some(v) = self.miss_ratio {
            cfg.miss_ratio = v;
        }
        if let Some(v) = self.mix.clone() {
            cfg.mix = v;
        }
        if let Some(v) = self.timeout_ms {
            cfg.timeout_ms = v;
        }
        if let Some(v) = self.running {
            cfg.running = v;
        }
    }
}

impl WorkloadConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.endpoint.trim().is_empty() {
            return Err(ConfigError::new("endpoint 不能为空"));
        }
        parse_endpoint(&self.endpoint)?;
        if !(1..=MAX_CONNECTIONS).contains(&self.connections) {
            return Err(ConfigError::new(format!(
                "connections 必须在 1..={MAX_CONNECTIONS}"
            )));
        }
        if !(1..=MAX_PIPELINE).contains(&self.pipeline) {
            return Err(ConfigError::new(format!(
                "pipeline 必须在 1..={MAX_PIPELINE}"
            )));
        }
        if !(1..=MAX_KEYSPACE).contains(&self.keyspace) {
            return Err(ConfigError::new(format!(
                "keyspace 必须在 1..={MAX_KEYSPACE}"
            )));
        }
        if self.key_prefix.is_empty() || self.key_prefix.chars().any(char::is_whitespace) {
            return Err(ConfigError::new("key_prefix 不能为空且不能含空白字符"));
        }
        if self.key_prefix.contains('{') || self.key_prefix.contains('}') {
            return Err(ConfigError::new("key_prefix 不能包含大括号"));
        }
        if let Some(slot) = self.target_slot {
            if slot > MAX_SLOT {
                return Err(ConfigError::new(format!(
                    "target_slot 必须在 0..={MAX_SLOT}"
                )));
            }
        }
        if !(1..=MAX_VALUE_SIZE).contains(&self.value_size_min)
            || !(1..=MAX_VALUE_SIZE).contains(&self.value_size_max)
        {
            return Err(ConfigError::new(format!(
                "value_size 必须在 1..={MAX_VALUE_SIZE}"
            )));
        }
        if self.value_size_min > self.value_size_max {
            return Err(ConfigError::new("value_size_min 不能大于 value_size_max"));
        }
        if !(0.0..=1.0).contains(&self.ttl_ratio) {
            return Err(ConfigError::new("ttl_ratio 必须在 0.0..=1.0"));
        }
        if !(1..=MAX_TTL_SECONDS).contains(&self.ttl_seconds) {
            return Err(ConfigError::new(format!(
                "ttl_seconds 必须在 1..={MAX_TTL_SECONDS}"
            )));
        }
        if !(0.0..=1.0).contains(&self.miss_ratio) {
            return Err(ConfigError::new("miss_ratio 必须在 0.0..=1.0"));
        }
        if self.mix.total() == 0 {
            return Err(ConfigError::new("mix 权重不能全为 0"));
        }
        for key in self.mix.extra.keys() {
            if crate::catalog::is_forbidden(key) {
                return Err(ConfigError::new(format!(
                    "命令 {key} 会破坏发压连接或清空实例, 拒绝调度"
                )));
            }
            if crate::catalog::lookup(key).is_none() {
                return Err(ConfigError::new(format!("未知发压命令: {key}")));
            }
        }
        if !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(ConfigError::new(format!(
                "timeout_ms 必须在 1..={MAX_TIMEOUT_MS}"
            )));
        }
        Ok(())
    }

    /// 应用局部更新并整体校验; 失败时调用方保持原快照不变.
    pub fn patched(&self, patch: &ConfigPatch) -> Result<Self, ConfigError> {
        let mut next = self.clone();
        patch.apply_to(&mut next);
        next.validate()?;
        Ok(next)
    }
}

/// 解析 `host:port`, 返回 (host, port).
pub fn parse_endpoint(raw: &str) -> Result<(String, u16), ConfigError> {
    let (host, port) = raw
        .rsplit_once(':')
        .ok_or_else(|| ConfigError::new(format!("endpoint 缺少端口: {raw} (应为 host:port)")))?;
    if host.is_empty() {
        return Err(ConfigError::new(format!("endpoint 主机为空: {raw}")));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| ConfigError::new(format!("endpoint 端口非法: {raw}")))?;
    if port == 0 {
        return Err(ConfigError::new(format!("endpoint 端口不能为 0: {raw}")));
    }
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests;
