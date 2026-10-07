//! JSON.* 命令集合 (Phase 11): GET/MGET/SET/DEL/TYPE/STRLEN/ARRLEN/OBJLEN/NUMINCRBY/
//! ARRAPPEND/UPDATE/MSET. 文档以 `serde_json::Value` 序列化 bytes 存于 **String KV**
//! (`storage.get/set/set_with_ttl`, 非 typed 写路径); 非 String key → WRONGTYPE.
//!
//! # 写路径
//!
//! ```text
//! JSON.SET / JSON.DEL / JSON.NUMINCRBY / JSON.ARRAPPEND / JSON.UPDATE
//!   ├─ KeyLock lock(key)
//!   ├─ load_json: storage.get → serde_json 解析 (无 key → nil / 空文档)
//!   ├─ JsonPathEngine.set / delete / incr / append 修改内存文档
//!   └─ write_back_json: 先 get_typed 取 expires_at, 再 set / set_with_ttl 写回 (保留 TTL)
//! ```
//!
//! # Invariant
//!
//! - JSON 存 String KV: 子路径修改用 `write_back_json` 保留原 TTL; 全新 key 直接 `set`.
//! - JSON.MSET: 对 batch 内全部 key 取 `key_lock.lock_keys_sorted` (去重 + 字典序).
//! - 路径缺省 `$`; JSONPath 根 `$` 与 `.` 等价 (见 `jsonpath/`).
//! - 全文档 RMW: 无 RedisJSON 级内存优化, 大文档每次修改全量重读重写.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value as JsonValue};
use tracing::instrument;

use crate::command::jsonpath::JsonPathEngine;
use crate::command::router::{self, KeyLock};
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::ServerMetrics;
use crate::storage::{is_wrongtype, now_ms, KvStorage};

pub struct JsonCommands {
    storage: Arc<dyn KvStorage>,
    key_lock: Arc<KeyLock>,
    metrics: Option<Arc<ServerMetrics>>,
    path_engine: JsonPathEngine,
}

impl JsonCommands {
    pub fn new(storage: Arc<dyn KvStorage>, key_lock: Arc<KeyLock>) -> Self {
        Self {
            storage,
            key_lock,
            metrics: None,
            path_engine: JsonPathEngine,
        }
    }

    pub fn with_metrics(
        storage: Arc<dyn KvStorage>,
        key_lock: Arc<KeyLock>,
        metrics: Arc<ServerMetrics>,
    ) -> Self {
        Self {
            storage,
            key_lock,
            metrics: Some(metrics),
            path_engine: JsonPathEngine,
        }
    }

    fn record(&self, command: &str, ok: bool) {
        if let Some(m) = &self.metrics {
            m.on_json_command(command, ok);
        }
    }

    async fn write_back_json(&self, db: usize, key: &[u8], json_bytes: &[u8]) -> Result<()> {
        let expire_at = self
            .storage
            .get_typed(db, key)
            .await?
            .and_then(|s| s.expires_at);
        if let Some(at) = expire_at {
            self.storage.set_with_ttl(db, key, json_bytes, at).await?;
            self.storage.release_seen_ttl(db).await
        } else {
            self.storage.set(db, key, json_bytes).await
        }
    }

    fn parse_path(args: &[Bytes], idx: usize) -> String {
        if args.len() > idx {
            String::from_utf8_lossy(&args[idx]).to_string()
        } else {
            "$".to_string()
        }
    }

    async fn load_json(&self, db: usize, key: &[u8]) -> Result<Option<JsonValue>> {
        match self.storage.get(db, key).await {
            Ok(Some(raw)) => Ok(Some(
                serde_json::from_slice(&raw).map_err(Self::invalid_json)?,
            )),
            Ok(None) => Ok(None),
            Err(e) if is_wrongtype(&e) => Err(router::wrongtype()),
            Err(e) => Err(e),
        }
    }

    fn key_str(key: &[u8]) -> String {
        String::from_utf8_lossy(key).into_owned()
    }

    fn record_span_key(key: &[u8]) {
        tracing::Span::current().record("key", tracing::field::display(Self::key_str(key)));
    }

    fn record_span_path(path: &str) {
        tracing::Span::current().record("path", path);
    }

    fn debug_cmd(command: &str, key: &[u8], path: Option<&str>) {
        let key = Self::key_str(key);
        match path {
            Some(p) => tracing::debug!(target: "cmd.json", command, key = %key, path = %p),
            None => tracing::debug!(target: "cmd.json", command, key = %key),
        }
    }

