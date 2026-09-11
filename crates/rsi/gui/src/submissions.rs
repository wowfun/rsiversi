use super::{Attachment, GuiApplication};
use crate::application::{Result, error};
use futures_util::future::BoxFuture;
use rsi_agent_session_protocol::{
    CommandArguments, DomainRequestId, MessageDelivery, MessageId, SessionCommandInvocation,
    SessionId,
};
use rsi_media_protocol::MediaRef;
use rsi_session_protocol::{SessionError, SessionInput, SubmitInput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const MAXIMUM_MESSAGE: usize = 8 * 1024 * 1024;
const MAXIMUM_COMMAND: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    pane: crate::SurfaceId,
    generation: String,
    text: String,
    images: Vec<MediaRef>,
    steer: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Restore {
    Open {
        pane: crate::SurfaceId,
        session: SessionId,
        header: String,
    },
    Create {
        pane: crate::SurfaceId,
        creation: rsi_session_protocol::CreateSession,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    session: SessionId,
    header: String,
    request: Request,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Message {
        input: SubmitInput,
    },
    Command {
        invocation: SessionCommandInvocation,
    },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dispatch,
    Query,
    RetryMessage,
}
impl Mode {
    fn parse(mode: &str) -> Result<Self> {
        match mode {
            "dispatch" => Ok(Self::Dispatch),
            "query" => Ok(Self::Query),
            "retry_message" => Ok(Self::RetryMessage),
            _ => Err("Invalid submission operation".into()),
        }
    }
}
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Settlement {
    Complete { receipt: String },
    NotAdmitted { error: String },
    Unknown { error: String },
}
impl Settlement {
    fn rejected(mode: Mode, reason: impl std::fmt::Display) -> Self {
        if mode == Mode::Dispatch {
            Self::NotAdmitted {
                error: error(reason),
            }
        } else {
            Self::Unknown {
                error: error(reason),
            }
        }
    }
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(error)
    }
}

fn not_admitted(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::Invalid(_)
            | SessionError::Capacity
            | SessionError::ShuttingDown
            | SessionError::NotFound(_)
    )
}

impl GuiApplication {
    /// Reopens an exact saved Session, or explicitly creates a new process-local Fresh Session.
    /// Only authoritative absence returns `expired`; uncertain reads do not authorize recreation.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn restore_session(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<String>> {
        let request = (|| -> Result<Restore> {
            if source.len() > 2048 {
                return Err("Saved Session selection exceeds its bound".into());
            }
            serde_json::from_str(source).map_err(error)
        })();
        let request = match request {
            Ok(request) => request,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        let pane = match &request {
            Restore::Open { pane, .. } | Restore::Create { pane, .. } => *pane,
        };
        self.admit(false, Some(pane), move |app| async move {
            let (id, expected) = match request {
                Restore::Open { session, header, .. } => {
                    let handle = match app.session.attach(&session).await {
                        Ok(handle) => handle,
                        Err(SessionError::NotFound(_)) => return Ok("{\"status\":\"expired\"}".into()),
                        Err(reason) => return Err(error(reason)),
                    };
                    if handle.header().await.map_err(error)?.fingerprint().map_err(error)? != header {
                        return Err("Saved Session Header changed; the draft was left untouched".into());
                    }
                    app.open(pane, session.clone(), None).await?;
                    (session, Some(header))
                }
                Restore::Create { mut creation, .. } => {
                    creation.session_id = SessionId::new(rsi_ui::fresh_identity("web")?).map_err(error)?;
                    let id = creation.session_id.clone();
                    app.open(pane, id.clone(), Some(creation)).await?;
                    (id, None)
                }
            };
            let pane = app.pane(pane)?;
            let current = pane.current.lock().expect("Web pane poisoned");
            let attached = current.as_ref().filter(|attached| attached.id == id).ok_or("The restored pane was replaced")?;
            if expected.is_some_and(|header| header != attached.header) {
                return Err("Saved Session Header changed while reopening; input was not replayed".into());
            }
            serde_json::to_string(&serde_json::json!({"status":"opened", "session":id, "header":attached.header, "creation":attached.creation})).map_err(error)
        })
    }

