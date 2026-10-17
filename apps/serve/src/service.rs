pub(crate) enum SignalEvent {
    #[cfg(any(unix, windows))]
    Stop,
    #[cfg(unix)]
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

#[cfg(windows)]
pub(crate) struct Signals {
    interrupt: tokio::signal::windows::CtrlC,
    break_signal: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
}
#[cfg(windows)]
impl Signals {
    pub fn new() -> Result<Self, String> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
        Ok(Self {
            interrupt: ctrl_c().map_err(|e| e.to_string())?,
            break_signal: ctrl_break().map_err(|e| e.to_string())?,
            close: ctrl_close().map_err(|e| e.to_string())?,
            logoff: ctrl_logoff().map_err(|e| e.to_string())?,
            shutdown: ctrl_shutdown().map_err(|e| e.to_string())?,
        })
    }
    pub async fn next(&mut self, _reload_ready: bool) -> SignalEvent {
        tokio::select! {
            _ = self.interrupt.recv() => (),
            _ = self.break_signal.recv() => (),
            _ = self.close.recv() => (),
            _ = self.logoff.recv() => (),
            _ = self.shutdown.recv() => (),
        }
        SignalEvent::Stop
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) enum Signals {}
#[cfg(not(any(unix, windows)))]
impl Signals {
    pub fn new() -> Result<Self, String> {
        Err("native Serve signals are unsupported on this platform".into())
    }
    pub fn next(&mut self, _reload_ready: bool) -> std::future::Ready<SignalEvent> {
        match *self {}
    }
}
