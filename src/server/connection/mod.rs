//! 单连接处理: 每连接一个 tokio task (`Connection::handle`), 负责 TCP 读写、RESP 解析、
//! 协议分发 (内联命令 / ATOM 事务 / MONITOR / Router)、HELLO 协商与响应编码.
//! 连接级状态 (`current_db`, `parser`, `tx_state`, `cluster_state`) 不跨连接共享.
//!
//! # 读-解析-写 pipeline 循环
//!
//! ```text
//! run():
//!   loop {
//!     ├─ shutdown / quit / idle_timeout → break
//!     ├─ read_buf (16 KiB, 受 read_timeout 约束)
//!     │    ├─ n == 0 (客户端关闭) → break
//!     │    └─ buffer_len + n > max_buffer_size → break (静默断连, 不写 ERR)
//!     ├─ parser.feed(&buf[..n])
//!     └─ 内层循环 (pipeline):
//!          parse_frame
//!            ├─ Ok(Some) → process_value → queue_response → (批末或立即命令) flush
//!            ├─ Ok(None) → break 内层 → flush_responses → 等更多数据
//!            ├─ Err fatal (is_fatal_protocol) → best-effort flush → return Err (断连)
//!            └─ Err recoverable → queue_error → 继续 (同批末 flush)
//!   }
//!
//! process_value : 顶层必须是 Array, 命令名与参数必须是 BulkString
//! process_command: PING/ECHO/HELLO/QUIT/MONITOR/ATOM.* 内联; 其余经 Router
//! write_response : queue_response (encode_into → write_buf); 不单独 write_all
//! flush_responses: write_all(write_buf) + clear; capacity>64KiB 时 shrink 回 8KiB
//! 立即 flush: QUIT / MONITOR / EXEC / ATOM.EXEC / SHUTDOWN
//! 阻塞前置 flush: registry flag `blocking` (BLPOP/BRPOP/BLMOVE/BZPOP*) 在 await 前刷出同批响应
//! ```
//!
//! # Invariant
//!
//! - 每连接一 task: `tokio::spawn(Connection::handle)`; 连接状态不跨连接共享.
//! - Pipeline 内层循环: 单次 `read` 后 `feed`, 然后循环 `parse_frame → process_value`
//!   直到 `Ok(None)`, 批末统一 `flush_responses` (与 `protocol/parser.rs` 单帧语义一致).
//! - Buffer 超限断连: `buffer_len + n > max_buffer_size()` 时直接 break, 不写 ERR.
//! - Fatal vs recoverable: `is_fatal_protocol` (depth / too large / buffer size /
//!   line too long) → 断连; 其它 `Protocol` → `queue_error` 后继续.
//! - HELLO 门控线格式: 默认 `ProtocolVersion::Resp3` 但 `protocol_negotiated = false`
//!   直到客户端发 `HELLO 2|3`; 仅协商 Resp3 后 `adapt_for_protocol` 才把
//!   `$-1`/`*-1` 转为 `_` (未协商时保持 RESP2 线格式).
//! - Router 懒加载: `ServerSharedState::router()` 经 `OnceLock` 首次调用时建
//!   `CommandRouter`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::{Bytes, BytesMut};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time;

use crate::command;
use crate::error::{Error, Result};
use crate::protocol::{is_fatal_protocol, ProtocolVersion, RespParser, RespValue};
use crate::server::config::ServerSharedState;

mod atom;
mod monitor;
pub(crate) mod pipeline;
mod protocol;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionMode {
    Normal,
    Monitor,
}

/// 原子事务状态 (ATOM.MULTI / EXEC / DISCARD / WATCH)
struct TransactionState {
    in_multi: bool,
    tx_queue: Vec<(String, Vec<Bytes>)>,
    watched_keys: HashMap<(usize, Vec<u8>), u64>,
}

impl TransactionState {
    fn new() -> Self {
        Self {
            in_multi: false,
            tx_queue: Vec::new(),
            watched_keys: HashMap::new(),
        }
    }

    fn reset(&mut self) {
        self.in_multi = false;
        self.tx_queue.clear();
        self.watched_keys.clear();
    }
}

