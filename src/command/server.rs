//! 服务端管理命令: INFO/TIME/CONFIG/CLIENT/COMMAND/SLOWLOG/LATENCY. 仅 `new_with_shared`
//! 装配 (router 的 `require_server` 校验; 测试路径 `new` 下调用 → ERR).
//!
//! # 命令与数据源
//!
//! - INFO: 委托 `InfoRenderer` 渲染 (字段清单见 observability module).
//! - TIME: Unix 秒 + 微秒.
//! - CONFIG GET/SET: 读写 `shared.config_map`; `slowlog-*` 联动; `appendonly` SET → ERR.
//!   REWRITE/RESETSTAT 由 `router.dispatch_config` 处理 (OK no-op).
//! - CLIENT LIST/SETNAME/GETNAME: 读写 `shared.clients`.
//! - COMMAND: 读 `registry` — 无参返回全量元数据; COUNT / INFO / GETKEYS / DOCS / HELP 子命令.
//! - SLOWLOG: 读 `shared.slow_query_log` (`SlowQueryEntry`).
//! - LATENCY: 读 `shared.latency_stats`; RESP2/RESP3 格式分支.
//! - OBJECT (router.dispatch_object): ENCODING 按 `ValueType`; REFCOUNT/IDLETIME/FREQ 为 stub.

use std::sync::Arc;

use bytes::Bytes;

use crate::command::{registry, router};
use crate::error::{Error, Result};
use crate::protocol::{ProtocolVersion, RespValue};
use crate::server::slowlog::SlowQueryEntry;
use crate::server::ServerSharedState;
use crate::storage::KvStorage;

pub struct ServerCommands {
    storage: Arc<dyn KvStorage>,
    shared: Arc<ServerSharedState>,
}

impl ServerCommands {
    pub fn new(storage: Arc<dyn KvStorage>, shared: Arc<ServerSharedState>) -> Self {
        Self { storage, shared }
    }

    pub async fn info(&self, _current_db: usize, args: &[Bytes]) -> Result<RespValue> {
        let sections: Vec<String> = args
            .iter()
            .map(|arg| String::from_utf8_lossy(arg).to_ascii_lowercase())
            .collect();
        let renderer = crate::server::InfoRenderer::new(&self.shared, self.storage.as_ref());
        let out = renderer.render(&sections).await;
        Ok(router::bulk(out.into_bytes()))
    }

    pub async fn time(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("TIME", args, 0)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs() as i64;
        let micros = now.subsec_micros() as i64;
        Ok(RespValue::Array(Some(vec![
            router::bulk(secs.to_string().into_bytes()),
            router::bulk(micros.to_string().into_bytes()),
        ])))
    }

