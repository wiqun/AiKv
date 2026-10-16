//! 脚本线程池: 只执行 `EVAL` / `EVALSHA`, 与 tokio worker 和阻塞线程池分开.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use tokio::sync::{oneshot, Semaphore};

use crate::error::{Error, Result};

struct Job {
    run: Box<dyn FnOnce() + Send>,
}

pub struct ScriptWorkerPool {
    tx: Sender<Job>,
    slots: Arc<Semaphore>,
}

impl ScriptWorkerPool {
    pub fn new(threads: usize) -> Self {
        assert!(threads >= 1, "script worker threads must be >= 1");
        let (tx, rx) = mpsc::channel();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..threads {
            let rx = Arc::clone(&rx);
            thread::Builder::new()
                .name("aikv-script".into())
                .spawn(move || worker_loop(rx))
                .expect("spawn script worker");
        }
        Self {
            tx,
            slots: Arc::new(Semaphore::new(threads)),
        }
    }

    pub async fn run<T, F>(&self, job: F) -> Result<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Command("ERR script thread failed: pool closed".into()))?;
        let (done_tx, done_rx) = oneshot::channel();
        let wrapped = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(job));
            let _ = done_tx.send(result);
            drop(permit);
        });
        self.tx
            .send(Job { run: wrapped })
            .map_err(|_| Error::Command("ERR script thread failed: pool closed".into()))?;
        match done_rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(Error::Command("ERR script thread failed: panic".into())),
            Err(_) => Err(Error::Command(
                "ERR script thread failed: worker dropped".into(),
            )),
        }
    }
}

fn worker_loop(rx: Arc<Mutex<Receiver<Job>>>) {
    loop {
        let job = {
            let guard = rx.lock().unwrap_or_else(|err| err.into_inner());
            guard.recv()
        };
        let Ok(job) = job else {
            break;
        };
        (job.run)();
    }
}

#[cfg(test)]
mod tests {
    use super::ScriptWorkerPool;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pool_runs_job_on_worker() {
        let pool = ScriptWorkerPool::new(1);
        let value = pool.run(|| 41).await.expect("run");
        assert_eq!(value, 41);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pool_panic_releases_slot() {
        let pool = ScriptWorkerPool::new(1);
        let err = pool
            .run(|| -> i32 { panic!("boom") })
            .await
            .expect_err("panic");
        let text = err.to_string();
        assert!(text.contains("script thread failed"), "{text}");
        let value = pool.run(|| 7).await.expect("slot released");
        assert_eq!(value, 7);
    }
}
