//! CommandRouter 指标与 cluster 命令分类辅助.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::ServerMetrics;

pub(super) fn record_command_outcome(
    metrics: &Option<Arc<ServerMetrics>>,
    cmd: &str,
    result: &Result<RespValue>,
    duration_us: u64,
) {
    let Some(metrics) = metrics else {
        return;
    };
    let name = crate::command::canonical_command(cmd);
    metrics.on_command_done(name, result.is_ok(), duration_us);
    if let Err(err) = result {
        if let Some(msg) = error_message_for_stats(err) {
            metrics.on_error_stat(msg);
        }
    }
}

pub(super) fn error_message_for_stats(err: &Error) -> Option<&str> {
    match err {
        Error::Command(msg) | Error::Protocol(msg) | Error::Storage(msg) | Error::Config(msg) => {
            Some(msg.as_str())
        }
        Error::Io(_) => None,
        #[cfg(feature = "cluster")]
        Error::Cluster(_) => None,
    }
}

#[cfg(feature = "monitoring")]
pub(super) fn record_command_span_status(result: &Result<RespValue>) {
    if result.is_ok() {
        return;
    }
    let span = tracing::Span::current();
    span.record("otel.status_code", "ERROR");
    if let Some(err) = result.as_ref().err() {
        span.record("otel.status_message", tracing::field::display(err));
        tracing::event!(
            parent: &span,
            tracing::Level::ERROR,
            exception.type = std::any::type_name::<Error>(),
            exception.message = %err,
            "command failed"
        );
    }
}

#[cfg(feature = "cluster")]
pub(super) fn is_multi_key_cmd(cmd: &str) -> bool {
    matches!(
        cmd.to_ascii_lowercase().as_str(),
        "mget"
            | "mset"
            | "del"
            | "exists"
            | "unlink"
            | "touch"
            | "mexecute"
            | "rpop"
            | "blpop"
            | "brpop"
    )
}

/// 集群模式下在本节点本地执行、不按 `args[0]` 做 slot 路由的命令.
///
/// 含管理面命令, 以及 KEYS / SCAN 族 (首参是 pattern 或 cursor, 不是 key).
#[cfg(feature = "cluster")]
pub(super) fn is_cluster_local_admin_cmd(cmd: &str) -> bool {
    matches!(
        cmd,
        "cluster"
            | "ping"
            | "echo"
            | "hello"
            | "quit"
            | "reset"
            | "info"
            | "time"
            | "config"
            | "client"
            | "shutdown"
            | "readonly"
            | "readwrite"
            | "asking"
            | "select"
            | "auth"
            | "save"
            | "bgsave"
            | "lastsave"
            | "command"
            | "latency"
            | "slowlog"
            | "script"
            | "dbsize"
            | "flushdb"
            | "flushall"
            | "keys"
            // Cursor-based iteration — MUST NOT be routed by key
            // (args.first() is the cursor, not a key).
            | "scan"
            | "hscan"
            | "sscan"
            | "zscan"
    )
}

#[cfg(feature = "cluster")]
pub(super) fn classify_command(cmd: &str) -> crate::cluster::router::CommandType {
    match cmd.to_ascii_lowercase().as_str() {
        "get" | "exists" | "hget" | "hgetall" | "hkeys" | "hvals" | "hlen" | "hexists"
        | "lrange" | "lindex" | "llen" | "smembers" | "scard" | "sismember" | "zrange"
        | "zcard" | "zscore" | "zrank" | "type" | "ttl" | "pttl" | "strlen" | "getbit"
        | "getrange" | "mget" | "json.get" | "json.mget" | "object" => {
            crate::cluster::router::CommandType::Read
        }
        _ => crate::cluster::router::CommandType::Write,
    }
}

#[cfg(all(test, feature = "cluster"))]
mod tests {
    use super::is_cluster_local_admin_cmd;

    /// KEYS/DBSIZE/FLUSH* 首参不是路由 key; 若走 slot 路由会误返 MOVED.
    #[test]
    fn keyspace_admin_cmds_are_local() {
        for cmd in ["keys", "dbsize", "flushdb", "flushall", "scan"] {
            assert!(
                is_cluster_local_admin_cmd(cmd),
                "{cmd} must execute locally without slot routing"
            );
        }
        assert!(
            !is_cluster_local_admin_cmd("get"),
            "GET must still go through slot routing"
        );
    }

    #[test]
    fn object_is_a_read() {
        assert_eq!(
            super::classify_command("object"),
            crate::cluster::router::CommandType::Read
        );
    }
}
