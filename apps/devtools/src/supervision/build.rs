use std::{
    io,
    process::{Child, Command, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Synchronous build child, with cancellation observed while the build lock is held.
pub(crate) struct BuildChild {
    process: Child,
    cancel: Option<Arc<AtomicBool>>,
    #[cfg(windows)]
    job: Option<rsi_process_native::Job>,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl BuildChild {
    pub(crate) fn spawn(mut command: Command, cancel: Option<Arc<AtomicBool>>) -> io::Result<Self> {
        if cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "development build stopped",
            ));
        }
        #[cfg(unix)]
        if cancel.is_some() {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        #[cfg(windows)]
        let job = if cancel.is_some() {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0000_0004);
            Some(rsi_process_native::Job::new()?)
        } else {
            None
        };
        let process = command.spawn()?;
        #[cfg(windows)]
        let mut process = process;
        #[cfg(windows)]
        if let Some(job) = &job
            && let Err(error) = job.admit_child(&process)
        {
            let _ = process.kill();
            let _ = process.wait();
            return Err(error);
        }
        #[cfg(unix)]
        let group = cancel
            .as_ref()
            .map(|_| rustix::process::Pid::from_child(&process));
        Ok(Self {
            process,
            cancel,
            #[cfg(windows)]
            job,
            #[cfg(unix)]
            group,
        })
    }
    pub(crate) fn stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.process.stdout.take()
    }
    pub(crate) fn stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.process.stderr.take()
    }
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.cancel.is_none() {
            return self.process.wait();
        }
        let mut stopping = None;
        loop {
            if self
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
                && stopping.is_none()
            {
                stopping = Some(Instant::now());
                #[cfg(unix)]
                if let Some(group) = self.group {
                    let _ =
                        rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
                }
            }
            if stopping.is_some_and(|start: Instant| start.elapsed() >= Duration::from_secs(15)) {
                self.kill();
            }
            #[cfg(unix)]
            let exited = {
                let Some(group) = self.group else {
                    // A settled child retains std::process::Child's cached status;
                    // its old process group no longer belongs to this owner.
                    return self.process.wait();
                };
                match rustix::process::waitid(
                    rustix::process::WaitId::Pid(group),
                    rustix::process::WaitIdOptions::EXITED
                        | rustix::process::WaitIdOptions::NOWAIT
                        | rustix::process::WaitIdOptions::NOHANG,
                ) {
                    Ok(Some(_)) => true,
                    Ok(None) | Err(rustix::io::Errno::INTR) => false,
                    Err(error) => return Err(error.into()),
                }
            };
            #[cfg(windows)]
            let exited = self.process.try_wait()?.is_some();
            if exited {
                self.kill();
                let status = self.process.wait()?;
                #[cfg(unix)]
                {
                    self.group = None;
                }
                #[cfg(windows)]
                if let Some(job) = &self.job {
                    let start = Instant::now();
                    while job.active()? != 0 {
                        if start.elapsed() >= Duration::from_secs(10) {
                            return Err(io::Error::other("build Job cleanup incomplete"));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                return Ok(status);
            }
            if stopping.is_some_and(|start: Instant| start.elapsed() >= Duration::from_secs(25)) {
                return Err(io::Error::other("development build cleanup incomplete"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            let _ = job.terminate();
        }
    }
}
impl Drop for BuildChild {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::io::Read as _;
    #[cfg(windows)]
    #[test]
    fn suspended_build_child_preserves_version_output_and_exit_status() {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/c", "ver"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = BuildChild::spawn(command, Some(Arc::new(AtomicBool::new(false)))).unwrap();
        let mut output = Vec::new();
        child.stdout().unwrap().read_to_end(&mut output).unwrap();
        let mut errors = Vec::new();
        child.stderr().unwrap().read_to_end(&mut errors).unwrap();
        let status = child.wait().unwrap();
        assert!(
            status.success(),
            "{status}: {}",
            String::from_utf8_lossy(&errors)
        );
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("Microsoft Windows"), "{output:?}");
    }

    #[cfg(unix)]
    #[test]
    fn settled_build_child_retains_exit_status_on_repeated_wait() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 7"]);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut child = BuildChild::spawn(command, Some(cancel.clone())).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(7));
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }
}
