//! Explicit task ownership and monotonic time below the composition runtime.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use futures_util::future::BoxFuture;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::oneshot;

#[cfg(not(target_family = "wasm"))]
mod native;

#[cfg(target_family = "wasm")]
mod browser;
#[cfg(target_family = "wasm")]
pub use browser::{BrowserExecutionError, BrowserExecutionSnapshot, browser_resource_snapshot};

/// Platform-owned execution. Implementations must retain submitted work even if
/// its waiter is dropped, and must use one monotonic clock domain for timers.
pub trait Backend: fmt::Debug + Send + Sync + 'static {
    /// Schedules one owned asynchronous job.
    fn spawn(&self, future: BoxFuture<'static, ()>);
    /// Schedules one owned synchronous preparation job.
    fn prepare(&self, job: Box<dyn FnOnce() + Send>);
    /// Returns monotonic elapsed time in this backend's clock domain.
    fn now(&self) -> Duration;
    /// Waits until one absolute time in this backend's clock domain.
    fn sleep_until(&self, instant: Duration) -> BoxFuture<'static, ()>;
}

/// Cloneable explicit platform dependency.
#[derive(Clone, Debug)]
pub struct Execution(Arc<dyn Backend>);

impl Execution {
    /// Uses a caller-owned platform backend.
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self(backend)
    }

    /// Captures a native Tokio executor. Its owner must outlive all owned jobs.
    #[cfg(not(target_family = "wasm"))]
    pub fn native(handle: tokio::runtime::Handle) -> Self {
        Self::new(Arc::new(native::Native::new(handle)))
    }

    /// Uses the current single-threaded Dedicated Worker's execution authority.
    /// The Worker must outlive all owned jobs and timers.
    ///
    /// # Errors
    /// Returns an error outside a Dedicated Worker or without a monotonic clock.
    #[cfg(target_family = "wasm")]
    pub fn browser() -> Result<Self, BrowserExecutionError> {
        Ok(Self::new(Arc::new(browser::Browser::new()?)))
    }

    /// Schedules work and returns a waiter whose Drop does not cancel that work.
    pub fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let (sender, receiver) = oneshot::channel();
        self.0.spawn(Box::pin(async move {
            let result = future.await;
            let _ = sender.send(result);
        }));
        Task(receiver)
    }

    /// Schedules preparation independently from the lifetime of its waiter.
    pub fn prepare<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        let (sender, receiver) = oneshot::channel();
        self.0.prepare(Box::new(move || {
            let result = job();
            let _ = sender.send(result);
        }));
        Task(receiver)
    }

    /// Captures one deadline, retaining the clock and execution authority.
    ///
    /// # Panics
    /// Panics if the trusted duration overflows the backend's elapsed clock.
    pub fn deadline_after(&self, duration: Duration) -> Deadline {
        Deadline {
            execution: self.clone(),
            at: self
                .0
                .now()
                .checked_add(duration)
                .expect("deadline overflow"),
        }
    }

    /// Waits for a duration using this execution dependency.
    pub async fn sleep(&self, duration: Duration) {
        self.deadline_after(duration).wait().await;
    }
}

/// Join-only task handle. Dropping it does not cancel the backend-owned job.
#[derive(Debug)]
pub struct Task<T>(oneshot::Receiver<T>);

impl<T> Future for Task<T> {
    type Output = Result<T, TaskError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context).map_err(|_| TaskError)
    }
}

/// A platform task ended without publishing a result, for example after panic
/// or after the embedder stopped its executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskError;

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("execution task panicked or stopped without a result")
    }
}
impl std::error::Error for TaskError {}

/// Absolute monotonic deadline carrying its own clock domain.
#[derive(Clone, Debug)]
pub struct Deadline {
    execution: Execution,
    at: Duration,
}

impl Deadline {
    /// Reports expiry without consulting an ambient executor or wall clock.
    pub fn has_elapsed(&self) -> bool {
        self.execution.0.now() >= self.at
    }

    /// Waits for this exact absolute deadline.
    pub fn wait(&self) -> BoxFuture<'static, ()> {
        self.execution.0.sleep_until(self.at)
    }

    /// Bounds a waiter. Expiry does not cancel independently owned work.
    ///
    /// # Errors
    /// Returns `Elapsed` when the deadline expires before result publication.
    pub async fn timeout<T>(&self, future: impl Future<Output = T>) -> Result<T, Elapsed> {
        if self.has_elapsed() {
            return Err(Elapsed);
        }
        tokio::select! {
            biased;
            () = self.wait() => Err(Elapsed),
            result = future => {
                if self.has_elapsed() { Err(Elapsed) } else { Ok(result) }
            }
        }
    }
}

/// A monotonic deadline elapsed before its result could be published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("execution deadline elapsed")
    }
}
impl std::error::Error for Elapsed {}
