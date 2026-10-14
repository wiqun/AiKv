//! Connection Pipeline 并发优化: 将多笔安全命令并行执行并聚合写凑批.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use crate::error::Result;
use crate::protocol::RespValue;

use super::{Connection, ConnectionMode};

impl Connection {
    /// 尝试对同一批中的命令进行流水线并发执行.
    /// 若包含事务、阻塞或状态敏感命令, 则回退到顺序执行.
    pub(super) async fn process_pipeline_batch(&mut self, batch: Vec<RespValue>) -> Result<()> {
        if self.tx_state.in_multi || self.mode != ConnectionMode::Normal {
            return self.process_sequential_batch(batch).await;
        }

        let mut parsed = Vec::with_capacity(batch.len());
        for frame in &batch {
            let Some((cmd, args)) = parse_command_parts(frame) else {
                return self.process_sequential_batch(batch).await;
            };
            if !is_safe_concurrent_command(&cmd) {
                return self.process_sequential_batch(batch).await;
            }
            parsed.push((cmd, args));
        }

        self.execute_concurrent_pipeline(parsed).await
    }

    async fn process_sequential_batch(&mut self, batch: Vec<RespValue>) -> Result<()> {
        for frame in batch {
            self.process_value(frame).await?;
        }
        Ok(())
    }

    async fn execute_concurrent_pipeline(
        &mut self,
        commands: Vec<(String, Vec<Bytes>)>,
    ) -> Result<()> {
        let router = self.state.router().clone();
        let client_id = self.client_id;
        let remote = self.remote;
        let proto = self.protocol_version;
        let current_db = self.current_db;
        #[cfg(feature = "cluster")]
        let cluster_state = self.cluster_state.clone();

        let gate = Arc::clone(&self.state.transaction_gate);
        let _guard = gate.read_owned().await;

        let total = commands.len();
        let mut join_set = tokio::task::JoinSet::new();

        for (idx, (cmd, args)) in commands.into_iter().enumerate() {
            let router = router.clone();
            #[cfg(feature = "cluster")]
            let cluster_state = cluster_state.clone();
            let track = super::should_track_observability(&cmd);
            let arg_strings: Vec<String> = if track {
                args.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect()
            } else {
                Vec::new()
            };
            let started = track.then(Instant::now);
            join_set.spawn(async move {
                let mut db = current_db;
                let result = router
                    .execute_with_client(
                        &cmd,
                        &args,
                        &mut db,
                        Some(client_id),
                        Some(remote),
                        proto,
                        #[cfg(feature = "cluster")]
                        Some(&cluster_state),
                    )
                    .await;
                (idx, cmd, args, arg_strings, started, result)
            });
        }

        let mut results = Vec::with_capacity(total);
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(item) => results.push(item),
                Err(e) => return Err(crate::error::Error::Command(format!("pipeline task failed: {e}"))),
            }
        }
        drop(_guard);

        results.sort_unstable_by_key(|(idx, ..)| *idx);

        for (_idx, cmd, args, arg_strings, started, result) in results {
            self.finish_immediate_command(&cmd, &args, arg_strings, started, result)
                .await?;
        }
        self.flush_responses().await?;
        Ok(())
    }
}

fn parse_command_parts(frame: &RespValue) -> Option<(String, Vec<Bytes>)> {
    let RespValue::Array(Some(items)) = frame else {
        return None;
    };
    if items.is_empty() {
        return None;
    }
    let cmd_bytes = super::extract_bulk(&items[0])?;
    let cmd = String::from_utf8_lossy(&cmd_bytes).to_ascii_uppercase();
    let mut args = Vec::with_capacity(items.len() - 1);
    for item in &items[1..] {
        args.push(super::extract_bulk(item)?);
    }
    Some((cmd, args))
}

fn is_safe_concurrent_command(cmd: &str) -> bool {
    match cmd {
        "MULTI" | "ATOM.MULTI" | "EXEC" | "ATOM.EXEC" | "DISCARD" | "ATOM.DISCARD"
        | "WATCH" | "ATOM.WATCH" | "UNWATCH" | "ATOM.UNWATCH" => false,
        "PING" | "ECHO" | "HELLO" | "QUIT" | "MONITOR" | "SHUTDOWN" => false,
        "SELECT" | "ASKING" | "READONLY" | "READWRITE" => false,
        _ => {
            !crate::command::lookup(cmd)
                .map(|info| info.flags.contains(&"blocking"))
                .unwrap_or(false)
        }
    }
}