    pub async fn config_get(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("CONFIG GET", args, 2)?;
        let param = String::from_utf8_lossy(&args[1]);
        if param == "*" {
            let map = self.shared.config_map.read();
            let mut pairs = Vec::new();
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                let v = self.config_value(&k);
                pairs.push(router::bulk(k.into_bytes()));
                pairs.push(router::bulk(v.into_bytes()));
            }
            return Ok(RespValue::Array(Some(pairs)));
        }
        if !self.shared.config_map.read().contains_key(param.as_ref()) {
            return Ok(RespValue::Array(Some(vec![])));
        }
        let value = self.config_value(param.as_ref());
        Ok(RespValue::Array(Some(vec![
            router::bulk(param.as_bytes().to_vec()),
            router::bulk(value.into_bytes()),
        ])))
    }

    fn config_value(&self, param: &str) -> String {
        match param {
            "slowlog-log-slower-than" => self.shared.slow_query_log.threshold_us().to_string(),
            "slowlog-max-len" => self.shared.slow_query_log.max_entries().to_string(),
            other => self
                .shared
                .config_map
                .read()
                .get(other)
                .cloned()
                .unwrap_or_default(),
        }
    }

    pub async fn config_set(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_min_args("CONFIG SET", args, 3)?;
        let param = String::from_utf8_lossy(&args[1]).to_string();
        let value = String::from_utf8_lossy(&args[2]).to_string();
        let mut map = self.shared.config_map.write();
        if !map.contains_key(&param) {
            return Err(Error::Command(format!(
                "ERR Unknown config parameter '{param}'"
            )));
        }
        if param == "appendonly" {
            return Err(Error::Command(
                "ERR Unsupported CONFIG parameter: appendonly".into(),
            ));
        }
        match param.as_str() {
            "slowlog-log-slower-than" => {
                let threshold: u64 = value
                    .parse()
                    .map_err(|_| Error::Config("Invalid slowlog-log-slower-than value".into()))?;
                self.shared.slow_query_log.set_threshold_us(threshold);
            }
            "slowlog-max-len" => {
                let max_len: usize = value
                    .parse()
                    .map_err(|_| Error::Config("Invalid slowlog-max-len value".into()))?;
                self.shared.slow_query_log.set_max_entries(max_len);
            }
            _ => {}
        }
        map.insert(param, value);
        Ok(router::ok())
    }

    pub async fn slowlog(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(router::wrong_args("SLOWLOG", ""));
        }
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "GET" => {
                let count = if args.len() > 1 {
                    String::from_utf8_lossy(&args[1])
                        .parse::<i64>()
                        .map_err(|_| {
                            Error::Command("ERR value is not an integer or out of range".into())
                        })?
                } else {
                    10
                };
                if count <= 0 {
                    return Ok(RespValue::Array(Some(vec![])));
                }
                let entries = self.shared.slow_query_log.get(count as usize);
                let items: Vec<RespValue> = entries.iter().map(slowlog_entry_to_resp).collect();
                Ok(RespValue::Array(Some(items)))
            }
            "LEN" => Ok(router::integer(self.shared.slow_query_log.len() as i64)),
            "RESET" => {
                self.shared.slow_query_log.reset();
                Ok(router::ok())
            }
            "HELP" => Ok(RespValue::Array(Some(vec![
                router::bulk(b"SLOWLOG GET [count] - Return slow query log entries".to_vec()),
                router::bulk(b"SLOWLOG LEN - Return slow query log length".to_vec()),
                router::bulk(b"SLOWLOG RESET - Reset slow query log".to_vec()),
            ]))),
            _ => Err(Error::Command(format!("ERR unknown subcommand '{sub}'"))),
        }
    }

    pub async fn command(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            let items: Vec<RespValue> = registry::all_commands()
                .iter()
                .map(command_info_to_resp)
                .collect();
            return Ok(RespValue::Array(Some(items)));
        }
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "COUNT" => Ok(router::integer(registry::command_count() as i64)),
            "INFO" => {
                if args.len() == 1 {
                    return Ok(RespValue::Array(Some(vec![])));
                }
                let items: Vec<RespValue> = args[1..]
                    .iter()
                    .map(|b| {
                        let name = String::from_utf8_lossy(b);
                        match registry::lookup(name.as_ref()) {
                            Some(info) => command_info_to_resp(&info),
                            None => RespValue::Null,
                        }
                    })
                    .collect();
                Ok(RespValue::Array(Some(items)))
            }
            "GETKEYS" => {
                router::require_min_args("COMMAND GETKEYS", args, 2)?;
                let cmd_name = String::from_utf8_lossy(&args[1]).to_ascii_uppercase();
                let cmd_args = &args[2..];
                let info = registry::lookup(&cmd_name).ok_or_else(|| {
                    Error::Command(format!("ERR Invalid command name '{cmd_name}'"))
                })?;
                let argc = cmd_args.len() + 1;
                validate_command_arity(&info, argc)?;
                let indices = registry::key_indices(&info, argc);
                let keys: Vec<RespValue> = indices
                    .iter()
                    .filter_map(|&idx| cmd_args.get(idx - 1).map(|b| router::bulk(b.to_vec())))
                    .collect();
                Ok(RespValue::Array(Some(keys)))
            }
            "DOCS" => Ok(RespValue::Array(Some(vec![]))),
            "HELP" => Ok(RespValue::Array(Some(vec![
                router::bulk(b"COMMAND COUNT - Return the total number of commands".to_vec()),
                router::bulk(
                    b"COMMAND DOCS [command-name ...] - Return command documentation".to_vec(),
                ),
                router::bulk(b"COMMAND INFO [command-name ...] - Return command metadata".to_vec()),
                router::bulk(
                    b"COMMAND GETKEYS command [arg ...] - Extract keys from a command".to_vec(),
                ),
            ]))),
            _ => Err(Error::Command(format!("ERR unknown subcommand '{sub}'"))),
        }
    }

    pub async fn latency(&self, args: &[Bytes], proto: ProtocolVersion) -> Result<RespValue> {
        if args.is_empty() {
            return Err(router::wrong_args("LATENCY", ""));
        }
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "HISTOGRAM" => {
                let filter: Option<Vec<&str>> = if args.len() > 1 {
                    Some(
                        args[1..]
                            .iter()
                            .map(|b| std::str::from_utf8(b).unwrap_or(""))
                            .collect(),
                    )
                } else {
                    None
                };
                let snapshots = self
                    .shared
                    .metrics
                    .command_histogram_snapshots(filter.as_deref());
                Ok(latency_histogram_resp(&snapshots, proto))
            }
            "LATEST" => {
                let snapshots = self.shared.latency_stats.histogram_snapshots(None);
                let now_s = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let items: Vec<RespValue> = snapshots
                    .iter()
                    .map(|(name, snap)| {
                        RespValue::Array(Some(vec![
                            router::bulk(name.as_bytes().to_vec()),
                            router::integer(now_s),
                            router::integer((snap.max_us / 1000) as i64),
                            router::integer(snap.calls as i64),
                        ]))
                    })
                    .collect();
                Ok(RespValue::Array(Some(items)))
            }
            "HISTORY" => {
                if args.len() < 2 {
                    return Err(router::wrong_args("LATENCY HISTORY", ""));
                }
                let event = String::from_utf8_lossy(&args[1]).to_string();
                let samples = self.shared.latency_stats.history(&event);
                let items: Vec<RespValue> = samples
                    .iter()
                    .map(|(ts, ms)| {
                        RespValue::Array(Some(vec![
                            router::integer(*ts as i64),
                            router::integer(*ms as i64),
                        ]))
                    })
                    .collect();
                Ok(RespValue::Array(Some(items)))
            }
            "RESET" => {
                let filter: Option<Vec<&str>> = if args.len() > 1 {
                    Some(
                        args[1..]
                            .iter()
                            .map(|b| std::str::from_utf8(b).unwrap_or(""))
                            .collect(),
                    )
                } else {
                    None
                };
                let count = self
                    .shared
                    .metrics
                    .reset_command_histograms(filter.as_deref());
                Ok(router::integer(count as i64))
            }
            "HELP" => {
                Ok(RespValue::Array(Some(vec![
        router::bulk(
          b"LATENCY HISTOGRAM [command ...] - Show latency histogram per command".to_vec(),
        ),
        router::bulk(b"LATENCY LATEST - Show latest latency spike events".to_vec()),
        router::bulk(b"LATENCY HISTORY event - Show latency time series for an event".to_vec()),
        router::bulk(
          b"LATENCY RESET [event ...] - Reset latency data (all or specific commands)".to_vec(),
        ),
      ])))
            }
            _ => Err(Error::Command(format!(
                "ERR unknown LATENCY subcommand '{sub}'"
            ))),
        }
    }

    pub async fn client_list(&self, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("CLIENT LIST", args, 1)?;
        let clients = self.shared.clients.read();
        let mut lines = Vec::new();
        for info in clients.values() {
            let name = info.name.as_deref().unwrap_or("");
            lines.push(format!(
                "id={} addr={} name={} db={}",
                info.id, info.addr, name, info.db
            ));
        }
        Ok(router::bulk(lines.join("\n").into_bytes()))
    }

    pub async fn client_setname(&self, id: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("CLIENT SETNAME", args, 2)?;
        let name = String::from_utf8_lossy(&args[1]).to_string();
        self.shared.set_client_name(id, Some(name));
        Ok(router::ok())
    }

    pub async fn client_getname(&self, id: usize, args: &[Bytes]) -> Result<RespValue> {
        router::require_args("CLIENT GETNAME", args, 1)?;
        let clients = self.shared.clients.read();
        let Some(info) = clients.get(&id) else {
            return Ok(router::nil_bulk());
        };
        match &info.name {
            Some(n) => Ok(router::bulk(n.clone().into_bytes())),
            None => Ok(router::nil_bulk()),
        }
    }
}

