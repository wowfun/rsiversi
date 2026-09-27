#[cfg(unix)]
use std::future::Future;
use std::process::{ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

const CAPTURE_LIMIT: usize = 8 * 1024 * 1024;
const DIAGNOSTIC_TAIL: usize = 8 * 1024;

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    total: usize,
    parsed: usize,
    accepted: bool,
    error: Option<String>,
}

impl Capture {
    fn append(&mut self, bytes: &[u8], stdout: bool) {
        self.total = self.total.saturating_add(bytes.len());
        let retained = bytes.len().min(CAPTURE_LIMIT - self.bytes.len());
        let previous = self.bytes.len();
        self.bytes.extend_from_slice(&bytes[..retained]);
        if stdout && !self.accepted {
            for offset in bytes[..retained]
                .iter()
                .enumerate()
                .filter_map(|(offset, byte)| (*byte == b'\n').then_some(offset))
            {
                let end = previous + offset;
                self.accepted |=
                    serde_json::from_slice::<serde_json::Value>(&self.bytes[self.parsed..end])
                        .is_ok_and(|value| value["type"] == "message");
                self.parsed = end + 1;
            }
        }
    }

    fn diagnostic(&self) -> String {
        format!(
            "bytes={} retained={} read_error={:?}\n{}",
            self.total,
            self.bytes.len(),
            self.error,
            String::from_utf8_lossy(
                &self.bytes[self.bytes.len().saturating_sub(DIAGNOSTIC_TAIL)..]
            )
        )
    }
}

fn drain(
    reader: impl AsyncRead + Unpin + Send + 'static,
    capture: Arc<Mutex<Capture>>,
    stdout: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = reader;
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => return,
                Ok(count) => capture.lock().unwrap().append(&buffer[..count], stdout),
                Err(error) => {
                    capture.lock().unwrap().error = Some(error.to_string());
                    return;
                }
            }
        }
    })
}

pub(super) struct ObservedChild {
    child: Child,
    stdout: Arc<Mutex<Capture>>,
    stderr: Arc<Mutex<Capture>>,
    readers: [Option<JoinHandle<()>>; 2],
    provider_entered: bool,
    signal: Option<&'static str>,
}

impl Drop for ObservedChild {
    fn drop(&mut self) {
        for reader in self.readers.iter().flatten() {
            reader.abort();
        }
        // The Child was configured with kill_on_drop at spawn. Explicit failure
        // paths below also wait for it; this covers a surrounding assertion panic.
    }
}

impl ObservedChild {
    pub(super) fn spawn(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let stdout = Arc::new(Mutex::new(Capture::default()));
        let stderr = Arc::new(Mutex::new(Capture::default()));
        let readers = [
            Some(drain(
                child.stdout.take().expect("piped stdout"),
                stdout.clone(),
                true,
            )),
            Some(drain(
                child.stderr.take().expect("piped stderr"),
                stderr.clone(),
                false,
            )),
        ];
        Ok(Self {
            child,
            stdout,
            stderr,
            readers,
            provider_entered: false,
            signal: None,
        })
    }

    #[cfg(unix)]
    pub(super) fn id(&self) -> u32 {
        self.child.id().expect("live observed child")
    }

    #[cfg(unix)]
    pub(super) fn signal_sent(&mut self, signal: &'static str) {
        self.signal = Some(signal);
    }

    fn diagnostic(&self, stage: &str) -> String {
        let stdout = self.stdout.lock().unwrap();
        let stderr = self.stderr.lock().unwrap();
        format!(
            "stage={stage} pid={:?} durable_acceptance={} provider_entered={} signal={:?}\nstdout: {}\nstderr: {}",
            self.child.id(),
            stdout.accepted,
            self.provider_entered,
            self.signal,
            stdout.diagnostic(),
            stderr.diagnostic()
        )
    }

