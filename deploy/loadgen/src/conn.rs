//! 目标连接封装: 单机多路复用连接 / 集群异步连接 + 单地址 PING 探活.

use std::time::Duration;

use redis::aio::MultiplexedConnection;
use redis::cluster::ClusterClient;
use redis::cluster_async::ClusterConnection;

use crate::config::{TargetMode, WorkloadConfig};

pub enum Conn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

fn timeout_error() -> redis::RedisError {
    redis::RedisError::from((redis::ErrorKind::Io, "连接超时"))
}

impl Conn {
    /// 按配置建立连接; `timeout_ms` 同时用于连接超时.
    pub async fn connect(cfg: &WorkloadConfig) -> redis::RedisResult<Self> {
        let timeout = Duration::from_millis(cfg.timeout_ms);
        match cfg.mode {
            TargetMode::Single => {
                let url = format!("redis://{}", cfg.endpoints[0]);
                let client = redis::Client::open(url)?;
                let conn = tokio::time::timeout(timeout, client.get_multiplexed_async_connection())
                    .await
                    .map_err(|_| timeout_error())??;
                Ok(Conn::Single(conn))
            }
            TargetMode::Cluster => {
                let urls: Vec<String> = cfg
                    .endpoints
                    .iter()
                    .map(|e| format!("redis://{e}"))
                    .collect();
                let client = ClusterClient::new(urls)?;
                let conn = tokio::time::timeout(timeout, client.get_async_connection())
                    .await
                    .map_err(|_| timeout_error())??;
                Ok(Conn::Cluster(conn))
            }
        }
    }

    /// 执行一轮 pipeline (所有命令 ignore, 无结果解析开销).
    pub async fn exec(&mut self, pipe: &redis::Pipeline) -> redis::RedisResult<()> {
        match self {
            Conn::Single(conn) => {
                let _: () = pipe.exec_async(conn).await?;
                Ok(())
            }
            Conn::Cluster(conn) => {
                let _: () = pipe.exec_async(conn).await?;
                Ok(())
            }
        }
    }
}

/// 单地址 PING 探活 (供 UI 展示连接可用性).
pub async fn ping(addr: &str, timeout: Duration) -> bool {
    let attempt = async {
        let client = match redis::Client::open(format!("redis://{addr}")) {
            Ok(client) => client,
            Err(_) => return false,
        };
        let mut conn = match client.get_multiplexed_async_connection().await {
            Ok(conn) => conn,
            Err(_) => return false,
        };
        matches!(
            redis::cmd("PING").query_async::<String>(&mut conn).await,
            Ok(pong) if pong == "PONG"
        )
    };
    tokio::time::timeout(timeout, attempt)
        .await
        .unwrap_or(false)
}
