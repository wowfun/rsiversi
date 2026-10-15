#[cfg(unix)]
#[path = "stdio_unix.rs"]
mod stdio;
#[cfg(windows)]
#[path = "stdio_windows.rs"]
mod stdio;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_acp::{
    Peer,
    server::{AgentBackend, run},
};
use rsi_acp_agent::AgentBackendContract;
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
            #[cfg(unix)]
            let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|_| rsi_acp::Error::Closed)?;
            #[cfg(unix)]
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|_| rsi_acp::Error::Closed)?;
            #[cfg(windows)]
            let mut interrupt = tokio::signal::windows::ctrl_c().map_err(|_| rsi_acp::Error::Closed)?;
            #[cfg(windows)]
            let mut terminate = tokio::signal::windows::ctrl_break().map_err(|_| rsi_acp::Error::Closed)?;
            let transport = Arc::new(stdio::create().map_err(|error| {
                let _ = writeln!(std::io::stderr(), "ACP stdio admission failed: {:?} {:?}", error.kind(), error.raw_os_error());
                rsi_acp::Error::Closed
            })?);
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
