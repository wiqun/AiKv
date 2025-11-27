use crate::command::CommandExecutor;
use crate::error::Result;
use crate::observability::Metrics;
use crate::protocol::{RespParser, RespValue};
use crate::server::monitor::MonitorBroadcaster;
use bytes::Bytes;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::select;
use tracing::{debug, warn};

static CLIENT_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Protocol version
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProtocolVersion {
    Resp2,
    Resp3,
}

/// Connection mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConnectionMode {
    Normal,
    Monitor,
}

/// Connection handler for a single client
pub struct Connection {
    stream: TcpStream,
    parser: RespParser,
    executor: CommandExecutor,
    protocol_version: ProtocolVersion,
    current_db: usize,
    client_id: usize,
    metrics: Option<Arc<Metrics>>,
    client_addr: String,
    monitor_broadcaster: Option<Arc<MonitorBroadcaster>>,
    mode: ConnectionMode,
}

impl Connection {
    pub fn new(
        stream: TcpStream,
        executor: CommandExecutor,
        metrics: Option<Arc<Metrics>>,
        monitor_broadcaster: Option<Arc<MonitorBroadcaster>>,
    ) -> Self {
        let client_id = CLIENT_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        let peer_addr = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // Register client
        if let Err(e) = executor
            .server_commands()
            .register_client(client_id, peer_addr.clone())
        {
            warn!("Failed to register client: {}", e);
        }

        Self {
            stream,
            parser: RespParser::new(8192),
            executor,
            protocol_version: ProtocolVersion::Resp2, // Default to RESP2
            current_db: 0,                            // Default to database 0
            client_id,
            metrics,
            client_addr: peer_addr,
            monitor_broadcaster,
            mode: ConnectionMode::Normal,
        }
    }

    /// Handle the connection using a state machine
    pub async fn handle(&mut self) -> Result<()> {
        loop {
            match self.mode {
                ConnectionMode::Normal => {
                    if !self.handle_normal_mode().await? {
                        break;
                    }
                }
                ConnectionMode::Monitor => {
                    if !self.handle_monitor_mode().await? {
                        break;
                    }
                }
            }
        }

        self.cleanup().await;
        Ok(())
    }

    /// Handle normal command mode. Returns false if connection should close.
    async fn handle_normal_mode(&mut self) -> Result<bool> {
        // Read data from the client
        let n = self.stream.read_buf(self.parser.buffer_mut()).await?;

        if n == 0 {
            // Connection closed
            return Ok(false);
        }

        // Record bytes received
        if let Some(ref metrics) = self.metrics {
            metrics.connections.record_bytes_received(n as u64);
        }

        // Parse and process commands
        while let Some(value) = self.parser.parse()? {
            let response = self.process_command(value).await;
            self.write_response(response).await?;

            // Check if mode changed to monitor
            if self.mode == ConnectionMode::Monitor {
                return Ok(true);
            }
        }

        Ok(true)
    }

