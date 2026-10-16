//! 持久化运维命令 (SAVE / BGSAVE / LASTSAVE / SHUTDOWN)

use std::sync::Arc;

use bytes::Bytes;

use crate::command::router;
use crate::error::{Error, Result};
use crate::protocol::RespValue;
use crate::server::ServerSharedState;
use crate::storage::{KvStorage, StorageEngineKind};

pub struct PersistenceCommands {
    storage: Arc<dyn KvStorage>,
    shared: Arc<ServerSharedState>,
}

impl PersistenceCommands {
    pub fn new(storage: Arc<dyn KvStorage>, shared: Arc<ServerSharedState>) -> Self {
        Self { storage, shared }
    }

    fn ensure_persistent_engine(&self) -> Result<()> {
        if self.storage.engine_kind() != StorageEngineKind::AiDb {
            return Err(Error::Command(
                "ERR Persistence not supported on memory engine".into(),
            ));
        }
        Ok(())
    }

    pub async fn save(&self) -> Result<RespValue> {
        self.ensure_persistent_engine()?;
        self.storage.flush_engine().await?;
        let dest = self.shared.backup_dir.clone();
        let _path = self.storage.create_checkpoint(&dest).await?;
        self.shared.record_save_success();
        tracing::info!(target: "persist", dest = %dest.display(), "bgsave.complete");
        Ok(router::ok())
    }

    pub async fn bgsave(&self) -> Result<RespValue> {
        self.ensure_persistent_engine()?;
        if self
            .shared
            .bgsave_in_progress
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return Err(Error::Command(
                "ERR Background saving already in progress".into(),
            ));
        }

        let storage = Arc::clone(&self.storage);
        let shared = Arc::clone(&self.shared);
        let dest = shared.backup_dir.clone();
        tokio::spawn(async move {
            let result = async {
                storage.flush_engine().await?;
                storage.create_checkpoint(&dest).await?;
                Ok::<(), Error>(())
            }
            .await;

            shared
                .bgsave_in_progress
                .store(false, std::sync::atomic::Ordering::SeqCst);
            match result {
                Ok(()) => {
                    shared.record_save_success();
                    let file_count = std::fs::read_dir(&dest).map(|rd| rd.count()).unwrap_or(0);
                    tracing::info!(
                      target: "persist",
                      file_count,
                      dest = %dest.display(),
                      "bgsave.complete"
                    );
                }
                Err(e) => {
                    shared.record_save_failure(e.to_string());
                    tracing::error!(target: "persist", error = %e, "bgsave.failed");
                }
            }
        });

        Ok(RespValue::SimpleString("Background saving started".into()))
    }

    pub async fn lastsave(&self) -> Result<RespValue> {
        let ts = if self.storage.engine_kind() == StorageEngineKind::AiDb {
            self.shared.last_save_time() as i64
        } else {
            0
        };
        Ok(router::integer(ts))
    }

    pub async fn shutdown(&self, args: &[Bytes]) -> Result<RespValue> {
        let mode = parse_shutdown_mode(args)?;
        match self.storage.engine_kind() {
            StorageEngineKind::Memory => {
                if mode == ShutdownMode::Save {
                    return Err(Error::Command(
                        "ERR Persistence not supported on memory engine".into(),
                    ));
                }
            }
            StorageEngineKind::AiDb => match mode {
                ShutdownMode::NoSave => {}
                ShutdownMode::Save | ShutdownMode::Default => {
                    self.storage.flush_engine().await?;
                    let dest = self.shared.backup_dir.clone();
                    self.storage.create_checkpoint(&dest).await?;
                    self.shared.record_save_success();
                }
            },
        }

        if self.storage.engine_kind() == StorageEngineKind::AiDb {
            self.storage.close_engine().await?;
        }
        self.shared.shutdown.cancel();
        Ok(router::ok())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownMode {
    Default,
    Save,
    NoSave,
}

fn parse_shutdown_mode(args: &[Bytes]) -> Result<ShutdownMode> {
    if args.is_empty() {
        return Ok(ShutdownMode::Default);
    }
    if args.len() != 1 {
        return Err(router::wrong_args("SHUTDOWN", ""));
    }
    match String::from_utf8_lossy(&args[0])
        .to_ascii_uppercase()
        .as_str()
    {
        "SAVE" => Ok(ShutdownMode::Save),
        "NOSAVE" => Ok(ShutdownMode::NoSave),
        other => Err(Error::Command(format!(
            "ERR invalid SHUTDOWN mode '{other}'"
        ))),
    }
}
