//! MONITOR 模式: 订阅广播并继续接受 QUIT.

use std::sync::Arc;
use std::time::Instant;

use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio::time;

use crate::error::{Error, Result};
use crate::protocol::{is_fatal_protocol, RespValue};

use super::{extract_bulk, Connection};

impl Connection {
    pub(super) async fn run_monitor(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut rx = self
            .monitor_rx
            .take()
            .expect("monitor mode requires monitor_rx");
        let result = self.run_monitor_loop(&mut rx, buf).await;
        self.monitor_rx = Some(rx);
        result
    }

    async fn run_monitor_loop(
        &mut self,
        rx: &mut broadcast::Receiver<String>,
        buf: &mut [u8],
    ) -> Result<()> {
        let config = Arc::clone(&self.state.connection_config);

        loop {
            if self.quit {
                break;
            }

            tokio::select! {
              read_result = self.read_monitor_input(&config, buf) => {
                match read_result {
                  Ok(0) => break,
                  Ok(n) => {
                    self.parser.feed(&buf[..n]);
                    loop {
                      match self.parse_frame().await {
                        Ok(Some(value)) => {
                          self.process_monitor_command(value).await?;
                          self.last_active = Instant::now();
                          if self.quit {
                            // QUIT 的 +OK 经 write_buf 排队, 断连前必须 flush
                            self.flush_responses().await?;
                            return Ok(());
                          }
                        }
                        Ok(None) => break,
                        Err(e) if is_fatal_protocol(&e) => {
                            // best-effort: 保留致命协议错误, 忽略 flush 自身 IO 失败
                            let _ = self.flush_responses().await;
                            return Err(e);
                        }
                        Err(_) => break,
                      }
                    }
                  }
                  Err(e) => return Err(e),
                }
              }
              msg = rx.recv() => {
                match msg {
                  Ok(line) => {
                    self.stream.write_all(line.as_bytes()).await?;
                  }
                  Err(broadcast::error::RecvError::Lagged(_)) => {}
                  Err(broadcast::error::RecvError::Closed) => break,
                }
              }
            }
        }
        Ok(())
    }

    async fn read_monitor_input(
        &mut self,
        config: &Arc<crate::server::config::ConnectionConfig>,
        buf: &mut [u8],
    ) -> Result<usize> {
        if let Some(idle) = self.state.runtime_config.idle_timeout() {
            if self.last_active.elapsed() > idle {
                return Ok(0);
            }
        }
        if let Some(timeout) = config.read_timeout {
            match time::timeout(timeout, self.read_buf(buf)).await {
                Ok(Ok(n)) => Ok(n),
                Ok(Err(e)) => Err(Error::Io(e)),
                Err(_) => Ok(0),
            }
        } else {
            self.read_buf(buf).await.map_err(Error::Io)
        }
    }

    async fn process_monitor_command(&mut self, value: RespValue) -> Result<()> {
        let RespValue::Array(items) = value else {
            return Ok(());
        };
        let Some(items) = items else {
            return Ok(());
        };
        if items.is_empty() {
            return Ok(());
        }
        let Some(cmd_bytes) = extract_bulk(&items[0]) else {
            return Ok(());
        };
        let cmd = String::from_utf8_lossy(&cmd_bytes).to_ascii_uppercase();
        if cmd == "QUIT" {
            self.write_response(RespValue::SimpleString("OK".into()))
                .await?;
            self.quit = true;
        }
        Ok(())
    }
}
