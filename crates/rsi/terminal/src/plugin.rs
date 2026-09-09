use crate::{Command, RsiError, SessionCommand};
#[cfg(test)]
mod tests;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_application::{ApplicationError, ApplicationRun, ApplicationRunContract};
use rsi_client::ConnectionLifetimeContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, MetaError, PluginFactory, PreparedActivation,
};
use rsi_session_protocol::SessionContract;
use rsi_workspace_protocol::WorkspaceRegistryContract;
use std::{
    ffi::OsString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::oneshot;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

static TERMINAL_IN_USE: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct TerminalLease;
impl TerminalLease {
    fn acquire() -> rsi_meta::Result<Self> {
        TERMINAL_IN_USE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                MetaError::Activation(
                    "the process terminal already has an application owner".into(),
                )
            })?;
        Ok(Self)
    }
}
impl Drop for TerminalLease {
    fn drop(&mut self) {
        TERMINAL_IN_USE.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Inspector,
    Devices,
    Cli,
    Headless,
    Tui,
}
#[derive(Debug)]
enum Prepared {
    Inspector(crate::inspector::Command),
    Devices(crate::devices::Command),
    Cli(SessionCommand),
    Headless(Command),
    Tui(SessionCommand),
}

#[derive(Debug)]
struct Factory {
    kind: Kind,
    arguments: Vec<OsString>,
    diagnostic: Mutex<Option<RsiError>>,
}
impl Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let result = self.prepare_inner(config);
        if let Err(error) = &result {
            let mut message = error.to_string();
            if message.len() > 4096 {
                let mut end = 4096;
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
            }
            *self
                .diagnostic
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(RsiError::Boot(message));
        }
        result
    }
    fn prepare_inner(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "terminal application configuration must be null".into(),
            ));
        }
        let bytes = self
            .arguments
            .iter()
            .map(|arg| arg.as_encoded_bytes().len())
            .sum::<usize>();
        if self.arguments.len() > 2048 || bytes > crate::MAXIMUM_TURN_TEXT_BYTES + 64 * 1024 {
            return Err(MetaError::InvalidInput(
                "terminal arguments exceed their count or byte bound".into(),
            ));
        }
        let state = match self.kind {
            Kind::Inspector => {
                crate::inspector::Command::parse(&self.arguments).map(Prepared::Inspector)
            }
            Kind::Devices => crate::devices::Command::parse(&self.arguments).map(Prepared::Devices),
            Kind::Cli => SessionCommand::parse(self.arguments.clone()).map(Prepared::Cli),
            Kind::Headless => Command::parse(self.arguments.clone()).map(Prepared::Headless),
            Kind::Tui => crate::tui::parse(self.arguments.clone()).map(Prepared::Tui),
        }
        .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let prepared = PreparedActivation::with_state(config.clone(), state, bytes + 64 * 1024);
        let session = |prepared: PreparedActivation| {
            prepared
                .requiring_local::<SessionContract>()
                .requiring_local::<WorkspaceRegistryContract>()
        };
        Ok(match self.kind {
            Kind::Inspector | Kind::Devices => {
                prepared.requiring_local::<rsi_api_protocol::ApiClientContract>()
            }
            Kind::Headless => {
                session(prepared).requiring_local::<rsi_media_protocol::MediaContract>()
            }
            Kind::Cli => {
                session(prepared).requiring_local::<rsi_process::ProcessOutputCacheContract>()
            }
            Kind::Tui => session(prepared)
                .requiring_local::<rsi_ui::UiContract>()
                .requiring_local::<rsi_ui::UiTargetContract>()
                .requiring_local::<rsi_process::ProcessOutputCacheContract>()
                .requiring_local::<rsi_ai_protocol::LanguageModelsContract>()
                .requiring_local::<ConnectionLifetimeContract>(),
        })
    }
    fn activate(mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = TerminalLease::acquire()?;
        let state = plan.take_state::<Prepared>()?;
        let stop = CancellationToken::new();
        let tasks = TaskTracker::new();
        let retiring = crate::work::ApplicationWork {
            stop: stop.clone(),
            tasks: tasks.clone(),
        };
        let run: BoxFuture<'static, u8> = match state {
            Prepared::Inspector(command) => Box::pin(crate::inspector::run(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
                command,
                retiring,
            )),
            Prepared::Devices(command) => Box::pin(crate::devices::run(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
                command,
                retiring,
            )),
            Prepared::Headless(command) => {
                let session = plan.local::<SessionContract>()?;
                let workspace = plan.local::<WorkspaceRegistryContract>()?;
                let media = plan.local::<rsi_media_protocol::MediaContract>()?;
                Box::pin(crate::run_headless_application(
                    session, workspace, media, command, retiring,
                ))
            }
            Prepared::Cli(command) => {
                let session = plan.local::<SessionContract>()?;
                let workspace = plan.local::<WorkspaceRegistryContract>()?;
                let output = plan.local::<rsi_process::ProcessOutputCacheContract>()?;
                Box::pin(crate::session_cli::run_session_application(
                    session,
                    output,
                    workspace,
                    command,
                    retiring,
                    plan.context().clone(),
                ))
            }
            Prepared::Tui(command) => {
                let session = plan.local::<SessionContract>()?;
                let workspace = plan.local::<WorkspaceRegistryContract>()?;
                let output = plan.local::<rsi_process::ProcessOutputCacheContract>()?;
                let models = plan.local::<rsi_ai_protocol::LanguageModelsContract>()?;
                let lifetime = *plan.local::<ConnectionLifetimeContract>()?;
                Box::pin(crate::tui::run(
                    crate::tui::Services {
                        ui: plan.local::<rsi_ui::UiContract>()?,
                        ui_target: plan.local::<rsi_ui::UiTargetContract>()?,
                        application: session,
                        output_cache: output,
                        model_catalog: models,
                        workspace,
                        lifetime,
                    },
                    command,
                    retiring,
                    plan.context().clone(),
                ))
            }
        };
        let runner = Arc::new(Runner {
            run: Mutex::new(Some(run)),
            stop,
            tasks,
            execution: plan.context().runtime().execution().clone(),
            lease: Mutex::new(Some(lease)),
        });
        let supply = plan
            .context()
            .provide_local::<ApplicationRunContract>(runner.clone())?;
        plan.defer(
            "withdraw terminal application",
            Box::new(move || {
                Box::pin(async move {
                    {
                        let mut run = runner.run.lock().expect("terminal entry poisoned");
                        runner.stop.cancel();
                        run.take();
                        runner.tasks.close();
                    }
                    drop(supply);
                    runner.tasks.wait().await;
                    runner.lease.lock().expect("terminal lease poisoned").take();
                    Ok(())
                })
            }),
        )
    }
}

