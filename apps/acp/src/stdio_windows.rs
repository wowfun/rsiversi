//! Cancellable Windows stdio; only the native module contains syscalls.
use async_trait::async_trait;
use rsi_acp::Transport;
#[cfg(test)]
use std::time::Duration;
use std::{fs::File, sync::Arc};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
#[allow(unsafe_code)]
#[path = "stdio_windows/native.rs"]
mod native;
#[path = "stdio_windows/workers.rs"]
mod workers;

#[derive(Debug)]
enum Io {
    Pipe(Arc<workers::Pipe>),
    File(tokio::sync::Mutex<Option<tokio::fs::File>>),
}
fn descriptor(file: File, input: bool) -> std::io::Result<Io> {
    match native::kind(&file)? {
        1 if file.metadata()?.is_file() => Ok(Io::File(tokio::sync::Mutex::new(Some(
            tokio::fs::File::from_std(file),
        )))),
        3 => Ok(Io::Pipe(workers::Pipe::new(file, input))),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "ACP stdio requires native pipes or regular disk files",
        )),
    }
}
#[derive(Debug)]
pub(super) struct Stdio {
    input: Io,
    output: Io,
}
pub(super) fn create() -> std::io::Result<Stdio> {
    Ok(Stdio {
        input: descriptor(native::standard_handle(-10)?, true)?,
        output: descriptor(native::standard_handle(-11)?, false)?,
    })
}
#[async_trait]
impl Transport for Stdio {
    async fn read(&self) -> Result<Vec<u8>, rsi_acp::Error> {
        if let Io::File(file) = &self.input {
            let mut bytes = vec![0; 65536];
            let read = file
                .lock()
                .await
                .as_mut()
                .ok_or(rsi_acp::Error::Closed)?
                .read(&mut bytes)
                .await
                .map_err(|_| rsi_acp::Error::Closed)?;
            bytes.truncate(read);
            return Ok(bytes);
        }
        let Io::Pipe(pipe) = &self.input else {
            unreachable!()
        };
        pipe.read().await
    }
    async fn write(&self, bytes: &[u8]) -> Result<(), rsi_acp::Error> {
        if let Io::File(file) = &self.output {
            return file
                .lock()
                .await
                .as_mut()
                .ok_or(rsi_acp::Error::Closed)?
                .write_all(bytes)
                .await
                .map_err(|_| rsi_acp::Error::Closed);
        }
        let Io::Pipe(pipe) = &self.output else {
            unreachable!()
        };
        pipe.write(bytes).await
    }
    async fn flush(&self) -> Result<(), rsi_acp::Error> {
        if let Io::File(file) = &self.output {
            file.lock()
                .await
                .as_mut()
                .ok_or(rsi_acp::Error::Closed)?
                .flush()
                .await
                .map_err(|_| rsi_acp::Error::Closed)?;
        }
        Ok(())
    }
    async fn close(&self) -> Result<(), rsi_acp::Error> {
        let mut result = Ok(());
        for io in [&self.input, &self.output] {
            if let Io::Pipe(pipe) = io
                && pipe.close().await.is_err()
            {
                result = Err(rsi_acp::Error::Closed);
            }
            if let Io::File(file) = io
                && let Some(mut file) = file.lock().await.take()
            {
                if file.flush().await.is_err() {
                    result = Err(rsi_acp::Error::Closed);
                }
                drop(file.into_std().await);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::windows::io::OwnedHandle;
    #[tokio::test]
    async fn native_pipe_eof_and_repeated_close_are_clean() {
        let (input, writer) = std::io::pipe().unwrap();
        let (_reader, output) = std::io::pipe().unwrap();
        let transport = Stdio {
            input: descriptor(File::from(OwnedHandle::from(input)), true).unwrap(),
            output: descriptor(File::from(OwnedHandle::from(output)), false).unwrap(),
        };
        drop(writer);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), transport.read())
                .await
                .unwrap()
                .unwrap()
                .is_empty()
        );
        transport.close().await.unwrap();
        transport.close().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_native_pipe_read_preserves_following_bytes_and_joins_workers() {
        let (input, mut writer) = std::io::pipe().unwrap();
        let (reader, output) = std::io::pipe().unwrap();
        let transport = Stdio {
            input: descriptor(
                File::from(OwnedHandle::from(input.try_clone().unwrap())),
                true,
            )
            .unwrap(),
            output: descriptor(File::from(OwnedHandle::from(output)), false).unwrap(),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(20), transport.read())
                .await
                .is_err()
        );
        writer.write_all(b"after cancellation\n").unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), transport.read())
                .await
                .unwrap()
                .unwrap(),
            b"after cancellation\n"
        );
        tokio::time::timeout(Duration::from_millis(100), transport.close())
            .await
            .unwrap()
            .unwrap();
        drop(transport);
        drop(writer);
        drop(reader);
        assert_eq!(
            native::pipe_mode(&File::from(OwnedHandle::from(input))).unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_full_native_output_pipe_can_be_cancelled_and_closed() {
        let (input, _writer) = std::io::pipe().unwrap();
        let (_reader, output) = std::io::pipe().unwrap();
        let transport = Stdio {
            input: descriptor(File::from(OwnedHandle::from(input)), true).unwrap(),
            output: descriptor(File::from(OwnedHandle::from(output)), false).unwrap(),
        };
        let bytes = vec![42; 1_048_576];
        assert!(
            tokio::time::timeout(Duration::from_millis(20), transport.write(&bytes))
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_millis(100), transport.close())
            .await
            .unwrap()
            .unwrap();
    }
}
