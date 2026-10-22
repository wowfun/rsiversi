use std::time::Duration;
fn problem(error: impl std::fmt::Display) -> String {
    error.to_string()
}
pub(crate) struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
    stopped: bool,
}
impl Signals {
    pub(crate) fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
            stopped: false,
        })
    }
    pub(crate) async fn interrupted(&mut self) {
        if !self.stopped {
            tokio::select! {
                _ = self.interrupt.recv() => {},
                _ = self.terminate.recv() => {},
                _ = self.hangup.recv() => {},
                _ = self.quit.recv() => {},
            }
            self.stopped = true;
        }
    }
}
pub(crate) fn application_succeeded(status: std::process::ExitStatus) -> bool {
    status.success() || application_interrupted(status)
}
fn application_interrupted(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.code() == Some(130) || status.signal() == Some(rustix::process::Signal::INT.as_raw())
}
pub(crate) struct Child {
    name: String,
    force_timeout: Duration,
    pub(crate) process: tokio::process::Child,
    pub(crate) group: Option<rustix::process::Pid>,
}
pub(crate) struct Foreground(nix::unistd::Pid);
impl Foreground {
    pub(crate) fn acquire(child: &Child) -> Result<Option<Self>, String> {
        use std::io::IsTerminal as _;
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let previous = nix::unistd::tcgetpgrp(std::io::stdin()).map_err(problem)?;
        let group = child.group.ok_or("TUI child has no process group")?;
        Self::set(nix::unistd::Pid::from_raw(group.as_raw_nonzero().get())).map_err(problem)?;
        // A child that read stdin before the handoff may have received SIGTTIN.
        child.signal(rustix::process::Signal::CONT);
        Ok(Some(Self(previous)))
    }
    fn set(group: nix::unistd::Pid) -> nix::Result<()> {
        use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
        let mut blocked = SigSet::empty();
        blocked.add(Signal::SIGTTOU);
        let mut previous = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))?;
        // Synchronous: the mask is restored on this same OS thread before returning.
        let result = nix::unistd::tcsetpgrp(std::io::stdin(), group);
        let restored = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
        result.and(restored)
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = Self::set(self.0);
    }
}
impl Child {
    pub(crate) fn spawn(mut command: tokio::process::Command, group: bool) -> Result<Self, String> {
        #[cfg(unix)]
        if group {
            command.process_group(0);
        }
        command.kill_on_drop(true);
        let name = command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();
        let child = command.spawn().map_err(problem)?;
        let group = if group {
            child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .and_then(rustix::process::Pid::from_raw)
        } else {
            None
        };
        Ok(Self {
            name,
            force_timeout: Duration::from_secs(2),
            process: child,
            group,
        })
    }
    fn signal(&self, signal: rustix::process::Signal) {
        if let Some(group) = self.group {
            let _ = rustix::process::kill_process_group(group, signal);
        } else if let Some(pid) = self
            .process
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, signal);
        }
    }
    pub(crate) async fn stop(&mut self) -> Result<(), String> {
        self.stop_with_timeout(Duration::from_secs(15)).await
    }
    pub(crate) async fn exited(&self) -> std::io::Result<()> {
        use rustix::process::{WaitIdOptions, waitid};
        let Some(group) = self.group else {
            return Ok(());
        };
        loop {
            match waitid(
                rustix::process::WaitId::Pid(group),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
            ) {
                Ok(Some(_)) => return Ok(()),
                Ok(None) | Err(rustix::io::Errno::INTR) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.exited().await?;
        // WNOWAIT pins the group ID until every signal has been sent. Never retain
        // a numeric group ID after reaping: it could then belong to another owner.
        self.signal(rustix::process::Signal::KILL);
        self.group = None;
        self.process.wait().await
    }
    pub(crate) async fn stop_with_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        self.signal(rustix::process::Signal::TERM);
        self.settle(timeout).await.map(|_| ())
    }
    async fn settle(&mut self, timeout: Duration) -> Result<std::process::ExitStatus, String> {
        if let Ok(result) = tokio::time::timeout(timeout, self.wait()).await {
            result.map_err(problem)
        } else {
            let name = format!("{} (PID {:?})", self.name, self.process.id());
            eprintln!(
                "development child {name} exceeded its graceful shutdown deadline; forcing termination"
            );
            self.signal(rustix::process::Signal::KILL);
            let timeout = self.force_timeout;
            kill_completion_with_timeout(self.wait(), &name, timeout).await
        }
    }
}
#[cfg(test)]
pub(crate) async fn kill_completion(
    wait: impl std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
    name: &str,
) -> Result<(), String> {
    kill_completion_with_timeout(wait, name, Duration::from_secs(2))
        .await
        .map(|_| ())
}
async fn kill_completion_with_timeout(
    wait: impl std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
    name: &str,
    timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    match tokio::time::timeout(timeout, wait).await {
        Ok(result) => result.map_err(problem),
        Err(_) => Err(format!(
            "development child {name} did not exit within {} seconds after KILL; cleanup incomplete",
            timeout.as_secs()
        )),
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.signal(rustix::process::Signal::KILL);
    }
}
