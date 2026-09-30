//! Background work that outlives a request or runs on a schedule (audit
//! entries written off the hot path, moderation reviews, the jobs).
//!
//! Everything spawned through [`spawn`] is tracked, so a graceful shutdown
//! can wait for it instead of tearing the runtime down mid-write (an audit
//! entry lost, an e-mail marked `sending` forever, a purge cancelled
//! halfway). Loops watch [`shutdown_token`] and stop scheduling new runs
//! as soon as the server stops accepting connections.

use std::{future::Future, sync::LazyLock, time::Duration};
use tokio::task::JoinHandle;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// A tracker and the token that tells its loops to stop.
pub struct Tasks {
    tracker: TaskTracker,
    shutdown: CancellationToken,
}

impl Default for Tasks {
    fn default() -> Self {
        Self::new()
    }
}

impl Tasks {
    pub fn new() -> Self {
        Self {
            tracker: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(future)
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    /// Signals every loop to stop and waits up to `grace` for the tracked
    /// tasks to finish. `false` when some were still running.
    pub async fn shutdown(&self, grace: Duration) -> bool {
        self.shutdown.cancel();
        self.tracker.close();
        tokio::time::timeout(grace, self.tracker.wait())
            .await
            .is_ok()
    }
}

static GLOBAL: LazyLock<Tasks> = LazyLock::new(Tasks::new);

/// Spawns `future` on the runtime, tracked for shutdown.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    GLOBAL.spawn(future)
}

/// A token cancelled once the server is shutting down. Long-running loops
/// select on it between iterations.
pub fn shutdown_token() -> CancellationToken {
    GLOBAL.shutdown_token()
}

/// `true` once [`shutdown`] has been called.
pub fn is_shutting_down() -> bool {
    GLOBAL.is_shutting_down()
}

/// Signals every loop to stop and waits up to `grace` for the tracked
/// tasks to finish. Returns `false` when some were still running.
pub async fn shutdown(grace: Duration) -> bool {
    GLOBAL.shutdown(grace).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test]
    async fn shutdown_waits_for_spawned_work_and_flips_the_token() {
        let tasks = Tasks::new();
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            flag.store(true, Ordering::SeqCst);
        });
        assert!(!tasks.is_shutting_down());
        assert!(tasks.shutdown(Duration::from_secs(5)).await);
        assert!(done.load(Ordering::SeqCst));
        assert!(tasks.is_shutting_down());
        assert!(tasks.shutdown_token().is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_gives_up_after_the_grace_period() {
        let tasks = Tasks::new();
        let (_keep, wait) = tokio::sync::oneshot::channel::<()>();
        tasks.spawn(async move {
            let _ = wait.await;
        });
        assert!(!tasks.shutdown(Duration::from_millis(20)).await);
    }
}