    /// Handle monitor mode - stream all commands to this client.
    /// Returns false if connection should close.
    async fn handle_monitor_mode(&mut self) -> Result<bool> {
        let broadcaster = match &self.monitor_broadcaster {
            Some(b) => b.clone(),
            None => {
                warn!("Monitor mode enabled but no broadcaster available");
                self.mode = ConnectionMode::Normal;
                return Ok(true);
            }
        };

        let mut receiver = broadcaster.subscribe();

        loop {
            select! {
                // Receive monitor messages
                msg = receiver.recv() => {
                    match msg {
                        Ok(monitor_msg) => {
                            // Format and send the monitor message
                            let formatted = monitor_msg.format();
                            let response = RespValue::simple_string(formatted);
                            if let Err(e) = self.write_response(response).await {
                                debug!("Monitor client write error: {}", e);
                                return Ok(false);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // We missed some messages due to slow reading
                            debug!("Monitor client {} lagged behind by {} messages", self.client_id, n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Broadcaster closed
                            return Ok(false);
                        }
                    }
                }
                // Check for client input (QUIT, RESET, or disconnect)
                result = self.stream.read_buf(self.parser.buffer_mut()) => {
                    match result {
                        Ok(0) => {
                            // Client disconnected
                            broadcaster.unregister_monitor(self.client_id).await;
                            return Ok(false);
                        }
                        Ok(_) => {
                            // Client sent data - check for QUIT or RESET
                            while let Some(value) = self.parser.parse()? {
                                if let RespValue::Array(Some(arr)) = &value {
                                    if !arr.is_empty() {
                                        if let RespValue::BulkString(Some(cmd)) = &arr[0] {
                                            let command = String::from_utf8_lossy(cmd).to_uppercase();
                                            if command == "QUIT" {
                                                broadcaster.unregister_monitor(self.client_id).await;
                                                self.write_response(RespValue::ok()).await?;
                                                return Ok(false);
                                            } else if command == "RESET" {
                                                broadcaster.unregister_monitor(self.client_id).await;
                                                self.mode = ConnectionMode::Normal;
                                                self.write_response(RespValue::simple_string("RESET")).await?;
                                                return Ok(true);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Monitor client read error: {}", e);
                            broadcaster.unregister_monitor(self.client_id).await;
                            return Ok(false);
                        }
                    }
                }
            }
        }
    }

    /// Cleanup on connection close
    async fn cleanup(&mut self) {
        // Unregister client
        if let Err(e) = self
            .executor
            .server_commands()
            .unregister_client(self.client_id)
        {
            warn!("Failed to unregister client: {}", e);
        }

        // Unregister from monitor if in monitor mode
        if self.mode == ConnectionMode::Monitor {
            if let Some(ref broadcaster) = self.monitor_broadcaster {
                broadcaster.unregister_monitor(self.client_id).await;
            }
        }
    }

    async fn process_command(&mut self, value: RespValue) -> RespValue {
        let start = Instant::now();

        match value {
            RespValue::Array(Some(arr)) if !arr.is_empty() => {
                // Extract command and arguments
                let command = match &arr[0] {
                    RespValue::BulkString(Some(cmd)) => String::from_utf8_lossy(cmd).to_string(),
                    _ => {
                        return RespValue::error("ERR invalid command format");
                    }
                };

                let command_upper = command.to_uppercase();

                // Handle HELLO command for protocol version negotiation
                if command_upper == "HELLO" {
                    return self.handle_hello(&arr[1..]);
                }

                // Handle MONITOR command
                if command_upper == "MONITOR" {
                    return self.handle_monitor().await;
                }

                let args: Vec<Bytes> = arr[1..]
                    .iter()
                    .filter_map(|v| match v {
                        RespValue::BulkString(Some(b)) => Some(b.clone()),
                        _ => None,
                    })
                    .collect();

                // Broadcast to monitors (except MONITOR itself and some internal commands)
                if !matches!(
                    command_upper.as_str(),
                    "MONITOR" | "DEBUG" | "SYNC" | "PSYNC"
                ) {
                    self.broadcast_to_monitors(&command_upper, &args);
                }

                let result =
                    self.executor
                        .execute(&command, &args, &mut self.current_db, self.client_id);

                // Record metrics
                if let Some(ref metrics) = self.metrics {
                    let duration = start.elapsed();
                    match &result {
                        Ok(_) => {
                            metrics.commands.record_command(&command, duration);
                            debug!(
                                command = %command,
                                duration_us = duration.as_micros(),
                                client = %self.client_addr,
                                db = self.current_db,
                                "Command executed"
                            );
                        }
                        Err(_) => {
                            metrics.commands.record_error(&command);
                        }
                    }
                }

                match result {
                    Ok(resp) => resp,
                    Err(e) => RespValue::error(format!("ERR {}", e)),
                }
            }
            _ => RespValue::error("ERR invalid command format"),
        }
    }

    /// Broadcast command to all monitoring clients
    fn broadcast_to_monitors(&self, command: &str, args: &[Bytes]) {
        if let Some(ref broadcaster) = self.monitor_broadcaster {
            if broadcaster.has_monitors() {
                let args_str: Vec<String> = args
                    .iter()
                    .map(|b| String::from_utf8_lossy(b).to_string())
                    .collect();
                broadcaster.broadcast_command(
                    self.current_db,
                    &self.client_addr,
                    command,
                    &args_str,
                );
            }
        }
    }

    /// Handle MONITOR command
    async fn handle_monitor(&mut self) -> RespValue {
        if let Some(ref broadcaster) = self.monitor_broadcaster {
            broadcaster
                .register_monitor(self.client_id, self.client_addr.clone())
                .await;
            self.mode = ConnectionMode::Monitor;
            RespValue::ok()
        } else {
            RespValue::error("ERR MONITOR not supported")
        }
    }

    fn handle_hello(&mut self, args: &[RespValue]) -> RespValue {
        if args.is_empty() {
            return RespValue::error("ERR wrong number of arguments for 'hello' command");
        }

        // Parse protocol version
        let version_str = match &args[0] {
            RespValue::BulkString(Some(v)) => String::from_utf8_lossy(v).to_string(),
            _ => return RespValue::error("ERR invalid protocol version"),
        };

        let version = match version_str.as_str() {
            "2" => ProtocolVersion::Resp2,
            "3" => ProtocolVersion::Resp3,
            _ => return RespValue::error("NOPROTO unsupported protocol version"),
        };

        self.protocol_version = version;

        // Build response based on protocol version
        match self.protocol_version {
            ProtocolVersion::Resp2 => {
                // RESP2 response: array
                RespValue::array(vec![
                    RespValue::bulk_string("server"),
                    RespValue::bulk_string("aikv"),
                    RespValue::bulk_string("version"),
                    RespValue::bulk_string("0.1.0"),
                    RespValue::bulk_string("proto"),
                    RespValue::integer(2),
                ])
            }
            ProtocolVersion::Resp3 => {
                // RESP3 response: map
                RespValue::map(vec![
                    (
                        RespValue::simple_string("server"),
                        RespValue::simple_string("aikv"),
                    ),
                    (
                        RespValue::simple_string("version"),
                        RespValue::simple_string("0.1.0"),
                    ),
                    (RespValue::simple_string("proto"), RespValue::integer(3)),
                ])
            }
        }
    }

    async fn write_response(&mut self, response: RespValue) -> Result<()> {
        let data = response.serialize();

        // Record bytes sent
        if let Some(ref metrics) = self.metrics {
            metrics.connections.record_bytes_sent(data.len() as u64);
        }

        self.stream.write_all(&data).await?;
        self.stream.flush().await?;
        Ok(())
    }
}
