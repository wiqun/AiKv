//! TCP Listener

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::error::Result;
use crate::server::config::ServerSharedState;
use crate::server::connection::Connection;

/// AiKv TCP 服务
pub struct Server;

impl Server {
    /// 绑定地址并进入 accept 循环
    pub async fn run(addr: SocketAddr, state: Arc<ServerSharedState>) -> Result<()> {
        let listener = TcpListener::bind(addr).await?;
        Self::run_with_listener(listener, state).await
    }

    /// 使用已有 listener 进入 accept 循环 (测试用)
    pub async fn run_with_listener(
        listener: TcpListener,
        state: Arc<ServerSharedState>,
    ) -> Result<()> {
        let addr = listener.local_addr()?;
        tracing::info!(%addr, "aikv listening");

        loop {
            tokio::select! {
              _ = state.shutdown.cancelled() => {
                tracing::info!("aikv shutdown requested");
                break;
              }
              accept_result = listener.accept() => {
                let (stream, remote) = match accept_result {
                    Ok(pair) => pair,
                    Err(err) => {
                        let errno = err.raw_os_error().map(|code| code.to_string()).unwrap_or_else(|| "other".into());
                        state.metrics.note_accept_error(&errno);
                        tracing::warn!(%errno, "accept failed");
                        continue;
                    }
                };
                tracing::debug!(%remote, "kv.accept");
                let Some(permit) = state.try_register_connection() else {
                  tracing::warn!(%remote, "connection rejected: maxclients reached");
                  drop(stream);
                  continue;
                };
                let conn_state = Arc::clone(&state);
                tokio::spawn(async move {
                  let _permit = permit;
                  Connection::handle(stream, remote, conn_state).await;
                });
              }
            }
        }

        Ok(())
    }
}
