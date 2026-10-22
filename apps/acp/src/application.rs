#[cfg(unix)]
#[path = "stdio_unix.rs"]
mod stdio;
#[cfg(windows)]
#[path = "stdio_windows.rs"]
mod stdio;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi::application_services::{AcpAttachment, AcpAttachmentContract};
use rsi_acp::{Peer, Transport, server::run};
use rsi_application::{ApplicationError, ApplicationRun, ApplicationRunContract};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use std::{
    ffi::OsString,
    io::Write as _,
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Ordinary native ACP stdio Application, with no alternate launcher dispatch.
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
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<AcpAttachmentContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let runner = Arc::new(Runner {
            attachment: plan.local::<AcpAttachmentContract>()?,
            service: plan
                .context()
                .lookup_local::<rsi::application_services::ServingServiceContract>(),
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
    attachment: Arc<AcpAttachment>,
    service: Option<Arc<dyn rsi::application_services::ServingService>>,
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
            #[cfg(unix)]
            let mut interrupt =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .map_err(|_| rsi_acp::Error::Closed)?;
            #[cfg(unix)]
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .map_err(|_| rsi_acp::Error::Closed)?;
            #[cfg(windows)]
            let mut interrupt =
                tokio::signal::windows::ctrl_c().map_err(|_| rsi_acp::Error::Closed)?;
            #[cfg(windows)]
            let mut terminate =
                tokio::signal::windows::ctrl_break().map_err(|_| rsi_acp::Error::Closed)?;
            let transport = Arc::new(stdio::create().map_err(|error| {
                let _ = writeln!(
                    std::io::stderr(),
                    "ACP stdio admission failed: {:?} {:?}",
                    error.kind(),
                    error.raw_os_error()
                );
                rsi_acp::Error::Closed
            })?);
            let work = async {
                match self.attachment.as_ref() {
                    AcpAttachment::Backend(backend) => {
                        run(Peer::start(transport), backend.clone(), self.stop.clone()).await
                    }
                    AcpAttachment::Remote(remote) => {
                        proxy(transport, remote.clone(), self.stop.clone()).await
                    }
                }
            };
            tokio::pin!(work);
            tokio::select! { biased;
                result = &mut work => result,
                _ = interrupt.recv() => { self.stop.cancel(); work.await },
                _ = terminate.recv() => { self.stop.cancel(); work.await },
                _ = async { match &self.service {
                    Some(service) => service.stopped().await,
                    None => std::future::pending().await,
                } } => { self.stop.cancel(); let _ = work.await; Err(rsi_acp::Error::Closed) },
            }
        }
        .await;
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

async fn proxy(
    stdio: Arc<dyn Transport>,
    remote: Arc<dyn Transport>,
    stop: CancellationToken,
) -> Result<(), rsi_acp::Error> {
    async fn forward(source: &dyn Transport, target: &dyn Transport) -> Result<(), rsi_acp::Error> {
        loop {
            let bytes = source.read().await?;
            if bytes.is_empty() {
                return Ok(());
            }
            if bytes.len() > 65536 {
                return Err(rsi_acp::Error::Closed);
            }
            target.write(&bytes).await?;
            target.flush().await?;
        }
    }
    let result = tokio::select! { biased;
        () = stop.cancelled() => Ok(()),
        result = forward(stdio.as_ref(), remote.as_ref()) => result,
        result = forward(remote.as_ref(), stdio.as_ref()) => result,
    };
    let (input, output) = tokio::join!(stdio.close(), remote.close());
    result.and(input).and(output)
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    #[tokio::test]
    async fn stdio_proxy_preserves_both_directions_and_closes_only_its_streams() {
        let (local, mut editor) = tokio::io::duplex(256);
        let (remote, mut host) = tokio::io::duplex(256);
        let (read, write) = tokio::io::split(local);
        let local = rsi_acp::StreamTransport::new(read, write);
        let (read, write) = tokio::io::split(remote);
        let remote = rsi_acp::StreamTransport::new(read, write);
        let stop = CancellationToken::new();
        let task = tokio::spawn(proxy(local, remote, stop.clone()));
        editor.write_all(b"{\"id\":1}\n").await.unwrap();
        let mut request = [0; 9];
        host.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"{\"id\":1}\n");
        let reply = b"{\"id\":1,\"result\":{}}\n";
        host.write_all(reply).await.unwrap();
        let mut response = vec![0; reply.len()];
        editor.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, reply);
        stop.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(host.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(editor.read(&mut [0]).await.unwrap(), 0);
    }
}
