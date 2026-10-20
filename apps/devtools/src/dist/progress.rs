use super::{Command, OsString, Path, PathBuf, Result, Stdio, fs};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const HEARTBEAT: Duration = Duration::from_secs(15);

pub(super) struct Stage {
    label: String,
    start: Instant,
    stop: mpsc::Sender<()>,
    task: Option<thread::JoinHandle<()>>,
    success: bool,
}
impl Stage {
    pub(super) fn new(label: impl Into<String>) -> Self {
        Self::with_interval(label.into(), HEARTBEAT)
    }
    fn with_interval(label: String, interval: Duration) -> Self {
        let start = Instant::now();
        diagnostic!("[start] {label}");
        let (stop, receiver) = mpsc::channel();
        let title = label.clone();
        let task = thread::spawn(move || {
            while matches!(
                receiver.recv_timeout(interval),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                diagnostic!(
                    "[running] {title} ({:.1}s elapsed)",
                    start.elapsed().as_secs_f64()
                );
            }
        });
        Self {
            label,
            start,
            stop,
            task: Some(task),
            success: false,
        }
    }
    pub(super) fn complete(mut self) {
        self.success = true;
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
        diagnostic!(
            "[{}] {} ({:.1}s)",
            if self.success { "ok" } else { "failed" },
            self.label,
            self.start.elapsed().as_secs_f64()
        );
    }
}

pub(super) fn build_lock(cache: &Path) -> Result<fs::File> {
    development_build_lock(cache, None)
}
pub(super) fn development_build_lock(
    cache: &Path,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<fs::File> {
    fs::create_dir_all(cache)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("build.lock"))?;
    match lock.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => {
            let stage = Stage::new("Waiting for the paired build owner");
            if let Some(cancellation) = cancellation {
                loop {
                    if cancellation.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(
                            "development interrupted while waiting for the paired build owner"
                                .into(),
                        );
                    }
                    match lock.try_lock() {
                        Ok(()) => break,
                        Err(std::fs::TryLockError::WouldBlock) => {
                            thread::sleep(Duration::from_millis(50));
                        }
                        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                    }
                }
            } else {
                lock.lock()?;
            }
            stage.complete();
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    Ok(lock)
}

pub(super) struct Runner {
    pub(super) cwd: PathBuf,
    pub(super) environment: BTreeMap<OsString, OsString>,
    log: Arc<Mutex<fs::File>>,
    log_path: PathBuf,
    verbose: bool,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl Runner {
    pub(super) fn new(
        cwd: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        log_path: PathBuf,
        verbose: bool,
    ) -> Result<Self> {
        Ok(Self {
            cwd,
            environment,
            log: Arc::new(Mutex::new(fs::File::create(&log_path)?)),
            log_path,
            verbose,
            cancellation: None,
        })
    }
    pub(super) fn with_cancellation(
        mut self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }
    fn command(&self, program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.environment);
        command
    }
    pub(super) fn version(&self, program: &str, args: &[&str]) -> Result<String> {
        let mut command = self.command(program, args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            crate::supervision::BuildChild::spawn(command, self.cancellation.clone())
                .map_err(|e| format!("{program}: {e}; run the source entrypoint's doctor"))?;
        let output = bounded_capture(child.stdout().ok_or("version stdout missing")?);
        let errors = bounded_capture(child.stderr().ok_or("version stderr missing")?);
        let status = child.wait()?;
        let (output, overflow) = output
            .join()
            .map_err(|_| "version stdout reader panicked")??;
        let (errors, error_overflow) = errors
            .join()
            .map_err(|_| "version stderr reader panicked")??;
        if !status.success() || overflow || error_overflow {
            return Err(format!(
                "{program} version check failed ({status}, output overflow: {}): {}",
                overflow || error_overflow,
                String::from_utf8_lossy(&errors).trim()
            )
            .into());
        }
        Ok(String::from_utf8(output)?.trim().into())
    }
    pub(super) fn run(&mut self, label: &str, program: &str, args: &[&str]) -> Result<()> {
        let stage = Stage::new(label);
        diagnostic!(
            "Command: {program} {args:?}\nLog: {}",
            self.log_path.display()
        );
        let start = {
            let mut log = self.log.lock().map_err(|_| "build log poisoned")?;
            let start = log.stream_position()?;
            writeln!(log, "\n[{label}] {program} {args:?}")?;
            start
        };
        let mut command = self.command(program, args);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::supervision::BuildChild::spawn(command, self.cancellation.clone())?;
        let output = mirror(
            child.stdout().ok_or("child stdout missing")?,
            self.log.clone(),
            self.verbose,
        );
        let errors = mirror(
            child.stderr().ok_or("child stderr missing")?,
            self.log.clone(),
            self.verbose,
        );
        let status = child.wait();
        let output = output.join().map_err(|_| "build stdout reader panicked")?;
        let errors = errors.join().map_err(|_| "build stderr reader panicked")?;
        let status = status?;
        if !status.success() {
            match diagnostics(&self.log_path, start) {
                Ok((first, tail)) if first == tail => diagnostic!("Log tail:\n{tail}"),
                Ok((first, tail)) => {
                    diagnostic!("First error context:\n{first}\nLog tail:\n{tail}");
                }
                Err(error) => diagnostic!("Cannot read build diagnostics: {error}"),
            }
            return Err(format!(
                "{label}: {program} {args:?} failed: {status}; log: {}",
                self.log_path.display()
            )
            .into());
        }
        output?;
        errors?;
        stage.complete();
        Ok(())
    }
}
fn bounded_capture(
    mut input: impl Read + Send + 'static,
) -> thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut overflow = false;
        let mut block = [0; 8192];
        loop {
            let count = input.read(&mut block)?;
            if count == 0 {
                break;
            }
            let retained = count.min((64 * 1024_usize).saturating_sub(output.len()));
            output.extend_from_slice(&block[..retained]);
            overflow |= retained < count;
        }
        Ok((output, overflow))
    })
}

