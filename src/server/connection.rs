use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use bytes::Bytes;
use crate::error::Result;
use crate::protocol::{RespParser, RespValue};
use crate::command::CommandExecutor;

/// Connection handler for a single client
pub struct Connection {
    stream: TcpStream,
    parser: RespParser,
    executor: CommandExecutor,
}

impl Connection {
    pub fn new(stream: TcpStream, executor: CommandExecutor) -> Self {
        Self {
            stream,
            parser: RespParser::new(8192),
            executor,
        }
    }

    /// Handle the connection
    pub async fn handle(&mut self) -> Result<()> {
        loop {
            // Read data from the client
            let n = self.stream.read_buf(self.parser.buffer_mut()).await?;
            
            if n == 0 {
                // Connection closed
                return Ok(());
            }

            // Parse and process commands
            loop {
                match self.parser.parse()? {
                    Some(value) => {
                        let response = self.process_command(value).await;
                        self.write_response(response).await?;
                    }
                    None => break, // Need more data
                }
            }
        }
    }

    async fn process_command(&self, value: RespValue) -> RespValue {
        match value {
            RespValue::Array(Some(arr)) if !arr.is_empty() => {
                // Extract command and arguments
                let command = match &arr[0] {
                    RespValue::BulkString(Some(cmd)) => {
                        String::from_utf8_lossy(cmd).to_string()
                    }
                    _ => {
                        return RespValue::error("ERR invalid command format");
                    }
                };

                let args: Vec<Bytes> = arr[1..].iter()
                    .filter_map(|v| match v {
                        RespValue::BulkString(Some(b)) => Some(b.clone()),
                        _ => None,
                    })
                    .collect();

                match self.executor.execute(&command, &args) {
                    Ok(resp) => resp,
                    Err(e) => RespValue::error(format!("ERR {}", e)),
                }
            }
            _ => RespValue::error("ERR invalid command format"),
        }
    }

    async fn write_response(&mut self, response: RespValue) -> Result<()> {
        let data = response.serialize();
        self.stream.write_all(&data).await?;
        self.stream.flush().await?;
        Ok(())
    }
}
