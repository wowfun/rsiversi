use crate::{projection::short, renderer::RendererFactory};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_protocol::{ApiError, ByteBudget, RetainedBytes};
use rsi_application::{Shell, ShellContract, ShellFactory};
use rsi_client::{ObservationSinkContract, SessionControllerContract, SessionControllerFactory};
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, FiberState, LocalContract, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, UpdateMode,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) type Result<T> = std::result::Result<T, String>;
pub(crate) fn error(value: impl std::fmt::Display) -> String {
    short(&value.to_string(), 4096).into()
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    UiSurface {
        pane: u8,
        generation: String,
        reference: rsi_ui::UiReference,
    },
    UiBlock {
        pane: u8,
        generation: String,
        key: String,
    },
    UiInvoke {
        ticket: String,
        reference: rsi_ui::UiReference,
        input: rsi_ui::ActionInput,
    },
    Refresh,
    WorkspacesNext,
    SessionsNext,
    ModelsNext,
    RegisterWorkspace {
        path: String,
    },
    Open {
        pane: u8,
        session: rsi_agent_session_protocol::SessionId,
    },
    Create {
        pane: u8,
        workspace: rsi_workspace_protocol::WorkspaceId,
        trust: bool,
    },
    Draft {
        pane: u8,
        generation: String,
        text: String,
    },
    ImageEdit {
        pane: u8,
        generation: String,
        revision: String,
        from: u8,
        to: Option<u8>,
    },
    InspectImage {
        pane: u8,
        generation: String,
        index: u8,
        media: rsi_media_protocol::MediaRef,
    },
    Model {
        pane: u8,
        generation: String,
        model: rsi_ai_protocol::ModelRef,
    },
    Submit {
        pane: u8,
        generation: String,
        text: String,
        steer: bool,
    },
    Cancel {
        pane: u8,
        generation: String,
    },
    History {
        pane: u8,
        generation: String,
    },
    Commands {
        pane: u8,
        generation: String,
    },
    RefreshCommandResult {
        pane: u8,
        generation: String,
    },
    Live {
        pane: u8,
        generation: String,
    },
    InspectSource {
        pane: u8,
        generation: String,
        source: rsi_conversation::SourceRef,
    },
    InspectBlock {
        pane: u8,
        generation: String,
        key: String,
    },
    BlockSourcesPage {
        ticket: String,
        forward: bool,
    },
    SourcePage {
        ticket: String,
        forward: bool,
    },
    InspectInteraction {
        pane: u8,
        generation: String,
        owner: String,
        id: String,
    },
    Answer {
        pane: u8,
        generation: String,
        id: String,
        answers: Vec<String>,
    },
    Approve {
        pane: u8,
        generation: String,
        owner: rsi_agent_session_protocol::SessionId,
        id: String,
        allow: bool,
    },
    SettingsRead {
        namespace: String,
    },
    SettingsList,
    SettingsNext {
        ticket: String,
    },
    SettingsSave {
        ticket: String,
        text: String,
    },
    CloseDetail,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Catalog {
    pub workspaces: Vec<rsi_workspace_protocol::WorkspaceRecord>,
    pub workspaces_more: bool,
    pub sessions: Vec<Recent>,
    pub sessions_more: bool,
    pub models: Vec<rsi_ai_protocol::ModelRef>,
    pub models_more: bool,
    #[serde(skip)]
    pub workspace_cursor: Option<rsi_workspace_protocol::WorkspaceCursor>,
    #[serde(skip)]
    pub session_cursor: Option<rsi_session_protocol::RecentSessionCursor>,
}
#[derive(Debug, Serialize)]
pub(crate) struct Recent {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsEditor {
    pub namespace: String,
    pub text: String,
    pub ticket: String,
    pub description: rsi_settings_protocol::SettingsDescription,
    #[serde(skip)]
    pub version: rsi_settings_protocol::SettingsVersion,
}

/// Ordinary Web application handle; its plugin retains all admitted command work.
#[derive(Debug)]
pub struct WebApplication {
    pub(crate) ui: Arc<rsi_ui::Ui>,
    pub(crate) session: Arc<dyn rsi_session_protocol::SessionService>,
    pub(crate) workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    pub(crate) models: Arc<dyn rsi_ai_protocol::LanguageModels>,
    pub(crate) settings: Arc<dyn rsi_settings_protocol::SettingsAccess>,
    pub(crate) preferences: rsi_client_preferences::Composer,
    pub(crate) media: Option<Arc<dyn rsi_media_protocol::Media>>,
    pub(crate) image_work: Arc<Semaphore>,
    pub(crate) panes: [Arc<crate::panes::Pane>; 2],
    pub(crate) shell: Arc<Shell>,
    pub(crate) has_files: bool,
    pub(crate) catalog: Mutex<Catalog>,
    pub(crate) catalog_work: tokio::sync::Mutex<()>,
    pub(crate) details: Mutex<crate::details::Details>,
    pub(crate) notice: Mutex<String>,
    pub(crate) execution: Execution,
    changed: watch::Sender<u64>,
    slots: Arc<Semaphore>,
    tasks: TaskTracker,
    stop: CancellationToken,
    frames: ByteBudget,
    admission: Mutex<()>,
}
impl WebApplication {
    /// Admits a bounded closed command before returning its response waiter.
    /// Dropping that waiter cannot replay a mutation or detach command ownership.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn command(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<()>> {
        if source.len() > 1024 * 1024 + 4096 {
            return Box::pin(async { Err("Web command exceeds 1 MiB".into()) });
        }
        let command: Command = match serde_json::from_str(source) {
            Ok(command) => command,
            Err(_) => return Box::pin(async { Err("Invalid Web command".into()) }),
        };
        self.admit(true, move |app| async move { app.execute(command).await })
    }
    pub(crate) fn admit<T, F, Fut>(
        self: &Arc<Self>,
        report_error: bool,
        operation: F,
    ) -> BoxFuture<'static, Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        let admission = self.admission.lock().expect("Web admission poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err("Web application is closed".into()) });
        }
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            return Box::pin(async { Err("Web application is busy".into()) });
        };
        let app = self.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            let result = tokio::select! { biased;
                () = app.stop.cancelled() => Err("Application closed; an admitted remote operation may still complete".into()),
                result = operation(app.clone()) => result,
            };
            if report_error && let Err(error) = &result { app.notice.lock().expect("Web notice poisoned").clone_from(error); }
            app.changed();
            result
        }));
        drop(admission);
        Box::pin(async move { task.await.map_err(error)? })
    }
    pub(crate) fn changed(&self) {
        self.changed
            .send_modify(|value| *value = value.saturating_add(1));
    }
    /// Subscribes to coalesced projection changes; this is independent of domain cursors.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn closed(&self) {
        self.stop.cancelled().await;
    }
    /// Encodes one immutable UI view under its independent 32 MiB frame reservation.
    ///
    /// # Panics
    /// Panics if an earlier application or renderer panic poisoned its state.
    pub fn view(&self) -> rsi_api_protocol::Result<RetainedBytes> {
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let reservation = self.frames.reserve(32 * 1024 * 1024)?;
        let panes = self
            .panes
            .iter()
            .map(|pane| pane.view(&self.ui))
            .collect::<Vec<_>>();
        let details = self.details.lock().expect("Web details poisoned");
        reservation.encode(&serde_json::json!({
            "panes": panes, "catalog": *self.catalog.lock().expect("Web catalog poisoned"),
            "preferences": self.preferences,
            "ui_detail": details.ui,
            "image_detail": details.image,
            "source_media": details.source.as_ref().and_then(crate::details::SourceDetail::media),
            "media_limits": crate::panes::images::limits(),
            "has_media": self.media.is_some(),
            "settings": details.editor,
            "settings_catalog": details.settings_catalog,
            "detail": details.interaction,
            "source_detail": details.source,
            "block_sources": details.block_sources,
            "notice": *self.notice.lock().expect("Web notice poisoned"),
        }))
    }
    async fn execute(&self, command: Command) -> Result<()> {
        match command {
            Command::UiSurface {
                pane,
                generation,
                reference,
            } => self.ui_surface(pane, &generation, &reference),
            Command::UiBlock {
                pane,
                generation,
                key,
            } => self.ui_block(pane, &generation, &key),
            Command::UiInvoke {
                ticket,
                reference,
                input,
            } => self.ui_invoke(&ticket, reference, input).await,
            Command::Refresh
            | Command::WorkspacesNext
            | Command::SessionsNext
            | Command::ModelsNext => self.refresh(command).await,
            Command::RegisterWorkspace { path } => {
                if path.len() > 16 * 1024 {
                    return Err("Workspace path exceeds its limit".into());
                }
                self.workspace
                    .get_or_create(std::path::Path::new(&path))
                    .await
                    .map_err(error)?;
                self.refresh(Command::Refresh).await
            }
            Command::SettingsRead { namespace } => self.read_settings(&namespace).await,
            Command::SettingsList => self.list_settings(None).await,
            Command::SettingsNext { ticket } => self.list_settings(Some(&ticket)).await,
            Command::SettingsSave { ticket, text } => self.save_settings(&ticket, &text).await,
            Command::CloseDetail => {
                self.details.lock().expect("Web details poisoned").begin()?;
                Ok(())
            }
            command => self.pane_command(command).await,
        }
    }
}

