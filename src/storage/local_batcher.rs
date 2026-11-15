//! 单机模式写合批: 把并发命令的 AdapterWriteOp 聚合成一次本地 write_batch.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::error::{Error, Result};
use crate::storage::adapter::{AdapterWriteOp, StorageAdapter};
use crate::storage::types::{BatchWriteHandle, ImmediateWriteHandle};

const MAX_BATCH_OPS: usize = 512;

struct BatchItem {
    ops: Vec<AdapterWriteOp>,
    done: oneshot::Sender<std::result::Result<(), String>>,
}

/// 单机写合批器. 第一次 enqueue 时启动后台任务; 直接写本地引擎.
pub struct LocalWriteBatcher {
    local: Arc<dyn StorageAdapter>,
    tx: OnceLock<mpsc::Sender<BatchItem>>,
}

impl LocalWriteBatcher {
    pub fn new(local: Arc<dyn StorageAdapter>) -> Self {
        Self {
            local,
            tx: OnceLock::new(),
        }
    }

    fn sender(&self) -> mpsc::Sender<BatchItem> {
        self.tx
            .get_or_init(|| {
                let (tx, rx) = mpsc::channel(4096);
                tokio::spawn(run(self.local.clone(), rx));
                tx
            })
            .clone()
    }

    pub async fn enqueue(&self, ops: Vec<AdapterWriteOp>) -> Result<Box<dyn BatchWriteHandle>> {
        if ops.is_empty() {
            return Ok(Box::new(ImmediateWriteHandle));
        }
        let (done, wait) = oneshot::channel();
        self.sender()
            .send(BatchItem { ops, done })
            .await
            .map_err(|_| Error::Storage("local write batcher stopped".into()))?;
        Ok(Box::new(LocalWriteHandle { wait }))
    }
}

struct LocalWriteHandle {
    wait: oneshot::Receiver<std::result::Result<(), String>>,
}

#[async_trait]
impl BatchWriteHandle for LocalWriteHandle {
    async fn wait(self: Box<Self>) -> Result<()> {
        match self.wait.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(Error::Storage(e)),
            Err(_) => Err(Error::Storage("local write batch dropped".into())),
        }
    }
}

async fn run(local: Arc<dyn StorageAdapter>, mut rx: mpsc::Receiver<BatchItem>) {
    while let Some(first) = rx.recv().await {
        let mut items = vec![first];
        let mut op_count = items[0].ops.len();

        while op_count < MAX_BATCH_OPS {
            match rx.try_recv() {
                Ok(item) => {
                    op_count += item.ops.len();
                    items.push(item);
                }
                Err(_) => break,
            }
        }

        // 让出一次执行权, 给并发连接一个入队窗口, 但不引入定时器延迟.
        if op_count < MAX_BATCH_OPS {
            tokio::task::yield_now().await;
            while op_count < MAX_BATCH_OPS {
                match rx.try_recv() {
                    Ok(item) => {
                        op_count += item.ops.len();
                        items.push(item);
                    }
                    Err(_) => break,
                }
            }
        }

        let mut batch = Vec::with_capacity(op_count);
        for item in &mut items {
            batch.append(&mut item.ops);
        }
        let message = local.write_batch(batch).await.map_err(|e| e.to_string());
        for item in items {
            let _ = item.done.send(message.clone());
        }
    }
}
