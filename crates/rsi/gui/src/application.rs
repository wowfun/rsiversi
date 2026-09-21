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
/// Bounds an external error for the GUI and its transport adapters.
pub fn display_error(value: impl std::fmt::Display) -> String {
    short(&value.to_string(), 4096).into()
}

pub(crate) use display_error as error;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    DelegationOpen {
        pane: crate::SurfaceId,
        generation: String,
        key: String,
    },
    AttentionOpen {
        pane: crate::SurfaceId,
        position: rsi_navigation_api::attention::Position,
        target: Option<rsi_navigation_api::attention::Target>,
    },
    ExternalCatalog {
        #[serde(default)]
        next: bool,
    },
    ExternalOpen {
        pane: crate::SurfaceId,
        id: rsi_acp_protocol::observation::ConversationId,
    },
    ExternalStart {
        pane: crate::SurfaceId,
        id: rsi_acp_protocol::observation::ConversationId,
        endpoint: String,
    },
    ExternalControl {
        pane: crate::SurfaceId,
        generation: String,
        command: rsi_client::ExternalCommand,
    },
    ExternalSource {
        pane: crate::SurfaceId,
        generation: String,
        source: rsi_conversation::ExternalSource,
        start: usize,
    },
    AddSurface {
        pane: crate::SurfaceId,
    },
    CloseSurface {
        pane: crate::SurfaceId,
    },
    Plugins {
        command: rsi_workbench_ui::PluginsCommand,
    },
    Setup {
        command: rsi_workbench_ui::SetupCommand,
    },
    Navigate {
        command: rsi_workbench_ui::NavigationCommand,
    },
    RemoteUiList {
        pane: crate::SurfaceId,
        generation: String,
    },
    RemoteUiNext {
        ticket: String,
    },
    RemoteUiSurface {
        ticket: String,
        bundle: String,
        surface: String,
    },
    ApplicationUiSurface {
        reference: rsi_ui::UiReference,
    },
    UiSurface {
        pane: crate::SurfaceId,
        generation: String,
        reference: rsi_ui::UiReference,
    },
    UiBlock {
        pane: crate::SurfaceId,
        generation: String,
        key: String,
    },
    UiVisible {
        pane: crate::SurfaceId,
        generation: String,
        sequence: String,
        keys: Vec<String>,
    },
    UiInvoke {
        ticket: String,
        name: String,
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
        pane: crate::SurfaceId,
        session: rsi_agent_session_protocol::SessionId,
    },
    Create {
        pane: crate::SurfaceId,
        workspace: rsi_workspace_protocol::WorkspaceId,
        #[serde(default)]
        reuse: Option<crate::panes::ReuseDraft>,
    },
    InspectImage {
        pane: crate::SurfaceId,
        generation: String,
        media: rsi_media_protocol::MediaRef,
    },
    Model {
        pane: crate::SurfaceId,
        generation: String,
        model: rsi_ai_protocol::ModelRef,
        reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
    },
    ModelRefresh {
        pane: crate::SurfaceId,
        generation: String,
    },
    Cancel {
        pane: crate::SurfaceId,
        generation: String,
    },
    History {
        pane: crate::SurfaceId,
        generation: String,
    },
    Commands {
        pane: crate::SurfaceId,
        generation: String,
    },
    Completions {
        pane: crate::SurfaceId,
        generation: String,
        query: String,
        sequence: String,
        #[serde(default)]
        refresh: bool,
    },
    ResourceRead {
        pane: crate::SurfaceId,
        generation: String,
        request: rsi_agent_session_protocol::SessionResourceRequest,
    },
    ResourceClose {
        pane: crate::SurfaceId,
        generation: String,
    },
    Live {
        pane: crate::SurfaceId,
        generation: String,
    },
    InspectSource {
        pane: crate::SurfaceId,
        generation: String,
        source: rsi_conversation::SourceRef,
    },
    InspectBlock {
        pane: crate::SurfaceId,
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
        pane: crate::SurfaceId,
        generation: String,
        owner: String,
        id: String,
    },
    Answer {
        pane: crate::SurfaceId,
        generation: String,
        id: String,
        answers: Vec<String>,
    },
    Approve {
        pane: crate::SurfaceId,
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

impl Command {
    fn affected_pane(&self) -> Option<crate::SurfaceId> {
        match self {
            Self::Open { pane, .. }
            | Self::DelegationOpen { pane, .. }
            | Self::AttentionOpen { pane, .. }
            | Self::ExternalOpen { pane, .. }
            | Self::ExternalStart { pane, .. }
            | Self::ExternalControl { pane, .. }
            | Self::ExternalSource { pane, .. }
            | Self::Create { pane, .. }
            | Self::Model { pane, .. }
            | Self::ModelRefresh { pane, .. }
            | Self::Cancel { pane, .. }
            | Self::History { pane, .. }
            | Self::Commands { pane, .. }
            | Self::Completions { pane, .. }
            | Self::ResourceRead { pane, .. }
            | Self::ResourceClose { pane, .. }
            | Self::Live { pane, .. }
            | Self::Answer { pane, .. }
            | Self::Approve { pane, .. } => Some(*pane),
            Self::ApplicationUiSurface { .. }
            | Self::ExternalCatalog { .. }
            | Self::UiSurface { .. }
            | Self::AddSurface { .. }
            | Self::CloseSurface { .. }
            | Self::Setup { .. }
            | Self::Plugins { .. }
            | Self::Navigate { .. }
            | Self::RemoteUiList { .. }
            | Self::RemoteUiNext { .. }
            | Self::RemoteUiSurface { .. }
            | Self::UiBlock { .. }
            | Self::UiVisible { .. }
            | Self::UiInvoke { .. }
            | Self::Refresh
            | Self::WorkspacesNext
            | Self::SessionsNext
            | Self::ModelsNext
            | Self::RegisterWorkspace { .. }
            | Self::InspectImage { .. }
            | Self::InspectSource { .. }
            | Self::InspectBlock { .. }
            | Self::BlockSourcesPage { .. }
            | Self::SourcePage { .. }
            | Self::InspectInteraction { .. }
            | Self::SettingsRead { .. }
            | Self::SettingsList
            | Self::SettingsNext { .. }
            | Self::SettingsSave { .. }
            | Self::CloseDetail => None,
        }
    }
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

/// Ordinary shared GUI application handle; its plugin retains all admitted command work.
#[derive(Debug)]
pub struct GuiApplication {
    pub(crate) external: Option<Arc<dyn rsi_acp_protocol::service::ExternalConversations>>,
    pub(crate) external_catalog: Mutex<crate::panes::ExternalCatalog>,
    pub(crate) plugins: Option<Arc<rsi_workbench_ui::PluginsFeature>>,
    pub(crate) setup: Option<Arc<rsi_workbench_ui::SetupFeature>>,
    pub(crate) navigation: Option<Arc<rsi_workbench_ui::NavigationFeature>>,
    pub(crate) ui: Arc<rsi_ui::Ui>,
    pub(crate) application_target: Option<Arc<rsi_ui::UiTarget>>,
    pub(crate) history_search: Option<rsi_history_api::Client>,
    pub(crate) remote_ui: Option<rsi_ui_api::UiClient>,
    pub(crate) session: Arc<dyn rsi_session_protocol::SessionService>,
    pub(crate) workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    pub(crate) models: Arc<dyn rsi_ai_protocol::LanguageModels>,
    pub(crate) settings: Arc<dyn rsi_settings_protocol::SettingsAccess>,
    pub(crate) preferences: rsi_client_preferences::Composer,
    pub(crate) media: Option<Arc<dyn rsi_media_protocol::Media>>,
    pub(crate) image_work: Arc<Semaphore>,
    pub(crate) panes: Mutex<std::collections::BTreeMap<crate::SurfaceId, Arc<crate::panes::Pane>>>,
    pub(crate) attachment_generation: std::sync::atomic::AtomicU64,
    pub(crate) shell: Arc<Shell>,
    pub(crate) has_files: bool,
    pub(crate) catalog: Mutex<Catalog>,
    pub(crate) catalog_work: tokio::sync::Mutex<()>,
    pub(crate) details: Mutex<crate::details::Details>,
    pub(crate) notice: Mutex<String>,
    pub(crate) execution: Execution,
    changed: watch::Sender<u64>,
    pub(crate) slots: Arc<Semaphore>,
    pub(crate) terminal_reads: Arc<Semaphore>,
    pub(crate) terminal_writes: Arc<Semaphore>,
    pub(crate) tasks: TaskTracker,
    pub(crate) stop: CancellationToken,
    frames: ByteBudget,
    stream: Mutex<crate::frames::FrameState>,
    admission: Mutex<()>,
}
impl GuiApplication {
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
        // Plugin operations publish their outcome in the shared workbench. A
        // recovered connection must not leave its previous error as a global banner.
        let global_error = !matches!(command, Command::Plugins { .. });
        self.admit(
            global_error,
            command.affected_pane(),
            move |app| async move { app.execute(command).await },
        )
    }
    pub(crate) fn admit<T, F, Fut>(
        self: &Arc<Self>,
        report_error: bool,
        affected_pane: Option<crate::SurfaceId>,
        operation: F,
    ) -> BoxFuture<'static, Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        match self.try_admit(report_error, affected_pane, operation) {
            Ok(waiter) => waiter,
            Err(error) => Box::pin(async { Err(error) }),
        }
    }
    pub(crate) fn try_admit<T, F, Fut>(
        self: &Arc<Self>,
        report_error: bool,
        affected_pane: Option<crate::SurfaceId>,
        operation: F,
    ) -> Result<BoxFuture<'static, Result<T>>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        self.try_admit_inner(report_error, affected_pane, true, &self.slots, operation)
    }
    pub(crate) fn try_admit_inner<T, F, Fut>(
        self: &Arc<Self>,
        report_error: bool,
        affected_pane: Option<crate::SurfaceId>,
        publish: bool,
        slots: &Arc<Semaphore>,
        operation: F,
    ) -> Result<BoxFuture<'static, Result<T>>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        let admission = self.admission.lock().expect("Web admission poisoned");
        if self.stop.is_cancelled() {
            return Err("Web application is closed".into());
        }
        let permit = slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Web application is busy")?;
        let app = self.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            let result = tokio::select! { biased;
                () = app.stop.cancelled() => Err("Application closed; an admitted remote operation may still complete".into()),
                result = operation(app.clone()) => result,
            };
            if report_error && let Err(error) = &result { app.notice.lock().expect("Web notice poisoned").clone_from(error); }
            if let Some(pane) = affected_pane.and_then(|index| app.panes.lock().expect("GUI surfaces poisoned").get(&index).cloned()) { pane.changed(); }
            if publish { app.changed(); }
            result
        }));
        drop(admission);
        Ok(Box::pin(async move { task.await.map_err(error)? }))
    }
    pub(crate) fn changed(&self) {
        self.changed
            .send_modify(|value| *value = value.saturating_add(1));
    }
    /// Reports non-queued image admission before a platform copies source bytes.
    /// The import itself acquires the authoritative permit.
    pub fn image_import_available(&self) -> bool {
        !self.stop.is_cancelled() && self.image_work.available_permits() > 0
    }
    /// Subscribes to coalesced projection changes; this is independent of domain cursors.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    /// Waits for application withdrawal without owning shutdown authority.
    pub async fn closed(&self) {
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
            .lock()
            .expect("GUI surfaces poisoned")
            .iter()
            .map(|(key, pane)| (key.to_string(), pane.view(&self.ui)))
            .collect::<serde_json::Map<_, _>>();
        let mut view = self.sections();
        view.as_object_mut()
            .expect("view sections")
            .insert("surfaces".into(), panes.into());
        reservation.encode(&view)
    }
    pub(crate) fn sections(&self) -> serde_json::Value {
        let details = self.details.lock().expect("Web details poisoned");
        serde_json::json!({
            "application_surfaces": self.application_target.as_ref().and_then(|target| self.ui.surfaces(target).ok()).unwrap_or_default(),
            "plugins": self.plugins.as_ref().map(|feature| feature.snapshot()),
            "setup": self.setup.as_ref().map(|feature| feature.view()),
            "navigation": self.navigation.as_ref().map(|feature| feature.view()),
            "external_catalog": self.external.as_ref().map(|_|self.external_catalog.lock().expect("external catalog").clone()),
            "catalog": *self.catalog.lock().expect("Web catalog poisoned"),
            "preferences": self.preferences,
            "ui_detail": details.ui,
            "remote_ui_catalog": details.remote_catalog,
            "has_remote_ui": self.remote_ui.is_some(),
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
        })
    }
    /// Encodes one incremental presentation frame against the document's exact base.
    /// An absent or mismatched base requests a full snapshot.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn next_frame(&self, base: Option<&str>) -> rsi_api_protocol::Result<RetainedBytes> {
        let mut stream = self.stream.lock().expect("Web frame stream poisoned");
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        #[cfg(feature = "test-support")]
        let started = {
            crate::test_support::take_frame_measurement();
            std::time::Instant::now()
        };
        let frame = stream.encode(self, &self.frames, base);
        #[cfg(feature = "test-support")]
        crate::test_support::update(|sample| sample.stream_lock_ns = started.elapsed().as_nanos());
        frame
    }

    /// Returns the exact latest encoded frame ID for acknowledgement.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its frame state.
    pub fn frame_id(&self) -> Option<String> {
        let id = self.stream.lock().expect("Web frame stream poisoned").id;
        (id > 0).then(|| id.to_string())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Closed application command dispatch forwards each operation to its owning feature"
    )]
    async fn execute(self: &Arc<Self>, command: Command) -> Result<()> {
        match command {
            Command::DelegationOpen {
                pane,
                generation,
                key,
            } => self.open_delegation(pane, &generation, &key).await,
            Command::AttentionOpen {
                pane,
                position,
                target,
            } => self.open_attention(pane, position, target).await,
            Command::ExternalCatalog { next } => self.refresh_external(next).await,
            Command::ExternalOpen { pane, id } => self.open_external(pane, id).await,
            Command::ExternalStart { pane, id, endpoint } => {
                self.start_external(pane, id, &endpoint).await
            }
            Command::ExternalControl {
                pane,
                generation,
                command,
            } => self.external_control(pane, &generation, command).await,
            Command::ExternalSource {
                pane,
                generation,
                source,
                start,
            } => self.external_source(pane, &generation, source, start).await,
            Command::AddSurface { pane } => self.add_surface(pane),
            Command::CloseSurface { pane } => self.close_surface(pane).await,
            Command::Plugins { command } => {
                self.plugins
                    .as_ref()
                    .ok_or("Plugin status is unavailable on this connection")?
                    .command(command)
                    .await
            }
            Command::Setup { command } => {
                self.setup
                    .as_ref()
                    .ok_or("Model setup is unavailable on this connection")?
                    .command(command)
                    .await?;
                self.refresh(Command::Refresh).await
            }
            Command::Navigate { command } => {
                self.navigation
                    .as_ref()
                    .ok_or("Session navigation is unavailable on this connection")?
                    .command(command)
                    .await
            }
            Command::ApplicationUiSurface { reference } => {
                self.application_ui_surface(&reference).await
            }
            Command::UiSurface {
                pane,
                generation,
                reference,
            } => self.ui_surface(pane, &generation, &reference).await,
            Command::UiBlock {
                pane,
                generation,
                key,
            } => self.ui_block(pane, &generation, &key).await,
            Command::UiVisible {
                pane,
                generation,
                sequence,
                keys,
            } => self.ui_visible(pane, &generation, &sequence, keys).await,
            Command::RemoteUiList { pane, generation } => {
                self.remote_ui_list(pane, &generation, None).await
            }
            Command::RemoteUiNext { ticket } => self.remote_ui_next(&ticket).await,
            Command::RemoteUiSurface {
                ticket,
                bundle,
                surface,
            } => self.remote_ui_surface(&ticket, &bundle, &surface).await,
            Command::UiInvoke {
                ticket,
                name,
                input,
            } => self.ui_invoke(&ticket, name, input).await,
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

/// Nominal application capability used by native and Worker bridges.
#[derive(Debug)]
pub struct GuiApplicationContract;
impl LocalContract for GuiApplicationContract {
    const KEY: &'static str = "rsi.gui.application";
    type Service = GuiApplication;
}

/// Ordinary coding-workspace application over independently supplied domains.
#[derive(Clone, Debug, Default)]
pub struct GuiApplicationFactory;
#[async_trait]
impl PluginFactory for GuiApplicationFactory {
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
        let api = plan
            .context()
            .lookup_local::<rsi_api_protocol::ApiClientContract>();
        let app = Arc::new(GuiApplication {
            external: plan
                .context()
                .lookup_local::<rsi_acp_protocol::service::ExternalConversationsContract>(),
            external_catalog: Mutex::new(crate::panes::ExternalCatalog::default()),
            plugins: plan
                .context()
                .lookup_local::<rsi_workbench_ui::PluginsFeatureContract>(),
            setup: plan
                .context()
                .lookup_local::<rsi_workbench_ui::SetupFeatureContract>(),
            navigation: plan
                .context()
                .lookup_local::<rsi_workbench_ui::NavigationFeatureContract>(),
            ui: plan.local::<rsi_ui::UiContract>()?,
            application_target: plan
                .context()
                .lookup_local::<rsi_ui::UiTargetContract>()
                .filter(|target| target.kind() == rsi_ui::TargetKind::Application),
            history_search: api
                .clone()
                .and_then(|api| rsi_history_api::Client::new(api).ok()),
            remote_ui: api.and_then(|client| rsi_ui_api::UiClient::new(client).ok()),
            session: plan.local::<rsi_session_protocol::SessionContract>()?,
            workspace: plan.local::<rsi_workspace_protocol::WorkspaceRegistryContract>()?,
            models: plan.local::<rsi_ai_protocol::LanguageModelsContract>()?,
            settings,
            preferences: preferences.web,
            media: plan
                .context()
                .lookup_local::<rsi_media_protocol::MediaContract>(),
            image_work: Arc::new(Semaphore::new(1)),
            panes: Mutex::new(std::collections::BTreeMap::from([(
                crate::SurfaceId::MAIN,
                Arc::new(crate::panes::Pane::default()),
            )])),
            attachment_generation: std::sync::atomic::AtomicU64::new(0),
            shell,
            has_files,
            catalog: Mutex::new(Catalog::default()),
            catalog_work: tokio::sync::Mutex::new(()),
            details: Mutex::new(crate::details::Details::default()),
            notice: Mutex::new(String::new()),
            execution: plan.context().runtime().execution().clone(),
            changed,
            slots: Arc::new(Semaphore::new(8)),
            terminal_reads: Arc::new(Semaphore::new(32)),
            terminal_writes: Arc::new(Semaphore::new(8)),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            frames: ByteBudget::new(32 * 1024 * 1024)
                .map_err(|error| MetaError::Activation(error.to_string()))?,
            admission: Mutex::new(()),
            stream: Mutex::new(crate::frames::FrameState::default()),
        });
        watch_features(&app);
        let supply = plan
            .context()
            .provide_local::<GuiApplicationContract>(app.clone())?;
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
                        app.terminal_reads.close();
                        app.terminal_writes.close();
                        app.tasks.close();
                    }
                    drop(supply);
                    app.tasks.wait().await;
                    let closed = app.close_surfaces().await;
                    *app.details.lock().expect("Web details poisoned") =
                        crate::details::Details::default();
                    *app.stream.lock().expect("Web frame stream poisoned") =
                        crate::frames::FrameState::default();
                    closed
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
    catalog
        .register_local_contract::<rsi_session_tree_ui::TreeReaderContract>()
        .map_err(meta)?;
    catalog
        .register_linked(
            "rsi.session.tree.ui-target",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_session_tree_ui::TreeTargetFactory),
        )
        .map_err(meta)?;
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
            "rsi.gui.renderer",
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
                "rsi.gui.shell",
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
    session: &rsi_agent_session_protocol::SessionId,
    cursor: Option<rsi_agent_turn_protocol::ObservationCursor>,
    has_files: bool,
) -> ProfileProgram {
    let mut entries = vec![
        ProfileEntry::new("renderer", "rsi.gui.renderer", ConfigValue::Null),
        ProfileEntry::new(
            "controller",
            "rsi.client.session-controller",
            serde_json::json!({"session_id":session,"cursor":cursor}),
        ),
        ProfileEntry::new("ui-target", "rsi.session.ui-target", ConfigValue::Null),
        ProfileEntry::new(
            "tree-ui-target",
            "rsi.session.tree.ui-target",
            ConfigValue::Null,
        ),
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

fn watch_features(app: &Arc<GuiApplication>) {
    let mut ui_changes = app.ui.membership_changes();
    for changes in [
        app.plugins.as_ref().map(|feature| feature.changes()),
        app.setup.as_ref().map(|feature| feature.changes()),
        app.navigation.as_ref().map(|feature| feature.changes()),
    ]
    .into_iter()
    .flatten()
    {
        let mut changes = changes;
        let watching = app.clone();
        drop(app.execution.spawn(app.tasks.track_future(async move {
                loop { tokio::select! { biased;
                    () = watching.stop.cancelled() => break,
                    result = changes.changed() => { if result.is_err() { break; } watching.changed(); }
                } }
            })));
    }
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
}