    async fn drain_after_exit(&mut self) -> Result<(), String> {
        for reader in &mut self.readers {
            if let Some(mut task) = reader.take() {
                if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), &mut task).await {
                    result.map_err(|error| format!("output reader failed: {error}"))?;
                } else {
                    task.abort();
                    let _ = task.await;
                    return Err("output pipe remained open after child exit".into());
                }
            }
        }
        Ok(())
    }

    async fn fail(&mut self, stage: &str) -> String {
        // Preserve the observed failure before teardown can change its evidence.
        let before = self.diagnostic(stage);
        let teardown = match tokio::time::timeout(Duration::from_secs(5), self.child.kill()).await {
            Ok(Ok(())) => "child killed and reaped".to_owned(),
            Ok(Err(error)) => format!("kill/reap error: {error}"),
            Err(_) => "kill/reap deadline exceeded".into(),
        };
        let readers = self.drain_after_exit().await;
        format!(
            "{before}\n{teardown}; output drain={readers:?}\n{}",
            self.diagnostic("after teardown")
        )
    }

    #[cfg(unix)]
    pub(super) async fn wait_provider(
        &mut self,
        entered: impl Future<Output = ()>,
        deadline: Duration,
    ) -> Result<(), String> {
        tokio::select! {
            biased;
            () = entered => { self.provider_entered = true; Ok(()) }
            status = self.child.wait() => {
                let readers = self.drain_after_exit().await;
                Err(format!("child exited before provider entry: {status:?}; output drain={readers:?}\n{}", self.diagnostic("provider entry")))
            }
            () = tokio::time::sleep(deadline) => Err(self.fail("provider entry deadline").await),
        }
    }

    async fn finish(&mut self, status: ExitStatus) -> Result<Output, String> {
        self.drain_after_exit().await?;
        for capture in [&self.stdout, &self.stderr] {
            let capture = capture.lock().unwrap();
            if capture.total > CAPTURE_LIMIT || capture.error.is_some() {
                return Err(format!("incomplete child output: {}", capture.diagnostic()));
            }
        }
        Ok(Output {
            status,
            stdout: std::mem::take(&mut self.stdout.lock().unwrap().bytes),
            stderr: std::mem::take(&mut self.stderr.lock().unwrap().bytes),
        })
    }

    pub(super) async fn wait(mut self, deadline: Duration) -> Result<Output, String> {
        match tokio::time::timeout(deadline, self.child.wait()).await {
            Ok(Ok(status)) => self.finish(status).await,
            Ok(Err(error)) => Err(self.fail(&format!("exit wait error: {error}")).await),
            Err(_) => Err(self.fail("exit deadline").await),
        }
    }
}

pub(super) trait CommandOutput {
    async fn observed_output(&mut self) -> Result<Output, String>;
    async fn observed_output_with_timeout(&mut self, deadline: Duration) -> Result<Output, String>;
}

impl CommandOutput for Command {
    async fn observed_output(&mut self) -> Result<Output, String> {
        self.observed_output_with_timeout(Duration::from_secs(30))
            .await
    }

    async fn observed_output_with_timeout(&mut self, deadline: Duration) -> Result<Output, String> {
        ObservedChild::spawn(self)?.wait(deadline).await
    }
}

#[cfg(unix)]
#[tokio::test]
async fn early_child_exit_reports_status_and_stderr_without_waiting_for_provider() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo startup-failed >&2; exit 17"]);
    let mut child = ObservedChild::spawn(&mut command).unwrap();
    let error = child
        .wait_provider(std::future::pending(), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(error.contains("exited before provider entry"), "{error}");
    assert!(error.contains("startup-failed"), "{error}");
    assert!(error.contains("durable_acceptance=false"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn blocked_child_timeout_retains_acceptance_and_reaps_the_process() {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "echo '{\"type\":\"message\"}'; echo waiting >&2; exec sleep 30",
    ]);
    let mut child = ObservedChild::spawn(&mut command).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !child.stdout.lock().unwrap().accepted {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture acceptance should be captured before injecting a deadline");
    let error = child
        .wait_provider(std::future::pending(), Duration::from_millis(200))
        .await
        .unwrap_err();
    assert!(error.contains("durable_acceptance=true"), "{error}");
    assert!(error.contains("waiting"), "{error}");
    assert!(error.contains("child killed and reaped"), "{error}");
    assert!(child.child.try_wait().unwrap().is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn normal_exit_preserves_complete_stdout_and_stderr() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo complete; echo diagnostic >&2"]);
    let output = command.observed_output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"complete\n");
    assert_eq!(output.stderr, b"diagnostic\n");
}

#[cfg(unix)]
#[tokio::test]
async fn output_overflow_is_drained_but_never_reported_as_complete_success() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "head -c 8388609 /dev/zero >&2"]);
    let error = command.observed_output().await.unwrap_err();
    assert!(error.contains("incomplete child output"));
    assert!(error.contains("bytes=8388609 retained=8388608"));
}
