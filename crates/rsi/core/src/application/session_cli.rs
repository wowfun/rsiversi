use super::*;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::MessageDelivery;
use rsi_user_questions_protocol::{QuestionAnswer, QuestionRequest};
use serde_json::{Value, json};
use std::collections::BTreeSet;

type Renderer = tokio::sync::mpsc::Sender<CliRenderMessage>;

async fn notice(renderer: &Renderer, kind: &'static str, value: Value) -> rsi::Result<()> {
    renderer
        .send(CliRenderMessage::Event(CliEvent::Notice { kind, value }))
        .await
        .map_err(|_| RsiError::Run("terminal renderer stopped".into()))
}

fn session_error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Run(error.to_string())
}

struct Observer {
    stop: CancellationToken,
    task: JoinHandle<()>,
}
impl Observer {
    async fn stop(self) {
        self.stop.cancel();
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(1), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}

async fn observer_finished(observer: &mut Option<Observer>) -> RsiError {
    let Some(observer) = observer else {
        return std::future::pending().await;
    };
    match (&mut observer.task).await {
        Ok(()) => session_error("Session observation stopped; reattach the Session to continue"),
        Err(error) => session_error(format!("Session observation task failed: {error}")),
    }
}

fn observe(
    handle: Arc<dyn SessionHandle>,
    mut cursor: ObservationCursor,
    renderer: Renderer,
) -> Observer {
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let task = tokio::spawn(async move {
        let work = async {
            let session = handle
                .header()
                .await
                .map_err(session_error)?
                .session_id()
                .clone();
            let _interactions = spawn_interactions(handle.clone(), renderer.clone(), &stopped);
            let mut delay = Duration::from_millis(250);
            let mut failures = 0;
            loop {
                let result = async {
                    let mut stream = handle.observe(cursor).await.map_err(session_error)?;
                    while let Some(update) = stream.next().await {
                        match update.map_err(session_error)? {
                            SessionObservation::Control { record, durable_control_seq } => {
                                cursor.control_seq = record.seq();
                                send_cli_event(&renderer, &stopped, &stopped, CliEvent::Notice { kind: "control", value: json!({"session_id":session,"record":record.as_ref(),"durable_control_seq":durable_control_seq}) }).await?;
                            }
                            SessionObservation::Fact { fact, durable_fact_seq } => {
                                cursor.fact_seq = fact.seq();
                                let terminal = match fact.body() { SessionFactBody::TurnTerminal { turn_id, outcome } => Some((turn_id.clone(), outcome.clone())), _ => None };
                                send_cli_event(&renderer, &stopped, &stopped, CliEvent::Fact { session_id: session.clone(), fact, durable_seq: durable_fact_seq }).await?;
                                if let Some((turn_id, outcome)) = terminal {
                                    send_cli_event(&renderer, &stopped, &stopped, CliEvent::Outcome { session_id: session.clone(), turn_id, outcome, durable_seq: durable_fact_seq }).await?;
                                    send_finish_line(&renderer, &stopped).await?;
                                }
                            }
                        }
                        delay = Duration::from_millis(250);
                        failures = 0;
                    }
                    Err::<(), _>(session_error("Session observation ended"))
                }.await;
                if let Err(error) = result {
                    failures += 1;
                    if failures >= 5 {
                        return Err(error);
                    }
                    notice(
                        &renderer,
                        "reconnecting",
                        json!({"message":error.to_string()}),
                    )
                    .await?;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
                if renderer.is_closed() {
                    return Ok::<(), RsiError>(());
                }
            }
        };
        tokio::select! {
            biased;
            () = stopped.cancelled() => {},
            result = work => if let Err(error) = result { let _ = notice(&renderer, "error", json!({"message":error.to_string()})).await; },
        }
    });
    Observer { stop, task }
}

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
) -> InteractionWatcher {
    let stopped = stop.child_token();
    InteractionWatcher(tokio::spawn(async move {
        let work = async {
            let mut delay = Duration::from_millis(250);
            let mut previous = Value::Null;
            let mut failures = 0;
            loop {
                match refresh_interactions(&handle, &renderer, &stopped, &mut previous).await {
                    Ok(changed) => {
                        failures = 0;
                        delay = if changed {
                            Duration::from_millis(250)
                        } else {
                            (delay * 2).min(Duration::from_secs(2))
                        };
                    }
                    Err(error) => {
                        failures += 1;
                        if failures >= 5 {
                            let _ = notice(&renderer, "error", json!({"message":format!("Interaction refresh stopped after {failures} consecutive failures; reattach the Session to retry: {error}")})).await;
                            break;
                        }
                        let _ = notice(
                            &renderer,
                            "reconnecting",
                            json!({"message":error.to_string()}),
                        )
                        .await;
                        delay = Duration::from_secs(2);
                    }
                }
                if renderer.is_closed() {
                    break;
                }
                tokio::time::sleep(delay).await;
            }
        };
        tokio::select! { biased; () = stopped.cancelled() => {}, () = work => {} }
    }))
}

pub(super) async fn refresh_interactions(
    handle: &Arc<dyn SessionHandle>,
    renderer: &Renderer,
    stop: &CancellationToken,
    previous: &mut Value,
) -> rsi::Result<bool> {
    let value = json!({"approvals":handle.pending_approvals().await.map_err(session_error)?, "questions":handle.pending_questions().await.map_err(session_error)?});
    let changed = *previous != value;
    if changed {
        *previous = value.clone();
        send_cli_event(
            renderer,
            stop,
            stop,
            CliEvent::Notice {
                kind: "interactions",
                value,
            },
        )
        .await?;
    }
    Ok(changed)
}

async fn history(
    handle: &Arc<dyn SessionHandle>,
    before: Option<u64>,
    renderer: &Renderer,
) -> rsi::Result<Option<u64>> {
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
    page: rsi_session::SessionHistoryPage,
    session: SessionId,
    renderer: &Renderer,
) -> rsi::Result<Option<u64>> {
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
    for fact in page.facts {
        renderer
            .send(CliRenderMessage::Event(CliEvent::Fact {
                session_id: session.clone(),
                fact: Arc::new(fact),
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
    application: &Arc<dyn SessionApplication>,
    after: Option<&rsi_session::RecentSessionCursor>,
    renderer: &Renderer,
) -> rsi::Result<Option<rsi_session::RecentSessionCursor>> {
    let page = application
        .list_recent(after, 20)
        .await
        .map_err(session_error)?;
    let next = if page.has_more {
        page.sessions
            .last()
            .map(|session| rsi_session::RecentSessionCursor {
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
) -> rsi::Result<Option<u64>> {
    attach_snapshot(
        handle,
        handle.inspect().await.map_err(session_error)?,
        renderer,
        observer,
    )
    .await
}

async fn attach_snapshot(
    handle: &Arc<dyn SessionHandle>,
    snapshot: rsi_agent_store_protocol::StoreSessionInspection,
    renderer: &Renderer,
    observer: &mut Option<Observer>,
) -> rsi::Result<Option<u64>> {
    let cursor = ObservationCursor {
        control_seq: snapshot.durable_control_seq,
        fact_seq: snapshot.durable_fact_seq,
    };
    let page = handle
        .history_before(Some(cursor.fact_seq.saturating_add(1)), 128)
        .await
        .map_err(session_error)?;
    if let Some(prior) = observer.take() {
        prior.stop().await;
    }
    notice(renderer, "session", json!(snapshot)).await?;
    let before = render_history(page, snapshot.header.session_id().clone(), renderer).await?;
    *observer = Some(observe(handle.clone(), cursor, renderer.clone()));
    Ok(before)
}

struct AnswerDraft {
    request: QuestionRequest,
    answers: Vec<String>,
}
impl AnswerDraft {
    async fn prompt(&self, renderer: &Renderer) -> rsi::Result<()> {
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
    fn push(&mut self, line: String) -> rsi::Result<()> {
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
    application: Arc<dyn SessionApplication>,
    command: SessionCommand,
) -> u8 {
    let (renderer, receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let rendering = spawn_cli_renderer(command.output, receiver);
    let result = run(application, command, &renderer).await;
    if let Err(error) = &result {
        let _ = notice(&renderer, "error", json!({"message":error.to_string()})).await;
    }
    drop(renderer);
    let render_result = join_cli_renderer(rendering).await;
    u8::from(result.is_err() || render_result.is_err())
}

#[allow(clippy::too_many_lines)] // One command owner preserves attachment, answer draft, and client input authority.
async fn run(
    application: Arc<dyn SessionApplication>,
    command: SessionCommand,
    renderer: &Renderer,
) -> rsi::Result<()> {
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
    // Register before emitting attachment/history: the UI may receive Ctrl-C immediately.
    let mut interrupt = Box::pin(tokio::signal::ctrl_c());
    if let Poll::Ready(result) = futures_util::poll!(&mut interrupt) {
        result.map_err(session_error)?;
    }
    let resumed = command.resume.is_some();
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
    let mut handle = resolve_application_handle(&application, selection).await?;
    let mut observer = None;
    let mut before = None;
    if resumed {
        before = attach(&handle, renderer, &mut observer).await?;
    } else {
        notice(
            renderer,
            "session",
            json!({"header":handle.header().await.map_err(session_error)?,"durable":false}),
        )
        .await?;
    }
    let mut history_exhausted = resumed && before.is_none();
    let mut input = spawn_session_input();
    let mut owned = BTreeSet::new();
    let mut recent = None;
    let mut recent_exhausted = false;
    let mut draft: Option<AnswerDraft> = None;
    let result = async {
        loop {
            let line = tokio::select! {
                biased;
                error = observer_finished(&mut observer) => {
                    // The completed JoinHandle has been consumed; do not poll it again in stop().
                    observer.take();
                    return Err(error);
                }
                signal = &mut interrupt => {
                    signal.map_err(session_error)?;
                    interrupt = Box::pin(tokio::signal::ctrl_c());
                    if draft.take().is_some() { notice(renderer, "answer_abandoned", json!({})).await?; }
                    else if observer.is_some() { cancel_owned(&handle, &owned).await?; }
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
                            let id = SessionId::new(arguments).map_err(session_error)?;
                            let next = application.attach(&id).await.map_err(session_error)?;
                            let snapshot = next.inspect().await.map_err(session_error)?;
                            let cursor = attach_snapshot(&next, snapshot, renderer, &mut observer).await?;
                            handle = next; before = cursor; history_exhausted = before.is_none(); owned.clear(); draft = None;
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
                        "cancel" => { cancel_explicit(&handle, arguments).await?; notice(renderer, "cancel_requested", json!({"target":arguments})).await?; }
                        "approvals" => notice(renderer, "approvals", json!(handle.pending_approvals().await.map_err(session_error)?)).await?,
                        "allow" | "deny" => {
                            let decision = if name == "allow" { rsi_approval_protocol::ApprovalDecision::AllowOnce } else { rsi_approval_protocol::ApprovalDecision::Deny };
                            let accepted = handle.answer_approval(arguments, decision).await.map_err(session_error)?;
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
                            let page = handle.read_output(id, offset, rsi_process::DEFAULT_OUTPUT_READ_BYTES).await.map_err(session_error)?;
                            notice(renderer, "output", json!({"id":page.id,"offset":page.offset,"next_offset":page.next_offset,"total_bytes":page.total_bytes,"text":String::from_utf8_lossy(&page.bytes),"bytes_hex":hex::encode(&page.bytes)})).await?;
                        }
                        "steer" => { if arguments.is_empty() { return Err(session_error("usage: :steer TEXT")); } delivery = MessageDelivery::Steer; text = arguments.to_owned(); }
                        "help" => notice(renderer, "help", json!({"commands":":sessions :attach SESSION :history [BEFORE] :status :agents :queue :steer TEXT :cancel [ID] :approvals :allow ID :deny ID :questions :answer ID :output ID [OFFSET] :exit ::TEXT"})).await?,
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
                let receipt = submit_with_reconciliation(&handle, SubmitInput { delivery, message_id: id.clone(), content: vec![MessageInput::Text { text }], model: None, sandbox: None }).await.map_err(session_error)?;
                owned.insert(id);
                renderer.send(CliRenderMessage::Event(CliEvent::Message { session_id: receipt.session_id, message_id: receipt.message_id, accepted_control_seq: receipt.accepted_control_seq })).await.map_err(session_error)?;
                if observer.is_none() { observer = Some(observe(handle.clone(), ObservationCursor { control_seq: 0, fact_seq: 0 }, renderer.clone())); }
                Ok(false)
            }.await;
            match operation { Ok(true) => break, Ok(false) => {}, Err(error) => notice(renderer, "error", json!({"message":error.to_string()})).await? }
        }
        Ok(())
    }.await;
    if let Some(observer) = observer {
        observer.stop().await;
    }
    renderer
        .send(CliRenderMessage::FinishLine)
        .await
        .map_err(session_error)?;
    result
}

async fn cancel_explicit(handle: &Arc<dyn SessionHandle>, id: &str) -> rsi::Result<()> {
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
            Err(SessionApplicationError::NotFound(_)) => {
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

async fn cancel_owned(
    handle: &Arc<dyn SessionHandle>,
    owned: &BTreeSet<MessageId>,
) -> rsi::Result<()> {
    let mut failure = None;
    for id in owned {
        let receipt = match handle.message_status(id).await {
            Ok(receipt) => receipt,
            Err(SessionApplicationError::NotFound(_)) => continue,
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
) -> rsi::Result<()> {
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
            Err(SessionApplicationError::NotFound(_)) => {
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
    async fn failed_attachment_history_does_not_publish_a_new_session() {
        let handle: Arc<dyn SessionHandle> = Arc::new(UnknownThenAcceptedHandle {
            history_error: Some(SessionApplicationError::Backend(
                "history unavailable".into(),
            )),
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
        let stop = CancellationToken::new();
        let mut observer = Some(Observer {
            stop: stop.clone(),
            task: tokio::spawn(std::future::pending()),
        });
        assert!(
            attach_snapshot(&handle, snapshot, &renderer, &mut observer)
                .await
                .is_err()
        );
        assert!(
            observer.is_some() && !stop.is_cancelled(),
            "failed attachment stopped the prior observation"
        );
        observer.unwrap().stop().await;
        assert!(
            events.try_recv().is_err(),
            "failed attachment already replaced the visible Session"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn idle_interactions_back_off_and_drop_stops_polling() {
        let concrete = Arc::new(UnknownThenAcceptedHandle::default());
        let (renderer, _rendered) = tokio::sync::mpsc::channel(32);
        let watcher = spawn_interactions(concrete.clone(), renderer, &CancellationToken::new());
        tokio::task::yield_now().await;
        assert_eq!(concrete.interaction_polls.load(Ordering::SeqCst), 1);
        for _ in 0..8 {
            tokio::time::advance(Duration::from_millis(250)).await;
            tokio::task::yield_now().await;
        }
        let polls = concrete.interaction_polls.load(Ordering::SeqCst);
        assert!(
            polls <= 4,
            "idle attachment made {polls} refreshes in two seconds"
        );
        *concrete.pending_question.lock().unwrap() = Some(QuestionRequest {
            id: "request".into(),
            session_id: "session".into(),
            turn_id: "turn".into(),
            questions: vec![rsi_user_questions_protocol::Question {
                id: "question".into(),
                prompt: "choose".into(),
                options: vec!["one".into()],
            }],
        });
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        let changed_at = concrete.interaction_polls.load(Ordering::SeqCst);
        assert!(changed_at > polls);
        tokio::time::advance(Duration::from_millis(250)).await;
        tokio::task::yield_now().await;
        let polls = concrete.interaction_polls.load(Ordering::SeqCst);
        assert_eq!(
            polls,
            changed_at + 1,
            "new interaction did not restore prompt refresh"
        );
        drop(watcher);
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert_eq!(concrete.interaction_polls.load(Ordering::SeqCst), polls);
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_interaction_retries_explain_how_to_resume_refresh() {
        let concrete = Arc::new(UnknownThenAcceptedHandle::default());
        concrete.interaction_failures.store(5, Ordering::SeqCst);
        let (renderer, mut events) = tokio::sync::mpsc::channel(32);
        let _watcher = spawn_interactions(concrete.clone(), renderer, &CancellationToken::new());
        for _ in 0..6 {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(2)).await;
        }
        assert_eq!(concrete.interaction_polls.load(Ordering::SeqCst), 5);
        let mut explained = false;
        while let Ok(message) = events.try_recv() {
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
        let observer = observe(
            fixture.clone(),
            ObservationCursor {
                fact_seq: 0,
                control_seq: 0,
            },
            renderer,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(message) = events.recv().await {
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
        observer.stop().await;
    }

    #[tokio::test]
    async fn owned_cancellation_visits_later_messages_after_missing_or_failed_lookup() {
        for failed in [false, true] {
            let fixture = Arc::new(UnknownThenAcceptedHandle {
                query_finds_message: true,
                first_status_error: Some(if failed {
                    SessionApplicationError::Backend("first lookup failed".into())
                } else {
                    SessionApplicationError::NotFound("a-first".into())
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
