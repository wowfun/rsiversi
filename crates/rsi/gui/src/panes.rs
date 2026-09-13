#[path = "images.rs"]
pub(crate) mod images;
#[path = "inline.rs"]
mod inline;
#[path = "remote_ui.rs"]
mod remote_ui;
#[path = "source_details.rs"]
mod source_details;
#[path = "submissions.rs"]
mod submissions;
#[path = "ui_details.rs"]
mod ui_details;

use crate::{
    application::{Command, GuiApplication, Result, error, surface_program},
    projection::Transcript,
    renderer::{Renderer, RendererContract},
};
use rsi_agent_session_protocol::{MessageId, SessionId, WorkspaceTrust};
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
    receipt: Mutex<Option<rsi_agent_session_protocol::SessionCommandReceipt>>,
    owned: Mutex<BTreeSet<MessageId>>,
    submissions: Arc<tokio::sync::Semaphore>,
}
impl SubmissionState {
    fn new() -> Self {
        Self {
            receipt: Mutex::new(None),
            owned: Mutex::new(BTreeSet::new()),
            submissions: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

#[derive(Debug)]
struct Attachment {
    inline: Mutex<BTreeMap<String, Arc<inline::InlineCard>>>,
    inline_work: tokio::sync::Mutex<u64>,
    commands: Mutex<Option<rsi_agent_session_protocol::SessionCommandsView>>,
    generation: u64,
    id: SessionId,
    path: String,
    header: String,
    creation: Option<rsi_session_protocol::CreateSession>,
    defaults: Option<[rsi_settings_protocol::SettingsVersion; 2]>,
    surface: Mutex<Option<Surface>>,
    handle: Arc<dyn SessionHandle>,
    controller: Arc<SessionController>,
    ui_target: Arc<rsi_ui::UiTarget>,
    renderer: Arc<Renderer>,
    submission: Arc<SubmissionState>,
    model: Mutex<rsi_ai_protocol::ModelRef>,
    durable: std::sync::atomic::AtomicBool,
    history_work: tokio::sync::Semaphore,
}
impl Attachment {
    async fn close(&self) -> Result<()> {
        let cards = std::mem::take(&mut *self.inline.lock().expect("inline cards poisoned"));
        for card in cards.values() {
            card.stop.cancel();
        }
        for card in cards.values() {
            card.lease.close().await.map_err(error)?;
        }
        let surface = self.surface.lock().expect("Web surface poisoned").take();
        if let Some(surface) = surface {
            let report = surface.close().await.map_err(error)?;
            if !report.is_clean() {
                return Err("Surface cleanup failed".into());
            }
        }
        Ok(())
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
        let metadata = serde_json::json!({
            "inline": inline::frames(&current, &state, ui),
            "generation": current.generation.to_string(), "selection": self.selection.load(std::sync::atomic::Ordering::Acquire).to_string(), "session":current.id, "path":current.path,
            "ui_surfaces": ui.surfaces(&current.ui_target).unwrap_or_default(),
            "ui_cards": ui.has_block_renderers(&current.ui_target),
            "ui_revision": ui.membership_changes().borrow().to_string(),
            "commands":*current.commands.lock().expect("Web commands poisoned"),
            "command_receipt":*current.submission.receipt.lock().expect("Web command receipt poisoned"),
            "header":current.header, "creation":current.creation,
            "projections":state.projections, "projection_notice":state.projection_notice,
            "model":*current.model.lock().expect("Web model poisoned"),
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
    pub(crate) async fn close_surface(&self, id: crate::SurfaceId) -> Result<()> {
        let pane = self.pane(id)?;
        let _switching = pane.switching.lock().await;
        if pane.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err("Surface is already retiring".into());
        }
        let current = pane.current.lock().expect("GUI surface poisoned").take();
        finish_retirement(&self.panes, id, async {
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
            Command::Open { pane, session } => self.open(pane, session, None).await,
            Command::Create {
                pane,
                workspace,
                trust,
                reuse,
            } => {
                if let Some(reuse) = reuse
                    && self.reuse_draft(pane, &workspace, trust, &reuse).await?
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
                        workspace_trust: if trust {
                            WorkspaceTrust::Trusted
                        } else {
                            WorkspaceTrust::Untrusted
                        },
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
            } => {
                model.validate().map_err(error)?;
                *self
                    .pane(pane)?
                    .attachment(&generation)?
                    .model
                    .lock()
                    .expect("Web model poisoned") = model;
                Ok(())
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
            inline: Mutex::new(BTreeMap::new()),
            inline_work: tokio::sync::Mutex::new(0),
            ui_target,
            commands: Mutex::new(None),
            generation,
            id,
            path: header.canonical_cwd().into(),
            header: header.fingerprint().map_err(error)?,
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
            model: Mutex::new(header.settings().default_model().clone()),
            durable: std::sync::atomic::AtomicBool::new(durable),
            history_work: tokio::sync::Semaphore::new(1),
        });
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
        trust: bool,
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
            || (creation.workspace_trust == WorkspaceTrust::Trusted) != trust
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
                != *draft.header.settings().default_model()
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