    /// Freezes a complete request without executing it. Its JSON string is opaque to the document.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its state.
    pub fn prepare_submission(
        self: &Arc<Self>,
        source: &str,
    ) -> BoxFuture<'static, Result<String>> {
        let input = (|| -> Result<_> {
            if source.len() > MAXIMUM_MESSAGE {
                return Err("Submission preparation exceeds 8 MiB".into());
            }
            let input: Prepare = serde_json::from_str(source).map_err(error)?;
            if input.text.len() > 1024 * 1024 || input.images.len() > super::images::MAXIMUM_IMAGES
            {
                return Err("Draft exceeds its text or image limit".into());
            }
            Ok(input)
        })();
        let input = match input {
            Ok(input) => input,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        self.admit(false, None, move |app| async move {
            let attached = app.pane(input.pane)?.attachment(&input.generation)?;
            let header = attached.handle.header().await.map_err(error)?.fingerprint().map_err(error)?;
            let mut command = None;
            if input.images.is_empty() && let Some((name, arguments)) = rsi_client::slash_command(&input.text) {
                let commands = attached.controller.commands().await.map_err(error)?;
                if commands.commands().iter().any(|command| command.name() == name) {
                    let id = DomainRequestId::new(rsi_ui::fresh_identity("command")?).map_err(error)?;
                    command = Some(rsi_client::command_invocation(&commands, name,
                        CommandArguments::new(arguments.into()).map_err(error)?, id).map_err(error)?);
                }
            }
            let (kind, id, request, maximum) = if let Some(invocation) = command {
                ("command", invocation.request_id.to_string(), Request::Command { invocation }, MAXIMUM_COMMAND)
            } else {
                let mut content = Vec::with_capacity(input.images.len() + 1);
                if !input.text.is_empty() { content.push(SessionInput::Text { text: input.text.clone() }); }
                content.extend(input.images.iter().cloned().map(|media| SessionInput::Image { media }));
                rsi_session_protocol::validate_session_input(&content).map_err(error)?;
                let id = MessageId::new(rsi_ui::fresh_identity("message")?).map_err(error)?;
                let request = SubmitInput {
                    message_id: id.clone(), content,
                    delivery: if input.steer { MessageDelivery::Steer } else { MessageDelivery::NextTurn },
                    model: (!input.steer).then(|| attached.model.lock().expect("Web model poisoned").clone()),
                    sandbox: None,
                };
                ("message", id.to_string(), Request::Message { input: request }, MAXIMUM_MESSAGE)
            };
            let frozen = Frozen { session: attached.id.clone(), header, request };
            let opaque = serde_json::to_string(&frozen).map_err(error)?;
            if opaque.len() > maximum { return Err("Frozen submission exceeds its encoded limit".into()); }
            serde_json::to_string(&serde_json::json!({ "kind":kind, "id":id, "opaque":opaque,
                "text_bytes":if kind == "message" { input.text.len() } else { 0 }, "images":input.images.len() })).map_err(error)
        })
    }