/// Nominal application capability used by the Worker input/rendering bridge.
#[derive(Debug)]
pub struct WebApplicationContract;
impl LocalContract for WebApplicationContract {
    const KEY: &'static str = "rsi.web.application";
    type Service = WebApplication;
}

/// Ordinary coding-workspace application over independently supplied domains.
#[derive(Clone, Debug, Default)]
pub struct WebApplicationFactory;
#[async_trait]
impl PluginFactory for WebApplicationFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Web application configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<rsi_ui::UiContract>()
            .requiring_local::<rsi_session_protocol::SessionContract>()
            .requiring_local::<rsi_workspace_protocol::WorkspaceRegistryContract>()
            .requiring_local::<rsi_ai_protocol::LanguageModelsContract>()
            .requiring_local::<rsi_settings_protocol::SettingsAccessContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (changed, _) = watch::channel(0_u64);
        let settings = plan.local::<rsi_settings_protocol::SettingsAccessContract>()?;
        let preferences = rsi_client_preferences::Preferences::load(settings.as_ref())
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let has_files = plan
            .context()
            .lookup_local::<rsi_session_files::SessionFilesContract>()
            .is_some();
        let shell = start_shell(plan.context(), changed.clone(), has_files).await?;
        let app = Arc::new(WebApplication {
            ui: plan.local::<rsi_ui::UiContract>()?,
            session: plan.local::<rsi_session_protocol::SessionContract>()?,
            workspace: plan.local::<rsi_workspace_protocol::WorkspaceRegistryContract>()?,
            models: plan.local::<rsi_ai_protocol::LanguageModelsContract>()?,
            settings,
            preferences: preferences.web,
            media: plan
                .context()
                .lookup_local::<rsi_media_protocol::MediaContract>(),
            image_work: Arc::new(Semaphore::new(1)),
            panes: std::array::from_fn(|_| Arc::new(crate::panes::Pane::default())),
            shell,
            has_files,
            catalog: Mutex::new(Catalog::default()),
            catalog_work: tokio::sync::Mutex::new(()),
            details: Mutex::new(crate::details::Details::default()),
            notice: Mutex::new(String::new()),
            execution: plan.context().runtime().execution().clone(),
            changed,
            slots: Arc::new(Semaphore::new(8)),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            frames: ByteBudget::new(32 * 1024 * 1024)
                .map_err(|error| MetaError::Activation(error.to_string()))?,
            admission: Mutex::new(()),
        });
        let mut ui_changes = app.ui.changes();
        let watching = app.clone();
        drop(app.execution.spawn(app.tasks.track_future(async move {
            loop {
                tokio::select! { biased;
                    () = watching.stop.cancelled() => break,
                    result = ui_changes.changed() => {
                        if result.is_err() { break; }
                        watching.prune_ui();
                        watching.changed();
                    }
                }
            }
        })));
        let supply = plan
            .context()
            .provide_local::<WebApplicationContract>(app.clone())?;
        plan.defer(
            "withdraw Web application and drain commands",
            Box::new(move || {
                Box::pin(async move {
                    {
                        let _admission = app.admission.lock().expect("Web admission poisoned");
                        app.stop.cancel();
                        app.details
                            .lock()
                            .expect("Web details poisoned")
                            .stop
                            .cancel();
                        app.slots.close();
                        app.tasks.close();
                    }
                    drop(supply);
                    app.tasks.wait().await;
                    Ok(())
                })
            }),
        )
    }
}

