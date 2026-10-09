//! 命令路由与 key 级锁: `CommandRouter` 是全部 Redis 命令的分发中心, 同时提供
//! `KeyLock` 分桶写锁、RESP 构造 helper (`ok`/`bulk`/`integer`/`nil_bulk`) 与 metrics 钩子.
//!
//! # 分发流程
//!
//! ```text
//! Connection ── execute_with_client(cmd, args, db, client_id, proto, cluster_state)
//!   │
//!   ├─ [feature cluster] cluster_route:            ┐
//!   │    admin 白名单 (PING/INFO/KEYS/DBSIZE/SCAN 族等) 直通 │ 命中 → 直接返回
//!   │    MIGRATE/RESTORE: 仅写冻结 TRYAGAIN        │ (MOVED/ASK/CROSSSLOT/
//!   │    多 key 命令: CROSSSLOT 校验               │  TRYAGAIN/CLUSTERDOWN)
//!   │    (MSET 取偶索引, BLPOP/BRPOP 去末参数)     ┘
//!   │
//!   ├─ span 记录 → execute_inner: 按命令名 match 到各域 handler
//!   │    GET / MGET / HGET / EXISTS 额外记录 keyspace hit/miss
//!   │    CONFIG / CLIENT / OBJECT 走 dispatch_config / dispatch_client / dispatch_object
//!   │
//!   └─ record_command_outcome: commandstats (成功/失败) + errorstats
//! ```
//!
//! # Invariant
//!
//! - KeyLock 写路径: mutating 单 key 写用 `lock(key)` (SET NX/XX, INCR, HSET, LPUSH 等);
//!   双 key 用 `lock_two` (按 key 字节序加锁, 同 key 不重入, 见 RENAME/COPY/LMOVE/SMOVE);
//!   多 key 用 `lock_keys_sorted` (去重 + 字典序, JSON.MSET), EVAL 用
//!   `lock_keys_sorted_with_timeout` (默认 30s).
//! - registry ↔ router 双维护: 新命令须同时更新 `registry.rs` `COMMAND_TABLE` 与
//!   本文件 `execute_inner` 的 match (或子 dispatch), 否则出现表内命中但路由漏分发的 bug.
//! - metrics 仅 `new_with_shared` 装配; `new` (测试路径) 无 INFO/SAVE,
//!   `require_server` / `require_persistence` 返回 ERR.
//!
//! 各域 handler 见 `string.rs` / `hash.rs` / `list.rs` / `set.rs` / `zset/` / `key.rs`;
//! extended (JSON/Lua/Server/Persistence) 语义见 `docs/modules/05-commands-extended.md`,
//! 类型分轨与空容器删除等核心约束见 `docs/modules/04-commands-core.md`.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use tracing::Instrument;

use crate::command::{
    database, hash, json, key, list, persistence, script, server, set, string, zset,
};
use crate::error::{Error, Result};
use crate::protocol::{ProtocolVersion, RespValue};
use crate::server::{ServerMetrics, ServerSharedState};
use crate::storage::KvStorage;

mod keylock;
mod stats;

use keylock::KEY_LOCK_BUCKETS;
pub use keylock::{KeyLock, KeyLocksGuard};
use stats::record_command_outcome;
#[cfg(feature = "monitoring")]
use stats::record_command_span_status;
#[cfg(feature = "cluster")]
use stats::{classify_command, is_cluster_local_admin_cmd, is_multi_key_cmd};

pub struct CommandRouter {
    storage: Arc<dyn KvStorage>,
    #[expect(dead_code)]
    key_lock: Arc<KeyLock>,
    metrics: Option<Arc<ServerMetrics>>,
    string: string::StringCommands,
    hash: hash::HashCommands,
    list: list::ListCommands,
    set: set::SetCommands,
    zset: zset::ZSetCommands,
    json: json::JsonCommands,
    script: script::ScriptCommands,
    database: database::DatabaseCommands,
    key_cmds: key::KeyCommands,
    server: Option<server::ServerCommands>,
    persistence: Option<persistence::PersistenceCommands>,
}

