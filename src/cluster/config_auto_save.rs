//! Automatic cluster config persistence — watches MetaRaft version changes
//! and writes nodes.conf when the cluster topology changes.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::watch;

use crate::cluster::commands::save_nodes_conf;
use aidb::cluster::MetaRaftNode;

/// Background task that periodically checks MetaRaft ClusterMeta.version
/// and persists to nodes.conf when it changes.
pub struct ConfigAutoSave {
    meta_raft: Arc<MetaRaftNode>,
    data_dir: std::path::PathBuf,
    last_saved_version: RwLock<u64>,
    interval: Duration,
}

impl ConfigAutoSave {
    /// 使用默认间隔 (2s)
    pub fn new(meta_raft: Arc<MetaRaftNode>, data_dir: std::path::PathBuf) -> Self {
        Self::new_with_interval(meta_raft, data_dir, Duration::from_secs(2))
    }

    /// 指定轮询间隔
    pub fn new_with_interval(
        meta_raft: Arc<MetaRaftNode>,
        data_dir: std::path::PathBuf,
        interval: Duration,
    ) -> Self {
        Self {
            meta_raft,
            data_dir,
            last_saved_version: RwLock::new(0),
            interval,
        }
    }

    /// 返回轮询间隔
    pub fn interval(&self) -> Duration {
        self.interval
    }

    fn tick(&self) {
        let current_version = self.meta_raft.get_cluster_meta().version;
        let last_version = *self.last_saved_version.read();

        if current_version == 0 || current_version == last_version {
            return; // no change or uninitialized
        }

        match save_nodes_conf(&self.meta_raft, &self.data_dir) {
            Ok(()) => {
                *self.last_saved_version.write() = current_version;
                tracing::debug!(version = current_version, "auto-saved cluster config");
            }
            Err(e) => {
                tracing::warn!(error = %e, "auto-save cluster config failed");
            }
        }
    }

    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) {
        tracing::info!(
          data_dir = %self.data_dir.display(),
          interval_secs = self.interval.as_secs(),
          "ConfigAutoSave started"
        );
        loop {
            tokio::select! {
              _ = tokio::time::sleep(self.interval) => {
                self.tick();
              }
              _ = shutdown_rx.changed() => {
                // Final save on shutdown
                self.tick();
                tracing::info!("ConfigAutoSave shutting down");
                break;
              }
            }
        }
    }
}

// ConfigAutoSave unit tests require Arc<MetaRaftNode> which needs the full
// openraft/gRPC stack. Constructor interface is verified at compile time.
// Integration/behavior coverage is handled by e2e/ cluster tests.
