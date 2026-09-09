use crate::{CliRenderMessage, Result, RsiError, session_cli::CliObservationSink};
use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_agent_turn_protocol::ObservationCursor;
use rsi_application::{Shell, ShellContract, ShellFactory, Surface};
use rsi_client::{
    ObservationSinkContract, SessionController, SessionControllerContract, SessionControllerFactory,
};
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberHandle, FiberState, LocalContract, MetaError,
    PluginFactory, PreparedActivation, ResolvedFactory, UpdateMode,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Run(error.to_string())
}

#[derive(Debug)]
struct TerminalObservation;
impl LocalContract for TerminalObservation {
    const KEY: &'static str = "rsi.terminal.observation";
    type Service = CliObservationSink;
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderConfig {
    session_id: SessionId,
    generation: u64,
}

#[derive(Debug)]
struct RendererFactory(mpsc::WeakSender<CliRenderMessage>);
#[async_trait]
impl PluginFactory for RendererFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: RenderConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = config.session_id.as_str().len() + 8;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            bytes,
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<RenderConfig>()?;
        let sink = Arc::new(CliObservationSink {
            session: Some(config.session_id),
            generation: Some(config.generation),
            renderer: self
                .0
                .upgrade()
                .ok_or_else(|| MetaError::Activation("terminal renderer stopped".into()))?,
            stop: CancellationToken::new(),
            finished: CancellationToken::new(),
        });
        let observation = plan
            .context()
            .provide_local::<ObservationSinkContract>(sink.clone())?;
        plan.defer(
            "withdraw terminal observation sink",
            Box::new(move || {
                Box::pin(async move {
                    drop(observation);
                    Ok(())
                })
            }),
        )?;
        let status = plan
            .context()
            .provide_local::<TerminalObservation>(sink.clone())?;
        plan.defer(
            "stop terminal observation delivery",
            Box::new(move || {
                Box::pin(async move {
                    sink.stop.cancel();
                    drop(status);
                    Ok(())
                })
            }),
        )
    }
}

/// Handle only: the ordinary Shell plugin owns openings, surfaces and cleanup.
#[derive(Debug)]
pub(crate) struct TerminalSurfaces {
    shell: Arc<Shell>,
    fiber: FiberHandle,
    has_ui: bool,
    has_files: bool,
}
impl TerminalSurfaces {
    pub async fn start(
        parent: &Context,
        renderer: &mpsc::Sender<CliRenderMessage>,
    ) -> Result<Self> {
        let mut catalog = HostBuilder::without_paths(std::env::consts::OS);
        catalog
            .register_local_contract::<ObservationSinkContract>()
            .map_err(error)?;
        catalog
            .register_local_contract::<SessionControllerContract>()
            .map_err(error)?;
        catalog
            .register_local_contract::<TerminalObservation>()
            .map_err(error)?;
        catalog
            .register_linked(
                "rsi.terminal.renderer",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(RendererFactory(renderer.downgrade())),
            )
            .map_err(error)?;
        catalog
            .register_linked(
                "rsi.client.session-controller",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(SessionControllerFactory),
            )
            .map_err(error)?;
        let has_ui = parent.lookup_local::<rsi_ui::UiContract>().is_some();
        let has_files = has_ui
            && parent
                .lookup_local::<rsi_session_files::SessionFilesContract>()
                .is_some();
        if has_files {
            catalog
                .register_local_contract::<rsi_session_files_ui::FilesBrowserContract>()
                .map_err(error)?;
            catalog
                .register_linked(
                    "rsi.session.files.ui-target",
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    Arc::new(rsi_session_files_ui::FilesUiTargetFactory),
                )
                .map_err(error)?;
        }
        if has_ui {
            catalog
                .register_local_contract::<rsi_ui::UiTargetContract>()
                .map_err(error)?;
            catalog
                .register_linked(
                    "rsi.session.ui-target",
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    Arc::new(rsi_session_ui::SessionUiTargetFactory),
                )
                .map_err(error)?;
        }
        let parent = parent
            .clone()
            .isolate_local_fresh::<ShellContract>()
            .map_err(error)?
            .0;
        let fiber = parent
            .apply(
                ResolvedFactory::linked(
                    "rsi.terminal.shell",
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    Arc::new(ShellFactory::new(catalog.build().map_err(error)?)),
                ),
                serde_json::json!({"maximum_surfaces": 2}),
            )
            .await
            .map_err(error)?;
        if fiber.snapshot().state != FiberState::Active {
            let report = fiber.dispose().await;
            return Err(error(format!(
                "terminal Shell activation failed; {} cleanup failures",
                report.total_failures()
            )));
        }
        let shell = parent
            .lookup_local::<ShellContract>()
            .ok_or_else(|| error("terminal Shell is unavailable"))?;
        Ok(Self {
            shell,
            fiber,
            has_ui,
            has_files,
        })
    }