fn slowlog_entry_to_resp(entry: &SlowQueryEntry) -> RespValue {
    let mut cmd_args = vec![router::bulk(entry.command.as_bytes().to_vec())];
    for arg in &entry.args {
        cmd_args.push(router::bulk(arg.as_bytes().to_vec()));
    }
    RespValue::Array(Some(vec![
        router::integer(entry.id as i64),
        router::integer(entry.timestamp_s),
        router::integer(entry.duration_us as i64),
        RespValue::Array(Some(cmd_args)),
        router::bulk(entry.client_addr.as_bytes().to_vec()),
        router::integer(entry.db_index as i64),
    ]))
}

fn command_info_to_resp(info: &registry::CommandInfo) -> RespValue {
    let flags: Vec<RespValue> = info
        .flags
        .iter()
        .map(|f| router::bulk(f.as_bytes().to_vec()))
        .collect();
    RespValue::Array(Some(vec![
        router::bulk(info.name.as_bytes().to_vec()),
        router::integer(info.arity),
        RespValue::Array(Some(flags)),
        router::integer(info.first_key),
        router::integer(info.last_key),
        router::integer(info.step),
    ]))
}

fn validate_command_arity(info: &registry::CommandInfo, argc: usize) -> Result<()> {
    if info.arity >= 0 {
        if argc as i64 != info.arity {
            return Err(Error::Command(format!(
                "ERR wrong number of arguments for '{}' command",
                info.name.to_ascii_lowercase()
            )));
        }
    } else if (argc as i64) < -info.arity {
        return Err(Error::Command(format!(
            "ERR wrong number of arguments for '{}' command",
            info.name.to_ascii_lowercase()
        )));
    }
    Ok(())
}

