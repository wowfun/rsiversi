use crate::{
    application::{Command, Result, WebApplication, error, surface_program},
    projection::Transcript,
    renderer::{Renderer, RendererContract},
};
use rsi_agent_session_protocol::{MessageDelivery, MessageId, SessionId, WorkspaceTrust};
use rsi_agent_turn_protocol::{CancelTarget, ObservationCursor};
use rsi_application::Surface;
use rsi_client::{SessionController, SessionControllerContract};
use rsi_session_protocol::{SessionHandle, SessionInput, SubmitInput};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
pub(crate) struct Pane {
    switching: tokio::sync::Mutex<()>,
    current: Mutex<Option<Arc<Attachment>>>,
    generation: Mutex<u64>,
    drafts: Mutex<BTreeMap<SessionId, Arc<SavedDraft>>>,
}

#[derive(Debug)]
struct SavedDraft {
    text: Mutex<String>,
    unresolved: Mutex<Option<SubmitInput>>,
    owned: Mutex<BTreeSet<MessageId>>,
    submissions: Arc<tokio::sync::Semaphore>,
}
impl SavedDraft {
    fn new() -> Self {
        Self {
            text: Mutex::new(String::new()),
            unresolved: Mutex::new(None),
            owned: Mutex::new(BTreeSet::new()),
            submissions: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

#[derive(Debug)]
struct Attachment {
    generation: u64,
    id: SessionId,
    path: String,
    surface: Mutex<Option<Surface>>,
    handle: Arc<dyn SessionHandle>,
    controller: Arc<SessionController>,
    renderer: Arc<Renderer>,
    draft: Arc<SavedDraft>,
    model: Mutex<rsi_ai_protocol::ModelRef>,
    durable: std::sync::atomic::AtomicBool,
    history_work: tokio::sync::Semaphore,
}
impl Attachment {
    async fn close(&self) -> Result<()> {
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
    fn draft(&self, id: &SessionId) -> Result<Arc<SavedDraft>> {
        let current = self.current.lock().expect("Web pane poisoned").clone();
        let mut drafts = self.drafts.lock().expect("Web drafts poisoned");
        if let Some(draft) = drafts.get(id) {
            return Ok(draft.clone());
        }
        if drafts.len() == 64 {
            drafts.retain(|_, draft| {
                current
                    .as_ref()
                    .is_some_and(|attachment| Arc::ptr_eq(&attachment.draft, draft))
                    || !draft.text.lock().expect("Web draft poisoned").is_empty()
                    || draft.submissions.available_permits() == 0
                    || draft
                        .unresolved
                        .lock()
                        .expect("Web submission poisoned")
                        .is_some()
            });
        }
        if drafts.len() == 64 {
            return Err("Saved draft capacity is full; send or clear a draft before opening another conversation".into());
        }
        let draft = Arc::new(SavedDraft::new());
        drafts.insert(id.clone(), draft.clone());
        Ok(draft)
    }
    fn set_draft(&self, draft: &SavedDraft, text: String) -> Result<()> {
        if text.len() > 1024 * 1024 {
            return Err("Draft exceeds 1 MiB".into());
        }
        let drafts = self.drafts.lock().expect("Web drafts poisoned");
        if !drafts
            .values()
            .any(|entry| std::ptr::eq(entry.as_ref(), draft))
        {
            return Err("This draft belongs to a replaced pane".into());
        }
        let total = drafts
            .values()
            .map(|draft| draft.text.lock().expect("Web draft poisoned").len())
            .sum::<usize>();
        let mut current = draft.text.lock().expect("Web draft poisoned");
        if total - current.len() + text.len() > 2 * 1024 * 1024 {
            return Err("Saved drafts exceed this pane's 2 MiB limit".into());
        }
        *current = text;
        Ok(())
    }
    fn submit_request(
        &self,
        attachment: &Attachment,
        text: &str,
        content: Vec<SessionInput>,
        steer: bool,
    ) -> Result<(SubmitInput, bool)> {
        let retained = attachment
            .draft
            .unresolved
            .lock()
            .expect("Web submission poisoned")
            .clone();
        let retry = retained.is_some();
        let request = if let Some(request) = retained {
            request
        } else {
            let drafts = self.drafts.lock().expect("Web drafts poisoned");
            let retained_bytes: usize = drafts
                .values()
                .filter_map(|draft| {
                    draft
                        .unresolved
                        .lock()
                        .expect("Web submission poisoned")
                        .as_ref()
                        .map(|request| {
                            request
                                .content
                                .iter()
                                .map(|input| match input {
                                    SessionInput::Text { text } => text.len(),
                                    SessionInput::Image { .. } => 0,
                                })
                                .sum::<usize>()
                        })
                })
                .sum();
            if retained_bytes + text.len() > 2 * 1024 * 1024 {
                return Err("Unresolved submissions exceed this pane's 2 MiB limit".into());
            }
            let id = MessageId::new(crate::identity::allocate("message")?).map_err(error)?;
            let mut owned = attachment
                .draft
                .owned
                .lock()
                .expect("Web pending identities poisoned");
            if owned.len() == 1024 {
                return Err("Pending message identity capacity is full; cancel or reconcile before submitting".into());
            }
            owned.insert(id.clone());
            let request = SubmitInput {
                message_id: id,
                content,
                delivery: if steer {
                    MessageDelivery::Steer
                } else {
                    MessageDelivery::NextTurn
                },
                model: (!steer)
                    .then(|| attachment.model.lock().expect("Web model poisoned").clone()),
                sandbox: None,
            };
            *attachment
                .draft
                .unresolved
                .lock()
                .expect("Web submission poisoned") = Some(request.clone());
            request
        };
        Ok((request, retry))
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
    pub fn view(&self) -> serde_json::Value {
        let current = self.current.lock().expect("Web pane poisoned").clone();
        let Some(current) = current else {
            return serde_json::json!(null);
        };
        let pending = current.renderer.pending();
        let state = current
            .renderer
            .state
            .lock()
            .expect("Web renderer poisoned");
        serde_json::json!({
            "generation": current.generation.to_string(), "session":current.id, "path":current.path,
            "unresolved_text":current.draft.unresolved.lock().expect("Web submission poisoned").as_ref().and_then(|request| request.content.first()).and_then(|input| match input { SessionInput::Text { text } => Some(text.clone()), SessionInput::Image { .. } => None }),
            "draft":*current.draft.text.lock().expect("Web draft poisoned"), "model":*current.model.lock().expect("Web model poisoned"),
            "transcript":state.history.as_ref().unwrap_or(&state.transcript), "historical":state.history.is_some(),
            "history_more":state.history_more, "active":state.transcript.active, "notice":state.notice, "pending":pending,
        })
    }
}

impl WebApplication {
    fn pane(&self, pane: u8) -> Result<&Arc<Pane>> {
        self.panes
            .get(usize::from(pane))
            .ok_or_else(|| "Unknown Web pane".into())
    }
    pub(crate) async fn pane_command(&self, command: Command) -> Result<()> {
        match command {
            Command::Open { pane, session } => self.open(pane, session, None).await,
            Command::Create {
                pane,
                workspace,
                trust,
            } => {
                let id = SessionId::new(crate::identity::allocate("web")?).map_err(error)?;
                self.open(
                    pane,
                    id.clone(),
                    Some(rsi_session_protocol::CreateSession {
                        session_id: id,
                        workspace_id: workspace,
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
            Command::Draft {
                pane,
                generation,
                text,
            } => {
                let pane = self.pane(pane)?;
                pane.set_draft(&pane.attachment(&generation)?.draft, text)
            }
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
            Command::Submit {
                pane,
                generation,
                text,
                steer,
            } => self.submit(pane, &generation, text, steer).await,
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
    async fn open(
        &self,
        index: u8,
        id: SessionId,
        create: Option<rsi_session_protocol::CreateSession>,
    ) -> Result<()> {
        let pane = self.pane(index)?;
        let _switching = pane.switching.lock().await;
        let draft = pane.draft(&id)?;
        let durable = create.is_none();
        let handle = match create {
            Some(create) => self.session.create(create).await.map_err(error)?,
            None => {
                rsi_client::read_with_capacity_retry(&self.execution, || self.session.attach(&id))
                    .await
                    .map_err(error)?
            }
        };
        let header = rsi_client::read_with_capacity_retry(&self.execution, || handle.header())
            .await
            .map_err(error)?;
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
        let generation = {
            let mut generation = pane.generation.lock().expect("Web generation poisoned");
            *generation = generation
                .checked_add(1)
                .ok_or("Pane generation exhausted")?;
            *generation
        };
        let surface = self
            .shell
            .open(surface_program(index, generation, &id, cursor))
            .await
            .map_err(error)?;
        let controller = surface
            .lookup_local::<SessionControllerContract>()
            .ok_or("Surface controller is unavailable")?;
        let renderer = surface
            .lookup_local::<RendererContract>()
            .ok_or("Surface renderer is unavailable")?;
        renderer.seed(transcript, before, more);
        let attachment = Arc::new(Attachment {
            generation,
            id,
            path: header.canonical_cwd().into(),
            surface: Mutex::new(Some(surface)),
            handle,
            controller,
            renderer,
            draft,
            model: Mutex::new(header.settings().default_model().clone()),
            durable: std::sync::atomic::AtomicBool::new(durable),
            history_work: tokio::sync::Semaphore::new(1),
        });
        let old = pane
            .current
            .lock()
            .expect("Web pane poisoned")
            .replace(attachment);
        self.changed();
        if let Some(old) = old {
            old.close().await?;
        }
        Ok(())
    }
    async fn submit(&self, index: u8, generation: &str, text: String, steer: bool) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        let _submission = attachment
            .draft
            .submissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| "A submission is still awaiting its receipt")?;
        self.pane(index)?
            .set_draft(&attachment.draft, text.clone())?;
        let content = vec![SessionInput::Text { text: text.clone() }];
        rsi_session_protocol::validate_session_input(&content).map_err(error)?;
        if attachment
            .durable
            .load(std::sync::atomic::Ordering::Acquire)
            && let Ok(inspection) = rsi_client::read_with_capacity_retry(&self.execution, || {
                attachment.handle.inspect()
            })
            .await
        {
            attachment
                .draft
                .owned
                .lock()
                .expect("Web pending identities poisoned")
                .retain(|id| {
                    inspection
                        .pending
                        .iter()
                        .any(|pending| &pending.message_id == id)
                });
        }
        let (request, retry) =
            self.pane(index)?
                .submit_request(&attachment, &text, content, steer)?;
        let id = request.message_id.clone();
        attachment
            .draft
            .owned
            .lock()
            .expect("Web pending identities poisoned")
            .insert(id.clone());
        attachment
            .durable
            .store(true, std::sync::atomic::Ordering::Release);
        let result = if retry {
            attachment.controller.retry(request.clone()).await
        } else {
            attachment.controller.submit(request.clone()).await
        };
        if let Err(error) = &result
            && !matches!(
                error,
                rsi_session_protocol::SessionError::MessageOutcomeUnknown { .. }
                    | rsi_session_protocol::SessionError::Api(
                        rsi_api_protocol::ApiError::OutcomeUnknown
                    )
            )
        {
            attachment
                .draft
                .owned
                .lock()
                .expect("Web pending identities poisoned")
                .remove(&id);
        }
        let unknown = matches!(
            &result,
            Err(
                rsi_session_protocol::SessionError::MessageOutcomeUnknown { .. }
                    | rsi_session_protocol::SessionError::Api(
                        rsi_api_protocol::ApiError::OutcomeUnknown
                    )
            )
        );
        if !unknown {
            *attachment
                .draft
                .unresolved
                .lock()
                .expect("Web submission poisoned") = None;
        }
        result.map_err(error)?;
        let mut draft = attachment.draft.text.lock().expect("Web draft poisoned");
        if request.content
            == vec![SessionInput::Text {
                text: draft.clone(),
            }]
        {
            draft.clear();
        }
        Ok(())
    }
    async fn cancel(&self, index: u8, generation: &str) -> Result<()> {
        let attachment = self.pane(index)?.attachment(generation)?;
        if !attachment
            .durable
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        let inspection =
            rsi_client::read_with_capacity_retry(&self.execution, || attachment.handle.inspect())
                .await
                .map_err(error)?;
        let pending = {
            let owned = attachment
                .draft
                .owned
                .lock()
                .expect("Web pending identities poisoned");
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
    async fn history(&self, index: u8, generation: &str) -> Result<()> {
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
        index: u8,
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
    fn draft_capacity_rejects_before_replacing_text_and_other_panes_remain_independent() {
        let pane = Pane::default();
        let other = Pane::default();
        let id = SessionId::new("one").unwrap();
        let draft = pane.draft(&id).unwrap();
        pane.set_draft(&draft, "x".repeat(1024 * 1024)).unwrap();
        let second = pane.draft(&SessionId::new("two").unwrap()).unwrap();
        pane.set_draft(&second, "y".repeat(1024 * 1024)).unwrap();
        let third = pane.draft(&SessionId::new("three").unwrap()).unwrap();
        assert!(pane.set_draft(&third, "excess".into()).is_err());
        assert!(third.text.lock().unwrap().is_empty());
        assert!(Arc::ptr_eq(&draft, &pane.draft(&id).unwrap()));
        assert!(other.draft(&id).unwrap().text.lock().unwrap().is_empty());
        assert!(pane.set_draft(&draft, "z".repeat(1024 * 1024 + 1)).is_err());
        assert_eq!(draft.text.lock().unwrap().as_bytes()[0], b'x');
        assert!(pane.attachment("999").is_err());
    }
    #[test]
    fn saved_draft_count_and_evicted_empty_cell_are_fenced() {
        let pane = Pane::default();
        let empty = pane.draft(&SessionId::new("empty").unwrap()).unwrap();
        for index in 0..63 {
            let draft = pane
                .draft(&SessionId::new(format!("session-{index}")).unwrap())
                .unwrap();
            pane.set_draft(&draft, "saved".into()).unwrap();
        }
        let final_draft = pane.draft(&SessionId::new("final").unwrap()).unwrap();
        pane.set_draft(&final_draft, "saved".into()).unwrap();
        assert!(pane.set_draft(&empty, "late input".into()).is_err());
        assert!(pane.draft(&SessionId::new("overflow").unwrap()).is_err());
        assert_eq!(pane.drafts.lock().unwrap().len(), 64);
    }
}