    /// Executes or reconciles an opaque prepared request, retaining admitted work through reply loss.
    /// `query` never mutates; `retry_message` requires authoritative absence before identical replay.
    pub fn dispatch_submission(
        self: &Arc<Self>,
        pane: crate::SurfaceId,
        generation: &str,
        opaque: &str,
        mode: &str,
    ) -> BoxFuture<'static, Result<String>> {
        let mode = match Mode::parse(mode) {
            Ok(mode) => mode,
            Err(reason) => return Box::pin(async { Err(reason) }),
        };
        let input = (|| -> Result<_> {
            if opaque.len() > MAXIMUM_MESSAGE {
                return Err("Frozen submission exceeds 8 MiB".into());
            }
            let frozen: Frozen = serde_json::from_str(opaque).map_err(error)?;
            if frozen.header.len() != 64
                || !frozen
                    .header
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err("Invalid frozen Header fingerprint".into());
            }
            match &frozen.request {
                Request::Message { input } => {
                    if input.sandbox.is_some()
                        || !matches!(
                            (input.delivery, &input.model),
                            (MessageDelivery::NextTurn, Some(_)) | (MessageDelivery::Steer, None)
                        )
                    {
                        return Err(
                            "Frozen message has an invalid delivery, model or sandbox policy"
                                .into(),
                        );
                    }
                    rsi_session_protocol::validate_session_input(&input.content).map_err(error)?;
                    let texts = input
                        .content
                        .iter()
                        .filter(|item| matches!(item, SessionInput::Text { .. }))
                        .count();
                    if texts > 1 || input.content.len() - texts > super::images::MAXIMUM_IMAGES ||
                        input.content.iter().any(|item| matches!(item, SessionInput::Text { text } if text.len() > 1024 * 1024)) {
                        return Err("Frozen draft exceeds its text or image limit".into());
                    }
                    if let Some(model) = &input.model {
                        model.validate().map_err(error)?;
                    }
                }
                Request::Command { invocation } => {
                    if opaque.len() > MAXIMUM_COMMAND {
                        return Err("Frozen command exceeds 32 KiB".into());
                    }
                    invocation.digest().map_err(error)?;
                    if mode == Mode::RetryMessage {
                        return Err("Commands can only be queried after dispatch".into());
                    }
                }
            }
            let attached = self.pane(pane)?.attachment(generation)?;
            if attached.id != frozen.session {
                return Err("Saved submission belongs to another Session".into());
            }
            let permit = attached
                .submission
                .submissions
                .clone()
                .try_acquire_owned()
                .map_err(|_| "A submission is still awaiting its receipt")?;
            Ok((attached, permit, frozen))
        })();
        let (attached, permit, frozen) = match input {
            Ok(value) => value,
            Err(reason) => {
                return Box::pin(async move { Settlement::rejected(mode, reason).encode() });
            }
        };
        let admitted = self.try_admit(false, Some(pane), move |app| async move {
            let _permit = permit;
            let header = attached.handle.header().await.and_then(|header| {
                header
                    .fingerprint()
                    .map_err(|reason| SessionError::Invalid(reason.to_string()))
            });
            match header {
                Ok(header) if header == frozen.header => {}
                Ok(_) => {
                    return Settlement::rejected(
                        mode,
                        "Saved Session Header changed; input was not replayed",
                    )
                    .encode();
                }
                Err(reason) => return Settlement::rejected(mode, reason).encode(),
            }
            app.settle_submission(&attached, frozen.request, mode)
                .await
                .encode()
        });
        match admitted {
            Ok(waiter) => waiter,
            Err(reason) => Box::pin(async move { Settlement::rejected(mode, reason).encode() }),
        }
    }

    async fn settle_submission(
        &self,
        attached: &Arc<Attachment>,
        request: Request,
        mode: Mode,
    ) -> Settlement {
        let result = match request {
            Request::Message { input } => {
                let newly_owned = if mode == Mode::Query {
                    false
                } else {
                    if attached.durable.load(std::sync::atomic::Ordering::Acquire)
                        && let Ok(inspection) = attached.handle.inspect().await
                    {
                        attached
                            .submission
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
                    let mut owned = attached
                        .submission
                        .owned
                        .lock()
                        .expect("Web pending identities poisoned");
                    if owned.len() == 1024 && !owned.contains(&input.message_id) {
                        return Settlement::rejected(
                            mode,
                            "Pending message identity capacity is full",
                        );
                    }
                    owned.insert(input.message_id.clone())
                };
                let id = input.message_id.clone();
                let result = match mode {
                    Mode::Dispatch => attached.controller.submit(input).await,
                    Mode::RetryMessage => attached.controller.retry(input).await,
                    Mode::Query => attached.handle.message_status(&id).await,
                };
                if newly_owned && mode == Mode::Dispatch && result.as_ref().is_err_and(not_admitted)
                {
                    attached
                        .submission
                        .owned
                        .lock()
                        .expect("Web pending identities poisoned")
                        .remove(&id);
                }
                if let Ok(receipt) = &result {
                    if receipt.session_id != attached.id || receipt.message_id != id {
                        return Settlement::Unknown {
                            error: "Message receipt identity does not match the saved request"
                                .into(),
                        };
                    }
                    if !attached
                        .durable
                        .swap(true, std::sync::atomic::Ordering::AcqRel)
                        && let Some(navigation) = &self.navigation
                    {
                        navigation.invalidate();
                    }
                }
                result.map(|receipt| serde_json::json!(receipt))
            }
            Request::Command { invocation } => {
                let result = if mode == Mode::Dispatch {
                    attached.controller.execute_command(invocation).await
                } else {
                    attached.controller.reconcile_command(invocation).await
                };
                if let Ok(receipt) = &result {
                    *attached
                        .submission
                        .receipt
                        .lock()
                        .expect("Web command receipt poisoned") = Some(receipt.clone());
                }
                result.map(|receipt| serde_json::json!(receipt))
            }
        };
        match result {
            Ok(receipt) => Settlement::Complete {
                receipt: receipt.to_string(),
            },
            Err(reason) if mode == Mode::Dispatch && not_admitted(&reason) => {
                Settlement::rejected(mode, reason)
            }
            Err(reason) => Settlement::Unknown {
                error: error(reason),
            },
        }
    }
}
