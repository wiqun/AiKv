pub mod connection;

use self::connection::Connection;
use crate::command::CommandExecutor;
use crate::error::Result;
use crate::storage::StorageAdapter;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{error, info};

/// AiKv server
pub struct Server {
    addr: String,
    port: u16,
    storage: StorageAdapter,
}

impl Server {
    pub fn new(addr: String) -> Self {
        // Extract port from address string using proper SocketAddr parsing
        // This handles both IPv4 (127.0.0.1:6379) and IPv6 ([::1]:6379) formats
        let port = addr
            .parse::<SocketAddr>()
            .map(|a| a.port())
            .unwrap_or_else(|_| {
                // Fallback: try to extract port from the end after last ':'
                // This handles edge cases where the string isn't a valid SocketAddr
                addr.rsplit(':')
                    .next()
                    .and_then(|p| p.trim_end_matches(']').parse().ok())
                    .unwrap_or(6379)
            });

        Self {
            addr,
            port,
            storage: StorageAdapter::new(),
        }
    }

    /// Run the server
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.addr).await?;
        info!("AiKv server listening on {}", self.addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("New connection from: {}", addr);

                    let executor = CommandExecutor::with_port(self.storage.clone(), self.port);

                    tokio::spawn(async move {
                        let mut conn = Connection::new(stream, executor);

                        if let Err(e) = conn.handle().await {
                            error!("Connection error: {}", e);
                        }

                        info!("Connection closed: {}", addr);
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }
}
