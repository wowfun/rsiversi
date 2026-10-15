use async_trait::async_trait;
use rsi_acp::Transport;
use rustix::fs::OFlags;
use std::{
    fs::File,
    os::fd::{AsRawFd, RawFd},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, Interest, unix::AsyncFd};

pub(super) fn create() -> std::io::Result<Stdio> {
    Ok(Stdio {
        input: descriptor(std::io::stdin())?,
        output: descriptor(std::io::stdout())?,
    })
}
#[derive(Debug)]
pub(super) struct Descriptor {
    file: File,
    flags: OFlags,
}
impl AsRawFd for Descriptor {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        let _ = rustix::fs::fcntl_setfl(&self.file, self.flags);
    }
}
#[derive(Debug)]
pub(super) enum Io {
    Poll(AsyncFd<Descriptor>),
    File(tokio::sync::Mutex<Option<tokio::fs::File>>),
}
pub(super) fn descriptor(fd: impl std::os::fd::AsFd) -> std::io::Result<Io> {
    let file = File::from(rustix::io::dup(fd)?);
    if file.metadata()?.is_file() {
        return Ok(Io::File(tokio::sync::Mutex::new(Some(
            tokio::fs::File::from_std(file),
        ))));
    }
    let flags = rustix::fs::fcntl_getfl(&file)?;
    let descriptor = Descriptor { file, flags };
    rustix::fs::fcntl_setfl(&descriptor.file, flags | OFlags::NONBLOCK)?;
    AsyncFd::new(descriptor).map(Io::Poll)
}
#[derive(Debug)]
pub(super) struct Stdio {
    pub(super) input: Io,
    pub(super) output: Io,
}
#[async_trait]
impl Transport for Stdio {
    async fn read(&self) -> Result<Vec<u8>, rsi_acp::Error> {
        let mut bytes = vec![0; 64 * 1024];
        let count = match &self.input {
            Io::Poll(input) => {
                input
                    .async_io(Interest::READABLE, |input| {
                        rustix::io::read(&input.file, &mut bytes).map_err(Into::into)
                    })
                    .await
            }
            Io::File(input) => {
                input
                    .lock()
                    .await
                    .as_mut()
                    .ok_or(rsi_acp::Error::Closed)?
                    .read(&mut bytes)
                    .await
            }
        }
        .map_err(|_| rsi_acp::Error::Closed)?;
        bytes.truncate(count);
        Ok(bytes)
    }
    async fn write(&self, mut bytes: &[u8]) -> Result<(), rsi_acp::Error> {
        let Io::Poll(output) = &self.output else {
            let Io::File(output) = &self.output else {
                unreachable!()
            };
            return output
                .lock()
                .await
                .as_mut()
                .ok_or(rsi_acp::Error::Closed)?
                .write_all(bytes)
                .await
                .map_err(|_| rsi_acp::Error::Closed);
        };
        while !bytes.is_empty() {
            let count = output
                .async_io(Interest::WRITABLE, |output| {
                    rustix::io::write(&output.file, bytes).map_err(Into::into)
                })
                .await
                .map_err(|_| rsi_acp::Error::Closed)?;
            if count == 0 {
                return Err(rsi_acp::Error::Closed);
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
    async fn flush(&self) -> Result<(), rsi_acp::Error> {
        if let Io::File(output) = &self.output {
            output
                .lock()
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
            if let Io::File(file) = io {
                let file = file.lock().await.take();
                if let Some(mut file) = file {
                    if file.flush().await.is_err() {
                        result = Err(rsi_acp::Error::Closed);
                    }
                    drop(file.into_std().await);
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::ApplicationFactory;
    use super::*;
    use rsi_meta::{ConfigValue, PluginFactory as _};
    use std::io::Write as _;

    #[test]
    fn preflight_rejects_arguments_and_configuration_without_touching_stdio() {
        assert!(
            ApplicationFactory::new(vec![])
                .prepare(&ConfigValue::Null)
                .is_ok()
        );
        assert!(matches!(
            ApplicationFactory::new(vec!["--extra".into()]).prepare(&ConfigValue::Null),
            Err(rsi_meta::MetaError::InvalidInput(_))
        ));
        assert!(matches!(
            ApplicationFactory::new(vec![]).prepare(&ConfigValue::Bool(true)),
            Err(rsi_meta::MetaError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn cancelling_idle_pipe_reads_releases_descriptors_and_restores_shared_flags() {
        let (input, writer) = std::io::pipe().unwrap();
        let (reader, output) = std::io::pipe().unwrap();
        let before = rustix::fs::fcntl_getfl(&input).unwrap();
        let transport = Stdio {
            input: descriptor(&input).unwrap(),
            output: descriptor(&output).unwrap(),
        };
        assert!(
            rustix::fs::fcntl_getfl(&input)
                .unwrap()
                .contains(OFlags::NONBLOCK)
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), transport.read())
                .await
                .is_err()
        );
        transport.close().await.unwrap();
        drop(transport);
        assert_eq!(rustix::fs::fcntl_getfl(&input).unwrap(), before);
        drop(reader);
        drop(writer);
    }

    #[tokio::test]
    async fn redirected_files_flush_and_close_without_reusing_released_handles() {
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(b"request\n").unwrap();
        std::io::Seek::rewind(&mut input).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let transport = Stdio {
            input: descriptor(&input).unwrap(),
            output: descriptor(&output).unwrap(),
        };
        assert_eq!(transport.read().await.unwrap(), b"request\n");
        transport.write(b"response\n").await.unwrap();
        transport.close().await.unwrap();
        assert!(transport.read().await.is_err());
        assert!(transport.write(b"late").await.is_err());
        std::io::Seek::rewind(&mut output).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut output, &mut bytes).unwrap();
        assert_eq!(bytes, b"response\n");
    }
}
