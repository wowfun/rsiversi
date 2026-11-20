#[path = "composer.rs"]
pub(crate) mod composer;
#[path = "export.rs"]
mod export;
#[path = "external.rs"]
mod external;
#[path = "file_input.rs"]
mod file_input;
pub(crate) use external::ExternalCatalog;
#[path = "images.rs"]
pub(crate) mod images;
#[path = "inline.rs"]
mod inline;
#[path = "model_selection.rs"]
mod model_selection;
#[path = "reference_input.rs"]
mod reference_input;
#[path = "remote_ui.rs"]
mod remote_ui;
#[path = "session_resource.rs"]
mod session_resource;
#[path = "source_details.rs"]
mod source_details;
#[path = "submissions.rs"]
mod submissions;
#[path = "terminal.rs"]
pub(crate) mod terminal;
#[path = "ui_details.rs"]
mod ui_details;

use crate::{
    application::{Command, GuiApplication, Result, error, surface_program},
    projection::Transcript,
    renderer::{Renderer, RendererContract},
};
use rsi_agent_session_protocol::{MessageId, SessionId};
use rsi_agent_turn_protocol::{CancelTarget, ObservationCursor};
use rsi_application::Surface;
use rsi_client::{SessionController, SessionControllerContract};
use rsi_session_protocol::SessionHandle;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
pub(crate) struct Pane {
    external: Mutex<Option<Arc<external::Attachment>>>,
    closed: std::sync::atomic::AtomicBool,
    switching: tokio::sync::Mutex<()>,
    revision: Mutex<Arc<()>>,
    current: Mutex<Option<Arc<Attachment>>>,
    submissions: Mutex<BTreeMap<SessionId, Arc<SubmissionState>>>,
    selection: std::sync::atomic::AtomicU64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReuseDraft {
    generation: String,
    header: String,
    session: Option<SessionId>,
}

#[derive(Debug)]
struct PreviousBlank {
    generation: u64,
    header: String,
    workspace: rsi_workspace_protocol::WorkspaceId,
    defaults: [rsi_settings_protocol::SettingsVersion; 2],
}

struct ReopenedBlank {
    id: SessionId,
    handle: Arc<dyn SessionHandle>,
    header: rsi_agent_session_protocol::SessionHeader,
}

enum Open {
    Existing(Option<Arc<dyn SessionHandle>>),
    Create(rsi_session_protocol::CreateSession, Option<ReuseDraft>),
    Preset {
        generation: String,
        header: String,
        preset: rsi_agent_session_protocol::AgentPresetId,
    },
}

#[derive(Debug)]
pub(crate) struct SubmissionState {
    previous_blank: Mutex<Option<PreviousBlank>>,
    model_command: rsi_client::CommandSubmission,
    receipt: Mutex<Option<rsi_agent_session_protocol::SessionCommandReceipt>>,
    owned: Mutex<BTreeMap<MessageId, Option<u64>>>,
    submissions: Arc<tokio::sync::Semaphore>,
}
impl SubmissionState {
    fn reconcile_pending(
        &self,
        through: u64,
        pending: &[rsi_agent_store_protocol::StorePendingMessage],
    ) {
        let pending: BTreeSet<_> = pending.iter().map(|message| &message.message_id).collect();
        self.owned
            .lock()
            .expect("Web pending identities poisoned")
            .retain(|id, accepted| {
                accepted.is_none_or(|seq| seq > through) || pending.contains(id)
            });
    }
    pub(crate) fn observe(&self, update: &rsi_agent_turn_protocol::SessionObservation) {
        use rsi_agent_session_protocol::AgentControlRecordBody as Control;
        let rsi_agent_turn_protocol::SessionObservation::Control { record, .. } = update else {
            return;
        };
        let id = match record.body() {
            Control::MessageClaimed { message_id, .. }
            | Control::MessageDiscarded { message_id, .. } => message_id,
            Control::MessageSuccessor { predecessor_id, .. } => predecessor_id,
            _ => return,
        };
        self.owned
            .lock()
            .expect("Web pending identities poisoned")
            .remove(id);
    }
    fn new() -> Self {
        Self {
            previous_blank: Mutex::default(),
            model_command: rsi_client::CommandSubmission::default(),
            receipt: Mutex::new(None),
            owned: Mutex::new(BTreeMap::new()),
            submissions: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WorkspaceRegistration {
    Registered {
        id: rsi_workspace_protocol::WorkspaceId,
        name: Option<rsi_workspace_protocol::WorkspaceName>,
    },
    Unregistered,
    Unavailable,
}

#[derive(Debug)]
struct Attachment {
    metrics: Mutex<Option<rsi_session_protocol::MetricsRead>>,
    metrics_work: tokio::sync::Mutex<()>,
    export: export::State,
    terminal_followers: Mutex<BTreeMap<String, Arc<terminal::Follower>>>,
    terminal_closed: std::sync::atomic::AtomicBool,
    files: Option<
        Arc<<rsi_session_files_ui::FilesBrowserContract as rsi_meta::LocalContract>::Service>,
    >,
    file_read: Mutex<tokio_util::sync::CancellationToken>,
    completion_catalog: tokio::sync::Mutex<Option<(Vec<rsi_client::InputCompletion>, String)>>,
    completion_sequence: std::sync::atomic::AtomicU64,
    completions: Mutex<Option<serde_json::Value>>,
    resource: Mutex<ResourcePreview>,
    inline: Mutex<BTreeMap<String, Arc<inline::InlineCard>>>,
    inline_work: tokio::sync::Mutex<u64>,
    commands: Mutex<Option<rsi_agent_session_protocol::SessionCommandsView>>,
    generation: u64,
    id: SessionId,
    coordinates: rsi_workspace_protocol::ExecutionCoordinates,
    workspace_key: rsi_workspace_protocol::WorkspaceKey,
    workspace_registration: Mutex<WorkspaceRegistration>,
    header: String,
    agent_preset: String,
    sandbox: rsi_sandbox::SandboxMode,
    protected: bool,
    creation: Option<rsi_session_protocol::CreateSession>,
    defaults: Option<[rsi_settings_protocol::SettingsVersion; 2]>,
    surface: Mutex<Option<Surface>>,
    handle: Arc<dyn SessionHandle>,
    controller: Arc<SessionController>,
    ui_target: Arc<rsi_ui::UiTarget>,
    renderer: Arc<Renderer>,
    submission: Arc<SubmissionState>,
    model: Mutex<rsi_agent_session_protocol::ModelSelection>,
    model_description: Mutex<Option<rsi_ai_protocol::LanguageModelDescription>>,
    durable: std::sync::atomic::AtomicBool,
    history_work: tokio::sync::Semaphore,
}
#[derive(Debug, Default)]
struct ResourcePreview {
    revision: u64,
    cancellation: tokio_util::sync::CancellationToken,
    value: Option<rsi_session_protocol::ResourceSnapshot>,
}
impl ResourcePreview {
    fn finish(
        &mut self,
        revision: u64,
        value: rsi_session_protocol::ResourceSnapshot,
    ) -> Result<()> {
        if self.revision == revision {
            self.revision = self
                .revision
                .checked_add(1)
                .ok_or("Resource preview capacity exhausted")?;
            self.value = Some(value);
        }
        Ok(())
    }
    fn clear(&mut self) -> Result<u64> {
        self.cancellation.cancel();
        self.value = None;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Resource preview capacity exhausted")?;
        self.cancellation = tokio_util::sync::CancellationToken::new();
        Ok(self.revision)
    }
}
impl Attachment {
    fn remember_blank(&self) {
        let origin = self
            .defaults
            .as_ref()
            .zip(self.creation.as_ref())
            .filter(|_| !self.durable.load(std::sync::atomic::Ordering::Acquire))
            .map(|(defaults, request)| PreviousBlank {
                generation: self.generation,
                header: self.header.clone(),
                workspace: request.workspace_id.clone(),
                defaults: defaults.clone(),
            });
        *self
            .submission
            .previous_blank
            .lock()
            .expect("Web blank origin poisoned") = origin;
    }

    async fn close(&self) -> Result<()> {
        self.export.close().await;
        let mut failure = self.detach_terminals().await.err();
        self.file_read
            .lock()
            .expect("File picker cancellation poisoned")
            .cancel();
        let resource_failure = self
            .resource
            .lock()
            .expect("GUI resource poisoned")
            .clear()
            .err();
        if let Some(error) = resource_failure {
            failure.get_or_insert(error);
        }
        let cards = std::mem::take(&mut *self.inline.lock().expect("inline cards poisoned"));
        for card in cards.values() {
            card.stop.cancel();
        }
        for card in cards.values() {
            if let Err(error) = card.lease.close().await {
                failure.get_or_insert_with(|| error.to_string());
            }
        }
        let surface = self.surface.lock().expect("Web surface poisoned").take();
        if let Some(surface) = surface {
            match surface.close().await {
                Ok(report) if !report.is_clean() => {
                    failure.get_or_insert_with(|| "Surface cleanup failed".into());
                }
                Err(error) => {
                    failure.get_or_insert_with(|| error.to_string());
                }
                Ok(_) => {}
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
impl Pane {
    fn submission(&self, id: &SessionId) -> Result<Arc<SubmissionState>> {
        let current = self.current.lock().expect("Web pane poisoned").clone();
        let mut submissions = self.submissions.lock().expect("Web submissions poisoned");
        if let Some(draft) = submissions.get(id) {
            return Ok(draft.clone());
        }
        if submissions.len() == 64 {
            submissions.retain(|_, draft| {
                current
                    .as_ref()
                    .is_some_and(|attachment| Arc::ptr_eq(&attachment.submission, draft))
                    || draft.submissions.available_permits() == 0
                    || !draft
                        .owned
                        .lock()
                        .expect("Web pending identities poisoned")
                        .is_empty()
            });
        }
        if submissions.len() == 64 {
            return Err("Session submission owner capacity is full; resolve pending messages before opening another conversation".into());
        }
        let draft = Arc::new(SubmissionState::new());
        submissions.insert(id.clone(), draft.clone());
        Ok(draft)
    }
    fn attachment(&self, generation: &str) -> Result<Arc<Attachment>> {
        self.current
            .lock()
            .expect("Web pane poisoned")
            .as_ref()
            .filter(|current| current.generation.to_string() == generation)
            .cloned()
            .ok_or_else(|| "This pane changed; retry the action in the current conversation".into())
    }
    pub(crate) fn changed(&self) {
        *self.revision.lock().expect("Web pane revision poisoned") = Arc::new(());
    }
    pub(crate) fn stamp(&self, ui: u64) -> crate::frames::PaneStamp {
        if let Some(current) = self.external.lock().expect("external pane").as_ref() {
            return crate::frames::PaneStamp {
                generation: Some(current.generation),
                pane: self.revision.lock().expect("pane revision").clone(),
                renderer: Some(current.controller.revision()),
                ui,
            };
        }
        let current = self.current.lock().expect("Web pane poisoned");
        crate::frames::PaneStamp {
            generation: current.as_ref().map(|current| current.generation),
            pane: self
                .revision
                .lock()
                .expect("Web pane revision poisoned")
                .clone(),
            renderer: current.as_ref().map(|current| current.renderer.revision()),
            ui,
        }
    }
    pub fn view(&self, ui: &rsi_ui::Ui) -> serde_json::Value {
        self.project(ui, |mut value, transcript, live| {
            if let Some(live) = live {
                value["queue"] = serde_json::to_value(live.queue.view(live.active.as_ref()))
                    .expect("bounded queue");
            }
            if let Some(transcript) = transcript {
                value["transcript"] = serde_json::to_value(transcript).expect("bounded transcript");
            }
            value
        })
    }
    pub(crate) fn frame_view(
        &self,
        ui: &rsi_ui::Ui,
        previous: Option<&crate::frames::CachedPane>,
    ) -> rsi_api_protocol::Result<crate::frames::CachedPane> {
        self.project(ui, |metadata, transcript, live| {
            crate::frames::CachedPane::capture_with_queue(metadata, transcript, live, previous)
        })
    }
    fn project<T>(
        &self,
        ui: &rsi_ui::Ui,
        project: impl FnOnce(serde_json::Value, Option<&Transcript>, Option<&Transcript>) -> T,
    ) -> T {
        if let Some(current) = self.external.lock().expect("external pane").as_ref() {
            let view = current.controller.view();
            let mut source = current.source.lock().expect("external source");
            if source
                .as_ref()
                .is_some_and(|source| source.source.epoch() != view.observed.snapshot.epoch)
            {
                source.take();
            }
            return project(
                serde_json::json!({"kind":"external","generation":current.generation.to_string(),"selection":self.selection.load(std::sync::atomic::Ordering::Acquire).to_string(),"external":view,"external_source":*source,"attention_focus":*current.focus.lock().expect("external attention focus")}),
                None,
                None,
            );
        }
        let current = self.current.lock().expect("Web pane poisoned").clone();
        let Some(current) = current else {
            return project(serde_json::Value::Null, None, None);
        };
        let state = current
            .renderer
            .state
            .lock()
            .expect("Web renderer poisoned");
        let selection = current.selection(state.projections.as_ref());
        let description = current
            .model_description
            .lock()
            .expect("model description poisoned");
        let profile = description
            .as_ref()
            .filter(|description| description.model() == &selection.model)
            .map(rsi_ai_protocol::LanguageModelDescription::profile);
        let (resource_revision, resource) = {
            let preview = current.resource.lock().expect("GUI resource poisoned");
            (preview.revision.to_string(), preview.value.clone())
        };
        let metrics = current
            .metrics
            .lock()
            .expect("GUI metrics poisoned")
            .clone();
        let metadata = serde_json::json!({
            "kind":"native",
            "inline": inline::frames(&current, &state, ui),
            "generation": current.generation.to_string(), "selection": self.selection.load(std::sync::atomic::Ordering::Acquire).to_string(), "session":current.id,
            "workspace":{"key":current.workspace_key,"coordinates":current.coordinates,"registration":*current.workspace_registration.lock().expect("workspace registration")},
            "ui_surfaces": ui.surfaces(&current.ui_target).unwrap_or_default(),
            "ui_cards": ui.has_block_renderers(&current.ui_target),
            "ui_revision": ui.membership_changes().borrow().to_string(),
            "commands":*current.commands.lock().expect("Web commands poisoned"),
            "completions":*current.completions.lock().expect("GUI completions poisoned"),
            "resource":resource, "resource_revision":resource_revision,
            "command_receipt":*current.submission.receipt.lock().expect("Web command receipt poisoned"),
            "protected":current.protected, "draft":!current.durable.load(std::sync::atomic::Ordering::Acquire),
            "header":current.header, "agent_preset":current.agent_preset, "creation":current.creation,
            "projections":state.projections, "projection_notice":state.projection_notice,
            "model":selection.model,"reasoning_effort":selection.reasoning_effort,
            "sandbox":current.sandbox,
            "effort_profile":profile.map(rsi_ai_protocol::LanguageProfile::reasoning_efforts),
            "model_command":current.submission.model_command.view(),
            "context_capacity_tokens": metrics.as_ref().and_then(|value| value.summary.last_context.as_ref()).map(rsi_conversation::ContextUsage::input_capacity),
            "metrics":metrics,
            "queue":null,
            "transcript":null, "historical":state.history.is_some(),
            "history_more":state.history_more, "active":state.transcript.active, "notice":state.notice(),
            "pending":state.pending(!current.protected),
        });
        project(
            metadata,
            Some(state.history.as_ref().unwrap_or(&state.transcript)),
            Some(&state.transcript),
        )
    }
}

async fn finish_retirement(
    panes: &Mutex<BTreeMap<crate::SurfaceId, Arc<Pane>>>,
    id: crate::SurfaceId,
    cleanup: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    let result = cleanup.await;
    panes.lock().expect("GUI surfaces poisoned").remove(&id);
    result
}

impl GuiApplication {
    // The caller holds catalog_work. These bounded observations are presentation;
    // the registry remains the authority for execution and registration changes.
    pub(crate) async fn refresh_attached_workspaces(&self) {
        let panes: Vec<_> = self
            .panes
            .lock()
            .expect("Web panes poisoned")
            .values()
            .cloned()
            .collect();
        for pane in panes {
            let Some(attached) = pane.current.lock().expect("Web pane poisoned").clone() else {
                continue;
            };
            self.refresh_workspace_registration(&pane, &attached).await;
        }
    }

    async fn refresh_workspace_registration(&self, pane: &Pane, attached: &Arc<Attachment>) {
        let registration = if self.available(&[rsi_workspace_api::Operation::Find.spec()]) {
            match self.workspace.find(&attached.workspace_key).await {
                Ok(Some(record)) => WorkspaceRegistration::Registered {
                    id: record.id,
                    name: record.name,
                },
                Ok(None) => WorkspaceRegistration::Unregistered,
                Err(error) => {
                    *self.notice.lock().expect("Web notice poisoned") =
                        format!("Workspace registration unavailable: {error}");
                    WorkspaceRegistration::Unavailable
                }
            }
        } else {
            WorkspaceRegistration::Unavailable
        };
        if pane
            .current
            .lock()
            .expect("Web pane poisoned")
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, attached))
        {
            *attached
                .workspace_registration
                .lock()
                .expect("workspace registration") = registration;
            pane.changed();
        }
    }

    pub(crate) async fn open_delegation(
        self: &Arc<Self>,
        index: crate::SurfaceId,
        generation: &str,
        key: &str,
    ) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        let id = {
            let state = attachment
                .renderer
                .state
                .lock()
                .expect("delegation presentation");
            let transcript = state.history.as_ref().unwrap_or(&state.transcript);
            let value = transcript
                .blocks
                .iter()
                .find(|block| block.key == key)
                .and_then(|block| block.tool.as_ref())
                .and_then(rsi_conversation::ToolState::external_conversation)
                .ok_or("This block has no current external conversation target")?;
            rsi_acp_protocol::observation::ConversationId::new(value).map_err(error)?
        };
        self.open_external(index, id).await
    }
    pub(crate) async fn open_attention(
        self: &Arc<Self>,
        index: crate::SurfaceId,
        position: rsi_navigation_api::attention::Position,
        target: Option<rsi_navigation_api::attention::Target>,
    ) -> Result<()> {
        use rsi_conversation::ConversationIdentity;
        use rsi_navigation_api::attention::Target;
        use rsi_session_protocol::ActivityRequest;
        position.validate().map_err(error)?;
        match &position.conversation {
            ConversationIdentity::Native(id) => {
                self.open(index, id.clone(), Open::Existing(None)).await?;
                if let Some(target) = target {
                    let Target::Native { request } = target else {
                        return Err("Attention backend changed".into());
                    };
                    let pane = self.pane(index)?;
                    let attachment = pane
                        .current
                        .lock()
                        .expect("native attention")
                        .clone()
                        .ok_or("Conversation closed")?;
                    let generation = attachment.generation.to_string();
                    let detail = match request {
                        ActivityRequest::Approval { turn, request: id } => {
                            let pending =
                                attachment.handle.pending_approvals().await.map_err(error)?;
                            let request = pending
                                .iter()
                                .find(|request| {
                                    request.id == id
                                        && request.subject.turn_id() == turn.as_str()
                                        && request.subject.session_id() == attachment.id.as_str()
                                })
                                .ok_or("Approval is no longer pending")?;
                            serde_json::json!({"pane":index,"generation":generation,"kind":"approval","request":request})
                        }
                        ActivityRequest::Question { turn, request: id } => {
                            let pending =
                                attachment.handle.pending_questions().await.map_err(error)?;
                            let request = pending
                                .iter()
                                .find(|request| {
                                    request.id == id
                                        && request.turn_id == turn.as_str()
                                        && request.session_id == attachment.id.as_str()
                                })
                                .ok_or("Question is no longer pending")?;
                            serde_json::json!({"pane":index,"generation":generation,"kind":"question","request":request})
                        }
                    };
                    pane.attachment(&generation)?;
                    let mut registry = self.details.lock().expect("attention detail");
                    let details = registry.open(
                        Some((index, generation.clone(), attachment.id.clone())),
                        serde_json::json!({"action":"inspect_interaction","pane":index,"generation":generation,"owner":attachment.id,"id":detail["request"]["id"]}),
                    )?;
                    let mut detail = detail;
                    detail["ticket"] = details.revision.to_string().into();
                    details.interaction = Some(detail);
                }
            }
            ConversationIdentity::External(id) => {
                self.open_external(index, id.clone()).await?;
                if let Some(target) = target {
                    let Target::External {
                        ref generation,
                        ref request,
                    } = target
                    else {
                        return Err("Attention backend changed".into());
                    };
                    let pane = self.pane(index)?;
                    let current = pane
                        .external
                        .lock()
                        .expect("external attention")
                        .clone()
                        .ok_or("Conversation closed")?;
                    if !current
                        .controller
                        .view()
                        .observed
                        .permissions
                        .iter()
                        .any(|pending| &pending.generation == generation && &pending.id == request)
                    {
                        return Err("External permission is no longer pending".into());
                    }
                    *current.focus.lock().expect("external focus") = Some(target);
                    pane.changed();
                }
            }
        }
        if let Some(navigation) = &self.navigation {
            navigation
                .command(rsi_workbench_ui::NavigationCommand::MarkRead { position })
                .await?;
        }
        Ok(())
    }
    fn pane(&self, pane: crate::SurfaceId) -> Result<Arc<Pane>> {
        self.panes
            .lock()
            .expect("GUI surfaces poisoned")
            .get(&pane)
            .filter(|pane| !pane.closed.load(std::sync::atomic::Ordering::Acquire))
            .cloned()
            .ok_or_else(|| "Unknown or retiring GUI surface".into())
    }
    pub(crate) async fn new_conversation(&self, id: crate::SurfaceId) -> Result<()> {
        self.pane(id)?;
        let cleanup = self.close_surface(id).await;
        let replacement = self.add_surface(id);
        cleanup.and(replacement)
    }

    pub(crate) async fn fork_conversation(
        &self,
        pane: crate::SurfaceId,
        generation: &str,
        turn: rsi_agent_session_protocol::TurnId,
        turns: rsi_agent_session_protocol::ForkTurnSelection,
    ) -> Result<()> {
        let attached = self.pane(pane)?.attachment(generation)?;
        let header = attached.handle.header().await.map_err(error)?;
        let id = SessionId::new(rsi_ui::fresh_identity("fork")?).map_err(error)?;
        let request = rsi_session_protocol::CreateSession {
            session_id: id.clone(),
            workspace_id: self
                .workspace
                .find(&attached.workspace_key)
                .await
                .map_err(error)?
                .ok_or("Workspace is not registered")?
                .id,
            agent_preset_id: Some(header.agent_preset_id().clone()),
            fork: Some(rsi_session_protocol::ForkDraft {
                source_session_id: attached.id.clone(),
                point: rsi_agent_session_protocol::ForkPoint::through(&turn),
                turns,
            }),
        };
        self.pane(pane)?.attachment(generation)?;
        self.open(pane, id, Open::Create(request, None)).await
    }

    pub(crate) fn add_surface(&self, id: crate::SurfaceId) -> Result<()> {
        let mut surfaces = self.panes.lock().expect("GUI surfaces poisoned");
        if surfaces.contains_key(&id) {
            return Err("Surface identity is already in use".into());
        }
        if surfaces.len() >= 2 {
            return Err("Close a conversation surface before opening another".into());
        }
        surfaces.insert(id, Arc::new(Pane::default()));
        Ok(())
    }
    pub(crate) async fn close_surfaces(&self) -> Result<()> {
        let panes: Vec<_> = self
            .panes
            .lock()
            .expect("GUI surfaces poisoned")
            .keys()
            .copied()
            .collect();
        let mut failure = None;
        for pane in panes {
            if let Err(error) = self.close_surface(pane).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub(crate) async fn close_surface(&self, id: crate::SurfaceId) -> Result<()> {
        let pane = self.pane(id)?;
        let _switching = pane.switching.lock().await;
        if pane.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err("Surface is already retiring".into());
        }
        let current = pane.current.lock().expect("GUI surface poisoned").take();
        finish_retirement(&self.panes, id, async {
            pane.detach_external().await;
            if let Some(current) = current {
                self.details
                    .lock()
                    .expect("GUI details poisoned")
                    .detach(id, &current.generation.to_string());
                current.close().await
            } else {
                Ok(())
            }
        })
        .await
    }
    #[allow(clippy::too_many_lines)] // Closed pane command dispatch keeps authority-bearing arguments visible together.
    pub(crate) async fn pane_command(&self, command: Command) -> Result<()> {
        match command {
            Command::InspectBlock {
                pane,
                generation,
                key,
            } => self.inspect_block(pane, &generation, &key),
            Command::BlockSourcesPage { ticket, forward } => {
                self.block_sources_page(&ticket, forward)
            }
            Command::InspectSource {
                pane,
                generation,
                source,
            } => {
                self.inspect_source(pane, &generation, source, 0, None)
                    .await
            }
            Command::SourcePage { ticket, forward } => self.source_page(&ticket, forward).await,
            Command::Commands { pane, generation } => {
                let attached = self.pane(pane)?.attachment(&generation)?;
                let commands = attached.controller.commands().await.map_err(error)?;
                *attached.commands.lock().expect("Web commands poisoned") = Some(commands);
                Ok(())
            }
            Command::Completions {
                pane,
                generation,
                query,
                sequence,
                refresh,
            } => {
                if query.len() > 256 {
                    return Err("Completion query exceeds its limit".into());
                }
                let sequence_value: u64 = sequence
                    .parse()
                    .map_err(|_| "Invalid completion sequence")?;
                let attached = self.pane(pane)?.attachment(&generation)?;
                attached
                    .completion_sequence
                    .fetch_max(sequence_value, std::sync::atomic::Ordering::AcqRel);
                let mut catalog = attached.completion_catalog.lock().await;
                if refresh || catalog.is_none() {
                    let (mut entries, notice) = attached
                        .controller
                        .completion_catalog(&["export", "compact"])
                        .await
                        .map_err(error)?;
                    entries.push(rsi_client::InputCompletion {
                        group: rsi_client::CompletionGroup::Command,
                        composer_input: Some(rsi_agent_session_protocol::ComposerCommandInput::Text { hint: "Filename and export options".into() }),
                        name: "export".into(), replacement: "/export".into(),
                        description: "Export Session: [filename] [-f markdown|md|json] [-i h,m,r,pie,lpr,last-provider-response]".into(),
                        resource: None,
                    });
                    entries.push(rsi_client::InputCompletion {
                        group: rsi_client::CompletionGroup::Command,
                        composer_input: None,
                        name: "compact".into(),
                        replacement: "/compact".into(),
                        description:
                            "Summarize eligible conversation history when the session is idle"
                                .into(),
                        resource: None,
                    });
                    *catalog = Some((entries, notice));
                }
                let (entries, notice) = catalog.as_ref().expect("loaded completion catalog");
                let agents = query.starts_with('@');
                let filtered = entries
                    .iter()
                    .filter(|entry| (entry.group == rsi_client::CompletionGroup::Agent) == agents);
                let filter = query.strip_prefix('@').unwrap_or(&query);
                if attached
                    .completion_sequence
                    .load(std::sync::atomic::Ordering::Acquire)
                    == sequence_value
                {
                    *attached
                        .completions
                        .lock()
                        .expect("GUI completions poisoned") = Some(
                        serde_json::json!({"query":query,"sequence":sequence,"entries":rsi_client::rank_completions(filtered, filter),"notice":notice}),
                    );
                }
                Ok(())
            }
            Command::ResourceRead {
                pane,
                generation,
                request,
            } => {
                let attached = self.pane(pane)?.attachment(&generation)?;
                let (revision, stop) = {
                    let mut preview = attached.resource.lock().expect("GUI resource poisoned");
                    let revision = preview.clear()?;
                    (revision, preview.cancellation.clone())
                };
                let resource = tokio::select! { biased;
                    () = stop.cancelled() => return Ok(()),
                    result = attached.controller.read_resource(request) => result.map_err(error)?,
                };
                let mut preview = attached.resource.lock().expect("GUI resource poisoned");
                preview.finish(revision, resource)
            }
            Command::ResourceClose { pane, generation } => {
                let attached = self.pane(pane)?.attachment(&generation)?;
                attached
                    .resource
                    .lock()
                    .expect("GUI resource poisoned")
                    .clear()?;
                Ok(())
            }
            Command::Open { pane, session } => self.open(pane, session, Open::Existing(None)).await,
            Command::SelectPreset {
                pane,
                generation,
                header,
                preset,
            } => {
                let id = self.pane(pane)?.attachment(&generation)?.id.clone();
                self.open(
                    pane,
                    id,
                    Open::Preset {
                        generation,
                        header,
                        preset,
                    },
                )
                .await
            }
            Command::Create {
                pane,
                workspace,
                agent_preset_id,
                reuse,
            } => {
                if agent_preset_id.is_none()
                    && let Some(current) =
                        reuse.as_ref().filter(|binding| binding.session.is_none())
                    && self.reuse_draft(pane, &workspace, current).await?
                {
                    return Ok(());
                }
                let id = SessionId::new(rsi_ui::fresh_identity("web")?).map_err(error)?;
                self.open(
                    pane,
                    id.clone(),
                    Open::Create(
                        rsi_session_protocol::CreateSession {
                            fork: None,
                            session_id: id,
                            workspace_id: workspace,
                            agent_preset_id,
                        },
                        reuse,
                    ),
                )
                .await
            }
            Command::InspectImage {
                pane,
                generation,
                media,
            } => self.inspect_image(pane, &generation, media),
            Command::Model {
                pane,
                generation,
                model,
                reasoning_effort,
            } => {
                self.select_model(
                    pane,
                    &generation,
                    rsi_agent_session_protocol::ModelSelection {
                        model,
                        reasoning_effort,
                    },
                )
                .await
            }
            Command::ModelRefresh { pane, generation } => {
                self.refresh_model(pane, &generation).await
            }
            Command::Metrics { pane, generation } => {
                let owner = self.pane(pane)?;
                let attached = owner.attachment(&generation)?;
                let _work = attached.metrics_work.lock().await;
                owner.attachment(&generation)?;
                let metrics = attached.handle.metrics().await.map_err(error)?;
                owner.attachment(&generation)?;
                *attached.metrics.lock().expect("GUI metrics poisoned") = Some(metrics);
                owner.changed();
                self.changed();
                Ok(())
            }
            Command::Cancel {
                pane,
                generation,
                turn_id,
            } => self.cancel(pane, &generation, turn_id).await,
            Command::History { pane, generation } => self.history(pane, &generation).await,
            Command::Live { pane, generation } => {
                let attachment = self.pane(pane)?.attachment(&generation)?;
                let mut state = attachment
                    .renderer
                    .state
                    .lock()
                    .expect("Web renderer poisoned");
                state.history_generation = state
                    .history_generation
                    .checked_add(1)
                    .ok_or("History generation exhausted")?;
                state.history = None;
                state.history_before = state.transcript.history_before();
                state.history_more = state.history_before.is_some_and(|before| before > 1);
                Ok(())
            }
            command => self.interaction_command(command).await,
        }
    }
    async fn interaction_command(&self, command: Command) -> Result<()> {
        match command {
            Command::Answer {
                pane,
                generation,
                id,
                answers,
                review,
            } => {
                let attachment = self.pane(pane)?.attachment(&generation)?;
                let answer = rsi_user_questions_protocol::QuestionAnswer { review, answers };
                answer.validate().map_err(error)?;
                if !attachment
                    .handle
                    .answer_question(&id, answer)
                    .await
                    .map_err(error)?
                {
                    return Err("Question is no longer pending".into());
                }
                self.details.lock().expect("Web details poisoned").settle(
                    pane,
                    &generation,
                    attachment.id.as_str(),
                    &id,
                );
                Ok(())
            }
            Command::Approve {
                pane,
                generation,
                owner,
                id,
                allow,
            } => {
                let attachment = self.pane(pane)?.attachment(&generation)?;
                let decision = if allow {
                    rsi_approval_protocol::ApprovalDecision::AllowOnce
                } else {
                    rsi_approval_protocol::ApprovalDecision::Deny
                };
                if !attachment
                    .handle
                    .answer_approval(&owner, &id, decision)
                    .await
                    .map_err(error)?
                {
                    return Err("Approval is no longer pending".into());
                }
                self.details.lock().expect("Web details poisoned").settle(
                    pane,
                    &generation,
                    owner.as_str(),
                    &id,
                );
                Ok(())
            }
            Command::InspectInteraction {
                pane,
                generation,
                owner,
                id,
            } => {
                self.inspect_interaction(pane, &generation, &owner, &id)
                    .await
            }
            _ => Err("Command does not belong to a pane".into()),
        }
    }
    #[allow(clippy::too_many_lines)] // Keep surface acquisition, generation publication and old-owner retirement together.
    async fn open(&self, index: crate::SurfaceId, mut id: SessionId, source: Open) -> Result<()> {
        let pane = self.pane(index)?;
        let _switching = pane.switching.lock().await;
        if pane.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Surface is retiring".into());
        }
        let reopening = matches!(source, Open::Existing(_));
        let before_defaults = if matches!(&source, Open::Create(request, _) if request.fork.is_none() && request.agent_preset_id.is_none())
        {
            self.defaults_stamp().await
        } else {
            None
        };
        let previous = match (&source, &before_defaults) {
            (Open::Create(request, Some(candidate)), Some(defaults)) => {
                self.previous_blank(&pane, &request.workspace_id, candidate, defaults)
                    .await
            }
            _ => None,
        };
        let reused = previous.is_some();
        if let Some(previous) = &previous {
            id = previous.id.clone();
        }
        let draft = pane.submission(&id)?;
        let mut creation = None;
        let mut selected_header = None;
        let _preset_permit;
        let handle = match source {
            Open::Create(mut request, _) => {
                let handle = if let Some(previous) = previous {
                    request.session_id = id.clone();
                    selected_header = Some(previous.header);
                    previous.handle
                } else {
                    self.session.create(request.clone()).await.map_err(error)?
                };
                creation = Some(request);
                handle
            }
            Open::Existing(Some(handle)) => handle,
            Open::Existing(None) => {
                rsi_client::read_with_capacity_retry(&self.execution, || self.session.attach(&id))
                    .await
                    .map_err(error)?
            }
            Open::Preset {
                generation,
                header,
                preset,
            } => {
                let attached = pane.attachment(&generation)?;
                _preset_permit = draft
                    .submissions
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| "A submission is still awaiting its receipt")?;
                let current = attached.handle.draft_snapshot().await.map_err(error)?;
                if current.header.fingerprint().map_err(error)? != header {
                    return Err("The draft preset changed; reopen the session".into());
                }
                let selected = attached
                    .handle
                    .select_preset(rsi_session_protocol::SelectDraftPreset {
                        preset_id: preset.clone(),
                        expected_revision: current.revision,
                    })
                    .await
                    .map_err(error)?;
                selected_header = Some(selected.header);
                creation = attached.creation.clone();
                if let Some(creation) = &mut creation {
                    creation.agent_preset_id = Some(preset);
                }
                attached.handle.clone()
            }
        };
        let unpublished = if reopening {
            match rsi_client::read_with_capacity_retry(&self.execution, || handle.draft_snapshot())
                .await
            {
                Ok(draft) => Some(draft.header),
                Err(rsi_session_protocol::SessionError::NotFound(_)) => None,
                Err(reason) => return Err(error(reason)),
            }
        } else {
            selected_header
        };
        let durable = reopening && unpublished.is_none();
        let header = match unpublished {
            Some(header) => header,
            None => rsi_client::read_with_capacity_retry(&self.execution, || handle.header())
                .await
                .map_err(error)?,
        };
        let mut transcript = Transcript::default();
        let (cursor, before, more) = if durable {
            let inspection =
                rsi_client::read_with_capacity_retry(&self.execution, || handle.inspect())
                    .await
                    .map_err(error)?;
            let before = inspection
                .durable_fact_seq
                .checked_add(1)
                .ok_or("History sequence exhausted")?;
            draft.reconcile_pending(inspection.durable_control_seq, &inspection.pending);
            let history = rsi_client::read_with_capacity_retry(&self.execution, || {
                handle.history_before(Some(before), 128)
            })
            .await
            .map_err(error)?;
            let (before, more) = transcript
                .history_page(handle.as_ref(), &history)
                .await
                .map_err(error)?;
            transcript.queue.seed(inspection.pending);
            transcript.active = inspection.active_turn_id;
            let cursor = ObservationCursor {
                fact_seq: inspection.durable_fact_seq,
                control_seq: inspection.durable_control_seq,
            };
            (Some(cursor), before, more)
        } else {
            (None, None, false)
        };
        let generation = self
            .attachment_generation
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |value| value.checked_add(1),
            )
            .map_err(|_| "Surface attachment generation exhausted")?
            + 1;
        let surface = self
            .shell
            .open(surface_program(&id, cursor, self.has_files))
            .await
            .map_err(error)?;
        let controller = surface
            .lookup_local::<SessionControllerContract>()
            .ok_or("Surface controller is unavailable")?;
        let renderer = surface
            .lookup_local::<RendererContract>()
            .ok_or("Surface renderer is unavailable")?;
        let ui_target = surface
            .lookup_local::<rsi_ui::UiTargetContract>()
            .ok_or("Surface UI target is unavailable")?;
        renderer.bind_submission(
            draft.clone(),
            handle.clone(),
            crate::panel_registry::InteractionPanels::new(&self.details, index, generation),
        );
        renderer.seed(transcript, before, more);
        let attachment = Arc::new(Attachment {
            metrics: Mutex::new(None),
            metrics_work: tokio::sync::Mutex::new(()),
            export: export::State::default(),
            terminal_followers: Mutex::new(BTreeMap::new()),
            terminal_closed: std::sync::atomic::AtomicBool::new(false),
            files: surface.lookup_local::<rsi_session_files_ui::FilesBrowserContract>(),
            file_read: Mutex::new(tokio_util::sync::CancellationToken::new()),
            inline: Mutex::new(BTreeMap::new()),
            inline_work: tokio::sync::Mutex::new(0),
            ui_target,
            commands: Mutex::new(None),
            completion_catalog: tokio::sync::Mutex::new(None),
            completion_sequence: std::sync::atomic::AtomicU64::new(0),
            completions: Mutex::new(None),
            resource: Mutex::new(ResourcePreview::default()),
            generation,
            id,
            coordinates: header.coordinates().clone(),
            workspace_registration: Mutex::new(WorkspaceRegistration::Unavailable),
            workspace_key: rsi_workspace_protocol::WorkspaceKey::from_coordinates(
                header.coordinates(),
            ),
            header: header.fingerprint().map_err(error)?,
            agent_preset: header.agent_preset_id().to_string(),
            sandbox: header.settings().sandbox(),
            protected: header.protection().is_some(),
            creation,
            defaults: if reused {
                before_defaults
            } else if let Some(before) = before_defaults {
                self.defaults_stamp().await.filter(|after| *after == before)
            } else {
                None
            },
            surface: Mutex::new(Some(surface)),
            handle,
            controller,
            renderer,
            submission: draft,
            model_description: Mutex::new(
                self.models
                    .describe_model(header.settings().default_model())
                    .await
                    .ok(),
            ),
            model: Mutex::new(rsi_agent_session_protocol::ModelSelection::baseline(
                header.settings(),
            )),
            durable: std::sync::atomic::AtomicBool::new(durable),
            history_work: tokio::sync::Semaphore::new(1),
        });
        pane.detach_external().await;
        let old = {
            let mut current = pane.current.lock().expect("Web pane poisoned");
            if let Some(old) = current.as_ref() {
                old.remember_blank();
                self.details
                    .lock()
                    .expect("Web details poisoned")
                    .detach(index, &old.generation.to_string());
            }
            current.replace(attachment.clone())
        };
        pane.selection
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |value| value.checked_add(1),
            )
            .map_err(|_| "Surface selection capacity exhausted")?;
        pane.changed();
        self.changed();
        if let Some(old) = old
            && let Err(error) = old.close().await
        {
            *self.notice.lock().expect("Web notice poisoned") =
                format!("Previous conversation cleanup failed: {error}");
            self.changed();
        }
        {
            let _work = self.catalog_work.lock().await;
            self.refresh_workspace_registration(&pane, &attachment)
                .await;
        }
        Ok(())
    }
    async fn previous_blank(
        &self,
        pane: &Pane,
        workspace: &rsi_workspace_protocol::WorkspaceId,
        candidate: &ReuseDraft,
        defaults: &[rsi_settings_protocol::SettingsVersion; 2],
    ) -> Option<ReopenedBlank> {
        let session = candidate.session.as_ref()?;
        let submission = pane
            .submissions
            .lock()
            .expect("Web submissions poisoned")
            .get(session)?
            .clone();
        let origin = submission
            .previous_blank
            .lock()
            .expect("Web blank origin poisoned")
            .take()?;
        if origin.generation.to_string() != candidate.generation
            || origin.header != candidate.header
            || &origin.workspace != workspace
            || &origin.defaults != defaults
            || !submission
                .owned
                .lock()
                .expect("Web pending identities poisoned")
                .is_empty()
        {
            return None;
        }
        let _permit = submission.submissions.try_acquire().ok()?;
        let model = submission.model_command.view();
        if model.pending.is_some() || model.receipt.is_some() {
            return None;
        }
        let handle = self.session.attach(session).await.ok()?;
        let draft = handle.draft_snapshot().await.ok()?;
        if draft.revision != 0 || draft.header.fingerprint().ok()? != origin.header {
            return None;
        }
        Some(ReopenedBlank {
            id: session.clone(),
            handle,
            header: draft.header,
        })
    }

    async fn defaults_stamp(&self) -> Option<[rsi_settings_protocol::SettingsVersion; 2]> {
        let version = |namespace| async move {
            self.settings
                .read(namespace)
                .await
                .map(|snapshot| snapshot.version())
        };
        let (agent, presets) =
            tokio::try_join!(version("rsi.agent"), version("rsi.agent-presets")).ok()?;
        Some([agent, presets])
    }
    async fn reuse_draft(
        &self,
        index: crate::SurfaceId,
        workspace: &rsi_workspace_protocol::WorkspaceId,
        reuse: &ReuseDraft,
    ) -> Result<bool> {
        let pane = self.pane(index)?;
        let _switching = pane.switching.lock().await;
        if pane.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Surface is retiring".into());
        }
        let Some(attached) = pane.current.lock().expect("Web pane poisoned").clone() else {
            return Ok(false);
        };
        let Some(creation) = &attached.creation else {
            return Ok(false);
        };
        if attached.generation.to_string() != reuse.generation
            || attached.header != reuse.header
            || &creation.workspace_id != workspace
            || attached.durable.load(std::sync::atomic::Ordering::Acquire)
            || !attached
                .submission
                .owned
                .lock()
                .expect("Web pending identities poisoned")
                .is_empty()
            || attached.defaults.is_none()
        {
            return Ok(false);
        }
        let Ok(_submission) = attached.submission.submissions.try_acquire() else {
            return Ok(false);
        };
        if attached.defaults != self.defaults_stamp().await {
            return Ok(false);
        }
        let Ok(draft) = attached.handle.draft_snapshot().await else {
            return Ok(false);
        };
        if draft.revision != 0
            || draft.header.fingerprint().map_err(error)? != attached.header
            || *attached.model.lock().expect("Web model poisoned")
                != rsi_agent_session_protocol::ModelSelection::baseline(draft.header.settings())
        {
            return Ok(false);
        }
        pane.selection
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |value| value.checked_add(1),
            )
            .map_err(|_| "Surface selection capacity exhausted")?;
        pane.changed();
        self.changed();
        Ok(true)
    }
    async fn cancel(
        &self,
        index: crate::SurfaceId,
        generation: &str,
        turn_id: rsi_agent_session_protocol::TurnId,
    ) -> Result<()> {
        self.displayed_cancel(index, generation, &turn_id)?;
        let attachment = self.pane(index)?.attachment(generation)?;
        attachment
            .handle
            .cancel(CancelTarget::Turn(turn_id), Some("GUI Stop".into()))
            .await
            .map_err(error)?;
        Ok(())
    }
    async fn history(&self, index: crate::SurfaceId, generation: &str) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        let _work = attachment
            .history_work
            .try_acquire()
            .map_err(|_| "A history page is still loading")?;
        let (before, history_generation) = {
            let state = attachment
                .renderer
                .state
                .lock()
                .expect("Web renderer poisoned");
            (state.history_before, state.history_generation)
        };
        let page = rsi_client::read_with_capacity_retry(&self.execution, || {
            attachment.handle.history_before(before, 128)
        })
        .await
        .map_err(error)?;
        let mut transcript = Transcript::default();
        let (next, more) = transcript
            .history_page(attachment.handle.as_ref(), &page)
            .await
            .map_err(error)?;
        let mut state = attachment
            .renderer
            .state
            .lock()
            .expect("Web renderer poisoned");
        if state.history_generation != history_generation {
            return Ok(());
        }
        state.history_before = next.or(before);
        state.history_more = more;
        state.history = Some(transcript);
        Ok(())
    }
    /// Reads or changes one displayed answer without retaining its note in a GUI frame.
    pub fn message_feedback(
        self: &Arc<Self>,
        source: &str,
    ) -> futures_util::future::BoxFuture<'static, Result<String>> {
        use rsi_agent_session_protocol::{
            MessageFeedback, MessageFeedbackChange, MessageFeedbackOutcome, SavedMessageFeedback,
        };
        #[derive(serde::Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Action {
            Read,
            Change {
                version: Option<String>,
                value: Option<MessageFeedback>,
            },
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            pane: crate::SurfaceId,
            generation: String,
            message: String,
            action: Action,
        }
        if source.len() > 65536 {
            return Box::pin(async { Err("Feedback request exceeds 64 KiB".into()) });
        }
        let request = serde_json::from_str::<Request>(source).map_err(error);
        self.admit_reply(move |app| async move {
            let request = request?;
            let pane = app.pane(request.pane)?;
            let attached = pane.attachment(&request.generation)?;
            let message = request.message.parse().map_err(error)?;
            let (conflict, current) = match request.action {
                Action::Read => (
                    false,
                    attached
                        .handle
                        .message_feedback(message)
                        .await
                        .map_err(error)?,
                ),
                Action::Change { version, value } => {
                    let change = MessageFeedbackChange {
                        message_seq: message,
                        if_version: version.map(|s| s.parse()).transpose().map_err(error)?,
                        value,
                    };
                    match attached
                        .handle
                        .change_message_feedback(change)
                        .await
                        .map_err(error)?
                    {
                        MessageFeedbackOutcome::Saved { current } => (false, current),
                        MessageFeedbackOutcome::Conflict { current } => (true, current),
                    }
                }
            };
            pane.attachment(&request.generation)?;
            let current = current.map(|SavedMessageFeedback { version, value }| {
                serde_json::json!({
                    "version": version.to_string(), "rating": value.rating
                })
            });
            Ok(serde_json::json!({ "conflict": conflict, "current": current }).to_string())
        })
    }

    /// Rechecks one displayed interaction after a decision reply was lost.
    pub fn read_pending_interaction(
        self: &Arc<Self>,
        source: &str,
    ) -> futures_util::future::BoxFuture<'static, Result<String>> {
        use crate::renderer::Interaction;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Approval,
            Question,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Read {
            kind: Kind,
            pane: crate::SurfaceId,
            generation: String,
            owner: SessionId,
            id: String,
        }
        if source.len() > 8192 {
            return Box::pin(async { Err("Interaction read exceeds 8 KiB".into()) });
        }
        let read = serde_json::from_str::<Read>(source).map_err(error);
        self.admit_reply(move |app| async move {
            let read = read?;
            let pane = app.pane(read.pane)?;
            let attachment = pane.attachment(&read.generation)?;
            let response = match read.kind {
                Kind::Approval => {
                    let requests = attachment.handle.pending_approvals().await.map_err(error)?;
                    let found = requests.iter().find(|item| {
                        item.id == read.id && item.subject.session_id() == read.owner.as_str()
                    });
                    serde_json::to_string(&found.map(Interaction::Approval))
                }
                Kind::Question => {
                    let requests = attachment.handle.pending_questions().await.map_err(error)?;
                    let found = requests
                        .iter()
                        .find(|item| item.id == read.id && item.session_id == read.owner.as_str());
                    serde_json::to_string(&found.map(Interaction::Question))
                }
            };
            pane.attachment(&read.generation)?;
            response.map_err(error)
        })
    }

    async fn inspect_interaction(
        &self,
        index: crate::SurfaceId,
        generation: &str,
        owner: &str,
        id: &str,
    ) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        let (questions, approvals) = tokio::join!(
            attachment.handle.pending_questions(),
            attachment.handle.pending_approvals(),
        );
        let detail = if let Some(request) = questions.as_ref().ok().and_then(|requests| {
            requests
                .iter()
                .find(|request| request.id == id && request.session_id == owner)
        }) {
            serde_json::json!({"pane":index,"generation":generation,"kind":"question","request":request})
        } else if let Some(request) = approvals.as_ref().ok().and_then(|requests| {
            requests
                .iter()
                .find(|request| request.id == id && request.subject.session_id() == owner)
        }) {
            serde_json::json!({"pane":index,"generation":generation,"kind":"approval","request":request})
        } else {
            let removed = {
                let mut registry = self.details.lock().expect("Web details poisoned");
                let question = questions.is_ok()
                    && registry.settle_observed(index, generation, owner, id, "question");
                let approval = approvals.is_ok()
                    && registry.settle_observed(index, generation, owner, id, "approval");
                question || approval
            };
            if !removed {
                questions.map_err(error)?;
                approvals.map_err(error)?;
            }
            return Ok(());
        };
        self.pane(index)?.attachment(generation)?;
        let mut registry = self.details.lock().expect("Web details poisoned");
        let details = registry.reopen(Some((index, generation.into(), attachment.id.clone())), serde_json::json!({"action":"inspect_interaction","pane":index,"generation":generation,"owner":owner,"id":id}))?;
        let mut detail = detail;
        detail["ticket"] = details.revision.to_string().into();
        details.interaction = Some(detail);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_completion_advances_revision_and_cannot_reopen_a_closed_preview() {
        let value = rsi_session_protocol::ResourceRetention::default()
            .reserve()
            .unwrap()
            .retain(rsi_agent_session_protocol::SessionResourceResponse {
                session_id: SessionId::new("preview").unwrap(),
                header_sha256: "a".repeat(64),
                composition_sha256: "b".repeat(64),
                request: rsi_agent_session_protocol::SessionResourceRequest::Sources,
                value: rsi_agent_session_protocol::SessionResourceValue::Sources {
                    sources: Vec::new(),
                },
            })
            .unwrap();
        let mut preview = ResourcePreview::default();
        let first = preview.clear().unwrap();
        let second = preview.clear().unwrap();
        preview.finish(first, value.clone()).unwrap();
        assert!(preview.value.is_none());
        preview.finish(second, value.clone()).unwrap();
        assert!(preview.revision > second);
        assert!(preview.value.is_some());
        let closed = preview.clear().unwrap();
        preview.finish(second, value).unwrap();
        assert_eq!(preview.revision, closed);
        assert!(preview.value.is_none());
    }
    #[tokio::test]
    async fn failed_cleanup_releases_the_surface_only_after_its_owner_finishes() {
        let id = crate::SurfaceId::MAIN;
        let panes = Mutex::new(BTreeMap::from([(id, Arc::new(Pane::default()))]));
        let (release, wait) = tokio::sync::oneshot::channel();
        let retiring = finish_retirement(&panes, id, async {
            wait.await.unwrap();
            Err("injected cleanup failure".into())
        });
        tokio::pin!(retiring);
        assert!(futures_util::poll!(&mut retiring).is_pending());
        assert_eq!(
            panes.lock().unwrap().len(),
            1,
            "cleanup still owns capacity"
        );
        release.send(()).unwrap();
        assert_eq!(retiring.await.unwrap_err(), "injected cleanup failure");
        assert!(
            panes.lock().unwrap().is_empty(),
            "failed cleanup burned a surface slot"
        );
        assert!(
            panes
                .lock()
                .unwrap()
                .insert(id, Arc::new(Pane::default()))
                .is_none()
        );
    }
    #[test]
    fn occupied_submission_owners_are_bounded_and_retained_across_admission_failure() {
        let pane = Pane::default();
        let mut owners = Vec::new();
        let mut permits = Vec::new();
        for index in 0..64 {
            let id = SessionId::new(format!("session-{index}")).unwrap();
            let owner = pane.submission(&id).unwrap();
            permits.push(owner.submissions.clone().try_acquire_owned().unwrap());
            owners.push((id, owner));
        }
        assert!(
            pane.submission(&SessionId::new("overflow").unwrap())
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &owners[0].1,
            &pane.submission(&owners[0].0).unwrap()
        ));
        drop(permits);
        pane.submission(&SessionId::new("replacement").unwrap())
            .unwrap();
        assert_eq!(pane.submissions.lock().unwrap().len(), 1);
    }

    #[test]
    fn durable_message_retirement_reclaims_settled_owners_but_keeps_unknown_and_in_flight() {
        use rsi_agent_session_protocol::{
            ActivationId, AgentControlRecord, AgentControlRecordBody as Control,
            MessageDiscardReason, QueueSlot, StepId, TurnId,
        };
        use rsi_agent_turn_protocol::{ObservationRetention, SessionObservation};
        let pane = Pane::default();
        let mut owners = Vec::new();
        let message = MessageId::new("message").unwrap();
        for index in 0..64 {
            let owner = pane
                .submission(&SessionId::new(format!("retained-{index}")).unwrap())
                .unwrap();
            owner.owned.lock().unwrap().insert(message.clone(), None);
            owners.push(owner);
        }
        let in_flight = owners[0].submissions.clone().try_acquire_owned().unwrap();
        assert!(
            pane.submission(&SessionId::new("blocked").unwrap())
                .is_err()
        );
        let observe = |owner: &SubmissionState, body| {
            let record = Arc::new(AgentControlRecord::new(2, 2, body).unwrap());
            let record = ObservationRetention::default()
                .retain_controls(vec![record])
                .unwrap()
                .pop()
                .unwrap();
            owner.observe(&SessionObservation::Control {
                fact_cursor: 0,
                record,
                durable_control_seq: 2,
            });
        };
        for (index, owner) in owners[..63].iter().enumerate() {
            observe(
                owner,
                Control::MessagePromoted {
                    message_id: message.clone(),
                },
            );
            assert!(owner.owned.lock().unwrap().contains_key(&message));
            observe(
                owner,
                match index % 3 {
                    0 => Control::MessageClaimed {
                        message_id: message.clone(),
                        activation_id: ActivationId::new("activation").unwrap(),
                        turn_id: TurnId::new("turn").unwrap(),
                        step_id: StepId::new("step").unwrap(),
                        claimed_fact_seq: 1,
                    },
                    1 => Control::MessageDiscarded {
                        message_id: message.clone(),
                        reason: MessageDiscardReason::Cancelled,
                    },
                    _ => Control::MessageSuccessor {
                        predecessor_id: message.clone(),
                        successor_id: MessageId::new("successor").unwrap(),
                        slot: QueueSlot::initial(&message, 1, 1),
                    },
                },
            );
            assert!(owner.owned.lock().unwrap().is_empty());
        }
        pane.submission(&SessionId::new("next").unwrap()).unwrap();
        assert_eq!(pane.submissions.lock().unwrap().len(), 3);
        assert!(owners[63].owned.lock().unwrap().contains_key(&message));
        assert_eq!(owners[0].submissions.available_permits(), 0);
        drop(in_flight);
    }
    #[test]
    fn reopening_reclaims_only_accepted_messages_covered_by_the_inspection() {
        use rsi_agent_session_protocol::{
            AgentMessageSourceKind, MessageDelivery, MessageTarget, QueueSlot,
        };
        let pane = Pane::default();
        let unknown = MessageId::new("unknown").unwrap();
        let future = MessageId::new("newer-than-inspection").unwrap();
        let pending_id = MessageId::new("pending").unwrap();
        let pending = rsi_agent_store_protocol::StorePendingMessage {
            queue_slot: QueueSlot::initial(&pending_id, 1, 1),
            source_kind: AgentMessageSourceKind::Human,
            has_turn_options: false,
            message_id: pending_id.clone(),
            delivery: MessageDelivery::NextTurn,
            target: MessageTarget::NextTurn,
            permits_promotion: true,
            bound_turn_id: None,
            accepted_control_seq: 1,
        };
        for index in 0..64 {
            let owner = pane
                .submission(&SessionId::new(format!("reopened-{index}")).unwrap())
                .unwrap();
            owner
                .owned
                .lock()
                .unwrap()
                .insert(MessageId::new("accepted").unwrap(), Some(2));
            if index == 0 {
                owner.owned.lock().unwrap().insert(unknown.clone(), None);
            }
            if index == 1 {
                owner.owned.lock().unwrap().insert(future.clone(), Some(4));
            }
            if index == 2 {
                owner
                    .owned
                    .lock()
                    .unwrap()
                    .insert(pending_id.clone(), Some(1));
            }
            owner.reconcile_pending(3, std::slice::from_ref(&pending));
        }
        pane.submission(&SessionId::new("next").unwrap()).unwrap();
        let owners = pane.submissions.lock().unwrap();
        assert_eq!(owners.len(), 4);
        assert!(
            owners[&SessionId::new("reopened-2").unwrap()]
                .owned
                .lock()
                .unwrap()
                .contains_key(&pending_id)
        );
        assert!(
            owners[&SessionId::new("reopened-0").unwrap()]
                .owned
                .lock()
                .unwrap()
                .contains_key(&unknown)
        );
        assert!(
            owners[&SessionId::new("reopened-1").unwrap()]
                .owned
                .lock()
                .unwrap()
                .contains_key(&future)
        );
    }
}