struct Runner {
    run: Mutex<Option<BoxFuture<'static, u8>>>,
    stop: CancellationToken,
    tasks: TaskTracker,
    execution: Execution,
    lease: Mutex<Option<TerminalLease>>,
}
impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalApplication")
            .finish_non_exhaustive()
    }
}
impl ApplicationRun for Runner {
    fn run(self: Arc<Self>) -> BoxFuture<'static, rsi_application::Result<u8>> {
        let mut pending = self.run.lock().expect("terminal entry poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(ApplicationError::ShuttingDown) });
        }
        let Some(run) = pending.take() else {
            return Box::pin(async { Err(ApplicationError::AlreadyStarted) });
        };
        let (sender, receiver) = oneshot::channel();
        let work = self.tasks.track_future(async move {
            let _ = sender.send(run.await);
        });
        drop(pending);
        drop(self.execution.spawn(work));
        Box::pin(async move { receiver.await.map_err(|_| ApplicationError::TaskStopped) })
    }
}

macro_rules! factory {
    ($name:ident, $kind:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug)]
        pub struct $name(Factory);
        impl $name {
            /// Freezes process arguments without preparing or starting an application.
            pub fn new(arguments: Vec<OsString>) -> Self {
                Self(Factory {
                    kind: Kind::$kind,
                    arguments,
                    diagnostic: Mutex::new(None),
                })
            }
            /// Takes this owner's bounded argument diagnostic after failed preparation.
            pub fn take_diagnostic(&self) -> Option<RsiError> {
                self.0
                    .diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            }
        }
        #[async_trait]
        impl PluginFactory for $name {
            fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
                self.0.prepare(config)
            }
            async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
                Factory::activate(plan)
            }
        }
    };
}
factory!(
    DevicesFactory,
    Devices,
    "Ordinary Session-independent device administration terminal application."
);
factory!(
    CliFactory,
    Cli,
    "Ordinary line-oriented terminal application factory."
);
factory!(
    HeadlessFactory,
    Headless,
    "Ordinary single-submission terminal application factory."
);
factory!(
    TuiFactory,
    Tui,
    "Ordinary fullscreen terminal application factory."
);

factory!(
    InspectorFactory,
    Inspector,
    "Ordinary Session-independent local Inspector application."
);
