use std::io;
use tokio::sync::mpsc;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone, Debug, Default)]
pub(crate) struct ApplicationWork {
    pub stop: CancellationToken,
    pub tasks: TaskTracker,
}

pub(crate) struct Input {
    receiver: mpsc::Receiver<crate::SessionInput>,
    stop: CancellationToken,
}
impl Input {
    pub async fn recv(&mut self) -> Option<crate::SessionInput> {
        self.receiver.recv().await
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) fn input(work: &ApplicationWork) -> io::Result<Input> {
    let stop = work.stop.child_token();
    let source = stdin(stop.clone())?;
    let (sender, receiver) = mpsc::channel(crate::SESSION_INPUT_CHANNEL_CAPACITY);
    let token = work.tasks.token();
    std::thread::Builder::new()
        .name("rsi-terminal-input".into())
        .spawn(move || {
            let _token = token;
            let mut reader = io::BufReader::new(source);
            crate::forward_session_input(&mut reader, &sender);
        })?;
    Ok(Input { receiver, stop })
}

#[cfg(unix)]
pub(crate) fn stdin(stop: CancellationToken) -> io::Result<impl io::Read + Send> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let file = std::fs::File::from(rustix::io::dup(io::stdin())?);
    let flags = fcntl_getfl(&file)?;
    fcntl_setfl(&file, flags | OFlags::NONBLOCK)?;
    Ok(Stdin { file, flags, stop })
}

#[cfg(unix)]
struct Stdin {
    file: std::fs::File,
    flags: rustix::fs::OFlags,
    stop: CancellationToken,
}
#[cfg(unix)]
impl io::Read for Stdin {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stop.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal input retired",
                ));
            }
            match self.file.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }
}
#[cfg(unix)]
impl Drop for Stdin {
    fn drop(&mut self) {
        // The exclusive process-terminal lease outlives this worker and restoration.
        let _ = rustix::fs::fcntl_setfl(&self.file, self.flags);
    }
}

#[cfg(not(unix))]
pub(crate) fn stdin(_stop: CancellationToken) -> io::Result<impl io::Read + Send> {
    Ok(io::stdin())
}

#[cfg(unix)]
pub(crate) struct Output {
    file: std::fs::File,
    flags: rustix::fs::OFlags,
    stop: CancellationToken,
}
#[cfg(unix)]
impl Output {
    pub fn new(fd: impl std::os::fd::AsFd, stop: CancellationToken) -> io::Result<Self> {
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
        let file = std::fs::File::from(rustix::io::dup(fd)?);
        let flags = fcntl_getfl(&file)?;
        fcntl_setfl(&file, flags | OFlags::NONBLOCK)?;
        Ok(Self { file, flags, stop })
    }
}
#[cfg(unix)]
impl io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            if self.stop.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal output retired",
                ));
            }
            match self.file.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for Output {
    fn drop(&mut self) {
        // dup shares flags with the process descriptor. Restore them when the
        // actual writer exits, including a timed-out diagnostic worker.
        let _ = rustix::fs::fcntl_setfl(&self.file, self.flags);
    }
}

fn diagnostic_text(messages: Vec<String>) -> Vec<u8> {
    let mut output = String::new();
    for message in messages {
        output.extend(
            message
                .trim_end_matches('\n')
                .chars()
                .take(4095)
                .map(|character| {
                    if character == '\n' {
                        ' '
                    } else {
                        rsi_terminal_ui::terminal_character(character)
                    }
                }),
        );
        output.push('\n');
    }
    output.into_bytes()
}

pub(crate) async fn diagnostic(messages: Vec<String>) -> io::Result<()> {
    let bytes = diagnostic_text(messages);
    if bytes.is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        diagnostic_to(io::stderr(), bytes).await
    }
    #[cfg(not(unix))]
    {
        deliver_diagnostic(&DIAGNOSTICS, move |_| {
            use io::Write as _;
            io::stderr().write_all(&bytes)
        })
        .await
    }
}

static DIAGNOSTICS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));