    pub async fn open(
        &self,
        session_id: &SessionId,
        cursor: Option<ObservationCursor>,
        generation: u64,
    ) -> Result<Observer> {
        let mut entries = vec![
            ProfileEntry::new(
                "renderer",
                "rsi.terminal.renderer",
                serde_json::json!({"session_id": session_id, "generation": generation}),
            ),
            ProfileEntry::new(
                "controller",
                "rsi.client.session-controller",
                serde_json::json!({"session_id": session_id, "cursor": cursor}),
            ),
        ];
        if self.has_ui {
            entries.push(ProfileEntry::new(
                "ui-target",
                "rsi.session.ui-target",
                ConfigValue::Null,
            ));
        }
        if self.has_files {
            entries.push(ProfileEntry::new(
                "files-ui-target",
                "rsi.session.files.ui-target",
                ConfigValue::Null,
            ));
        }
        let program = ProfileProgram::from_profile(Profile::new(entries));
        let surface = self.shell.open(program).await.map_err(error)?;
        let controller = surface
            .lookup_local::<SessionControllerContract>()
            .ok_or_else(|| error("Session controller is unavailable"))?;
        let sink = surface
            .lookup_local::<TerminalObservation>()
            .ok_or_else(|| error("terminal observation sink is unavailable"))?;
        let ui_target = surface.lookup_local::<rsi_ui::UiTargetContract>();
        Ok(Observer {
            ui_target,
            surface,
            controller,
            finished: sink.finished.clone(),
        })
    }

    pub async fn close(self) -> Result<()> {
        let report = self.fiber.dispose().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(error(format!(
                "terminal Shell cleanup reported {} failures",
                report.total_failures()
            )))
        }
    }
}

#[derive(Debug)]
pub(crate) struct Observer {
    surface: Surface,
    pub controller: Arc<SessionController>,
    pub ui_target: Option<Arc<rsi_ui::UiTarget>>,
    pub finished: CancellationToken,
}
impl Observer {
    pub async fn stop(self) -> Result<()> {
        let report = self.surface.close().await.map_err(error)?;
        if report.is_clean() {
            Ok(())
        } else {
            Err(error(format!(
                "terminal surface cleanup reported {} failures",
                report.total_failures()
            )))
        }
    }
}

#[cfg(test)]
pub(crate) async fn fixture(
    handle: Arc<dyn rsi_session_protocol::SessionHandle>,
    renderer: &mpsc::Sender<CliRenderMessage>,
    ui: bool,
) -> (rsi_meta::Runtime, TerminalSurfaces) {
    #[derive(Debug)]
    struct Domain(Arc<dyn rsi_session_protocol::SessionHandle>);
    #[async_trait]
    impl rsi_session_protocol::SessionService for Domain {
        async fn create(
            &self,
            _: rsi_session_protocol::CreateSession,
        ) -> rsi_session_protocol::Result<Arc<dyn rsi_session_protocol::SessionHandle>> {
            Ok(self.0.clone())
        }
        async fn attach(
            &self,
            _: &SessionId,
        ) -> rsi_session_protocol::Result<Arc<dyn rsi_session_protocol::SessionHandle>> {
            Ok(self.0.clone())
        }
        async fn list_recent(
            &self,
            _: Option<&rsi_session_protocol::RecentSessionCursor>,
            _: usize,
        ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
            Err(rsi_session_protocol::SessionError::NotFound(
                "fixture list".into(),
            ))
        }
    }
    #[async_trait]
    impl PluginFactory for Domain {
        fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(ConfigValue::Null))
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            let supply = plan
                .context()
                .provide_local::<rsi_session_protocol::SessionContract>(Arc::new(Self(
                    self.0.clone(),
                )))?;
            plan.defer(
                "withdraw fixture Session",
                Box::new(move || {
                    Box::pin(async move {
                        drop(supply);
                        Ok(())
                    })
                }),
            )
        }
    }
    let runtime = rsi_meta::Runtime::default();
    let domain = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture-session",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(Domain(handle)),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert_eq!(domain.snapshot().state, FiberState::Active);
    if ui {
        for (name, factory, config) in [
            (
                "ui",
                Arc::new(rsi_ui::UiFactory) as Arc<dyn PluginFactory>,
                ConfigValue::Null,
            ),
            (
                "ui-application",
                Arc::new(rsi_ui::UiTargetFactory),
                serde_json::json!("application"),
            ),
            (
                "session-ui",
                Arc::new(rsi_session_ui::SessionUiFactory),
                ConfigValue::Null,
            ),
        ] {
            let fiber = runtime
                .root()
                .apply(
                    ResolvedFactory::linked(name, "test", UpdateMode::RestartRequired, factory),
                    config,
                )
                .await
                .unwrap();
            assert_eq!(fiber.snapshot().state, FiberState::Active);
        }
    }
    let surfaces = TerminalSurfaces::start(&runtime.root(), renderer)
        .await
        .unwrap();
    (runtime, surfaces)
}