    fn invalid_json(e: serde_json::Error) -> Error {
        Error::Command(format!("ERR invalid JSON: {e}"))
    }

    fn is_finite_number(v: &JsonValue) -> bool {
        v.as_f64().map(|n| n.is_finite()).unwrap_or(true)
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_get",
    skip(self, args),
    fields(cmd.name = "JSON.GET", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_get(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.GET", args, 1)?;
        let key = &args[0];
        let path = Self::parse_path(args, 1);
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("get", key, Some(&path));

        let result = async {
            let Some(json_val) = self.load_json(db, key).await? else {
                return Ok(router::nil_bulk());
            };

            let extracted = if path == "$" || path == "." {
                json_val
            } else {
                self.path_engine.extract(&json_val, &path)?
            };
            let json_string = serde_json::to_string(&extracted).map_err(Self::invalid_json)?;
            Ok(router::bulk(json_string.into_bytes()))
        }
        .await;

        self.record("get", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_mget",
    skip(self, args),
    fields(cmd.name = "JSON.MGET", key_count = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_mget(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.MGET", args, 2)?;
        let path = String::from_utf8_lossy(&args[args.len() - 1]).to_string();
        let keys = &args[..args.len() - 1];
        tracing::Span::current().record("key_count", keys.len());
        Self::record_span_path(&path);
        Self::debug_cmd("mget", &args[0], Some(&path));

        let result = async {
            let mut out = Vec::with_capacity(keys.len());
            for key in keys {
                let Some(json_val) = self.load_json(db, key).await? else {
                    out.push(RespValue::Null);
                    continue;
                };
                let extracted = if path == "$" || path == "." {
                    json_val
                } else {
                    match self.path_engine.extract(&json_val, &path) {
                        Ok(v) => v,
                        Err(_) => {
                            out.push(RespValue::Null);
                            continue;
                        }
                    }
                };
                let json_string = serde_json::to_string(&extracted).map_err(Self::invalid_json)?;
                out.push(router::bulk(json_string.into_bytes()));
            }
            Ok(RespValue::Array(Some(out)))
        }
        .await;

        self.record("mget", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_set",
    skip(self, args),
    fields(cmd.name = "JSON.SET", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_set(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.SET", args, 3)?;
        let key = &args[0];
        let path = String::from_utf8_lossy(&args[1]).to_string();
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("set", key, Some(&path));
        let value_str = String::from_utf8_lossy(&args[2]).to_string();

        let result = async {
            let mut nx = false;
            let mut xx = false;
            let mut xe = false;
            let mut expire_seconds: Option<u64> = None;

            for arg in args.iter().skip(3) {
                let raw = String::from_utf8_lossy(arg);
                let option = raw.to_ascii_uppercase();
                match option.as_str() {
                    "NX" => nx = true,
                    "XX" | "NN" => xx = true,
                    "XE" => xe = true,
                    _ => {
                        if let Ok(secs) = raw.parse::<u64>() {
                            if secs > 0 {
                                expire_seconds = Some(secs);
                            }
                        }
                    }
                }
            }

            if nx && (xx || xe) {
                return Err(Error::Command(
                    "ERR NX and XX/XE are mutually exclusive".into(),
                ));
            }

            let _lock = self.key_lock.lock(key).await;
            let exists = self.storage.exists(db, key).await?;

            if xe && exists {
                return Err(Error::Command("ERR Key already exists (XE)".into()));
            }
            if nx && exists {
                return Ok(router::nil_bulk());
            }
            if xx && !exists {
                return Ok(router::nil_bulk());
            }

            if path.contains("[?(@") && !matches!(path.as_str(), "$" | ".") {
                if let Some(ref existing) = self.storage.get(db, key).await? {
                    let json_check: JsonValue =
                        serde_json::from_slice(existing).map_err(Self::invalid_json)?;
                    let matched = self.path_engine.extract(&json_check, &path)?;
                    match &matched {
                        JsonValue::Array(arr) if arr.is_empty() => {
                            return Err(Error::Command(
                                "ERR No elements match the where condition".into(),
                            ));
                        }
                        JsonValue::Null => {
                            return Err(Error::Command(
                                "ERR No elements match the where condition".into(),
                            ));
                        }
                        _ => {}
                    }
                }
            }

            let new_value: JsonValue =
                serde_json::from_str(&value_str).map_err(Self::invalid_json)?;

            let result_json = if path == "$" || path == "." {
                new_value
            } else {
                let mut json_doc = match self.storage.get(db, key).await? {
                    Some(existing) => {
                        serde_json::from_slice(&existing).map_err(Self::invalid_json)?
                    }
                    None => json!({}),
                };
                self.path_engine.set(&mut json_doc, &path, new_value)?;
                json_doc
            };

            let json_bytes = serde_json::to_vec(&result_json).map_err(Self::invalid_json)?;
            if let Some(secs) = expire_seconds {
                let expire_at = now_ms().saturating_add(secs * 1000);
                self.storage
                    .set_with_ttl(db, key, &json_bytes, expire_at)
                    .await?;
            } else if path == "$" || path == "." {
                self.storage.set(db, key, &json_bytes).await?;
            } else {
                self.write_back_json(db, key, &json_bytes).await?;
            }

            Ok(router::ok())
        }
        .await;

        self.record("set", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_del",
    skip(self, args),
    fields(cmd.name = "JSON.DEL", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_del(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.DEL", args, 1)?;
        let key = &args[0];
        let path = Self::parse_path(args, 1);
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("del", key, Some(&path));

        let result = async {
            let _lock = self.key_lock.lock(key).await;
            if path == "$" || path == "." {
                let deleted = self.storage.delete(db, key).await?;
                return Ok(router::integer(i64::from(deleted)));
            }

            let Some(raw) = self.storage.get(db, key).await? else {
                return Ok(router::integer(0));
            };
            let mut json_val: JsonValue =
                serde_json::from_slice(&raw).map_err(Self::invalid_json)?;

            let count = self.path_engine.delete(&mut json_val, &path)?;
            if count > 0 {
                let json_bytes = serde_json::to_vec(&json_val).map_err(Self::invalid_json)?;
                self.write_back_json(db, key, &json_bytes).await?;
            }
            Ok(router::integer(count as i64))
        }
        .await;

        self.record("del", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_type",
    skip(self, args),
    fields(cmd.name = "JSON.TYPE", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_type(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.TYPE", args, 1)?;
        let key = &args[0];
        let path = Self::parse_path(args, 1);
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("type", key, Some(&path));

        let result = async {
            let Some(json_val) = self.load_json(db, key).await? else {
                return Ok(router::nil_bulk());
            };
            let target = if path == "$" || path == "." {
                json_val
            } else {
                match self.path_engine.extract(&json_val, &path) {
                    Ok(v) => v,
                    Err(_) => return Ok(router::nil_bulk()),
                }
            };
            let type_name = json_type_name(&target);
            Ok(RespValue::SimpleString(type_name.into()))
        }
        .await;

        self.record("type", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_strlen",
    skip(self, args),
    fields(cmd.name = "JSON.STRLEN", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_strlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.STRLEN", args, 1)?;
        let key = &args[0];
        let path = Self::parse_path(args, 1);
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("strlen", key, Some(&path));

        let result = async {
            let Some(json_val) = self.load_json(db, key).await? else {
                return Ok(router::nil_bulk());
            };
            let target = if path == "$" || path == "." {
                &json_val
            } else {
                match self.path_engine.extract(&json_val, &path) {
                    Ok(v) => {
                        return match &v {
                            JsonValue::String(s) => Ok(router::integer(s.len() as i64)),
                            _ => Ok(router::nil_bulk()),
                        };
                    }
                    Err(_) => return Ok(router::nil_bulk()),
                }
            };
            if let JsonValue::String(s) = target {
                Ok(router::integer(s.len() as i64))
            } else {
                Ok(router::nil_bulk())
            }
        }
        .await;

        self.record("strlen", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_arrlen",
    skip(self, args),
    fields(
      cmd.name = "JSON.ARRLEN",
      key = tracing::field::Empty,
      path = tracing::field::Empty,
      arr_length = tracing::field::Empty
    )
  )]
    pub async fn json_arrlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.ARRLEN", args, 1)?;
        let key = &args[0];
        let path = Self::parse_path(args, 1);
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("arrlen", key, Some(&path));

        let result = async {
            let Some(json_val) = self.load_json(db, key).await? else {
                return Ok(router::nil_bulk());
            };
            let target = if path == "$" || path == "." {
                json_val
            } else {
                match self.path_engine.extract(&json_val, &path) {
                    Ok(v) => v,
                    Err(_) => return Ok(router::nil_bulk()),
                }
            };
            if let JsonValue::Array(arr) = target {
                tracing::Span::current().record("arr_length", arr.len());
                Ok(router::integer(arr.len() as i64))
            } else {
                Ok(router::nil_bulk())
            }
        }
        .await;

        self.record("arrlen", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_objlen",
    skip(self, args),
    fields(cmd.name = "JSON.OBJLEN", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_objlen(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.OBJLEN", args, 1)?;
        let key = &args[0];
        let path = Self::parse_path(args, 1);
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("objlen", key, Some(&path));

        let result = async {
            let Some(json_val) = self.load_json(db, key).await? else {
                return Ok(router::nil_bulk());
            };
            let target = if path == "$" || path == "." {
                json_val
            } else {
                match self.path_engine.extract(&json_val, &path) {
                    Ok(v) => v,
                    Err(_) => return Ok(router::nil_bulk()),
                }
            };
            if let JsonValue::Object(obj) = target {
                Ok(router::integer(obj.len() as i64))
            } else {
                Ok(router::nil_bulk())
            }
        }
        .await;

        self.record("objlen", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_numincrby",
    skip(self, args),
    fields(cmd.name = "JSON.NUMINCRBY", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_numincrby(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("JSON.NUMINCRBY", args, 3)?;
        let key = &args[0];
        let path = String::from_utf8_lossy(&args[1]).to_string();
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("numincrby", key, Some(&path));
        let incr_str = String::from_utf8_lossy(&args[2]).to_string();

        let result = async {
            let incr: f64 = incr_str
                .parse()
                .map_err(|_| Error::Command(format!("ERR Invalid increment: {incr_str}")))?;
            if !incr.is_finite() {
                return Err(Error::Command("ERR increment must be finite".into()));
            }

            if path == "$" || path == "." {
                return Err(Error::Command("ERR Cannot increment root".into()));
            }

            let _lock = self.key_lock.lock(key).await;
            let Some(raw) = self.storage.get(db, key).await? else {
                return Ok(router::nil_bulk());
            };
            let mut json_val: JsonValue =
                serde_json::from_slice(&raw).map_err(Self::invalid_json)?;

            let path_normalized = path
                .trim_start_matches('$')
                .trim_start_matches('.')
                .to_string();
            let parts = JsonPathEngine::split_path_parts(&path_normalized);
            self.path_engine.incr(&mut json_val, &parts, incr)?;

            let result = self.path_engine.extract(&json_val, &path)?;
            if !Self::is_finite_number(&result) {
                return Err(Error::Command("ERR result is not a finite number".into()));
            }

            let json_bytes = serde_json::to_vec(&json_val).map_err(Self::invalid_json)?;
            self.write_back_json(db, key, &json_bytes).await?;
            let json_string = serde_json::to_string(&result).map_err(Self::invalid_json)?;
            Ok(router::bulk(json_string.into_bytes()))
        }
        .await;

        self.record("numincrby", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_arrappend",
    skip(self, args),
    fields(cmd.name = "JSON.ARRAPPEND", key = tracing::field::Empty, path = tracing::field::Empty)
  )]
    pub async fn json_arrappend(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.ARRAPPEND", args, 3)?;
        let key = &args[0];
        let path = String::from_utf8_lossy(&args[1]).to_string();
        Self::record_span_key(key);
        Self::record_span_path(&path);
        Self::debug_cmd("arrappend", key, Some(&path));

        let result = async {
            let mut values = Vec::new();
            for arg in args.iter().skip(2) {
                let val_str = String::from_utf8_lossy(arg);
                let val: JsonValue = serde_json::from_str(&val_str).map_err(Self::invalid_json)?;
                values.push(val);
            }

            if path == "$" || path == "." {
                return Err(Error::Command("ERR Cannot append to root".into()));
            }

            let _lock = self.key_lock.lock(key).await;
            let Some(raw) = self.storage.get(db, key).await? else {
                return Ok(router::nil_bulk());
            };
            let mut json_val: JsonValue =
                serde_json::from_slice(&raw).map_err(Self::invalid_json)?;

            let path_normalized = path
                .trim_start_matches('$')
                .trim_start_matches('.')
                .to_string();
            let parts = JsonPathEngine::split_path_parts(&path_normalized);
            self.path_engine.append(&mut json_val, &parts, &values)?;

            let extracted = self.path_engine.extract(&json_val, &path)?;
            let len = if let JsonValue::Array(arr) = extracted {
                arr.len() as i64
            } else {
                return Ok(router::nil_bulk());
            };

            let json_bytes = serde_json::to_vec(&json_val).map_err(Self::invalid_json)?;
            self.write_back_json(db, key, &json_bytes).await?;
            Ok(router::integer(len))
        }
        .await;

        self.record("arrappend", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_update",
    skip(self, args),
    fields(
      cmd.name = "JSON.UPDATE",
      key = tracing::field::Empty,
      where_path = tracing::field::Empty
    )
  )]
    pub async fn json_update(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("JSON.UPDATE", args, 4)?;
        let key = &args[0];
        let where_path = String::from_utf8_lossy(&args[1]).to_string();
        Self::record_span_key(key);
        tracing::Span::current().record("where_path", where_path.as_str());
        Self::debug_cmd("update", key, Some(&where_path));

        let result = async {
            let _lock = self.key_lock.lock(key).await;
            let exists = self.storage.exists(db, key).await?;
            if !exists {
                let last_arg = String::from_utf8_lossy(&args[args.len() - 1]).to_ascii_uppercase();
                if last_arg == "NN" {
                    return Ok(router::ok());
                }
                return Err(Error::Command("ERR Key does not exist".into()));
            }

            if !where_path.is_empty() && !matches!(where_path.as_str(), "$" | "." | "$[*]") {
                let raw = self.storage.get(db, key).await?.expect("exists");
                let json_check: JsonValue =
                    serde_json::from_slice(&raw).map_err(Self::invalid_json)?;
                let matched = self.path_engine.extract(&json_check, &where_path)?;
                match &matched {
                    JsonValue::Array(arr) if arr.is_empty() => {
                        let last_arg = String::from_utf8_lossy(&args[args.len() - 1]);
                        if last_arg.to_ascii_uppercase() == "NN" {
                            return Ok(router::ok());
                        }
                        return Err(Error::Command(
                            "ERR No elements match the where condition".into(),
                        ));
                    }
                    JsonValue::Null => {
                        return Err(Error::Command(
                            "ERR No elements match the where condition".into(),
                        ));
                    }
                    _ => {}
                }
            }

            let raw = self.storage.get(db, key).await?.expect("exists");
            let mut json_val: JsonValue =
                serde_json::from_slice(&raw).map_err(Self::invalid_json)?;

            let has_flag = {
                let last = String::from_utf8_lossy(&args[args.len() - 1]);
                last.is_empty() || last.to_ascii_uppercase() == "NN"
            };
            let end = if has_flag { args.len() - 1 } else { args.len() };

            let mut i = 2;
            while i + 1 < end {
                let path_str = String::from_utf8_lossy(&args[i]).to_string();
                let val_str = String::from_utf8_lossy(&args[i + 1]).to_string();
                let val: JsonValue = serde_json::from_str(&val_str).map_err(Self::invalid_json)?;
                self.path_engine.set(&mut json_val, &path_str, val)?;
                i += 2;
            }

            let json_bytes = serde_json::to_vec(&json_val).map_err(Self::invalid_json)?;
            self.write_back_json(db, key, &json_bytes).await?;
            Ok(router::ok())
        }
        .await;

        self.record("update", result.is_ok());
        result
    }

    #[instrument(
    level = "debug",
    name = "cmd_json_mset",
    skip(self, args),
    fields(cmd.name = "JSON.MSET", key_count = tracing::field::Empty)
  )]
    pub async fn json_mset(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.len() < 3 || !args.len().is_multiple_of(3) {
            return Err(router::wrong_args("JSON.MSET", ""));
        }
        let key_count = args.len() / 3;
        tracing::Span::current().record("key_count", key_count);
        tracing::debug!(target: "cmd.json", command = "mset", key_count);

        let result = async {
            let key_refs: Vec<&[u8]> = args.iter().step_by(3).map(|b| b.as_ref()).collect();
            let _guard = self.key_lock.lock_keys_sorted(&key_refs).await;

            for chunk in args.chunks(3) {
                let key = &chunk[0];
                let path = String::from_utf8_lossy(&chunk[1]).to_string();
                let val_str = String::from_utf8_lossy(&chunk[2]).to_string();
                let new_value: JsonValue =
                    serde_json::from_str(&val_str).map_err(Self::invalid_json)?;

                let result_json = if path == "$" || path == "." {
                    new_value
                } else {
                    let mut json_doc = match self.storage.get(db, key).await? {
                        Some(existing) => {
                            serde_json::from_slice(&existing).map_err(Self::invalid_json)?
                        }
                        None => json!({}),
                    };
                    self.path_engine.set(&mut json_doc, &path, new_value)?;
                    json_doc
                };

                let json_bytes = serde_json::to_vec(&result_json).map_err(Self::invalid_json)?;
                if self.storage.exists(db, key).await? {
                    self.write_back_json(db, key, &json_bytes).await?;
                } else {
                    self.storage.set(db, key, &json_bytes).await?;
                }
            }
            Ok(router::ok())
        }
        .await;

        self.record("mset", result.is_ok());
        result
    }
}

fn json_type_name(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}
