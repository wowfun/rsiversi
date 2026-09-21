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
#[path = "source_details.rs"]
mod source_details;
#[path = "submissions.rs"]
mod submissions;
#[path = "terminal.rs"]
mod terminal;
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
}

#[derive(Debug)]
struct SubmissionState {
    model_command: rsi_client::CommandSubmission,
    receipt: Mutex<Option<rsi_agent_session_protocol::SessionCommandReceipt>>,
    owned: Mutex<BTreeSet<MessageId>>,
    submissions: Arc<tokio::sync::Semaphore>,
}
impl SubmissionState {
    fn new() -> Self {
        Self {
            model_command: rsi_client::CommandSubmission::default(),
            receipt: Mutex::new(None),
            owned: Mutex::new(BTreeSet::new()),
            submissions: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

#[derive(Debug)]
struct Attachment {
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
    path: String,
    header: String,
    agent_preset: String,
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
    async fn close(&self) -> Result<()> {
        self.detach_terminals().await;
        self.file_read
            .lock()
            .expect("File picker cancellation poisoned")
            .cancel();
        let mut failure = self
            .resource
            .lock()
            .expect("GUI resource poisoned")
            .clear()
            .err();
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
        self.project(ui, |mut value, transcript| {
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
        self.project(ui, |metadata, transcript| {
            crate::frames::CachedPane::capture(metadata, transcript, previous)
        })
    }
    fn project<T>(
        &self,
        ui: &rsi_ui::Ui,
        project: impl FnOnce(serde_json::Value, Option<&Transcript>) -> T,
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
            );
        }
        let current = self.current.lock().expect("Web pane poisoned").clone();
        let Some(current) = current else {
            return project(serde_json::Value::Null, None);
        };
        let pending = current.renderer.pending();
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
        let metadata = serde_json::json!({
            "kind":"native","conversation":rsi_conversation::ConversationIdentity::Native(current.id.clone()),
            "capabilities":rsi_conversation::ConversationCapabilities::native(
                current.controller.goal_changes().borrow().as_ref().is_some_and(std::result::Result::is_ok),
                current.creation.is_some() && !current.durable.load(std::sync::atomic::Ordering::Acquire), true),
            "inline": inline::frames(&current, &state, ui),
            "generation": current.generation.to_string(), "selection": self.selection.load(std::sync::atomic::Ordering::Acquire).to_string(), "session":current.id, "path":current.path,
            "ui_surfaces": ui.surfaces(&current.ui_target).unwrap_or_default(),
            "ui_cards": ui.has_block_renderers(&current.ui_target),
            "ui_revision": ui.membership_changes().borrow().to_string(),
            "commands":*current.commands.lock().expect("Web commands poisoned"),
            "completions":*current.completions.lock().expect("GUI completions poisoned"),
            "resource":resource, "resource_revision":resource_revision,
            "command_receipt":*current.submission.receipt.lock().expect("Web command receipt poisoned"),
            "header":current.header, "agent_preset":current.agent_preset, "creation":current.creation,
            "projections":state.projections, "projection_notice":state.projection_notice,
            "model":selection.model,"reasoning_effort":selection.reasoning_effort,
            "effort_profile":profile.map(rsi_ai_protocol::LanguageProfile::reasoning_efforts),
            "model_command":current.submission.model_command.view(),
            "transcript":null, "historical":state.history.is_some(),
            "history_more":state.history_more, "active":state.transcript.active, "notice":state.notice(), "pending":pending,
        });
        project(
            metadata,
            Some(state.history.as_ref().unwrap_or(&state.transcript)),
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
                self.open(index, id.clone(), None).await?;
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
                    let mut details = self.details.lock().expect("attention detail");
                    details.begin()?;
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
                let detached = self
                    .details
                    .lock()
                    .expect("GUI details poisoned")
                    .detach(id, &current.generation.to_string());
                let closed = current.close().await;
                detached.and(closed)
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
                    let (entries, notice) = attached
                        .controller
                        .completion_catalog(&[])
                        .await
                        .map_err(error)?;
                    *catalog = Some((entries, notice));
                }
                let (entries, notice) = catalog.as_ref().expect("loaded completion catalog");
                if attached
                    .completion_sequence
                    .load(std::sync::atomic::Ordering::Acquire)
                    == sequence_value
                {
                    *attached
                        .completions
                        .lock()
                        .expect("GUI completions poisoned") = Some(
                        serde_json::json!({"query":query,"sequence":sequence,"entries":rsi_client::rank_completions(entries, &query),"notice":notice}),
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
            Command::Open { pane, session } => self.open(pane, session, None).await,
            Command::Create {
                pane,
                workspace,
                reuse,
            } => {
                if let Some(reuse) = reuse
                    && self.reuse_draft(pane, &workspace, &reuse).await?
                {
                    return Ok(());
                }
                let id = SessionId::new(rsi_ui::fresh_identity("web")?).map_err(error)?;
                self.open(
                    pane,
                    id.clone(),
                    Some(rsi_session_protocol::CreateSession {
                        session_id: id,
                        workspace_id: workspace,
                        // Saved Fresh intent follows the default-preset contract in ../README.md.
                        agent_preset_id: None,
                    }),
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
            Command::Cancel { pane, generation } => self.cancel(pane, &generation).await,
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
            } => {
                let attachment = self.pane(pane)?.attachment(&generation)?;
                let answer = rsi_user_questions_protocol::QuestionAnswer { answers };
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
            } => self.inspect_interaction(pane, &generation, &owner, &id),
            _ => Err("Command does not belong to a pane".into()),
        }
    }
    #[allow(clippy::too_many_lines)] // Keep surface acquisition, generation publication and old-owner retirement together.
    async fn open(
        &self,
        index: crate::SurfaceId,
        id: SessionId,
        create: Option<rsi_session_protocol::CreateSession>,
    ) -> Result<()> {
        let pane = self.pane(index)?;
        let _switching = pane.switching.lock().await;
        if pane.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Surface is retiring".into());
        }
        let draft = pane.submission(&id)?;
        let reopening = create.is_none();
        let before_defaults = if reopening {
            None
        } else {
            self.defaults_stamp().await
        };
        let creation = create.clone();
        let handle = match create {
            Some(create) => self.session.create(create).await.map_err(error)?,
            None => {
                rsi_client::read_with_capacity_retry(&self.execution, || self.session.attach(&id))
                    .await
                    .map_err(error)?
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
            None
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
            let history = rsi_client::read_with_capacity_retry(&self.execution, || {
                handle.history_before(Some(before), 128)
            })
            .await
            .map_err(error)?;
            for fact in &history.facts {
                transcript.fact(fact);
            }
            transcript.omitted |= history.has_more;
            transcript.active = inspection.active_turn_id;
            let cursor = ObservationCursor {
                fact_seq: inspection.durable_fact_seq,
                control_seq: inspection.durable_control_seq,
            };
            (
                Some(cursor),
                history
                    .facts
                    .first()
                    .map(rsi_agent_session_protocol::SessionFact::seq),
                history.has_more,
            )
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
        renderer.seed(transcript, before, more);
        let attachment = Arc::new(Attachment {
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
            path: header.canonical_cwd().into(),
            header: header.fingerprint().map_err(error)?,
            agent_preset: header.agent_preset_id().to_string(),
            creation,
            defaults: match (before_defaults, self.defaults_stamp().await) {
                (Some(before), Some(after)) if before == after => Some(after),
                _ => None,
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
                self.details
                    .lock()
                    .expect("Web details poisoned")
                    .detach(index, &old.generation.to_string())?;
            }
            current.replace(attachment)
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
        if let Some(old) = old {
            old.close().await?;
        }
        Ok(())
    }
    async fn defaults_stamp(&self) -> Option<[rsi_settings_protocol::SettingsVersion; 2]> {
        Some([
            self.settings.read("rsi.agent").await.ok()?.version(),
            self.settings
                .read("rsi.agent-presets")
                .await
                .ok()?
                .version(),
        ])
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
    async fn cancel(&self, index: crate::SurfaceId, generation: &str) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        if !attachment
            .durable
            .load(std::sync::atomic::Ordering::Acquire)
            && attachment
                .submission
                .owned
                .lock()
                .expect("Web pending identities poisoned")
                .is_empty()
        {
            return Ok(());
        }
        let inspection =
            rsi_client::read_with_capacity_retry(&self.execution, || attachment.handle.inspect())
                .await
                .map_err(error)?;
        let pending = {
            let mut owned = attachment
                .submission
                .owned
                .lock()
                .expect("Web pending identities poisoned");
            owned.retain(|id| {
                inspection
                    .pending
                    .iter()
                    .any(|pending| &pending.message_id == id)
            });
            inspection
                .pending
                .iter()
                .filter(|pending| owned.contains(&pending.message_id))
                .map(|pending| pending.message_id.clone())
                .collect::<Vec<_>>()
        };
        for id in pending {
            attachment
                .handle
                .cancel(CancelTarget::Message(id), None)
                .await
                .map_err(error)?;
        }
        if let Some(turn) = inspection.active_turn_id {
            attachment
                .handle
                .cancel(
                    CancelTarget::Turn(turn),
                    Some("Web client cancellation".into()),
                )
                .await
                .map_err(error)?;
        }
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
        for fact in &page.facts {
            transcript.fact(fact);
        }
        transcript.omitted |= page.has_more;
        let mut state = attachment
            .renderer
            .state
            .lock()
            .expect("Web renderer poisoned");
        if state.history_generation != history_generation {
            return Ok(());
        }
        state.history_before = page
            .facts
            .first()
            .map(rsi_agent_session_protocol::SessionFact::seq)
            .or(before);
        state.history_more = page.has_more;
        state.history = Some(transcript);
        Ok(())
    }
    fn inspect_interaction(
        &self,
        index: crate::SurfaceId,
        generation: &str,
        owner: &str,
        id: &str,
    ) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        let state = attachment
            .renderer
            .state
            .lock()
            .expect("Web renderer poisoned");
        let snapshot = state
            .interactions
            .as_ref()
            .ok_or("Interactions are not available")?;
        let detail = if let Some(request) = snapshot
            .questions()
            .iter()
            .find(|request| request.id == id && request.session_id == owner)
        {
            serde_json::json!({"pane":index,"generation":generation,"kind":"question","request":request})
        } else if let Some(request) = snapshot
            .approvals()
            .iter()
            .find(|request| request.id == id && request.subject.session_id() == owner)
        {
            serde_json::json!({"pane":index,"generation":generation,"kind":"approval","request":request})
        } else {
            return Err("Interaction is no longer pending".into());
        };
        let mut details = self.details.lock().expect("Web details poisoned");
        details.begin()?;
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
}
