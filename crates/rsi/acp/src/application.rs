use crate::AgentBackendContract;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_acp::{
    Peer, Transport,
    server::{AgentBackend, run},
};
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
            .requiring_local::<rsi_serve::ServingServiceContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let runner = Arc::new(Runner {
            backend: plan.local::<AgentBackendContract>()?,
            service: plan.local::<rsi_serve::ServingServiceContract>()?,
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
    service: Arc<dyn rsi_serve::ServingService>,
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
