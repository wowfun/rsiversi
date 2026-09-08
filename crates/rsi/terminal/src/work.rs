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
        // dup shares flags with the process descriptor. The exclusive terminal
        // lease outlives this tracked worker and restoration of both streams.
        let _ = rustix::fs::fcntl_setfl(&self.file, self.flags);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn output_cancellation_drains_worker_and_restores_shared_descriptor_flags() {
        let (mut writer, _unread) = std::os::unix::net::UnixStream::pair().unwrap();
        // Darwin records FWASWRITTEN in F_GETFL after the first write. Establish
        // that kernel history before comparing all descriptor flags exactly.
        std::io::Write::write_all(&mut writer, b"x").unwrap();
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
