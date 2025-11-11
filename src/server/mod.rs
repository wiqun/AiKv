pub mod connection;

use self::connection::Connection;
use crate::command::CommandExecutor;
use crate::error::Result;
use crate::storage::StorageAdapter;
use tokio::net::TcpListener;
use tracing::{error, info};

/// AiKv server
pub struct Server {
    addr: String,
    storage: StorageAdapter,
}

impl Server {
    pub fn new(addr: String) -> Self {
        Self {
            addr,
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

                    let executor = CommandExecutor::new(self.storage.clone());

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
