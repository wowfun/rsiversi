use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

/// Process-level signal emitted after a durable command requires a daemon
/// restart. It is deliberately separate from ordinary operator shutdown so
/// transports can use the WebSocket restart close code and the process can
/// return the supervisor-facing restart exit status.
#[derive(Clone, Debug, Default)]
pub struct DaemonLifecycle {
    stopping: CancellationToken,
    restart: Arc<AtomicBool>,
}

impl DaemonLifecycle {
    pub fn request_restart(&self) {
        self.restart.store(true, Ordering::Release);
        self.stopping.cancel();
    }

    pub fn request_shutdown(&self) {
        self.stopping.cancel();
    }

    pub async fn restarting(&self) {
        self.stopping.cancelled().await;
    }

    pub fn is_restarting(&self) -> bool {
        self.restart.load(Ordering::Acquire)
    }
}
