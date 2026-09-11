use crate::{ApplicationError, ApplicationRunContract, Result};
use rsi_host::RunningHost;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

/// One application's entry and awaitable teardown, independent of its platform loop.
#[derive(Debug, Default)]
pub struct ApplicationLifetime {
    started: AtomicBool,
    stop: CancellationToken,
    stopped: CancellationToken,
}

impl ApplicationLifetime {
    /// Requests the same stop path used by normal entry completion.
    pub fn request_stop(&self) {
        self.stop.cancel();
    }

    /// Waits for completion of the prepared application's Runtime cleanup.
    pub async fn stopped(&self) {
        self.stopped.cancelled().await;
    }

    /// Runs once, then joins actual cleanup even after entry failure or an early stop.
    /// The caller must retain this future until completion; cancelling it does not
    /// prove that cleanup has finished.
    pub async fn run(&self, running: &RunningHost) -> Result<u8> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(ApplicationError::AlreadyStarted);
        }
        let result = match running.lookup_local::<ApplicationRunContract>() {
            Some(entry) => {
                tokio::select! {
                    biased;
                    () = self.stop.cancelled() => Ok(0),
                    result = entry.run() => result,
                }
            }
            None => Err(ApplicationError::MissingEntry),
        };
        let cleanup = running.shutdown().await;
        self.stopped.cancel();
        if !cleanup.is_clean() {
            return Err(ApplicationError::CleanupFailed);
        }
        result
    }
}
