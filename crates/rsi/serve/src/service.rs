use async_trait::async_trait;
use rsi_meta::LocalContract;
use std::fmt;

/// Observation and reload of the service generation selected by the composition.
#[async_trait]
pub trait ServingService: fmt::Debug + Send + Sync + 'static {
    /// Waits for service retirement or failure without owning its shutdown authority.
    async fn stopped(&self) -> Result<(), String>;
    /// Requests a Profile reload; dropping the waiter must not own service shutdown.
    async fn reload(&self) -> Result<(), String>;
}

/// Composition-provided control for the independently owned service generation.
#[derive(Debug)]
pub struct ServingServiceContract;
impl LocalContract for ServingServiceContract {
    const KEY: &'static str = "rsi.serve.service";
    type Service = dyn ServingService;
}

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