pub struct Connection {
    stream: TcpStream,
    remote: SocketAddr,
    parser: RespParser,
    write_buf: BytesMut,
    state: Arc<ServerSharedState>,
    protocol_version: ProtocolVersion,
    /// 客户端是否通过 HELLO 命令显式协商过协议版本.
    /// 仅在协商后才会按 RESP3 格式编码 null(`_` 替代 `$-1`).
    protocol_negotiated: bool,
    last_active: Instant,
    client_id: usize,
    current_db: usize,
    quit: bool,
    mode: ConnectionMode,
    monitor_rx: Option<broadcast::Receiver<String>>,
    #[cfg(feature = "cluster")]
    /// 集群模式连接级状态 (asking/readonly)
    cluster_state: crate::cluster::connection::ClusterConnectionState,
    /// 原子事务状态 (ATOM.MULTI / EXEC / DISCARD / WATCH)
    tx_state: TransactionState,
    pending_timing: Option<self::pipeline::PendingBatchTiming>,
}

impl Connection {
    pub async fn handle(stream: TcpStream, remote: SocketAddr, state: Arc<ServerSharedState>) {
        let client_id = state.alloc_client_id();
        state.register_client(client_id, remote);
        let mut conn = Self {
            stream,
            remote,
            parser: RespParser::new(),
            write_buf: BytesMut::with_capacity(protocol::DEFAULT_WRITE_BUF_CAPACITY),
            state: state.clone(),
            protocol_version: ProtocolVersion::default(),
            protocol_negotiated: false,
            last_active: Instant::now(),
            client_id,
            current_db: 0,
            quit: false,
            mode: ConnectionMode::Normal,
            monitor_rx: None,
            #[cfg(feature = "cluster")]
            cluster_state: crate::cluster::connection::ClusterConnectionState::new(),
            tx_state: TransactionState::new(),
            pending_timing: None,
        };
        tracing::debug!(remote = %remote, client_id, "kv.connection.open");
        let _ = conn.run().await;
        let gate = Arc::clone(&conn.state.transaction_gate);
        let _guard = gate.write_owned().await;
        conn.release_watches();
        drop(_guard);
        state.unregister_client(client_id);
        tracing::debug!(remote = %conn.remote, client_id, "kv.connection.close");
    }

    async fn run(&mut self) -> Result<()> {
        let config = Arc::clone(&self.state.connection_config);
        let mut buf = vec![0u8; 65536];
        let conn_start = Instant::now();

        loop {
            if self.state.shutdown.is_cancelled() {
                tracing::debug!(
                    target: "kv.conn.close_reason",
                    client_id = self.client_id,
                    reason = "shutdown",
                    alive_ms = conn_start.elapsed().as_millis() as u64,
                    "kv.connection.close.reason"
                );
                self.state.metrics.note_connection_close("shutdown");
                break;
            }
            if self.quit {
                tracing::debug!(
                    target: "kv.conn.close_reason",
                    client_id = self.client_id,
                    reason = "quit",
                    alive_ms = conn_start.elapsed().as_millis() as u64,
                    "kv.connection.close.reason"
                );
                self.state.metrics.note_connection_close("quit");
                break;
            }

            if self.mode == ConnectionMode::Monitor {
                self.run_monitor(&mut buf).await?;
                break;
            }

            if let Some(idle) = config.idle_timeout {
                if self.last_active.elapsed() > idle {
                    tracing::debug!(
                        target: "kv.conn.close_reason",
                        client_id = self.client_id,
                        reason = "idle_timeout",
                        idle_ms = self.last_active.elapsed().as_millis() as u64,
                        alive_ms = conn_start.elapsed().as_millis() as u64,
                        "kv.connection.close.reason"
                    );
                    self.state.metrics.note_connection_close("idle_timeout");
                    break;
                }
            }

            let n = if let Some(timeout) = config.read_timeout {
                match time::timeout(timeout, self.read_buf(&mut buf)).await {
                    Ok(Ok(n)) => n,
                    Ok(Err(e)) => {
                        tracing::debug!(
                            target: "kv.conn.close_reason",
                            client_id = self.client_id,
                            reason = "read_error",
                            error = %e,
                            alive_ms = conn_start.elapsed().as_millis() as u64,
                            "kv.connection.close.reason"
                        );
                        self.state.metrics.note_connection_close("read_error");
                        return Err(e.into());
                    }
                    Err(_) => {
                        tracing::debug!(
                            target: "kv.conn.close_reason",
                            client_id = self.client_id,
                            reason = "read_timeout",
                            timeout_ms = timeout.as_millis() as u64,
                            alive_ms = conn_start.elapsed().as_millis() as u64,
                            "kv.connection.close.reason"
                        );
                        self.state.metrics.note_connection_close("read_timeout");
                        break;
                    }
                }
            } else {
                self.read_buf(&mut buf).await?
            };

            if n == 0 {
                tracing::debug!(
                    target: "kv.conn.close_reason",
                    client_id = self.client_id,
                    reason = "client_closed",
                    alive_ms = conn_start.elapsed().as_millis() as u64,
                    "kv.connection.close.reason"
                );
                self.state.metrics.note_connection_close("client_closed");
                break;
            }

            if self.parser.buffer_len() + n > self.parser.max_buffer_size() {
                tracing::debug!(
                    target: "kv.conn.close_reason",
                    client_id = self.client_id,
                    reason = "buffer_overflow",
                    buflen = self.parser.buffer_len(),
                    added = n,
                    alive_ms = conn_start.elapsed().as_millis() as u64,
                    "kv.connection.close.reason"
                );
                self.state.metrics.note_connection_close("buffer_overflow");
                break;
            }

            self.parser.feed(&buf[..n]);
            let drained = self.drain_ready(&mut buf)?;

            loop {
                match self.process_next_batch(&mut buf).await {
                    Ok(true) => continue,
                    Ok(false) => break,
                    Err(e) if is_fatal_protocol(&e) => {
                        let _ = self.flush_responses().await;
                        return Err(e);
                    }
                    Err(e) => {
                        self.write_error(&e).await?;
                    }
                }
            }
            self.flush_responses().await?;
            if let Some(reason) = self::pipeline::close_reason(drained) {
                self.state.metrics.note_connection_close(reason);
                break;
            }
        }
        Ok(())
    }