impl CommandRouter {
    pub fn new(storage: Arc<dyn KvStorage>) -> Self {
        let key_lock = Arc::new(KeyLock::new(KEY_LOCK_BUCKETS));
        let transaction_gate = Arc::new(tokio::sync::RwLock::new(()));
        Self {
            string: string::StringCommands::new(storage.clone(), key_lock.clone()),
            hash: hash::HashCommands::new(storage.clone(), key_lock.clone()),
            list: list::ListCommands::new(
                storage.clone(),
                key_lock.clone(),
                Arc::clone(&transaction_gate),
            ),
            set: set::SetCommands::new(storage.clone(), key_lock.clone()),
            zset: zset::ZSetCommands::new(storage.clone(), key_lock.clone(), transaction_gate),
            json: json::JsonCommands::new(storage.clone(), key_lock.clone()),
            script: script::ScriptCommands::new(storage.clone(), key_lock.clone()),
            database: database::DatabaseCommands::new(storage.clone()),
            key_cmds: key::KeyCommands::new(storage.clone(), key_lock.clone()),
            server: None,
            persistence: None,
            metrics: None,
            storage,
            key_lock,
        }
    }

    pub fn new_with_shared(storage: Arc<dyn KvStorage>, shared: Arc<ServerSharedState>) -> Self {
        let key_lock = Arc::new(KeyLock::new(KEY_LOCK_BUCKETS));
        Self {
            string: string::StringCommands::new(storage.clone(), key_lock.clone()),
            hash: hash::HashCommands::new(storage.clone(), key_lock.clone()),
            list: list::ListCommands::with_metrics(
                storage.clone(),
                key_lock.clone(),
                shared.metrics.clone(),
                Arc::clone(&shared.transaction_gate),
            ),
            set: set::SetCommands::new(storage.clone(), key_lock.clone()),
            zset: zset::ZSetCommands::with_metrics(
                storage.clone(),
                key_lock.clone(),
                shared.metrics.clone(),
                Arc::clone(&shared.transaction_gate),
            ),
            json: json::JsonCommands::with_metrics(
                storage.clone(),
                key_lock.clone(),
                shared.metrics.clone(),
            ),
            script: script::ScriptCommands::with_metrics(
                storage.clone(),
                key_lock.clone(),
                shared.metrics.clone(),
            ),
            database: database::DatabaseCommands::new(storage.clone()),
            key_cmds: key::KeyCommands::new(storage.clone(), key_lock.clone()),
            server: Some(server::ServerCommands::new(storage.clone(), shared.clone())),
            persistence: Some(persistence::PersistenceCommands::new(
                storage.clone(),
                shared.clone(),
            )),
            metrics: Some(Arc::clone(&shared.metrics)),
            storage,
            key_lock,
        }
    }

    pub fn storage(&self) -> Arc<dyn KvStorage> {
        Arc::clone(&self.storage)
    }

