use super::*;
use rsi_agent_session_protocol::MessageDelivery;
use rsi_user_questions_protocol::{QuestionAnswer, QuestionRequest};
use serde_json::{Value, json};
use std::collections::BTreeSet;

static HISTORY_RETENTION: std::sync::LazyLock<rsi_agent_turn_protocol::ObservationRetention> =
    std::sync::LazyLock::new(rsi_agent_turn_protocol::ObservationRetention::default);

type Renderer = tokio::sync::mpsc::Sender<CliRenderMessage>;

#[derive(Default)]
struct LineWork {
    application: ApplicationWork,
    interrupt: tokio::sync::Notify,
    handled: tokio::sync::Notify,
    submissions: CancellationToken,
}

async fn interruptible(
    work: &LineWork,
    running: impl std::future::Future<Output = Result<()>>,
    signals: impl futures_util::Stream<Item = std::io::Result<()>>,
) -> Result<bool> {
    use futures_util::StreamExt;
    let _stop = work.submissions.clone().drop_guard();
    tokio::pin!(running, signals);
    let mut grace = None;
    loop {
        tokio::select! { biased;
            signal = signals.next() => {
                signal.ok_or_else(|| session_error("terminal interrupt source stopped"))?.map_err(session_error)?;
                if grace.is_some() { return Ok(true); }
                work.interrupt.notify_one();
                grace = Some(tokio::time::Instant::now() + Duration::from_secs(1));
            },
            () = work.handled.notified(), if grace.is_some() => { grace = None; },
            () = async { match grace { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } } => return Ok(true),
            result = &mut running => return result.map(|()| grace.is_some()),
        }
    }
}

pub(super) async fn notice(renderer: &Renderer, kind: &'static str, value: Value) -> Result<()> {
    renderer
        .send(CliRenderMessage::Event(CliEvent::Notice { kind, value }))
        .await
        .map_err(|_| RsiError::Run("terminal renderer stopped".into()))
}

fn session_error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Run(error.to_string())
}

pub(super) use crate::surfaces::Observer;

pub(super) async fn observer_finished(observer: &mut Option<Observer>) -> RsiError {
    let Some(observer) = observer else {
        return std::future::pending().await;
    };
    observer.finished.cancelled().await;
    session_error("Session observation stopped; reattach the Session to continue")
}

#[derive(Debug)]
pub(super) struct CliObservationSink {
    pub session: Option<SessionId>,
    pub renderer: Renderer,
    pub stop: CancellationToken,
    pub finished: CancellationToken,
    pub generation: Option<u64>,
}

#[async_trait::async_trait]
impl rsi_client::ObservationSink for CliObservationSink {
    async fn observation(
        &self,
        update: SessionObservation,
    ) -> Result<(), rsi_client::ObservationFailure> {
        let session = self
            .session
            .as_ref()
            .ok_or(rsi_client::ObservationFailure::SinkStopped)?;
        render_observation(update, session, self)
            .await
            .map_err(|_| rsi_client::ObservationFailure::SinkStopped)
    }
    async fn interactions(
        &self,
        snapshot: rsi_session_protocol::InteractionSnapshot,
    ) -> Result<(), rsi_client::ObservationFailure> {
        self.send(CliEvent::Interactions { snapshot })
            .await
            .map_err(|_| rsi_client::ObservationFailure::SinkStopped)
    }

    async fn projections(
        &self,
        snapshot: rsi_session_protocol::ProjectionSnapshot,
    ) -> Result<(), rsi_client::ObservationFailure> {
        self.send(CliEvent::Projections { snapshot })
            .await
            .map_err(|_| rsi_client::ObservationFailure::SinkStopped)
    }

    async fn reconnecting(
        &self,
        kind: rsi_client::ObservationKind,
        error: &rsi_client::ObservationFailure,
    ) -> Result<(), rsi_client::ObservationFailure> {
        self.send(CliEvent::Notice {
            kind: if kind == rsi_client::ObservationKind::Projections {
                "projection_reconnecting"
            } else {
                "reconnecting"
            },
            value: json!({"message": format!("{kind:?}: {error}")}),
        })
        .await
        .map_err(|_| rsi_client::ObservationFailure::SinkStopped)
    }

    async fn stopped(
        &self,
        kind: rsi_client::ObservationKind,
        error: &rsi_client::ObservationFailure,
    ) {
        let name = match kind {
            rsi_client::ObservationKind::Facts => "Session",
            rsi_client::ObservationKind::Interactions => "Interaction",
            rsi_client::ObservationKind::Projections => "Extension state",
        };
        let _ = self.send(CliEvent::Notice { kind: if kind == rsi_client::ObservationKind::Projections { "projection_stopped" } else { "error" }, value: json!({"message":format!("{name} observation stopped; reattach to continue: {error}")}) }).await;
        if kind == rsi_client::ObservationKind::Facts {
            self.finished.cancel();
        }
    }
}