async fn deliver_diagnostic(
    capacity: &std::sync::Arc<tokio::sync::Semaphore>,
    write: impl FnOnce(CancellationToken) -> io::Result<()> + Send + 'static,
) -> io::Result<()> {
    let permit = capacity.clone().try_acquire_owned().map_err(|_| {
        io::Error::new(io::ErrorKind::WouldBlock, "terminal diagnostic worker busy")
    })?;
    let stop = CancellationToken::new();
    let _cancel = stop.clone().drop_guard();
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("rsi-terminal-diagnostic".into())
        .spawn(move || {
            let result = write(stop);
            drop(permit);
            let _delivered = send.send(result);
        })?;
    tokio::time::timeout(std::time::Duration::from_secs(1), receive)
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "terminal diagnostic delivery timed out",
            )
        })?
        .map_err(io::Error::other)?
}

#[cfg(unix)]
async fn diagnostic_to(
    fd: impl std::os::fd::AsFd + Send + 'static,
    bytes: Vec<u8>,
) -> io::Result<()> {
    deliver_diagnostic(&DIAGNOSTICS, move |stop| {
        use io::Write as _;
        // Regular-file writes can block despite O_NONBLOCK. Do not alter their
        // shared descriptor flags; the bounded worker retains the duplicate.
        if rustix::fs::FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode)
            == rustix::fs::FileType::RegularFile
        {
            std::fs::File::from(rustix::io::dup(fd)?).write_all(&bytes)
        } else {
            let mut output = Output::new(fd, stop)?;
            output.write_all(&bytes)?;
            output.flush()
        }
    })
    .await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn diagnostics_neutralize_controls_and_bound_each_result() {
        let bytes = diagnostic_text(vec![format!(
            "path\x1b]52;c;secret\x07\r\n\u{202e}{}",
            "界".repeat(20000)
        )]);
        assert!(bytes.len() <= 16 * 1024);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains(['\x1b', '\x07', '\r', '\u{202e}']));
        assert_eq!(text.matches('\n').count(), 1);
    }
    #[tokio::test]
    async fn stalled_diagnostic_times_out_and_restores_descriptor_flags() {
        let (writer, _unread) = std::os::unix::net::UnixStream::pair().unwrap();
        let flags = rustix::fs::fcntl_getfl(&writer).unwrap();
        rustix::fs::fcntl_setfl(&writer, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
        while rustix::io::write(&writer, &[b'x'; 8192]).is_ok() {}
        rustix::fs::fcntl_setfl(&writer, flags).unwrap();
        let original = rustix::fs::fcntl_getfl(&writer).unwrap();
        let observed = writer.try_clone().unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            diagnostic_to(writer, b"settled export\n".to_vec()),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while rustix::fs::fcntl_getfl(&observed).unwrap() != original {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn blocked_system_write_has_a_deadline_and_retains_its_capacity() {
        let capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let (release, blocked) = std::sync::mpsc::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        let mut delivery = Box::pin(deliver_diagnostic(&capacity, move |_| {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            Ok(())
        }));
        assert!(futures_util::poll!(&mut delivery).is_pending());
        started.await.unwrap();
        assert_eq!(delivery.await.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            deliver_diagnostic(&capacity, |_| Ok(()))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), capacity.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
    #[test]
    fn output_cancellation_drains_worker_and_restores_shared_descriptor_flags() {
        let (writer, _unread) = std::os::unix::net::UnixStream::pair().unwrap();
        // Darwin records FWASWRITTEN in F_GETFL after the first write. Establish
        // that kernel history before comparing all descriptor flags exactly.
        // UnixStream::write uses send(), which does not set this write(2) bit.
        assert_eq!(rustix::io::write(&writer, b"x").unwrap(), 1);
        let original = rustix::fs::fcntl_getfl(&writer).unwrap();
        let stop = CancellationToken::new();
        let mut output = Output::new(&writer, stop.clone()).unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let result = output.write_all(&vec![b'x'; 4 * 1024 * 1024]);
            drop(output);
            done.send(result).unwrap();
        });
        let initial = finished.recv_timeout(std::time::Duration::from_millis(50));
        stop.cancel();
        assert!(
            matches!(initial, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "writer completed before cancellation: {initial:?}"
        );
        assert_eq!(
            finished
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        thread.join().unwrap();
        assert_eq!(rustix::fs::fcntl_getfl(&writer).unwrap(), original);
    }
}
