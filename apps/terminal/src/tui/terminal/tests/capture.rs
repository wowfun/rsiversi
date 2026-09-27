use std::io::{self, Read as _};
use std::os::fd::{AsRawFd, RawFd};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// Borrow the master's descriptor only while safe duplication creates an owned
// reader. The wrapper never closes or transfers the master's descriptor.
struct BorrowedMaster<'a>(&'a dyn portable_pty::MasterPty);
impl AsRawFd for BorrowedMaster<'_> {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd().expect("Unix PTY descriptor")
    }
}

pub(super) struct PtyCapture {
    stop: CancellationToken,
    task: Option<JoinHandle<Vec<u8>>>,
}

impl PtyCapture {
    pub(super) fn start(master: &dyn portable_pty::MasterPty) -> Self {
        let mut reader = filedescriptor::FileDescriptor::dup(&BorrowedMaster(master)).unwrap();
        reader.set_non_blocking(true).unwrap();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        assert!(
                            output.len() + count <= 8 * 1024 * 1024,
                            "PTY output exceeded capture bound"
                        );
                        output.extend_from_slice(&buffer[..count]);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if stopping.is_cancelled() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => panic!("PTY read failed: {error}"),
                }
            }
            output
        });
        Self {
            stop,
            task: Some(task),
        }
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        self.stop.cancel();
        self.task.take().unwrap().join().unwrap()
    }
}

impl Drop for PtyCapture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