    async fn process_next_batch(&mut self, buf: &mut [u8]) -> Result<bool> {
        let first = match self.parse_frame().await {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(false),
            Err(e) => return Err(e),
        };

        let is_immediate = should_flush_immediately(&first);
        let is_blocking = is_blocking_command(&first);
        if is_immediate || is_blocking {
            self.process_single_frame(first, buf).await?;
            return Ok(true);
        }

        let mut batch = vec![first];
        while batch.len() < 128 {
            match self.parse_frame().await {
                Ok(Some(value)) => {
                    let is_imm = should_flush_immediately(&value);
                    let is_blk = is_blocking_command(&value);
                    batch.push(value);
                    if is_imm || is_blk {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(e),
            }
        }

        let batch_len = batch.len();
        self::pipeline::record_batch_len(batch_len);

        if batch_len == 1 {
            self.process_single_frame(batch.pop().unwrap(), buf).await?;
        } else {
            if let Err(e) = self.process_pipeline_batch(batch).await {
                let _ = self.flush_responses().await;
                return Err(e);
            }
            self.last_active = Instant::now();
            if self.quit {
                self.flush_responses().await?;
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn process_single_frame(&mut self, value: RespValue, buf: &mut [u8]) -> Result<()> {
        let is_immediate_flush = should_flush_immediately(&value);
        if is_blocking_command(&value) {
            self.flush_responses().await?;
        }
        if let Err(e) = self.process_value(value).await {
            let _ = self.flush_responses().await;
            return Err(e);
        }
        self.last_active = Instant::now();
        if is_immediate_flush || self.quit {
            self.flush_responses().await?;
        }
        if self.mode == ConnectionMode::Monitor {
            self.flush_responses().await?;
            self.run_monitor(buf).await?;
        }
        Ok(())
    }

    async fn read_buf(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.stream.read(buf).await?;
        if n > 0 {
            if let Some(timing) = self.pending_timing.take() {
                timing.report_read_next(n);
            }
            self.state.metrics().on_net_input_bytes(n as u64);
            tracing::debug!(bytes = n, "kv.read.complete");
        }
        Ok(n)
    }

    async fn parse_frame(&mut self) -> Result<Option<RespValue>> {
        let before = self.parser.buffer_len();
        let result = self.parser.parse();
        if let Ok(Some(_)) = &result {
            let frame_size = before.saturating_sub(self.parser.buffer_len());
            tracing::debug!(frame_size, "kv.parse.complete");
        }
        result
    }

    async fn process_value(&mut self, value: RespValue) -> Result<()> {
        let RespValue::Array(items) = value else {
            return self
                .write_response(RespValue::Error("Protocol error: expected array".into()))
                .await;
        };

        let Some(items) = items else {
            return Ok(());
        };
        if items.is_empty() {
            return Ok(());
        }

        let Some(cmd_bytes) = extract_bulk(&items[0]) else {
            return self
                .write_response(RespValue::Error(
                    "Protocol error: command must be bulk string".into(),
                ))
                .await;
        };
        let cmd = String::from_utf8_lossy(&cmd_bytes).to_ascii_uppercase();
        let args: Result<Vec<Bytes>> = items[1..]
            .iter()
            .map(|v| {
                extract_bulk(v).ok_or_else(|| {
                    Error::Protocol("Protocol error: arguments must be bulk strings".into())
                })
            })
            .collect();
        let args = match args {
            Ok(a) => a,
            Err(e) => return self.write_error(&e).await,
        };

        self.process_command(&cmd, &args).await
    }

    fn release_watches(&mut self) {
        let registry = self.state.storage.watch_registry();
        for (db, key) in self.tx_state.watched_keys.keys() {
            registry.unwatch(*db, key);
        }
        self.tx_state.watched_keys.clear();
    }

    async fn process_command(&mut self, cmd: &str, args: &[Bytes]) -> Result<()> {
        if cmd != "MONITOR" && self.mode != ConnectionMode::Monitor {
            self.broadcast_monitor(cmd, args);
        }

        match cmd {
            "MULTI" | "ATOM.MULTI" => self.cmd_atom_multi().await,
            "EXEC" | "ATOM.EXEC" => self.cmd_atom_exec(args).await,
            "DISCARD" | "ATOM.DISCARD" => self.cmd_atom_discard().await,
            "WATCH" | "ATOM.WATCH" => self.cmd_atom_watch(args).await,
            "UNWATCH" | "ATOM.UNWATCH" => self.cmd_atom_unwatch().await,
            "PING" | "ECHO" | "HELLO" | "QUIT" | "MONITOR" => {
                let gate = Arc::clone(&self.state.transaction_gate);
                let _guard = gate.read_owned().await;
                self.process_inline_command(cmd, args).await
            }
            _ => {
                if self.tx_state.in_multi {
                    return self.cmd_atom_enqueue(cmd, args).await;
                }
                let is_blocking =
                    command::lookup(cmd).is_some_and(|info| info.flags.contains(&"blocking"));
                if is_blocking {
                    self.process_command_immediate(cmd, args).await
                } else {
                    let gate = Arc::clone(&self.state.transaction_gate);
                    let _guard = gate.read_owned().await;
                    self.process_command_immediate(cmd, args).await
                }
            }
        }
    }

    async fn process_inline_command(&mut self, cmd: &str, args: &[Bytes]) -> Result<()> {
        match cmd {
            "PING" => self.cmd_ping(args).await,
            "ECHO" => self.cmd_echo(args).await,
            "HELLO" => self.cmd_hello(args).await,
            "QUIT" => self.cmd_quit().await,
            "MONITOR" => self.cmd_monitor().await,
            _ => unreachable!("non-inline command"),
        }
    }

    async fn process_command_immediate(&mut self, cmd: &str, args: &[Bytes]) -> Result<()> {
        #[cfg(feature = "cluster")]
        if cmd.eq_ignore_ascii_case("asking") {
            self.cluster_state.set_asking(true);
            return self
                .write_response(RespValue::SimpleString("OK".into()))
                .await;
        }
        #[cfg(feature = "cluster")]
        if cmd.eq_ignore_ascii_case("readonly") {
            self.cluster_state.set_readonly(true);
            return self
                .write_response(RespValue::SimpleString("OK".into()))
                .await;
        }
        #[cfg(feature = "cluster")]
        if cmd.eq_ignore_ascii_case("readwrite") {
            self.cluster_state.set_readonly(false);
            return self
                .write_response(RespValue::SimpleString("OK".into()))
                .await;
        }

        let track = should_track_observability(cmd);
        let arg_strings: Vec<String> = if track {
            args.iter()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .collect()
        } else {
            Vec::new()
        };
        let started = track.then(Instant::now);
        let result = self
            .state
            .router()
            .execute_with_client(
                cmd,
                args,
                &mut self.current_db,
                Some(self.client_id),
                Some(self.remote),
                self.protocol_version,
                #[cfg(feature = "cluster")]
                Some(&self.cluster_state),
            )
            .await;
        #[cfg(feature = "cluster")]
        self.cluster_state.reset_asking();
        self.finish_immediate_command(cmd, args, arg_strings, started, result)
            .await
    }

    pub(super) async fn finish_immediate_command(
        &mut self,
        cmd: &str,
        _args: &[Bytes],
        arg_strings: Vec<String>,
        started: Option<Instant>,
        result: Result<RespValue>,
    ) -> Result<()> {
        match result {
            Ok(resp) => {
                self.state.set_client_db(self.client_id, self.current_db);
                if let Some(start) = started {
                    #[cfg(feature = "cluster")]
                    let skip_obs = is_cluster_redirect_response(&resp);
                    #[cfg(not(feature = "cluster"))]
                    let skip_obs = false;
                    if !skip_obs {
                        self.record_command_observability(cmd, &arg_strings, start, true);
                    }
                }
                self.write_response(resp).await?;
                if cmd == "SHUTDOWN" {
                    self.quit = true;
                }
                Ok(())
            }
            Err(error) => {
                if let Some(start) = started {
                    self.record_command_observability(cmd, &arg_strings, start, false);
                }
                tracing::error!(
                  command = cmd,
                  client_id = self.client_id,
                  error = %error,
                  "kv.command.error"
                );
                self.write_error(&error).await
            }
        }
    }

    fn broadcast_monitor(&self, cmd: &str, args: &[Bytes]) {
        let line = format_monitor_line(self.current_db, cmd, args);
        let _ = self.state.monitor_tx.send(line);
    }

    async fn cmd_ping(&mut self, args: &[Bytes]) -> Result<()> {
        if args.len() > 1 {
            return self
                .write_response(RespValue::Error(
                    "ERR wrong number of arguments for 'ping' command".into(),
                ))
                .await;
        }
        if args.is_empty() {
            self.write_response(RespValue::SimpleString("PONG".into()))
                .await
        } else {
            self.write_response(RespValue::BulkString(Some(args[0].clone())))
                .await
        }
    }

    async fn cmd_echo(&mut self, args: &[Bytes]) -> Result<()> {
        if args.len() != 1 {
            return self
                .write_response(RespValue::Error(
                    "ERR wrong number of arguments for 'echo' command".into(),
                ))
                .await;
        }
        self.write_response(RespValue::BulkString(Some(args[0].clone())))
            .await
    }

    async fn cmd_hello(&mut self, args: &[Bytes]) -> Result<()> {
        if args.len() > 1 {
            return self
                .write_response(RespValue::Error(
                    "ERR wrong number of arguments for 'hello' command".into(),
                ))
                .await;
        }

        if args.len() == 1 {
            let ver = String::from_utf8_lossy(&args[0]);
            match ver.as_ref() {
                "2" => self.protocol_version = ProtocolVersion::Resp2,
                "3" => self.protocol_version = ProtocolVersion::Resp3,
                _ => {
                    return self
                        .write_response(RespValue::Error("invalid protocol version".into()))
                        .await;
                }
            }
            self.protocol_negotiated = true;
        }

        let response = hello_map(self.client_id, self.protocol_version);
        self.write_response(response).await
    }

    async fn cmd_quit(&mut self) -> Result<()> {
        self.write_response(RespValue::SimpleString("OK".into()))
            .await?;
        self.quit = true;
        Ok(())
    }

    async fn cmd_monitor(&mut self) -> Result<()> {
        self.write_response(RespValue::SimpleString("OK".into()))
            .await?;
        self.mode = ConnectionMode::Monitor;
        self.monitor_rx = Some(self.state.monitor_tx.subscribe());
        Ok(())
    }

    fn record_command_observability(&self, cmd: &str, args: &[String], start: Instant, _ok: bool) {
        let duration_us = start.elapsed().as_micros() as u64;
        self.state.latency_stats.record(cmd, duration_us);
        self.state.slow_query_log.record(
            cmd,
            args,
            duration_us,
            &self.remote.to_string(),
            self.current_db as u16,
        );
        if duration_us >= self.state.slow_query_log.threshold_us() {
            self.state.metrics().on_slowlog_command(cmd, duration_us);
            #[cfg(feature = "monitoring")]
            self.state.metrics().on_slow_query(cmd);
        }
    }
}

pub(super) fn should_track_observability(cmd: &str) -> bool {
    !matches!(
        cmd,
        "SLOWLOG" | "MONITOR" | "PING" | "ECHO" | "HELLO" | "QUIT"
    )
}

#[cfg(feature = "cluster")]
fn is_cluster_redirect_response(resp: &RespValue) -> bool {
    matches!(resp, RespValue::Error(msg) if atom::is_redirect_error_msg(msg))
}

fn format_monitor_line(db: usize, cmd: &str, args: &[Bytes]) -> String {
    let sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut line = format!("+{sec} [db {db}] \"{cmd}\"");
    for arg in args {
        let s = String::from_utf8_lossy(arg);
        line.push_str(&format!(" \"{s}\""));
    }
    line.push_str("\r\n");
    line
}

fn hello_map(client_id: usize, proto: ProtocolVersion) -> RespValue {
    #[cfg(feature = "cluster")]
    let mode = match crate::cluster::state::CLUSTER_STATE_MGR.get() {
        Some(_) => "cluster",
        None => "standalone",
    };
    #[cfg(not(feature = "cluster"))]
    let mode = "standalone";

    #[cfg(feature = "cluster")]
    let role = match crate::cluster::state::CLUSTER_STATE_MGR.get() {
        Some(mgr) => {
            let meta = mgr.meta_raft.get_cluster_meta();
            crate::cluster::replication::node_replication_role(&meta, mgr.node_id).to_string()
        }
        None => "master".to_string(),
    };
    #[cfg(not(feature = "cluster"))]
    let role = "master".to_string();

    // 对齐 Redis 7 原生 HELLO 响应格式:
    //   RESP3 Map — proto/id 为 Integer, 其余为 BulkString
    //   RESP2 Array — 全部 BulkString (RESP2 无 Map, 用 flat array 交替 key-value)
    let version = env!("CARGO_PKG_VERSION");
    match proto {
        ProtocolVersion::Resp3 => RespValue::Map(vec![
            (
                RespValue::BulkString(Some(Bytes::from_static(b"server"))),
                RespValue::BulkString(Some(Bytes::from_static(b"aikv"))),
            ),
            (
                RespValue::BulkString(Some(Bytes::from_static(b"version"))),
                RespValue::BulkString(Some(Bytes::from(version))),
            ),
            (
                RespValue::BulkString(Some(Bytes::from_static(b"proto"))),
                RespValue::Integer(proto.as_u8() as i64),
            ),
            (
                RespValue::BulkString(Some(Bytes::from_static(b"id"))),
                RespValue::Integer(client_id as i64),
            ),
            (
                RespValue::BulkString(Some(Bytes::from_static(b"mode"))),
                RespValue::BulkString(Some(Bytes::from(mode))),
            ),
            (
                RespValue::BulkString(Some(Bytes::from_static(b"role"))),
                RespValue::BulkString(Some(Bytes::from(role))),
            ),
        ]),
        ProtocolVersion::Resp2 => RespValue::Array(Some(vec![
            RespValue::BulkString(Some(Bytes::from_static(b"server"))),
            RespValue::BulkString(Some(Bytes::from_static(b"aikv"))),
            RespValue::BulkString(Some(Bytes::from_static(b"version"))),
            RespValue::BulkString(Some(Bytes::from(version))),
            RespValue::BulkString(Some(Bytes::from_static(b"proto"))),
            RespValue::BulkString(Some(Bytes::from(format!("{}", proto.as_u8())))),
            RespValue::BulkString(Some(Bytes::from_static(b"id"))),
            RespValue::BulkString(Some(Bytes::from(format!("{client_id}")))),
            RespValue::BulkString(Some(Bytes::from_static(b"mode"))),
            RespValue::BulkString(Some(Bytes::from(mode))),
            RespValue::BulkString(Some(Bytes::from_static(b"role"))),
            RespValue::BulkString(Some(Bytes::from(role))),
        ])),
    }
}

pub(super) fn extract_bulk(value: &RespValue) -> Option<Bytes> {
    match value {
        RespValue::BulkString(Some(b)) => Some(b.clone()),
        RespValue::BulkString(None) => Some(Bytes::new()),
        _ => None,
    }
}

pub(super) fn should_flush_immediately(frame: &RespValue) -> bool {
    let Some(cmd) = frame_command_bytes(frame) else {
        return false;
    };
    matches!(
        cmd.as_slice(),
        b"QUIT" | b"MONITOR" | b"EXEC" | b"ATOM.EXEC" | b"SHUTDOWN"
    )
}

pub(super) fn is_blocking_command(frame: &RespValue) -> bool {
    let Some(cmd) = frame_command_bytes(frame) else {
        return false;
    };
    let name = std::str::from_utf8(&cmd).unwrap_or("");
    command::lookup(name)
        .map(|info| info.flags.contains(&"blocking"))
        .unwrap_or(false)
}

fn frame_command_bytes(frame: &RespValue) -> Option<Vec<u8>> {
    let RespValue::Array(Some(items)) = frame else {
        return None;
    };
    let cmd_bytes = items.first().and_then(extract_bulk)?;
    Some(cmd_bytes.to_ascii_uppercase())
}