fn mirror(
    mut input: impl Read + Send + 'static,
    log: Arc<Mutex<fs::File>>,
    verbose: bool,
) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        let mut failure = None;
        loop {
            let count = match input.read(&mut buffer) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                break;
            }
            // Drain both pipes even after an output error, so the compiler cannot deadlock.
            let written = log
                .lock()
                .map_err(|_| std::io::Error::other("build log poisoned"))
                .and_then(|mut file| file.write_all(&buffer[..count]));
            if let Err(error) = written {
                failure.get_or_insert(error);
            }
            if verbose {
                let _ = std::io::stderr().lock().write_all(&buffer[..count]);
            }
        }
        failure.map_or(Ok(()), Err)
    })
}
fn diagnostics(path: &Path, start: u64) -> std::io::Result<(String, String)> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut block = [0; 8192];
    let mut tail = Vec::new();
    let mut first = None;
    let mut position = start;
    loop {
        let count = file.read(&mut block)?;
        if count == 0 {
            break;
        }
        let scan = tail.len().saturating_sub(7);
        tail.extend_from_slice(&block[..count]);
        position += count as u64;
        if first.is_none() {
            first = [
                b"error:".as_slice(),
                b"error[",
                b"fatal:",
                b"Error:",
                b"ERR_PNPM",
            ]
            .iter()
            .filter_map(|needle| {
                tail[scan..]
                    .windows(needle.len())
                    .position(|part| part == *needle)
            })
            .min()
            .map(|offset| position - tail.len() as u64 + (scan + offset) as u64);
        }
        if tail.len() > 32768 {
            tail.drain(..tail.len() - 32768);
        }
    }
    let tail = String::from_utf8_lossy(&tail).into_owned();
    let first = if let Some(offset) = first {
        file.seek(SeekFrom::Start(offset.saturating_sub(1024).max(start)))?;
        let mut context = Vec::new();
        file.take(32768).read_to_end(&mut context)?;
        String::from_utf8_lossy(&context).into_owned()
    } else {
        tail.clone()
    };
    Ok((first, tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_lock_wait_can_stop_without_changing_the_current_owner() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let owner = build_lock(directory.path()).unwrap();
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                cancelled.store(true, Ordering::SeqCst);
            });
            let started = Instant::now();
            let error = development_build_lock(directory.path(), Some(&cancelled)).unwrap_err();
            assert!(error.to_string().contains("interrupted"));
            assert!(started.elapsed() < Duration::from_secs(2));
        });
        let contender = fs::File::options()
            .read(true)
            .write(true)
            .open(directory.path().join("build.lock"))
            .unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(owner);
        contender.try_lock().unwrap();
    }
    #[test]
    fn first_error_keeps_following_context_across_read_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("build.log");
        for offset in [65530, 65533, 65536] {
            let mut bytes = vec![b'.'; offset];
            bytes.extend_from_slice(
                b"error[E0001]: useful message\n --> src/main.rs:42\n detailed explanation\n",
            );
            bytes.extend(vec![b'x'; 100_000]);
            fs::write(&path, bytes).unwrap();
            let (first, tail) = diagnostics(&path, 0).unwrap();
            assert!(first.contains(
                "error[E0001]: useful message\n --> src/main.rs:42\n detailed explanation"
            ));
            assert_eq!(first.find("error["), Some(1024));
            assert!(first.len() <= 32768 && tail.len() <= 32768);
        }
        fs::write(&path, "error: short failure\n").unwrap();
        let (first, tail) = diagnostics(&path, 0).unwrap();
        assert_eq!(first, tail);
    }
    #[test]
    fn diagnostics_ignore_previous_commands_and_recognize_frontend_failures() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("build.log");
        let previous = b"[successful stage] error: expected negative fixture\n";
        for marker in [
            "TypeError: frontend failed",
            "ERR_PNPM_FETCH_404 missing package",
        ] {
            let mut bytes = previous.to_vec();
            bytes.extend_from_slice(marker.as_bytes());
            bytes.extend(vec![b'x'; 100_000]);
            bytes.extend_from_slice(b"command complete\n");
            fs::write(&path, bytes).unwrap();
            let (first, tail) = diagnostics(&path, previous.len() as u64).unwrap();
            assert!(first.contains(marker));
            assert!(!first.contains("successful stage") && !tail.contains("successful stage"));
            assert!(tail.ends_with("command complete\n"));
            assert!(first.len() <= 40960 && tail.len() <= 32768);
        }
        fs::write(
            &path,
            [previous.as_slice(), b"unrecognized tool failure"].concat(),
        )
        .unwrap();
        let (first, tail) = diagnostics(&path, previous.len() as u64).unwrap();
        assert_eq!(first, "unrecognized tool failure");
        assert_eq!(tail, first);
    }

    #[test]
    fn closed_stderr_fixture() {
        if std::env::var_os("RSI_CLOSED_STDERR_FIXTURE").is_none() {
            return;
        }
        std::io::stdin().read_exact(&mut [0]).unwrap();
        let stage = Stage::with_interval("closed pipe".into(), Duration::from_millis(1));
        thread::sleep(Duration::from_millis(20));
        stage.complete();
    }

    #[test]
    fn progress_and_drop_survive_a_closed_stderr_pipe() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dist::progress::tests::closed_stderr_fixture",
                "--nocapture",
            ])
            .env("RSI_CLOSED_STDERR_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stderr.take());
        child.stdin.take().unwrap().write_all(b"!").unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn error_context_survives_a_long_tail_and_output_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("build.log");
        let mut bytes = b"error[E1234]: first diagnostic\n".to_vec();
        bytes.extend(vec![b'x'; 200_000]);
        bytes.extend_from_slice(b"last line\n");
        fs::write(&path, bytes).unwrap();
        let (first, tail) = diagnostics(&path, 0).unwrap();
        assert!(first.contains("error[E1234]"));
        assert!(first.len() <= 40960 && tail.len() <= 32768);
        assert!(tail.ends_with("last line\n"));
    }
    #[test]
    fn heartbeat_owner_stops_without_waiting_for_an_interval() {
        let started = Instant::now();
        Stage::with_interval("fixture".into(), Duration::from_mins(1)).complete();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn child_output_fixture() {
        if std::env::var_os("RSI_OUTPUT_FIXTURE").is_none() {
            return;
        }
        let out = thread::spawn(|| {
            let mut output = std::io::stdout().lock();
            for _ in 0..256 {
                output.write_all(&[b'o'; 8192]).unwrap();
            }
            output.write_all(b"stdout complete\n").unwrap();
        });
        let mut errors = std::io::stderr().lock();
        errors
            .write_all(b"error[E9999]: fixture failure\n")
            .unwrap();
        for _ in 0..256 {
            errors.write_all(&[b'e'; 8192]).unwrap();
        }
        errors.write_all(b"stderr complete\n").unwrap();
        drop(errors);
        out.join().unwrap();
        panic!("fixture exits unsuccessfully");
    }

    #[test]
    fn runner_drains_both_large_pipes_and_preserves_failure_status() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("build.log");
        let mut environment = super::super::environment(root.path()).unwrap();
        environment.insert("RSI_OUTPUT_FIXTURE".into(), "1".into());
        let mut runner = Runner::new(root.path().into(), environment, log.clone(), false).unwrap();
        let executable = std::env::current_exe().unwrap();
        let failure = runner
            .run(
                "fixture",
                executable.to_str().unwrap(),
                &[
                    "--exact",
                    "dist::progress::tests::child_output_fixture",
                    "--nocapture",
                ],
            )
            .unwrap_err();
        assert!(failure.to_string().contains("failed:"));
        let bytes = fs::read(&log).unwrap();
        assert!(bytes.len() >= 4 * 1024 * 1024);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("stdout complete") && text.contains("stderr complete"));
    }
}