async fn start_shell(
    parent: &rsi_meta::Context,
    changed: watch::Sender<u64>,
    has_files: bool,
) -> rsi_meta::Result<Arc<Shell>> {
    let mut catalog = HostBuilder::without_paths("browser");
    if has_files {
        catalog
            .register_local_contract::<rsi_session_files_ui::FilesBrowserContract>()
            .map_err(meta)?;
        catalog
            .register_linked(
                "rsi.session.files.ui-target",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(rsi_session_files_ui::FilesUiTargetFactory),
            )
            .map_err(meta)?;
    }
    catalog
        .register_local_contract::<ObservationSinkContract>()
        .map_err(meta)?;
    catalog
        .register_local_contract::<SessionControllerContract>()
        .map_err(meta)?;
    catalog
        .register_local_contract::<crate::renderer::RendererContract>()
        .map_err(meta)?;
    catalog
        .register_linked(
            "rsi.web.renderer",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(RendererFactory { changed }),
        )
        .map_err(meta)?;
    catalog
        .register_linked(
            "rsi.client.session-controller",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(SessionControllerFactory),
        )
        .map_err(meta)?;
    catalog
        .register_local_contract::<rsi_ui::UiTargetContract>()
        .map_err(meta)?;
    catalog
        .register_linked(
            "rsi.session.ui-target",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_session_ui::SessionUiTargetFactory),
        )
        .map_err(meta)?;
    let parent = parent.clone().isolate_local_fresh::<ShellContract>()?.0;
    let fiber = parent
        .apply(
            ResolvedFactory::linked(
                "rsi.web.shell",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(ShellFactory::new(catalog.build().map_err(meta)?)),
            ),
            serde_json::json!({"maximum_surfaces":4}),
        )
        .await?;
    if fiber.snapshot().state != FiberState::Active {
        let report = fiber.dispose().await;
        return Err(MetaError::Activation(format!(
            "Web Shell failed; {} cleanup failures",
            report.total_failures()
        )));
    }
    parent
        .lookup_local::<ShellContract>()
        .ok_or_else(|| MetaError::Activation("Web Shell was not published".into()))
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

pub(crate) fn surface_program(
    pane: u8,
    generation: u64,
    session: &rsi_agent_session_protocol::SessionId,
    cursor: Option<rsi_agent_turn_protocol::ObservationCursor>,
    has_files: bool,
) -> ProfileProgram {
    let mut entries = vec![
        ProfileEntry::new(
            "renderer",
            "rsi.web.renderer",
            serde_json::json!({"pane":pane,"generation":generation}),
        ),
        ProfileEntry::new(
            "controller",
            "rsi.client.session-controller",
            serde_json::json!({"session_id":session,"cursor":cursor}),
        ),
        ProfileEntry::new("ui-target", "rsi.session.ui-target", ConfigValue::Null),
    ];
    if has_files {
        entries.push(ProfileEntry::new(
            "files-ui-target",
            "rsi.session.files.ui-target",
            ConfigValue::Null,
        ));
    }
    ProfileProgram::from_profile(Profile::new(entries))
}