impl CliObservationSink {
    async fn send(&self, event: CliEvent) -> Result<()> {
        let message = match self.generation {
            Some(generation) => CliRenderMessage::Observed { generation, event },
            None => CliRenderMessage::Event(event),
        };
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(session_error("terminal observation stopped")),
            result = self.renderer.send(message) => result.map_err(session_error),
        }
    }
}

async fn render_observation(
    update: SessionObservation,
    session: &SessionId,
    sink: &CliObservationSink,
) -> Result<()> {
    match update {
        SessionObservation::Control {
            record,
            durable_control_seq,
        } => {
            sink.send(CliEvent::Control {
                session_id: session.clone(),
                record,
                durable_control_seq,
            })
            .await?;
        }
        SessionObservation::Fact {
            fact,
            durable_fact_seq,
        } => {
            let terminal = match fact.body() {
                SessionFactBody::TurnTerminal { turn_id, outcome } => {
                    Some((turn_id.clone(), outcome.clone()))
                }
                _ => None,
            };
            sink.send(CliEvent::Fact {
                session_id: session.clone(),
                fact,
                durable_seq: durable_fact_seq,
            })
            .await?;
            if let Some((turn_id, outcome)) = terminal {
                sink.send(CliEvent::Outcome {
                    session_id: session.clone(),
                    turn_id,
                    outcome,
                    durable_seq: durable_fact_seq,
                })
                .await?;
                send_finish_line(&sink.renderer, &sink.stop).await?;
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct InteractionWatcher(JoinHandle<()>);
impl Drop for InteractionWatcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) fn spawn_interactions(
    handle: Arc<dyn SessionHandle>,
    renderer: Renderer,
    stop: &CancellationToken,
    work: &ApplicationWork,
) -> InteractionWatcher {
    let stopped = stop.child_token();
    let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
    InteractionWatcher(work.tasks.spawn(async move {
        let sink = CliObservationSink {
            session: None,
            renderer: renderer.clone(),
            stop: stopped.clone(),
                finished: CancellationToken::new(),
                generation: None,
        };
        tokio::select! { biased;
            () = stopped.cancelled() => {},
            result = rsi_client::observe_interactions(handle.as_ref(), &sink, &execution) => {
                if let Err(error) = result {
                    let _ = notice(&renderer, "error", json!({"message":format!("Interaction observation stopped; reattach the Session to retry: {error}")})).await;
                }
            },
        }
    }))
}

async fn history(
    handle: &Arc<dyn SessionHandle>,
    before: Option<u64>,
    renderer: &Renderer,
) -> Result<Option<u64>> {
    let page = handle
        .history_before(before, 128)
        .await
        .map_err(session_error)?;
    let session = handle
        .header()
        .await
        .map_err(session_error)?
        .session_id()
        .clone();
    render_history(page, session, renderer).await
}

async fn render_history(
    page: rsi_session_protocol::SessionHistoryPage,
    session: SessionId,
    renderer: &Renderer,
) -> Result<Option<u64>> {
    let next = if page.has_more {
        Some(
            page.facts
                .first()
                .ok_or_else(|| session_error("history page made no progress"))?
                .seq(),
        )
    } else {
        None
    };
    notice(renderer, "history", json!({"session_id":session,"before_seq":page.before_seq,"durable_seq":page.durable_seq,"next_before_seq":next})).await?;
    let facts = HISTORY_RETENTION
        .retain_facts(page.facts.into_iter().map(Arc::new).collect())
        .map_err(session_error)?;
    for fact in facts {
        renderer
            .send(CliRenderMessage::Event(CliEvent::Fact {
                session_id: session.clone(),
                fact,
                durable_seq: page.durable_seq,
            }))
            .await
            .map_err(session_error)?;
    }
    renderer
        .send(CliRenderMessage::FinishLine)
        .await
        .map_err(session_error)?;
    Ok(next)
}

async fn sessions(
    application: &Arc<dyn SessionService>,
    after: Option<&rsi_session_protocol::RecentSessionCursor>,
    renderer: &Renderer,
) -> Result<Option<rsi_session_protocol::RecentSessionCursor>> {
    let page = application
        .list_recent(after, 20)
        .await
        .map_err(session_error)?;
    let next = if page.has_more {
        page.sessions
            .last()
            .map(|session| rsi_session_protocol::RecentSessionCursor {
                created_at_ms: session.header.created_at_ms(),
                session_id: session.header.session_id().clone(),
            })
    } else {
        None
    };
    notice(renderer, "sessions", json!({"sessions":page.sessions.iter().map(|session| &session.header).collect::<Vec<_>>(),"has_more":page.has_more})).await?;
    Ok(next)
}

async fn attach(
    handle: &Arc<dyn SessionHandle>,
    renderer: &Renderer,
    observer: &mut Option<Observer>,
    surfaces: &crate::surfaces::TerminalSurfaces,
) -> Result<Option<u64>> {
    attach_snapshot(
        handle,
        handle.inspect().await.map_err(session_error)?,
        renderer,
        observer,
        surfaces,
    )
    .await
}

async fn attach_snapshot(
    handle: &Arc<dyn SessionHandle>,
    snapshot: rsi_agent_store_protocol::StoreSessionInspection,
    renderer: &Renderer,
    observer: &mut Option<Observer>,
    surfaces: &crate::surfaces::TerminalSurfaces,
) -> Result<Option<u64>> {
    let cursor = ObservationCursor {
        control_seq: snapshot.durable_control_seq,
        fact_seq: snapshot.durable_fact_seq,
    };
    let page = handle
        .history_before(Some(cursor.fact_seq.saturating_add(1)), 128)
        .await
        .map_err(session_error)?;
    if let Some(prior) = observer.take() {
        prior.stop().await?;
    }
    notice(renderer, "session", json!(snapshot)).await?;
    let before = render_history(page, snapshot.header.session_id().clone(), renderer).await?;
    *observer = Some(
        surfaces
            .open(snapshot.header.session_id(), Some(cursor), 0)
            .await?,
    );
    Ok(before)
}

struct AnswerDraft {
    request: QuestionRequest,
    answers: Vec<String>,
}
impl AnswerDraft {
    async fn prompt(&self, renderer: &Renderer) -> Result<()> {
        let question = self
            .request
            .questions
            .get(self.answers.len())
            .ok_or_else(|| session_error("answer draft has no unanswered question"))?;
        notice(
            renderer,
            "answer_prompt",
            json!({"request_id":self.request.id,"index":self.answers.len()+1,"question":question}),
        )
        .await
    }
    fn push(&mut self, line: String) -> Result<()> {
        let question = self
            .request
            .questions
            .get(self.answers.len())
            .ok_or_else(|| session_error("answer draft has no unanswered question"))?;
        let answer = line
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| question.options.get(index))
            .cloned()
            .unwrap_or(line);
        if answer.trim().is_empty() {
            return Err(session_error("answer must be nonempty"));
        }
        self.answers.push(answer);
        Ok(())
    }
}

/// Runs one line-oriented client over the transport-independent Session seam.
pub(crate) async fn run_session_application(
    application: Arc<dyn SessionService>,
    output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    command: SessionCommand,
    work: ApplicationWork,
    context: rsi_meta::Context,
) -> u8 {
    let (renderer, receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let rendering = spawn_cli_renderer(command.output, receiver, &work);
    let surfaces = match crate::surfaces::TerminalSurfaces::start(&context, &renderer).await {
        Ok(surfaces) => surfaces,
        Err(error) => return report_error(&error),
    };
    let line = LineWork {
        application: work.clone(),
        ..Default::default()
    };
    let signals =
        futures_util::stream::unfold((), |()| async { Some((tokio::signal::ctrl_c().await, ())) });
    let result = tokio::select! { biased;
        () = work.stop.cancelled() => Ok(false),
        result = interruptible(&line, run(application, output_cache, workspace, command, &renderer, &line, &surfaces), signals) => result,
    };
    line.submissions.cancel();
    let interrupted = matches!(result, Ok(true));
    if let Err(error) = &result {
        let _ = notice(&renderer, "error", json!({"message":error.to_string()})).await;
    }
    if interrupted {
        let _ = tokio::time::timeout(Duration::from_secs(1), notice(&renderer, "interrupted", json!({"message":"Service wait interrupted; dispatched mutations may still complete. Use the submitted identity for status lookup."}))).await;
    }
    let cleanup = surfaces.close().await;
    drop(renderer);
    let render_result = if interrupted {
        tokio::time::timeout(Duration::from_secs(1), join_cli_renderer(rendering))
            .await
            .unwrap_or(Ok(()))
    } else {
        join_cli_renderer(rendering).await
    };
    if interrupted {
        130
    } else {
        u8::from(result.is_err() || render_result.is_err() || cleanup.is_err())
    }
}

#[allow(clippy::too_many_lines)] // One command owner preserves attachment, answer draft, and client input authority.
async fn run(
    application: Arc<dyn SessionService>,
    output_cache: Arc<dyn rsi_process::ProcessOutputCache>,
    workspace: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    command: SessionCommand,
    renderer: &Renderer,
    work: &LineWork,
    surfaces: &crate::surfaces::TerminalSurfaces,
) -> Result<()> {
    if command.list {
        sessions(&application, None, renderer).await?;
        return Ok(());
    }
    if let Some(id) = command.history {
        history(
            &application.attach(&id).await.map_err(session_error)?,
            None,
            renderer,
        )
        .await?;
        return Ok(());
    }
    let resumed = command.resume.is_some();
    let mut durable = resumed;
    let selection = match command.resume {
        Some(session_id) => SessionSelection::Resume {
            session_id,
            cwd: command.cwd,
        },
        None => SessionSelection::Fresh {
            cwd: command
                .cwd
                .map_or_else(std::env::current_dir, Ok)
                .map_err(session_error)?,
            session_id: command.session_id,
            agent_preset_id: command.agent_preset,
            workspace_trust: if command.trust_workspace {
                WorkspaceTrust::Trusted
            } else {
                WorkspaceTrust::Untrusted
            },
        },
    };
    let mut handle = resolve_application_handle(&application, &workspace, selection).await?;
    let mut observer = None;
    let mut before = None;
    if resumed {
        before = attach(&handle, renderer, &mut observer, surfaces).await?;
    } else {
        notice(
            renderer,
            "session",
            json!({"header":handle.header().await.map_err(session_error)?,"durable":false}),
        )
        .await?;
    }
    if observer.is_none() {
        observer = Some(
            surfaces
                .open(
                    handle.header().await.map_err(session_error)?.session_id(),
                    None,
                    0,
                )
                .await?,
        );
    }
    let mut history_exhausted = resumed && before.is_none();
    let mut input = crate::work::input(&work.application).map_err(session_error)?;
    let mut owned = BTreeSet::new();
    let mut recent = None;
    let mut recent_exhausted = false;
    let mut draft: Option<AnswerDraft> = None;
    let extension = rsi_client::CommandSubmission::default();
    let result = async {
        loop {
            let line = tokio::select! {
                biased;
                error = observer_finished(&mut observer) => {
                    // The completed JoinHandle has been consumed; do not poll it again in stop().
                    observer.take();
                    return Err(error);
                }
                () = work.interrupt.notified() => {
                    if draft.take().is_some() { notice(renderer, "answer_abandoned", json!({})).await?; }
                    else if durable { cancel_owned(&handle, &owned).await?; }
                    work.handled.notify_one();
                    continue;
                }
                incoming = input.recv() => match incoming.unwrap_or(SessionInput::Eof) {
                    SessionInput::Line(line) => line,
                    SessionInput::Eof => break,
                    SessionInput::TooLarge => { notice(renderer, "error", json!({"message":"input line exceeds its byte limit"})).await?; continue; },
                    SessionInput::InvalidUtf8 => { notice(renderer, "error", json!({"message":"input line is not UTF-8"})).await?; continue; },
                    SessionInput::Error(error) => return Err(session_error(error)),
                },
            };
            if line.trim().is_empty() { continue; }
            let operation = async {
                let mut delivery = MessageDelivery::NextTurn;
                let mut text = line.clone();
                if let Some(escaped) = line.strip_prefix("::") { text = format!(":{escaped}"); }
                else if let Some(command) = line.strip_prefix(':') {
                    let (name, arguments) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
                    let arguments = arguments.trim();
                    match name {
                        "exit" => return Ok(true),
                        "sessions" => {
                            if recent_exhausted { notice(renderer, "sessions", json!({"sessions":[],"has_more":false})).await?; }
                            else { recent = sessions(&application, recent.as_ref(), renderer).await?; recent_exhausted = recent.is_none(); }
                        }
                        "attach" => {
                            if extension.view().pending.is_some() { return Err(session_error("Resolve the pending command with :command-result before changing sessions")); }
                            let id = SessionId::new(arguments).map_err(session_error)?;
                            let next = application.attach(&id).await.map_err(session_error)?;
                            let snapshot = next.inspect().await.map_err(session_error)?;
                            let cursor = attach_snapshot(&next, snapshot, renderer, &mut observer, surfaces).await?;
                            handle = next; durable = true; before = cursor; history_exhausted = before.is_none(); owned.clear(); draft = None;
                        }
                        "history" => {
                            if arguments.is_empty() && history_exhausted {
                                notice(renderer, "history", json!({"has_more":false,"next_before_seq":null})).await?;
                            } else {
                                let cursor = if arguments.is_empty() { before } else { Some(arguments.parse::<u64>().map_err(session_error)?) };
                                before = history(&handle, cursor, renderer).await?;
                                history_exhausted = before.is_none();
                            }
                        }
                        "status" | "agents" | "queue" => {
                            let snapshot = handle.inspect().await.map_err(session_error)?;
                            let value = match name { "agents" => json!(snapshot.tree), "queue" => json!(snapshot.pending), _ => json!(snapshot) };
                            notice(renderer, "inspection", value).await?;
                        }
                        "commands" => {
                            let controller = &observer.as_ref().ok_or_else(|| session_error("Session controller is unavailable"))?.controller;
                            notice(renderer, "commands", json!(controller.commands().await.map_err(session_error)?)).await?;
                        }
                        "command-result" => {
                            let controller = &observer.as_ref().ok_or_else(|| session_error("Session controller is unavailable"))?.controller;
                            if extension.view().pending.is_some() { extension.refresh(controller).await.map_err(session_error)?; }
                            notice(renderer, "command_result", json!(extension.view())).await?;
                        }
                        "cancel" => { cancel_explicit(&handle, arguments).await?; notice(renderer, "cancel_requested", json!({"target":arguments})).await?; }
                        "approvals" => notice(renderer, "approvals", json!(handle.pending_approvals().await.map_err(session_error)?)).await?,
                        "allow" | "deny" => {
                            let decision = if name == "allow" { rsi_approval_protocol::ApprovalDecision::AllowOnce } else { rsi_approval_protocol::ApprovalDecision::Deny };
                            let (subject, id) = arguments.split_once(char::is_whitespace).ok_or_else(|| session_error("usage: :allow SESSION ID or :deny SESSION ID"))?;
                            let subject = SessionId::new(subject).map_err(session_error)?;
                            let accepted = handle.answer_approval(&subject, id.trim(), decision).await.map_err(session_error)?;
                            notice(renderer, "approval_answer", json!({"id":arguments,"accepted":accepted})).await?;
                        }
                        "questions" => notice(renderer, "questions", json!(handle.pending_questions().await.map_err(session_error)?)).await?,
                        "answer" => {
                            let request = handle.pending_questions().await.map_err(session_error)?.into_iter().find(|request| request.id == arguments)
                                .ok_or_else(|| session_error("question is unavailable"))?;
                            request.validate().map_err(session_error)?;
                            let next = AnswerDraft { request, answers: Vec::new() }; next.prompt(renderer).await?; draft = Some(next);
                        }
                        "output" => {
                            let mut parts = arguments.split_whitespace();
                            let id = parts.next().ok_or_else(|| session_error("usage: :output ID [OFFSET]"))?;
                            let offset = parts.next().map(str::parse::<u64>).transpose().map_err(session_error)?.unwrap_or(0);
                            if parts.next().is_some() { return Err(session_error("usage: :output ID [OFFSET]")); }
                            let page = output_cache.read(id, offset, rsi_process::DEFAULT_OUTPUT_READ_BYTES).await.map_err(session_error)?;
                            notice(renderer, "output", json!({"id":page.id,"offset":page.offset,"next_offset":page.next_offset,"total_bytes":page.total_bytes,"text":String::from_utf8_lossy(&page.bytes),"bytes_hex":hex::encode(&page.bytes)})).await?;
                        }
                        "steer" => { if arguments.is_empty() { return Err(session_error("usage: :steer TEXT")); } delivery = MessageDelivery::Steer; text = arguments.to_owned(); }
                        "help" => notice(renderer, "help", json!({"commands":":sessions :attach SESSION :history [BEFORE] :status :agents :queue :commands :command-result /NAME ARGUMENTS :steer TEXT :cancel [ID] :approvals :allow SESSION ID :deny SESSION ID :questions :answer ID :output ID [OFFSET] :exit ::TEXT"})).await?,
                        _ => return Err(session_error(format!("unknown Session command: :{name}"))),
                    }
                    if name != "steer" { return Ok(false); }
                }
                if delivery != MessageDelivery::Steer && let Some(answer) = &mut draft {
                    answer.push(text)?;
                    if answer.answers.len() == answer.request.questions.len() {
                        let answer = draft.take().expect("complete answer draft");
                        let accepted = handle.answer_question(&answer.request.id, QuestionAnswer { answers: answer.answers }).await.map_err(session_error)?;
                        notice(renderer, "question_answer", json!({"id":answer.request.id,"accepted":accepted,"durability":"live_receipt"})).await?;
                    } else { answer.prompt(renderer).await?; }
                    return Ok(false);
                }
                if !owned.is_empty() { prune_owned(&handle, &mut owned).await?; }
                let id = generated_cli_message_id()?;
                let controller = &observer.as_ref().ok_or_else(|| session_error("Session controller is unavailable"))?.controller;
                if let Some(pending) = extension.view().pending { return Err(session_error(format!("Command {} is unresolved; use :command-result", pending.request_id))); }
                let command_id = rsi_agent_session_protocol::DomainRequestId::new(format!("command-{id}")).map_err(session_error)?;
                if let Some(receipt) = extension.try_slash(controller, &text, command_id).await.map_err(session_error)? {
                    notice(renderer, "command_result", json!(receipt)).await?;
                    return Ok(false);
                }
                notice(renderer, "submitting", json!({"session_id":controller.session_id(),"message_id":id})).await?;
                let receipt = controller.submit_cancellable(SubmitInput { delivery, message_id: id.clone(), content: vec![MessageInput::Text { text }], model: None, sandbox: None }, work.submissions.clone()).await.map_err(session_error)?;
                durable = true;
                owned.insert(id);
                renderer.send(CliRenderMessage::Event(CliEvent::Message { session_id: receipt.session_id, message_id: receipt.message_id, accepted_control_seq: receipt.accepted_control_seq })).await.map_err(session_error)?;
                Ok(false)
            }.await;
            match operation { Ok(true) => break, Ok(false) => {}, Err(error) => notice(renderer, "error", json!({"message":error.to_string()})).await? }
        }
        Ok(())
    }.await;
    if let Some(observer) = observer {
        observer.stop().await?;
    }
    renderer
        .send(CliRenderMessage::FinishLine)
        .await
        .map_err(session_error)?;
    result
}

async fn cancel_explicit(handle: &Arc<dyn SessionHandle>, id: &str) -> Result<()> {
    let target = if id.is_empty() {
        CancelTarget::Turn(
            handle
                .inspect()
                .await
                .map_err(session_error)?
                .active_turn_id
                .ok_or_else(|| session_error("no active Turn"))?,
        )
    } else {
        let message = MessageId::new(id).map_err(session_error)?;
        match handle.message_status(&message).await {
            Ok(receipt) => match receipt.state {
                MessageState::Claimed { turn_id, .. } => CancelTarget::Turn(turn_id),
                _ => CancelTarget::Message(message),
            },
            Err(SessionError::NotFound(_)) => {
                CancelTarget::Turn(TurnId::new(id).map_err(session_error)?)
            }
            Err(error) => return Err(session_error(error)),
        }
    };
    let reason =
        matches!(&target, CancelTarget::Turn(_)).then(|| "explicit client cancellation".into());
    handle.cancel(target, reason).await.map_err(session_error)?;
    Ok(())
}

async fn cancel_owned(handle: &Arc<dyn SessionHandle>, owned: &BTreeSet<MessageId>) -> Result<()> {
    let mut failure = None;
    for id in owned {
        let receipt = match handle.message_status(id).await {
            Ok(receipt) => receipt,
            Err(SessionError::NotFound(_)) => continue,
            Err(error) => {
                failure.get_or_insert(error);
                continue;
            }
        };
        let target = match receipt.state {
            MessageState::Pending => Some(CancelTarget::Message(id.clone())),
            MessageState::Claimed { turn_id, .. } => Some(CancelTarget::Turn(turn_id)),
            MessageState::Discarded { .. } => None,
        };
        if let Some(target) = target {
            let reason =
                matches!(&target, CancelTarget::Turn(_)).then(|| "client interrupt".into());
            if let Err(error) = handle.cancel(target, reason).await {
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), |error| Err(session_error(error)))
}

// Keep pending cancellation identities and one representative of client-owned active work.
// Prune before admission so an inspection failure cannot hide an accepted receipt or grow this set.
async fn prune_owned(
    handle: &Arc<dyn SessionHandle>,
    owned: &mut BTreeSet<MessageId>,
) -> Result<()> {
    let snapshot = handle.inspect().await.map_err(session_error)?;
    let pending: BTreeSet<_> = snapshot
        .pending
        .iter()
        .map(|entry| &entry.message_id)
        .collect();
    let mut kept_current = false;
    let mut failure = None;
    for id in owned.iter().cloned().collect::<Vec<_>>() {
        if pending.contains(&id) {
            continue;
        }
        let current = match handle.message_status(&id).await {
            Ok(current) => current,
            Err(SessionError::NotFound(_)) => {
                owned.remove(&id);
                continue;
            }
            Err(error) => {
                failure.get_or_insert(error);
                continue;
            }
        };
        if !kept_current
            && matches!(current.state, MessageState::Claimed { turn_id, .. } if Some(&turn_id) == snapshot.active_turn_id.as_ref())
        {
            kept_current = true;
        } else {
            owned.remove(&id);
        }
    }
    failure.map_or(Ok(()), |error| Err(session_error(error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::UnknownThenAcceptedHandle;
    use std::sync::atomic::Ordering;
    #[tokio::test]
    async fn interrupts_stop_busy_waits_and_a_second_signal_interrupts_idle_cancellation() {
        for idle in [false, true] {
            let work = LineWork::default();
            let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
            let signals = futures_util::stream::poll_fn(move |cx| receive.poll_recv(cx));
            let cancelled = std::sync::atomic::AtomicBool::new(false);
            let running = async {
                if idle {
                    work.interrupt.notified().await;
                    cancelled.store(true, Ordering::SeqCst);
                }
                // Models an unresponsive query, attachment, submission or cancellation.
                std::future::pending::<Result<()>>().await
            };
            let mut operation = Box::pin(interruptible(&work, running, signals));
            assert!(futures_util::poll!(&mut operation).is_pending());
            send.send(Ok(())).unwrap();
            if idle {
                assert!(futures_util::poll!(&mut operation).is_pending());
                assert!(cancelled.load(Ordering::SeqCst));
                send.send(Ok(())).unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_secs(2), operation)
                    .await
                    .unwrap()
                    .unwrap()
            );
            assert!(work.submissions.is_cancelled());
        }
    }
    fn unscoped(event: CliRenderMessage) -> CliRenderMessage {
        match event {
            CliRenderMessage::Observed { event, .. } => CliRenderMessage::Event(event),
            event => event,
        }
    }
    #[tokio::test]
    async fn failed_attachment_history_does_not_publish_a_new_session() {
        let handle: Arc<dyn SessionHandle> = Arc::new(UnknownThenAcceptedHandle {
            history_error: Some(SessionError::Backend("history unavailable".into())),
            ..Default::default()
        });
        let header = handle.header().await.unwrap();
        let snapshot = rsi_agent_store_protocol::StoreSessionInspection {
            header: header.clone(),
            durable_fact_seq: 4,
            durable_control_seq: 3,
            pending: Vec::new(),
            active_turn_id: None,
            activation_phase: None,
            tree: rsi_agent_store_protocol::StoreAgentSubtreeSnapshot {
                session: rsi_agent_store_protocol::StoreAgentSessionStatus {
                    last_settled_control_seq: 0,
                    session_id: header.session_id().clone(),
                    durable_control_seq: 3,
                    has_waking_message: false,
                    has_open_turn: false,
                    has_active_activation: false,
                },
                descendants: Vec::new(),
            },
        };
        let (renderer, mut events) = tokio::sync::mpsc::channel(32);
        let (runtime, surfaces) = crate::surfaces::fixture(handle.clone(), &renderer, false).await;
        let mut observer = Some(surfaces.open(header.session_id(), None, 0).await.unwrap());
        let prior = observer.as_ref().unwrap().controller.clone();
        assert!(
            attach_snapshot(&handle, snapshot, &renderer, &mut observer, &surfaces)
                .await
                .is_err()
        );
        assert!(
            observer
                .as_ref()
                .is_some_and(|observer| Arc::ptr_eq(&prior, &observer.controller)),
            "failed attachment stopped the prior observation"
        );
        observer.unwrap().stop().await.unwrap();
        assert!(runtime.shutdown().await.is_clean());
        assert!(
            events.try_recv().map(unscoped).is_err(),
            "failed attachment already replaced the visible Session"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn idle_interactions_do_not_poll_and_drop_stops_observation() {
        let concrete = Arc::new(UnknownThenAcceptedHandle::default());
        let (renderer, mut events) = tokio::sync::mpsc::channel(32);
        let watcher = spawn_interactions(
            concrete.clone(),
            renderer,
            &CancellationToken::new(),
            &ApplicationWork::default(),
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            events.try_recv().map(unscoped),
            Ok(CliRenderMessage::Event(CliEvent::Interactions { .. }))
        ));
        assert_eq!(concrete.interaction_polls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_mins(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(concrete.interaction_polls.load(Ordering::SeqCst), 1);
        drop(watcher);
        tokio::task::yield_now().await;
        assert_eq!(Arc::strong_count(&concrete), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn capacity_retries_survive_the_backend_failure_cutoff_and_cancel_cleanly() {
        for capacity_error in [
            None,
            Some(SessionError::Api(rsi_api_protocol::ApiError::Capacity)),
        ] {
            let fixture = Arc::new(UnknownThenAcceptedHandle {
                capacity_error,
                ..Default::default()
            });
            fixture.interaction_capacity.store(7, Ordering::SeqCst);
            fixture.observation_capacity.store(7, Ordering::SeqCst);
            let (renderer, mut events) = tokio::sync::mpsc::channel(128);
            let (runtime, surfaces) =
                crate::surfaces::fixture(fixture.clone(), &renderer, false).await;
            let observer = surfaces
                .open(
                    fixture.header().await.unwrap().session_id(),
                    Some(ObservationCursor::default()),
                    0,
                )
                .await
                .unwrap();
            for _ in 0..10 {
                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            assert!(fixture.interaction_polls.load(Ordering::SeqCst) >= 8);
            assert!(fixture.observations.load(Ordering::SeqCst) >= 8);
            let mut baseline = false;
            let mut delivered = false;
            while let Ok(event) = events.try_recv().map(unscoped) {
                match event {
                    CliRenderMessage::Event(CliEvent::Interactions { .. }) => baseline = true,
                    CliRenderMessage::Event(CliEvent::Control { .. }) => delivered = true,
                    CliRenderMessage::Event(CliEvent::Notice {
                        kind: "error",
                        value,
                    }) => panic!("capacity became a permanent failure: {value}"),
                    _ => {}
                }
            }
            assert!(baseline && delivered);
            observer.stop().await.unwrap();
            assert!(runtime.shutdown().await.is_clean());
            let calls = fixture.interaction_polls.load(Ordering::SeqCst);
            tokio::time::advance(Duration::from_secs(10)).await;
            assert_eq!(fixture.interaction_polls.load(Ordering::SeqCst), calls);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_interaction_retries_explain_how_to_resume_refresh() {
        let concrete = Arc::new(UnknownThenAcceptedHandle::default());
        concrete.interaction_failures.store(5, Ordering::SeqCst);
        let (renderer, mut events) = tokio::sync::mpsc::channel(32);
        let _watcher = spawn_interactions(
            concrete.clone(),
            renderer,
            &CancellationToken::new(),
            &ApplicationWork::default(),
        );
        for _ in 0..6 {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(2)).await;
        }
        assert_eq!(concrete.interaction_polls.load(Ordering::SeqCst), 5);
        let mut explained = false;
        while let Ok(message) = events.try_recv().map(unscoped) {
            if let CliRenderMessage::Event(CliEvent::Notice {
                kind: "error",
                value,
            }) = message
            {
                explained |= value["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("stopped") && text.contains("reattach"));
            }
        }
        assert!(
            explained,
            "watcher disappeared without its stopped state and recovery action"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn observation_stops_after_five_failures_with_a_visible_error() {
        let fixture = Arc::new(UnknownThenAcceptedHandle {
            fail_observation: true,
            ..Default::default()
        });
        let (renderer, mut events) = tokio::sync::mpsc::channel(32);
        let (runtime, surfaces) = crate::surfaces::fixture(fixture.clone(), &renderer, false).await;
        let observer = surfaces
            .open(
                fixture.header().await.unwrap().session_id(),
                Some(ObservationCursor::default()),
                0,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(message) = events.recv().await.map(unscoped) {
                if matches!(
                    message,
                    CliRenderMessage::Event(CliEvent::Notice { kind: "error", .. })
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            fixture
                .observations
                .load(std::sync::atomic::Ordering::SeqCst),
            5
        );
        observer.stop().await.unwrap();
        assert!(runtime.shutdown().await.is_clean());
    }

    #[tokio::test]
    async fn owned_cancellation_visits_later_messages_after_missing_or_failed_lookup() {
        for failed in [false, true] {
            let fixture = Arc::new(UnknownThenAcceptedHandle {
                query_finds_message: true,
                first_status_error: Some(if failed {
                    SessionError::Backend("first lookup failed".into())
                } else {
                    SessionError::NotFound("a-first".into())
                }),
                ..Default::default()
            });
            let handle: Arc<dyn SessionHandle> = fixture.clone();
            let owned = BTreeSet::from([
                MessageId::new("a-first").unwrap(),
                MessageId::new("b-second").unwrap(),
            ]);
            assert_eq!(cancel_owned(&handle, &owned).await.is_err(), failed);
            assert_eq!(
                fixture.cancellations.lock().unwrap().as_slice(),
                &[CancelTarget::Message(MessageId::new("b-second").unwrap())]
            );
        }
    }

    #[tokio::test]
    async fn answer_draft_rejects_empty_input_without_consuming_the_question() {
        let mut draft = AnswerDraft {
            request: QuestionRequest {
                id: "request".into(),
                session_id: "session".into(),
                turn_id: "turn".into(),
                questions: vec![rsi_user_questions_protocol::Question {
                    id: "question".into(),
                    prompt: "choose".into(),
                    options: vec!["one".into()],
                }],
            },
            answers: Vec::new(),
        };
        assert!(draft.push("  ".into()).is_err());
        assert!(draft.answers.is_empty());
        draft.push("1".into()).unwrap();
        assert_eq!(draft.answers, ["one"]);
        assert!(draft.push("again".into()).is_err());
        let (renderer, _) = tokio::sync::mpsc::channel(1);
        assert!(draft.prompt(&renderer).await.is_err());
    }
}