    pub async fn execute(&self, cmd: &str, args: &[Bytes], db: &mut usize) -> Result<RespValue> {
        self.execute_with_client(
            cmd,
            args,
            db,
            None,
            None,
            ProtocolVersion::Resp2,
            #[cfg(feature = "cluster")]
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_with_client(
        &self,
        cmd: &str,
        args: &[Bytes],
        db: &mut usize,
        client_id: Option<usize>,
        client_addr: Option<SocketAddr>,
        protocol_version: ProtocolVersion,
        #[cfg(feature = "cluster")] conn_state: Option<
            &crate::cluster::connection::ClusterConnectionState,
        >,
    ) -> Result<RespValue> {
        let span = aidb::observability::optional_span("kv_command");
        if aidb::observability::traces_enabled() {
            span.record("cmd", crate::command::canonical_command(cmd));
        }
        #[cfg(feature = "cluster")]
        if let Some(state) = conn_state {
            if let Some(result) = self.cluster_route(cmd, args, state).await {
                return result;
            }
        }
        let _ = client_addr;
        let started = std::time::Instant::now();
        let result = async {
            #[cfg(feature = "monitoring")]
            crate::server::otel::record_trace_ids_on_span(&tracing::Span::current());
            self.execute_inner(cmd, args, db, client_id, protocol_version)
                .await
        }
        .instrument(span)
        .await;
        let duration_us = started.elapsed().as_micros() as u64;
        record_command_outcome(&self.metrics, cmd, &result, duration_us);
        #[cfg(feature = "monitoring")]
        record_command_span_status(&result);
        result
    }

    /// 同一条连接上已经读完的 EVAL / EVALSHA。声明 key 相同的相邻脚本一次提交。
    pub async fn execute_script_burst(
        &self,
        db: usize,
        calls: Vec<(String, Vec<Bytes>)>,
    ) -> Vec<Result<RespValue>> {
        self.script.eval_many(db, calls).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_script_burst_with_client(
        &self,
        calls: Vec<(String, Vec<Bytes>)>,
        db: &mut usize,
        client_id: Option<usize>,
        client_addr: Option<SocketAddr>,
        protocol_version: ProtocolVersion,
        #[cfg(feature = "cluster")] conn_state: Option<
            &crate::cluster::connection::ClusterConnectionState,
        >,
    ) -> Vec<Result<RespValue>> {
        #[cfg(not(feature = "cluster"))]
        let _ = (client_id, client_addr, protocol_version);
        #[cfg(feature = "cluster")]
        if let Some(state) = conn_state {
            let mut local = true;
            for (cmd, args) in &calls {
                if self.cluster_route(cmd, args, state).await.is_some() {
                    local = false;
                    break;
                }
            }
            if !local {
                let mut out = Vec::with_capacity(calls.len());
                for (cmd, args) in &calls {
                    out.push(
                        self.execute_with_client(
                            cmd,
                            args,
                            db,
                            client_id,
                            client_addr,
                            protocol_version,
                            Some(state),
                        )
                        .await,
                    );
                }
                return out;
            }
        }
        let names: Vec<String> = calls.iter().map(|(cmd, _)| cmd.clone()).collect();
        let started = std::time::Instant::now();
        let results = self.script.eval_many(*db, calls).await;
        let duration_us = started.elapsed().as_micros() as u64;
        for (cmd, result) in names.iter().zip(&results) {
            record_command_outcome(&self.metrics, cmd, result, duration_us);
            #[cfg(feature = "monitoring")]
            record_command_span_status(result);
        }
        results
    }

    /// Cluster routing check: returns Some(response) if the request should be
    /// redirected (MOVED/ASK) or rejected (CROSSSLOT/CLUSTERDOWN).
    #[cfg(feature = "cluster")]
    async fn cluster_route(
        &self,
        cmd: &str,
        args: &[Bytes],
        conn_state: &crate::cluster::connection::ClusterConnectionState,
    ) -> Option<Result<RespValue>> {
        let _ = crate::cluster::state::CLUSTER_STATE_MGR.get()?;
        let lower = cmd.to_ascii_lowercase();
        // F-056-A1: SCAN 族在任意活跃迁移期间一律 TRYAGAIN (不按 slot 精细过滤).
        // 这些命令在 admin 白名单里, 平时不经 key 路由; 必须在白名单短路之前拦截,
        // 否则会落到本地 for_each_prefix, 读到不一致的半迁移视图.
        if let Some(crate::cluster::router::RouteDecision::TryAgain { reason }) =
            crate::cluster::router::scan_tryagain_if_migrating(&lower)
        {
            if let Some(m) = self.metrics.as_ref() {
                m.on_error_stat(&reason);
            }
            return Some(Ok(RespValue::Error(reason)));
        }
        // 节点本地命令: 不按 args[0] 做 slot 路由 (含无 key / pattern / cursor 族).
        if is_cluster_local_admin_cmd(&lower) {
            return None;
        }
        // MIGRATE/RESTORE: 平时不走 MOVED/ASK (本地读 + 远端 RESTORE), 但 F-056
        // Frozen/Ready 写冻结仍须 TRYAGAIN (白名单外不放行用户数据写); cluster_state
        // fail (分区) 时同样须 CLUSTERDOWN, 与 decide 全局门控保持一致.
        if lower == "migrate" || lower == "restore" {
            if let Some(key) = crate::cluster::routing_key::cluster_routing_key(cmd, args) {
                let decision = crate::cluster::router::ClusterRouter::decide(
                    key,
                    crate::cluster::router::CommandType::Write,
                    conn_state.is_asking(),
                    conn_state.is_readonly(),
                );
                match decision {
                    crate::cluster::router::RouteDecision::TryAgain { reason } => {
                        if let Some(m) = self.metrics.as_ref() {
                            m.on_error_stat(&reason);
                        }
                        return Some(Ok(RespValue::Error(reason)));
                    }
                    crate::cluster::router::RouteDecision::ClusterDown(msg) => {
                        if let Some(m) = self.metrics.as_ref() {
                            m.on_error_stat(&msg);
                        }
                        return Some(Ok(RespValue::Error(msg)));
                    }
                    _ => {}
                }
            }
            return None;
        }
        // Multi-key CROSSSLOT check
        if args.len() > 1 && is_multi_key_cmd(cmd) {
            // MSET args are [key, val, key, val, ...] — only even indices are keys.
            let is_mset = cmd.eq_ignore_ascii_case("mset");
            let is_blocking_pop =
                cmd.eq_ignore_ascii_case("blpop") || cmd.eq_ignore_ascii_case("brpop");
            let key_bytes: Vec<&[u8]> = if is_mset {
                args.iter().step_by(2).map(|a| a.as_ref()).collect()
            } else if is_blocking_pop {
                args[..args.len() - 1].iter().map(|a| a.as_ref()).collect()
            } else {
                args.iter().map(|a| a.as_ref()).collect()
            };
            if let Err(msg) = crate::cluster::router::check_cross_slot(&key_bytes) {
                return Some(Ok(RespValue::Error(msg)));
            }
        }
        // Single-key routing (EVAL/EVALSHA use first declared key, not script body).
        if let Some(key) = crate::cluster::routing_key::cluster_routing_key(cmd, args) {
            let cmd_type = classify_command(cmd);
            let decision = crate::cluster::router::ClusterRouter::decide(
                key,
                cmd_type,
                conn_state.is_asking(),
                conn_state.is_readonly(),
            );
            if let Some(resp) = self.route_decision_to_response(decision) {
                return Some(Ok(resp));
            }
        }
        None
    }

    #[cfg(feature = "cluster")]
    #[doc(hidden)]
    pub async fn preflight_transaction(
        &self,
        queue: &[(String, Vec<Bytes>)],
        conn_state: &crate::cluster::connection::ClusterConnectionState,
    ) -> Result<Option<RespValue>> {
        if crate::cluster::state::CLUSTER_STATE_MGR.get().is_none() {
            return Ok(None);
        }

        for (cmd, _) in queue {
            if let Some(info) = crate::command::lookup(cmd) {
                if info.flags.contains(&"movablekeys") {
                    return Ok(Some(RespValue::Error(
                        "ERR movable-key commands are not supported inside MULTI/EXEC".into(),
                    )));
                }
                if info.flags.contains(&"blocking") {
                    return Ok(Some(RespValue::Error(
                        "ERR blocking commands are not supported inside MULTI/EXEC".into(),
                    )));
                }
            }
        }

        let keys: Vec<&[u8]> = queue
            .iter()
            .flat_map(|(cmd, args)| crate::command::command_keys(cmd, args))
            .collect();
        if let Err(msg) = crate::cluster::router::check_cross_slot(&keys) {
            return Ok(Some(RespValue::Error(msg)));
        }

        for (cmd, args) in queue {
            let lower = cmd.to_ascii_lowercase();
            if let Some(crate::cluster::router::RouteDecision::TryAgain { reason }) =
                crate::cluster::router::scan_tryagain_if_migrating(&lower)
            {
                if let Some(m) = self.metrics.as_ref() {
                    m.on_error_stat(&reason);
                }
                return Ok(Some(RespValue::Error(reason)));
            }

            let cmd_keys = crate::command::command_keys(cmd, args);
            let Some(first_key) = cmd_keys.first() else {
                continue;
            };

            let is_readonly = crate::command::lookup(cmd)
                .map(|info| info.flags.contains(&"readonly"))
                .unwrap_or(false);
            let cmd_type = if is_readonly {
                crate::cluster::router::CommandType::Read
            } else {
                crate::cluster::router::CommandType::Write
            };

            let decision = crate::cluster::router::ClusterRouter::decide(
                first_key,
                cmd_type,
                conn_state.is_asking(),
                conn_state.is_readonly(),
            );

            if let Some(resp) = self.route_decision_to_response(decision) {
                return Ok(Some(resp));
            }
        }

        Ok(None)
    }

    #[cfg(feature = "cluster")]
    fn route_decision_to_response(
        &self,
        decision: crate::cluster::router::RouteDecision,
    ) -> Option<RespValue> {
        match decision {
            crate::cluster::router::RouteDecision::Execute => None,
            crate::cluster::router::RouteDecision::Moved { slot, addr, .. } => {
                if let Some(m) = self.metrics.as_ref() {
                    m.on_cluster_redirect("moved");
                }
                Some(RespValue::Error(format!("MOVED {slot} {addr}")))
            }
            crate::cluster::router::RouteDecision::Ask { slot, addr, .. } => {
                if let Some(m) = self.metrics.as_ref() {
                    m.on_cluster_redirect("ask");
                }
                Some(RespValue::Error(format!("ASK {slot} {addr}")))
            }
            crate::cluster::router::RouteDecision::TryAgain { reason } => {
                if let Some(m) = self.metrics.as_ref() {
                    m.on_error_stat(&reason);
                }
                Some(RespValue::Error(reason))
            }
            crate::cluster::router::RouteDecision::ClusterDown(msg) => Some(RespValue::Error(msg)),
        }
    }

    async fn execute_inner(
        &self,
        cmd: &str,
        args: &[Bytes],
        db: &mut usize,
        client_id: Option<usize>,
        protocol_version: ProtocolVersion,
    ) -> Result<RespValue> {
        match cmd {
            "GET" => self.string.get(*db, args).await,
            "SET" => self.string.set(*db, args).await,
            "MGET" => self.string.mget(*db, args).await,
            "MSET" => self.string.mset(*db, args).await,
            "DEL" => self.string.del(*db, args).await,
            "EXISTS" => self.string.exists(*db, args).await,
            "STRLEN" => self.string.strlen(*db, args).await,
            "GETRANGE" => self.string.getrange(*db, args).await,
            "SETRANGE" => self.string.setrange(*db, args).await,
            "SETBIT" => self.string.setbit(*db, args).await,
            "GETBIT" => self.string.getbit(*db, args).await,
            "APPEND" => self.string.append(*db, args).await,
            "INCR" => self.string.incr(*db, args).await,
            "DECR" => self.string.decr(*db, args).await,
            "INCRBY" => self.string.incrby(*db, args).await,
            "DECRBY" => self.string.decrby(*db, args).await,
            "INCRBYFLOAT" => self.string.incrbyfloat(*db, args).await,
            "GETDEL" => self.string.getdel(*db, args).await,
            "GETEX" => self.string.getex(*db, args).await,
            "SETNX" => self.string.setnx(*db, args).await,
            "SETEX" => self.string.setex(*db, args).await,
            "PSETEX" => self.string.psetex(*db, args).await,
            "HSET" => self.hash.hset(*db, args).await,
            "HMSET" => self.hash.hmset(*db, args).await,
            "HGET" => self.hash.hget(*db, args).await,
            "HDEL" => self.hash.hdel(*db, args).await,
            "HEXISTS" => self.hash.hexists(*db, args).await,
            "HLEN" => self.hash.hlen(*db, args).await,
            "HKEYS" => self.hash.hkeys(*db, args).await,
            "HVALS" => self.hash.hvals(*db, args).await,
            "HGETALL" => self.hash.hgetall(*db, args).await,
            "HMGET" => self.hash.hmget(*db, args).await,
            "HSETNX" => self.hash.hsetnx(*db, args).await,
            "HINCRBY" => self.hash.hincrby(*db, args).await,
            "HINCRBYFLOAT" => self.hash.hincrbyfloat(*db, args).await,
            "HSCAN" => self.hash.hscan(*db, args).await,
            "LPUSH" => self.list.lpush(*db, args).await,
            "RPUSH" => self.list.rpush(*db, args).await,
            "LPOP" => self.list.lpop(*db, args).await,
            "RPOP" => self.list.rpop(*db, args).await,
            "LLEN" => self.list.llen(*db, args).await,
            "LRANGE" => self.list.lrange(*db, args).await,
            "LINDEX" => self.list.lindex(*db, args).await,
            "LSET" => self.list.lset(*db, args).await,
            "LREM" => self.list.lrem(*db, args).await,
            "LTRIM" => self.list.ltrim(*db, args).await,
            "LINSERT" => self.list.linsert(*db, args).await,
            "LMOVE" => self.list.lmove(*db, args).await,
            "LPOS" => self.list.lpos(*db, args).await,
            "BLPOP" => self.list.blpop(*db, args).await,
            "BRPOP" => self.list.brpop(*db, args).await,
            "BLMOVE" => self.list.blmove_blocking(*db, args).await,
            "SADD" => self.set.sadd(*db, args).await,
            "SREM" => self.set.srem(*db, args).await,
            "SISMEMBER" => self.set.sismember(*db, args).await,
            "SMEMBERS" => self.set.smembers(*db, args).await,
            "SCARD" => self.set.scard(*db, args).await,
            "SPOP" => self.set.spop(*db, args).await,
            "SRANDMEMBER" => self.set.srandmember(*db, args).await,
            "SUNION" => self.set.sunion(*db, args).await,
            "SINTER" => self.set.sinter(*db, args).await,
            "SDIFF" => self.set.sdiff(*db, args).await,
            "SUNIONSTORE" => self.set.sunionstore(*db, args).await,
            "SINTERSTORE" => self.set.sinterstore(*db, args).await,
            "SDIFFSTORE" => self.set.sdiffstore(*db, args).await,
            "SMOVE" => self.set.smove(*db, args).await,
            "SSCAN" => self.set.sscan(*db, args).await,
            "ZADD" => self.zset.zadd(*db, args).await,
            "ZREM" => self.zset.zrem(*db, args).await,
            "ZSCORE" => self.zset.zscore(*db, args).await,
            "ZRANK" => self.zset.zrank(*db, args).await,
            "ZREVRANK" => self.zset.zrevrank(*db, args).await,
            "ZRANGE" => self.zset.zrange(*db, args).await,
            "ZREVRANGE" => self.zset.zrevrange(*db, args).await,
            "ZRANGEBYSCORE" => self.zset.zrangebyscore(*db, args).await,
            "ZREVRANGEBYSCORE" => self.zset.zrevrangebyscore(*db, args).await,
            "ZCARD" => self.zset.zcard(*db, args).await,
            "ZCOUNT" => self.zset.zcount(*db, args).await,
            "ZINCRBY" => self.zset.zincrby(*db, args).await,
            "ZSCAN" => self.zset.zscan(*db, args).await,
            "ZPOPMIN" => self.zset.zpopmin(*db, args).await,
            "ZPOPMAX" => self.zset.zpopmax(*db, args).await,
            "BZPOPMIN" => self.zset.bzpopmin(*db, args).await,
            "BZPOPMAX" => self.zset.bzpopmax(*db, args).await,
            "ZRANGEBYLEX" => self.zset.zrangebylex(*db, args).await,
            "ZREVRANGEBYLEX" => self.zset.zrevrangebylex(*db, args).await,
            "ZLEXCOUNT" => self.zset.zlexcount(*db, args).await,
            "ZINTER" => self.zset.zinter(*db, args).await,
            "ZUNION" => self.zset.zunion(*db, args).await,
            "ZDIFF" => self.zset.zdiff(*db, args).await,
            "SELECT" => self.database.select(args, db).await,
            "DBSIZE" => self.database.dbsize(*db, args).await,
            "FLUSHDB" => self.database.flushdb(*db, args).await,
            "FLUSHALL" => self.database.flushall(args).await,
            "SWAPDB" => self.database.swapdb(args).await,
            "MOVE" => self.database.move_key(*db, args).await,
            "EXPIRE" => self.key_cmds.expire(*db, args).await,
            "EXPIREAT" => self.key_cmds.expireat(*db, args).await,
            "PEXPIRE" => self.key_cmds.pexpire(*db, args).await,
            "PEXPIREAT" => self.key_cmds.pexpireat(*db, args).await,
            "TTL" => self.key_cmds.ttl(*db, args).await,
            "PTTL" => self.key_cmds.pttl(*db, args).await,
            "PERSIST" => self.key_cmds.persist(*db, args).await,
            "KEYS" => self.key_cmds.keys(*db, args).await,
            "SCAN" => {
                let value = self.key_cmds.scan(*db, args).await?;
                if let Some(metrics) = self.metrics.as_ref() {
                    metrics.note_scan_entries(scan_key_count(&value));
                }
                Ok(value)
            }
            "RANDOMKEY" => self.key_cmds.randomkey(*db, args).await,
            "RENAME" => self.key_cmds.rename(*db, args).await,
            "RENAMENX" => self.key_cmds.renamenx(*db, args).await,
            "TYPE" => self.key_cmds.type_cmd(*db, args).await,
            "COPY" => self.key_cmds.copy(*db, args).await,
            "OBJECT" => self.dispatch_object(*db, args).await,
            "EXPIRETIME" => self.key_cmds.expiretime(*db, args).await,
            "PEXPIRETIME" => self.key_cmds.pexpiretime(*db, args).await,
            "DUMP" => self.key_cmds.dump(*db, args).await,
            "RESTORE" => self.key_cmds.restore(*db, args).await,
            "MIGRATE" => self.key_cmds.migrate(*db, args).await,
            "JSON.SET" => self.json.json_set(*db, args).await,
            "JSON.GET" => self.json.json_get(*db, args).await,
            "JSON.MGET" => self.json.json_mget(*db, args).await,
            "JSON.DEL" => self.json.json_del(*db, args).await,
            "JSON.TYPE" => self.json.json_type(*db, args).await,
            "JSON.STRLEN" => self.json.json_strlen(*db, args).await,
            "JSON.ARRLEN" => self.json.json_arrlen(*db, args).await,
            "JSON.OBJLEN" => self.json.json_objlen(*db, args).await,
            "JSON.NUMINCRBY" => self.json.json_numincrby(*db, args).await,
            "JSON.ARRAPPEND" => self.json.json_arrappend(*db, args).await,
            "JSON.UPDATE" => self.json.json_update(*db, args).await,
            "JSON.MSET" => self.json.json_mset(*db, args).await,
            "EVAL" => self.script.eval(*db, args).await,
            "EVALSHA" => self.script.evalsha(*db, args).await,
            "SCRIPT" => self.script.script(args).await,
            "INFO" => self.require_server()?.info(*db, args).await,
            "TIME" => self.require_server()?.time(args).await,
            "CONFIG" => self.dispatch_config(args).await,
            "CLIENT" => self.dispatch_client(client_id, args).await,
            "LATENCY" => self.require_server()?.latency(args, protocol_version).await,
            "SLOWLOG" => self.require_server()?.slowlog(args).await,
            "COMMAND" => self.require_server()?.command(args).await,
            #[cfg(feature = "cluster")]
            "CLUSTER" => {
                let sub = args.first().map(|a| std::str::from_utf8(a).unwrap_or(""));
                crate::cluster::dispatch_cluster(sub, args).await
            }
            "SAVE" => self.require_persistence()?.save().await,
            "BGSAVE" => self.require_persistence()?.bgsave().await,
            "LASTSAVE" => self.require_persistence()?.lastsave().await,
            "SHUTDOWN" => self.require_persistence()?.shutdown(args).await,
            _ => Err(Error::Command(format!("ERR unknown command '{cmd}'"))),
        }
    }

    fn require_server(&self) -> Result<&server::ServerCommands> {
        self.server
            .as_ref()
            .ok_or_else(|| Error::Command("ERR server commands unavailable".into()))
    }

    fn require_persistence(&self) -> Result<&persistence::PersistenceCommands> {
        self.persistence
            .as_ref()
            .ok_or_else(|| Error::Command("ERR persistence commands unavailable".into()))
    }

    async fn dispatch_object(&self, db: usize, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(wrong_args("OBJECT", ""));
        }
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "ENCODING" => {
                if args.len() < 2 {
                    return Err(wrong_args("OBJECT ENCODING", ""));
                }
                let key = &args[1];
                match self.key_cmds.storage().get_typed(db, key).await? {
                    None => Err(Error::Command("ERR no such key".into())),
                    Some(stored) => {
                        use crate::storage::ValueType;
                        let enc = match &stored.value {
                            ValueType::String(v) => {
                                if v.len() <= 44 {
                                    "embstr"
                                } else {
                                    "raw"
                                }
                            }
                            ValueType::Hash(_) => "listpack",
                            ValueType::List(_) => "listpack",
                            ValueType::Set(_) => "listpack",
                            ValueType::ZSet(_) => "listpack",
                            ValueType::CollectionHeader { .. } => "listpack",
                        };
                        Ok(bulk(enc.as_bytes().to_vec()))
                    }
                }
            }
            "REFCOUNT" => {
                if args.len() < 2 {
                    return Err(wrong_args("OBJECT REFCOUNT", ""));
                }
                let exists = self.key_cmds.storage().exists(db, &args[1]).await?;
                if exists {
                    Ok(integer(1))
                } else {
                    Err(Error::Command("ERR no such key".into()))
                }
            }
            "IDLETIME" => {
                if args.len() < 2 {
                    return Err(wrong_args("OBJECT IDLETIME", ""));
                }
                let exists = self.key_cmds.storage().exists(db, &args[1]).await?;
                if exists {
                    Ok(integer(0))
                } else {
                    Err(Error::Command("ERR no such key".into()))
                }
            }
            "FREQ" => {
                if args.len() < 2 {
                    return Err(wrong_args("OBJECT FREQ", ""));
                }
                let exists = self.key_cmds.storage().exists(db, &args[1]).await?;
                if exists {
                    Ok(integer(0))
                } else {
                    Err(Error::Command("ERR no such key".into()))
                }
            }
            "HELP" => Ok(RespValue::Array(Some(vec![
                bulk(b"OBJECT ENCODING key - Return encoding of the object".to_vec()),
                bulk(b"OBJECT REFCOUNT key - Return reference count (stub: 1)".to_vec()),
                bulk(b"OBJECT IDLETIME key - Return idle time in seconds (stub: 0)".to_vec()),
                bulk(b"OBJECT FREQ key - Return access frequency (stub: 0)".to_vec()),
            ]))),
            _ => Err(Error::Command(format!(
                "ERR unknown OBJECT subcommand '{sub}'"
            ))),
        }
    }

    async fn dispatch_config(&self, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(wrong_args("CONFIG", ""));
        }
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "GET" => self.require_server()?.config_get(args).await,
            "SET" => self.require_server()?.config_set(args).await,
            "REWRITE" => Ok(ok()),
            "RESETSTAT" => Ok(ok()),
            _ => Err(Error::Command("ERR unknown CONFIG subcommand".into())),
        }
    }

    async fn dispatch_client(&self, client_id: Option<usize>, args: &[Bytes]) -> Result<RespValue> {
        if args.is_empty() {
            return Err(wrong_args("CLIENT", ""));
        }
        let sub = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
        match sub.as_str() {
            "LIST" => self.require_server()?.client_list(args).await,
            "SETNAME" => {
                let id = client_id.ok_or_else(|| {
                    Error::Command("ERR CLIENT SETNAME requires an active connection".into())
                })?;
                self.require_server()?.client_setname(id, args).await
            }
            "GETNAME" => {
                let id = client_id.ok_or_else(|| {
                    Error::Command("ERR CLIENT GETNAME requires an active connection".into())
                })?;
                self.require_server()?.client_getname(id, args).await
            }
            // Stub: StackExchange.Redis sends CLIENT SETINFO LIB-NAME / LIB-VER on connect.
            // Redis 7+ supports this; aikv returns OK to avoid ERR noise and connection churn.
            "SETINFO" => Ok(ok()),
            _ => Err(Error::Command("ERR unknown CLIENT subcommand".into())),
        }
    }
}

