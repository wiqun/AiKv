use crate::command::CommandExecutor;
use crate::error::Result;
use crate::protocol::{RespParser, RespValue};
use bytes::Bytes;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

static CLIENT_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Protocol version
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProtocolVersion {
    Resp2,
    Resp3,
}

/// Connection handler for a single client
pub struct Connection {
    stream: TcpStream,
    parser: RespParser,
    executor: CommandExecutor,
    protocol_version: ProtocolVersion,
    current_db: usize,
    client_id: usize,
}

impl Connection {
    pub fn new(stream: TcpStream, executor: CommandExecutor) -> Self {
        let client_id = CLIENT_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        let peer_addr = stream
            .peer_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        // Register client
        if let Err(e) = executor
            .server_commands()
            .register_client(client_id, peer_addr)
        {
            eprintln!("Failed to register client: {}", e);
        }

        Self {
            stream,
            parser: RespParser::new(8192),
            executor,
            protocol_version: ProtocolVersion::Resp2, // Default to RESP2
            current_db: 0,                            // Default to database 0
            client_id,
        }
    }

    /// Handle the connection
    pub async fn handle(&mut self) -> Result<()> {
        loop {
            // Read data from the client
            let n = self.stream.read_buf(self.parser.buffer_mut()).await?;

            if n == 0 {
                // Connection closed - unregister client
                if let Err(e) = self
                    .executor
                    .server_commands()
                    .unregister_client(self.client_id)
                {
                    eprintln!("Failed to unregister client: {}", e);
                }
                return Ok(());
            }

            // Parse and process commands
            while let Some(value) = self.parser.parse()? {
                let response = self.process_command(value).await;
                self.write_response(response).await?;
            }
        }
    }

    async fn process_command(&mut self, value: RespValue) -> RespValue {
        match value {
            RespValue::Array(Some(arr)) if !arr.is_empty() => {
                // Extract command and arguments
                let command = match &arr[0] {
                    RespValue::BulkString(Some(cmd)) => String::from_utf8_lossy(cmd).to_string(),
                    _ => {
                        return RespValue::error("ERR invalid command format");
                    }
                };

                // Handle HELLO command for protocol version negotiation
                if command.to_uppercase() == "HELLO" {
                    return self.handle_hello(&arr[1..]);
                }

                let args: Vec<Bytes> = arr[1..]
                    .iter()
                    .filter_map(|v| match v {
                        RespValue::BulkString(Some(b)) => Some(b.clone()),
                        _ => None,
                    })
                    .collect();

                match self
                    .executor
                    .execute(&command, &args, &mut self.current_db, self.client_id)
                {
                    Ok(resp) => resp,
                    Err(e) => RespValue::error(format!("ERR {}", e)),
                }
            }
            _ => RespValue::error("ERR invalid command format"),
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
        self.stream.write_all(&data).await?;
        self.stream.flush().await?;
        Ok(())
    }
}
