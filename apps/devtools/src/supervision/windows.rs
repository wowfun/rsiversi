use std::{io, process::ExitStatus, time::Duration};

pub(crate) struct Signals {
    interrupt: tokio::signal::windows::CtrlC,
    breaking: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
    stopped: bool,
    #[cfg(test)]
    request: tokio_util::sync::CancellationToken,
}
impl Signals {
    pub(crate) fn new() -> io::Result<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
        Ok(Self {
            interrupt: ctrl_c()?,
            breaking: ctrl_break()?,
            close: ctrl_close()?,
            logoff: ctrl_logoff()?,
            shutdown: ctrl_shutdown()?,
            stopped: false,
            #[cfg(test)]
            request: tokio_util::sync::CancellationToken::new(),
        })
    }
    pub(crate) async fn interrupted(&mut self) {
        #[cfg(test)]
        {
            let request = self.request.clone();
            tokio::select! { () = request.cancelled() => self.stopped = true, () = self.platform_interrupted() => {} }
        }
        #[cfg(not(test))]
        self.platform_interrupted().await;
    }
    #[cfg(test)]
    pub(crate) fn test_request(&self) -> tokio_util::sync::CancellationToken {
        self.request.clone()
    }
    async fn platform_interrupted(&mut self) {
        if !self.stopped {
            tokio::select! { _ = self.interrupt.recv() => {}, _ = self.breaking.recv() => {}, _ = self.close.recv() => {}, _ = self.logoff.recv() => {}, _ = self.shutdown.recv() => {} }
            self.stopped = true;
        }
    }
}
pub(crate) struct Child {
    pub(crate) process: tokio::process::Child,
    job: rsi_process_native::Job,
    settled: bool,
}
impl Child {
    pub(crate) async fn spawn_development(
        mut command: tokio::process::Command,
    ) -> Result<Self, String> {
        let job = rsi_process_native::Job::new().map_err(problem)?;
        command.creation_flags(0x0000_0004).kill_on_drop(true);
        let mut process = command.spawn().map_err(problem)?;
        if let Err(error) = job.admit_tokio_child(&process) {
            // Admission rollback retains both exact owners until the direct
            // child is reaped; no detached cleanup can be lost at runtime exit.
            let _ = process.kill().await;
            let _ = process.wait().await;
            let _ = job.terminate();
            return Err(problem(error));
        }
        Ok(Self {
            process,
            job,
            settled: false,
        })
    }
    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.process.wait().await?;
        self.job.terminate()?;
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.job.active()? != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<(), io::Error>(())
        })
        .await
        .map_err(|_| io::Error::other("Windows Job cleanup incomplete after 10 seconds"))??;
        self.settled = true;
        Ok(status)
    }
    pub(crate) async fn stop(&mut self) -> Result<(), String> {
        self.stop_after_eof().await.map(|_| ())
    }
    pub(crate) async fn stop_after_eof(&mut self) -> Result<ExitStatus, String> {
        self.stop_after_eof_with_timeout(Duration::from_secs(15))
            .await
    }
    pub(crate) async fn stop_after_eof_with_timeout(
        &mut self,
        grace: Duration,
    ) -> Result<ExitStatus, String> {
        if self.settled {
            return self.process.wait().await.map_err(problem);
        }
        // Console Ctrl+C reaches attached applications independently. Web and
        // Node additionally receive EOF from their owning control pipes.
        if let Ok(result) = tokio::time::timeout(grace, self.wait()).await {
            return result.map_err(problem);
        }
        eprintln!(
            "development child (PID {:?}) exceeded its graceful shutdown deadline; forcing termination",
            self.process.id()
        );
        self.job.terminate().map_err(problem)?;
        tokio::time::timeout(Duration::from_secs(10), self.wait())
            .await
            .map_err(|_| "Windows child cleanup incomplete")?
            .map_err(problem)
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.job.terminate();
        }
    }
}
pub(crate) fn application_succeeded(status: ExitStatus) -> bool {
    status.success() || application_interrupted(status)
}
pub(crate) fn application_interrupted(status: ExitStatus) -> bool {
    const STATUS_CONTROL_C_EXIT: i32 = -1_073_741_510;
    status.code() == Some(STATUS_CONTROL_C_EXIT) || status.code() == Some(130)
}
fn problem(error: impl std::fmt::Display) -> String {
    error.to_string()
}
