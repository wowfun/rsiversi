pub(crate) enum SignalEvent {
    Stop,
    Reload,
}

#[cfg(unix)]
pub(crate) struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    reload: tokio::signal::unix::Signal,
    reload_open: bool,
}
#[cfg(unix)]
impl Signals {
    pub fn new() -> Result<Self, String> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt()).map_err(|error| error.to_string())?,
            terminate: signal(SignalKind::terminate()).map_err(|error| error.to_string())?,
            reload: signal(SignalKind::hangup()).map_err(|error| error.to_string())?,
            reload_open: true,
        })
    }
    pub async fn next(&mut self, reload_ready: bool) -> SignalEvent {
        loop {
            tokio::select! {
                _ = self.interrupt.recv() => return SignalEvent::Stop,
                _ = self.terminate.recv() => return SignalEvent::Stop,
                value = self.reload.recv(), if self.reload_open && reload_ready => match value {
                    Some(()) => return SignalEvent::Reload,
                    None => self.reload_open = false,
                },
            }
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct Signals;
#[cfg(not(unix))]
impl Signals {
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }
    pub async fn next(&mut self, _reload_ready: bool) -> SignalEvent {
        let _ = tokio::signal::ctrl_c().await;
        SignalEvent::Stop
    }
}
