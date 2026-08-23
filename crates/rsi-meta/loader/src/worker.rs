use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

pub(super) fn spawn_native_worker<T>(
    name: &'static str,
    callback: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<tokio::sync::oneshot::Receiver<T>>
where
    T: Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _ = sender.send(callback());
        })?;
    Ok(receiver)
}

pub(super) enum CallbackWaitError {
    TimedOut,
    Disconnected(tokio::sync::oneshot::error::RecvError),
}

pub(super) async fn run_bounded_callback<T>(
    mut receiver: tokio::sync::oneshot::Receiver<T>,
    completion: Arc<CallbackCompletion>,
    timeout: Duration,
    on_timeout: Arc<dyn Fn() + Send + Sync>,
) -> Result<T, CallbackWaitError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let watchdog_completion = Arc::clone(&completion);
    let watchdog_timeout = Arc::clone(&on_timeout);
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = watchdog_completion.wait_finished() => {}
            () = tokio::time::sleep_until(deadline) => {
                if watchdog_completion.time_out() {
                    watchdog_timeout();
                }
            }
        }
    });
    let result = if let Ok(result) = tokio::time::timeout_at(deadline, &mut receiver).await {
        result
    } else {
        if completion.time_out() {
            on_timeout();
        }
        if completion.is_timed_out() {
            return Err(CallbackWaitError::TimedOut);
        }
        receiver.await
    };
    if completion.is_timed_out() {
        Err(CallbackWaitError::TimedOut)
    } else {
        result.map_err(CallbackWaitError::Disconnected)
    }
}

/// Marks a watchdog operation complete before its adapter-owned callback gate
/// is released, closing the interval in which a successful callback could be
/// mistaken for an in-flight one.
pub(super) struct CallbackCompletion {
    state: AtomicU8,
    finished: Notify,
}

impl CallbackCompletion {
    const RUNNING: u8 = 0;
    const COMPLETED: u8 = 1;
    const TIMED_OUT: u8 = 2;

    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::RUNNING),
            finished: Notify::new(),
        }
    }

    pub(super) fn complete(&self) -> bool {
        let completed = self
            .state
            .compare_exchange(
                Self::RUNNING,
                Self::COMPLETED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if completed {
            self.finished.notify_waiters();
        }
        completed
    }

    pub(super) fn time_out(&self) -> bool {
        let timed_out = self
            .state
            .compare_exchange(
                Self::RUNNING,
                Self::TIMED_OUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if timed_out {
            self.finished.notify_waiters();
        }
        timed_out
    }

    pub(super) fn is_timed_out(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::TIMED_OUT
    }

    async fn wait_finished(&self) {
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.load(Ordering::Acquire) != Self::RUNNING {
                return;
            }
            notified.as_mut().await;
        }
    }
}

pub(super) struct CompletionOnDrop<'a>(pub(super) &'a CallbackCompletion);

impl Drop for CompletionOnDrop<'_> {
    fn drop(&mut self) {
        self.0.complete();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropSignal(Arc<Notify>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn completed_callback_releases_watchdog_resources_before_its_deadline() {
        let completion = Arc::new(CallbackCompletion::new());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(completion.complete());
        sender.send(7_u8).unwrap();

        let dropped = Arc::new(Notify::new());
        let drop_signal = DropSignal(Arc::clone(&dropped));
        let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = &drop_signal;
        });

        let result =
            run_bounded_callback(receiver, completion, Duration::from_mins(1), on_timeout).await;
        assert!(matches!(result, Ok(7)));
        tokio::time::timeout(Duration::from_secs(1), dropped.notified())
            .await
            .expect("completed callback retained its watchdog until the deadline");
    }
}
