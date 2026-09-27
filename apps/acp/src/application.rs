use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_acp::{
    Peer, Transport,
    server::{AgentBackend, run},
};
use rsi_acp_agent::AgentBackendContract;
use rsi_application::{ApplicationError, ApplicationRun, ApplicationRunContract};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use rustix::fs::OFlags;
use std::{
    ffi::OsString,
    fs::File,
    io::Write as _,
    os::fd::{AsRawFd, RawFd},
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, Interest, unix::AsyncFd};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Ordinary Unix ACP stdio Application, with no alternate launcher dispatch.
#[derive(Debug)]
pub struct ApplicationFactory {
    arguments: Vec<OsString>,
}
impl ApplicationFactory {
    /// Captures arguments for pure Profile preflight; ACP accepts no extra flags.
    pub fn new(arguments: Vec<OsString>) -> Self {
        Self { arguments }
    }
}
#[async_trait]
impl PluginFactory for ApplicationFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() || !self.arguments.is_empty() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "ACP stdio accepts no application arguments or configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<AgentBackendContract>()
            .requiring_local::<rsi::application_services::ServingServiceContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let runner = Arc::new(Runner {
            backend: plan.local::<AgentBackendContract>()?,
            service: plan.local::<rsi::application_services::ServingServiceContract>()?,
            started: Mutex::new(false),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            execution: plan.context().runtime().execution().clone(),
        });
        let supply = plan
            .context()
            .provide_local::<ApplicationRunContract>(runner.clone())?;
        plan.defer(
            "retire ACP Application",
            Box::new(move || {
                Box::pin(async move {
                    {
                        let _started = runner.started.lock().expect("ACP application");
                        runner.stop.cancel();
                        runner.tasks.close();
                    }
                    runner.tasks.wait().await;
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Runner {
    backend: Arc<dyn AgentBackend>,
    service: Arc<dyn rsi::application_services::ServingService>,
    started: Mutex<bool>,
    stop: CancellationToken,
    tasks: TaskTracker,
    execution: rsi_meta::Execution,
}
impl ApplicationRun for Runner {
    fn run(self: Arc<Self>) -> BoxFuture<'static, rsi_application::Result<u8>> {
        let mut started = self.started.lock().expect("ACP application");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(ApplicationError::ShuttingDown) });
        }
        if *started {
            return Box::pin(async { Err(ApplicationError::AlreadyStarted) });
        }
        *started = true;
        let runner = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let work = self.tasks.track_future(async move {
            let _ = send.send(runner.serve().await);
        });
        drop(started);
        drop(self.execution.spawn(work));
        Box::pin(async move { receive.await.map_err(|_| ApplicationError::TaskStopped) })
    }
}
impl Runner {
    async fn serve(&self) -> u8 {
        let result = async {
            let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|_| rsi_acp::Error::Closed)?;
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|_| rsi_acp::Error::Closed)?;
            let transport = Arc::new(Stdio { input: descriptor(std::io::stdin()).map_err(|_| rsi_acp::Error::Closed)?, output: descriptor(std::io::stdout()).map_err(|_| rsi_acp::Error::Closed)? });
            let work = run(Peer::start(transport), self.backend.clone(), self.stop.clone());
            tokio::pin!(work);
            tokio::select! { biased;
                result = &mut work => result,
                _ = interrupt.recv() => { self.stop.cancel(); work.await },
                _ = terminate.recv() => { self.stop.cancel(); work.await },
                _ = self.service.stopped() => { self.stop.cancel(); let _ = work.await; Err(rsi_acp::Error::Closed) },
            }
        }.await;
        if result.is_ok() {
            0
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "ACP connection could not complete cleanly"
            );
            1
        }
    }
}

#[derive(Debug)]
struct Descriptor {
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
enum Io {
    Poll(AsyncFd<Descriptor>),
    File(tokio::sync::Mutex<Option<tokio::fs::File>>),
}
fn descriptor(fd: impl std::os::fd::AsFd) -> std::io::Result<Io> {
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
struct Stdio {
    input: Io,
    output: Io,
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
    use super::*;

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