pub(crate) fn wrong_args(cmd: &str, expected: &str) -> Error {
    Error::Command(format!(
        "ERR wrong number of arguments for '{cmd}' command{expected}"
    ))
}

pub(crate) fn require_args(cmd: &str, args: &[Bytes], n: usize) -> Result<()> {
    if args.len() != n {
        return Err(wrong_args(cmd, ""));
    }
    Ok(())
}

pub(crate) fn require_min_args(cmd: &str, args: &[Bytes], n: usize) -> Result<()> {
    if args.len() < n {
        return Err(wrong_args(cmd, ""));
    }
    Ok(())
}

fn scan_key_count(value: &RespValue) -> u64 {
    let RespValue::Array(Some(parts)) = value else {
        return 0;
    };
    let Some(RespValue::Array(Some(keys))) = parts.get(1) else {
        return 0;
    };
    keys.len() as u64
}

pub(crate) fn ok() -> RespValue {
    RespValue::SimpleString("OK".into())
}

pub(crate) fn nil_bulk() -> RespValue {
    RespValue::BulkString(None)
}

pub(crate) fn bulk(b: Vec<u8>) -> RespValue {
    RespValue::BulkString(Some(Bytes::from(b)))
}

pub(crate) fn integer(n: i64) -> RespValue {
    RespValue::Integer(n)
}

pub(crate) fn wrongtype() -> Error {
    Error::Command(crate::storage::types::WRONGTYPE.into())
}