fn latency_histogram_resp(
    snapshots: &[(String, crate::server::latency::CommandLatencySnapshot)],
    proto: ProtocolVersion,
) -> RespValue {
    match proto {
        ProtocolVersion::Resp2 => {
            let mut items = Vec::with_capacity(snapshots.len() * 2);
            for (cmd, snap) in snapshots {
                items.push(router::bulk(cmd.as_bytes().to_vec()));
                items.push(latency_detail_array(snap));
            }
            RespValue::Array(Some(items))
        }
        ProtocolVersion::Resp3 => {
            let items: Vec<(RespValue, RespValue)> = snapshots
                .iter()
                .map(|(cmd, snap)| {
                    (
                        router::bulk(cmd.as_bytes().to_vec()),
                        latency_detail_map(snap),
                    )
                })
                .collect();
            RespValue::Map(items)
        }
    }
}

fn latency_detail_array(snap: &crate::server::latency::CommandLatencySnapshot) -> RespValue {
    let mut bucket_items = Vec::with_capacity(snap.buckets.len() * 2);
    for (bound, count) in &snap.buckets {
        bucket_items.push(router::integer(*bound as i64));
        bucket_items.push(router::integer(*count as i64));
    }
    RespValue::Array(Some(vec![
        router::bulk(b"calls".to_vec()),
        router::integer(snap.calls as i64),
        router::bulk(b"histogram_usec".to_vec()),
        RespValue::Array(Some(bucket_items)),
    ]))
}

fn latency_detail_map(snap: &crate::server::latency::CommandLatencySnapshot) -> RespValue {
    let bucket_map: Vec<(RespValue, RespValue)> = snap
        .buckets
        .iter()
        .map(|(bound, count)| {
            (
                router::integer(*bound as i64),
                router::integer(*count as i64),
            )
        })
        .collect();
    RespValue::Map(vec![
        (
            router::bulk(b"calls".to_vec()),
            router::integer(snap.calls as i64),
        ),
        (
            router::bulk(b"histogram_usec".to_vec()),
            RespValue::Map(bucket_map),
        ),
    ])
}
