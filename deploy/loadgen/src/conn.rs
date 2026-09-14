//! 目标连接封装: 单机多路复用连接 / 集群异步连接 + 单地址 PING 探活.

use std::time::Duration;

use redis::aio::MultiplexedConnection;
use redis::cluster::ClusterClientBuilder;
use redis::cluster_async::ClusterConnection;

use crate::config::{TargetMode, WorkloadConfig};

pub enum Conn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

fn timeout_error() -> redis::RedisError {
    redis::RedisError::from((redis::ErrorKind::Io, "连接超时"))
}

/// 集群握手要连 seed + 拓扑里的其余节点, 总超时按单次 timeout 放大.
pub(crate) fn cluster_connect_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms.saturating_mul(8).min(60_000).max(timeout_ms))
}

/// 传输层失败才重建连接; CROSSSLOT / 命令超时继续用原连接.
pub(crate) fn needs_reconnect(err: &redis::RedisError) -> bool {
    err.is_connection_dropped()
        || err.is_connection_refusal()
        || matches!(err.kind(), redis::ErrorKind::ClusterConnectionNotFound)
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
                // 只把第一个地址当 seed, CLUSTER SLOTS 再发现其余节点;
                // 把全部 endpoint 塞进 initial_nodes 会并行打满 Docker 端口转发.
                let seed = format!("redis://{}", cfg.endpoints[0]);
                let client = ClusterClientBuilder::new(vec![seed])
                    .connection_timeout(timeout)
                    .build()?;
                let conn = tokio::time::timeout(
                    cluster_connect_timeout(cfg.timeout_ms),
                    client.get_async_connection(),
                )
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

/// 解析 `CLUSTER INFO` 文本中的 `cluster_state`.
pub(crate) fn parse_cluster_state(info: &str) -> Option<&str> {
    info.lines().find_map(|line| {
        line.strip_prefix("cluster_state:")
            .map(str::trim)
            .filter(|state| !state.is_empty())
    })
}

pub(crate) async fn cluster_state(addr: &str, timeout: Duration) -> redis::RedisResult<String> {
    let attempt = async {
        let client = redis::Client::open(format!("redis://{addr}"))?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        let info: String = redis::cmd("CLUSTER")
            .arg("INFO")
            .query_async(&mut conn)
            .await?;
        parse_cluster_state(&info)
            .map(str::to_string)
            .ok_or_else(|| {
                redis::RedisError::from((redis::ErrorKind::Parse, "CLUSTER INFO 无 cluster_state"))
            })
    };
    tokio::time::timeout(timeout, attempt)
        .await
        .map_err(|_| timeout_error())?
}

/// 启动门闩: seed 必须 PING 通; cluster 还要求 `cluster_state:ok`.
pub async fn check_ready(cfg: &WorkloadConfig) -> Result<(), String> {
    let seed = cfg
        .endpoints
        .first()
        .ok_or_else(|| "endpoints 不能为空".to_string())?;
    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));
    if !ping(seed, timeout).await {
        return Err(format!("seed {seed} 不通, 拒绝启动"));
    }
    if cfg.mode != TargetMode::Cluster {
        return Ok(());
    }
    match cluster_state(seed, timeout).await {
        Ok(state) if state.eq_ignore_ascii_case("ok") => Ok(()),
        Ok(state) => Err(format!("集群状态为 {state}, 拒绝启动")),
        Err(err) => Err(format!("无法读取 CLUSTER INFO: {err}")),
    }
}

/// 运行中是否应暂停派发 (不停 worker).
pub(crate) fn pause_reason(all_unreachable: bool, cluster_state: Option<&str>) -> Option<String> {
    if all_unreachable {
        return Some("全部节点不可达, 已暂停派发".to_string());
    }
    if let Some(state) = cluster_state {
        if !state.eq_ignore_ascii_case("ok") {
            return Some(format!("集群状态为 {state}, 已暂停派发"));
        }
    }
    None
}

/// CLUSTER INFO 成功则交出真实 cluster_state; 读失败返回 None, 不臆造 unreachable.
pub(crate) fn cluster_view_after_probe(info: redis::RedisResult<String>) -> Option<String> {
    match info {
        Ok(state) => Some(state),
        Err(err) => {
            tracing::warn!(%err, "CLUSTER INFO 读取失败, 本次探活不据此暂停");
            None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_timeout_scales_but_caps() {
        assert_eq!(cluster_connect_timeout(1_000), Duration::from_secs(8));
        assert_eq!(cluster_connect_timeout(10_000), Duration::from_secs(60));
        assert_eq!(cluster_connect_timeout(60_000), Duration::from_secs(60));
    }

    #[test]
    fn reconnects_only_on_transport_errors() {
        let io = redis::RedisError::from((redis::ErrorKind::Io, "reset"));
        assert!(needs_reconnect(&io));
        let cmd = redis::RedisError::from((redis::ErrorKind::Extension, "CROSSSLOT"));
        assert!(!needs_reconnect(&cmd));
        let timeout = redis::RedisError::from(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert!(!needs_reconnect(&timeout));
    }

    #[test]
    fn parses_cluster_state() {
        let info = "cluster_state:ok\ncluster_slots_assigned:16384\n";
        assert_eq!(parse_cluster_state(info), Some("ok"));
        assert_eq!(parse_cluster_state("cluster_state:fail\n"), Some("fail"));
        assert_eq!(parse_cluster_state("nope"), None);
    }

    #[test]
    fn pause_when_all_down_or_cluster_fail() {
        assert!(pause_reason(true, Some("ok")).is_some());
        assert!(pause_reason(false, Some("fail")).is_some());
        assert!(pause_reason(false, Some("ok")).is_none());
        assert!(pause_reason(false, None).is_none());
    }

    /// CLUSTER INFO 读失败不是 Redis 的 cluster_state, 不能写成 unreachable 去暂停派发.
    #[test]
    fn cluster_info_read_error_does_not_invent_unreachable() {
        let err = redis::RedisError::from((redis::ErrorKind::Io, "timeout"));
        assert_eq!(cluster_view_after_probe(Err(err)), None);
        assert_eq!(cluster_view_after_probe(Ok("ok".into())), Some("ok".into()));
        assert_eq!(
            cluster_view_after_probe(Ok("fail".into())),
            Some("fail".into())
        );
    }
}
