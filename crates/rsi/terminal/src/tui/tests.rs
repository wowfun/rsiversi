use super::*;
use crate::tests::{UnknownThenAcceptedHandle, UnusedWorkspace};

#[derive(Debug)]
struct Application(Arc<UnknownThenAcceptedHandle>);

#[async_trait::async_trait]
impl SessionService for Application {
    async fn create(
        &self,
        _: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        Ok(self.0.clone())
    }
    async fn attach(&self, _: &SessionId) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        Ok(self.0.clone())
    }
    async fn list_recent(
        &self,
        _: Option<&rsi_session_protocol::RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
        unreachable!("not used")
    }
}

struct TestSurface {
    observer: crate::surfaces::Observer,
    _receiver: mpsc::Receiver<CliRenderMessage>,
}
impl TestSurface {
    async fn stop(self) {
        self.observer.stop().await.unwrap();
    }
}

async fn client() -> (
    Client,
    Arc<UnknownThenAcceptedHandle>,
    rsi_meta::Runtime,
    TestSurface,
) {
    let handle = Arc::new(UnknownThenAcceptedHandle::default());
    client_with(handle).await
}

async fn client_with(
    handle: Arc<UnknownThenAcceptedHandle>,
) -> (
    Client,
    Arc<UnknownThenAcceptedHandle>,
    rsi_meta::Runtime,
    TestSurface,
) {
    let attached = attachment(handle.clone(), false).await.unwrap();
    let (events, receiver) = mpsc::channel(32);
    let (runtime, surfaces) = crate::surfaces::fixture(handle.clone(), &events).await;
    let surface = surfaces
        .open(attached.header.session_id(), None, 0)
        .await
        .unwrap();
    (
        Client::new(
            Arc::new(Application(handle.clone())),
            handle.clone(),
            handle.clone(),
            Arc::new(UnusedWorkspace),
            attached,
            false,
            surface.controller.clone(),
        ),
        handle,
        runtime,
        TestSurface {
            observer: surface,
            _receiver: receiver,
        },
    )
}

#[tokio::test(start_paused = true)]
async fn pending_menu_waits_for_transient_read_capacity_without_losing_the_draft() {
    use rsi_agent_store_protocol::{StoreAgentSessionStatus, StoreAgentSubtreeSnapshot};
    let (mut client, handle, runtime, surface) = client().await;
    *handle.inspection.lock().unwrap() = Some(StoreSessionInspection {
        header: client.state.header.clone(),
        durable_fact_seq: 0,
        durable_control_seq: 0,
        pending: Vec::new(),
        active_turn_id: None,
        activation_phase: None,
        tree: StoreAgentSubtreeSnapshot {
            session: StoreAgentSessionStatus {
                session_id: client.state.header.session_id().clone(),
                durable_control_seq: 0,
                has_open_turn: false,
                has_active_activation: false,
                has_waking_message: false,
            },
            descendants: Vec::new(),
        },
    });
    handle
        .inspection_capacity
        .store(2, std::sync::atomic::Ordering::SeqCst);
    client.state.editor.insert("preserved draft").unwrap();
    client.action(Action::Queue);
    let update = client.tasks.next().await.unwrap().result.unwrap();
    assert!(matches!(update, Update::Menu(Menu { title, .. }) if title == "Pending inputs"));
    assert_eq!(client.state.editor.text, "preserved draft");
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn explicit_retry_queries_before_replaying_the_frozen_submission() {
    use std::sync::atomic::Ordering;
    for found in [true, false] {
        let handle = Arc::new(UnknownThenAcceptedHandle {
            query_finds_message: found,
            status_error: std::sync::Mutex::new(Some(SessionError::Backend(
                "query offline".into(),
            ))),
            ..Default::default()
        });
        let (mut client, handle, runtime, surface) = client_with(handle).await;
        client.state.editor.insert("effect once").unwrap();
        client.submit(MessageDelivery::NextTurn, false);
        let update = client.tasks.next().await.unwrap().result.unwrap();
        assert!(matches!(
            update,
            Update::Submitted(Err(SessionError::MessageOutcomeUnknown { .. }))
        ));
        client.submission.busy = false;
        let frozen = client.submission.request.clone().unwrap();
        client.state.editor.insert("next draft").unwrap();
        client.action(Action::Retry);
        let update = client.tasks.next().await.unwrap().result.unwrap();
        assert!(matches!(
            update,
            Update::Submitted(Err(SessionError::MessageOutcomeUnknown { .. }))
        ));
        client.submission.busy = false;
        assert_eq!(handle.submitted_requests.lock().unwrap().len(), 1);
        assert_eq!(handle.queries.load(Ordering::SeqCst), 2);
        *handle.status_error.lock().unwrap() = None;
        client.action(Action::Retry);
        let update = client.tasks.next().await.unwrap().result.unwrap();
        assert!(matches!(update, Update::Submitted(Ok(_))));
        client.submission.busy = false;
        let requests = handle.submitted_requests.lock().unwrap().clone();
        assert_eq!(requests.len(), if found { 1 } else { 2 });
        assert!(
            requests
                .iter()
                .all(|request| serde_json::to_value(request).unwrap()
                    == serde_json::to_value(&frozen).unwrap())
        );
        assert_eq!(client.state.editor.text, "next draft");
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn submission_reconciliation_freezes_id_content_and_model_while_editor_changes() {
    let (mut client, handle, runtime, surface) = client().await;
    let model = ModelRef::new("first", "model-a").unwrap();
    client.state.model = Some(model.clone());
    client.state.editor.insert("original input").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    let frozen = client.submission.request.clone().unwrap();
    client.state.model = Some(ModelRef::new("second", "model-b").unwrap());
    client.state.editor.insert("next draft").unwrap();
    let result = client.tasks.next().await.unwrap().result.unwrap();
    assert!(matches!(result, Update::Submitted(Ok(_))));
    let requests = handle.submitted_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    for request in &*requests {
        assert_eq!(request.message_id, frozen.message_id);
        assert_eq!(request.model, Some(model.clone()));
        assert!(
            matches!(&request.content[0], MessageInput::Text { text } if text == "original input")
        );
    }
    assert_eq!(client.state.editor.text, "next draft");
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn idle_steer_is_rejected_locally_and_active_steer_never_carries_model_override() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.editor.insert("steering").unwrap();
    client.state.model = Some(ModelRef::new("chosen", "model").unwrap());
    client.submit(MessageDelivery::Steer, false);
    assert!(client.submission.request.is_none());
    assert_eq!(client.state.editor.text, "steering");
    client.state.active = true;
    client.submit(MessageDelivery::Steer, false);
    assert!(matches!(
        client.tasks.next().await.unwrap().result.unwrap(),
        Update::Submitted(Ok(_))
    ));
    let requests = handle.submitted_requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .all(|request| request.delivery == MessageDelivery::Steer
                && request.model.is_none()
                && request.sandbox.is_none())
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn cancel_keeps_draft_and_targets_attached_turn_and_only_owned_pending_messages() {
    use rsi_agent_store_protocol::{
        StoreAgentSessionStatus, StoreAgentSubtreeSnapshot, StorePendingMessage,
    };
    let (mut client, handle, runtime, surface) = client().await;
    let header = client.state.header.clone();
    let own = MessageId::new("owned").unwrap();
    let other = MessageId::new("foreign").unwrap();
    let pending = [own.clone(), other.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, message_id)| StorePendingMessage {
            message_id,
            delivery: MessageDelivery::NextTurn,
            target: rsi_agent_session_protocol::MessageTarget::NextTurn,
            permits_promotion: false,
            bound_turn_id: None,
            accepted_control_seq: u64::try_from(index + 1).unwrap(),
        })
        .collect();
    let turn = TurnId::new("attached-turn").unwrap();
    let status = StoreAgentSessionStatus {
        session_id: header.session_id().clone(),
        durable_control_seq: 2,
        has_open_turn: true,
        has_active_activation: false,
        has_waking_message: true,
    };
    *handle.inspection.lock().unwrap() = Some(StoreSessionInspection {
        header,
        durable_fact_seq: 1,
        durable_control_seq: 2,
        pending,
        active_turn_id: Some(turn.clone()),
        activation_phase: None,
        tree: StoreAgentSubtreeSnapshot {
            session: status,
            descendants: Vec::new(),
        },
    });
    client.owned.insert(own.clone());
    client.state.editor.insert("preserved draft").unwrap();
    client.state.menu = Some(Menu::actions());
    client.cancel();
    assert!(client.tasks.next().await.unwrap().result.is_ok());
    let targets = handle.cancellations.lock().unwrap().clone();
    assert_eq!(
        *targets,
        [CancelTarget::Turn(turn), CancelTarget::Message(own)]
    );
    assert_eq!(client.state.editor.text, "preserved draft");
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn stale_question_draft_is_not_dispatched_and_empty_draft_does_not_prefetch() {
    let (mut client, _, runtime, surface) = client().await;
    assert!(!client.history.backfill);
    let request = rsi_user_questions_protocol::QuestionRequest {
        id: "request".into(),
        session_id: client.state.header.session_id().to_string(),
        turn_id: "turn".into(),
        questions: vec![rsi_user_questions_protocol::Question {
            id: "question".into(),
            prompt: "Choose".into(),
            options: vec!["A".into(), "B".into()],
        }],
    };
    let mut editor = editor::Editor::default();
    editor.insert("2").unwrap();
    client.state.answer = Some(state::Answer {
        scroll: 0,
        request,
        editor,
        answers: Vec::new(),
    });
    client.interactions = Some(
        rsi_session_protocol::InteractionRetention::default()
            .retain(Vec::new(), Vec::new())
            .unwrap(),
    );
    client.answer();
    assert!(client.state.answer.is_none());
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn bounded_requests_reserve_submission_and_cancellation_capacity() {
    let (mut client, _, runtime, surface) = client().await;
    for _ in 0..20 {
        client.spawn(std::future::pending());
    }
    assert_eq!(client.tasks.len(), 8);
    client.state.editor.insert("durable input").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    assert_eq!(client.tasks.len(), 9);
    assert!(client.submission.busy);
    client.cancel();
    assert_eq!(client.tasks.len(), 10);
    assert!(client.submission.cancel_when_accepted);
    assert!(client.cancelling);
    client.cancel();
    assert_eq!(client.tasks.len(), 10);
    assert!(client.cancellation_queued);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn saturated_queue_keeps_unknown_submission_identity_and_view_close_invalidates_reads() {
    let (mut client, _, runtime, surface) = client().await;
    client.state.editor.insert("original").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    let frozen = client.submission.request.clone().unwrap();
    client.tasks.clear();
    client.submission.busy = false;
    for _ in 0..12 {
        client.spawn_as(WorkKind::Cancel, std::future::pending());
    }
    client.submit(MessageDelivery::NextTurn, true);
    assert!(
        !client.submission.rejected,
        "unknown is never reclassified as definite rejection"
    );
    assert!(!client.submission.busy);
    assert_eq!(
        client.submission.request.as_ref().unwrap().message_id,
        frozen.message_id
    );
    client.tasks.clear();
    client.spawn(async { Ok(Update::Menu(Menu::actions())) });
    client.state.escape();
    let work = client.tasks.next().await.unwrap();
    assert_ne!(work.view_revision, client.state.view_revision);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

fn history_fact(seq: u64) -> SessionFact {
    SessionFact::new(
        seq,
        1,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
            text: format!("history-{seq}"),
            model: Some(ModelRef::new("test", "model").unwrap()),
            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn full_history_window_advances_backwards_and_end_restores_live_output() {
    let (mut client, _, runtime, surface) = client().await;
    for seq in 1000..1512 {
        client.state.transcript.apply(&history_fact(seq));
    }
    client.history.before = Some(1000);
    client.history(true);
    for seq in [999, 998] {
        client.page(rsi_session_protocol::SessionHistoryPage {
            before_seq: seq + 1,
            durable_seq: 1512,
            facts: vec![history_fact(seq)],
            has_more: true,
        });
        assert_eq!(client.state.transcript.blocks[0].first, seq);
        assert_eq!(client.state.transcript.blocks.len(), 512);
        client.history(true);
        assert_eq!(client.history.before, Some(seq));
    }
    // A new authoritative Fact advances only the retained live view while browsing.
    client.live_fact(&history_fact(1512));
    assert_eq!(client.state.transcript.blocks[0].first, 998);
    client.follow_live();
    assert_eq!(client.state.transcript.blocks.last().unwrap().first, 1512);
    assert_eq!(client.state.transcript.blocks.len(), 512);
    assert!(client.state.top.is_none());
    assert!(!client.history.backfill);
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn scrolling_resolves_the_rendered_source_after_projection_replacement() {
    let (mut client, _, runtime, surface) = client().await;
    for seq in 1..40 {
        client.state.transcript.apply(&history_fact(seq));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let mut view = render::View::default();
    terminal
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    client.state.transcript = transcript::Transcript::default();
    client.state.transcript.apply(&history_fact(100));
    client.scroll(&view, true);
    client.scroll(&view, false);
    assert!(
        client.state.top.is_none(),
        "evicted source must not select an unrelated block"
    );
    assert!(
        client.tasks.is_empty(),
        "stale view must not issue a history request"
    );
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn scroll_anchors_survive_history_prepend_and_reject_another_session() {
    let (mut client, _, runtime, surface) = client().await;
    for seq in 100..140 {
        client.state.transcript.apply(&history_fact(seq));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let mut view = render::View::default();
    terminal
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    let (block, offset) = view.location(&client.state, 3).unwrap();
    let expected = client.state.transcript.blocks[block].anchor(offset);
    client.state.transcript.apply_history(&history_fact(1));
    client.scroll(&view, false);
    assert_eq!(client.state.top, expected);
    let mut header = serde_json::to_value(&client.state.header).unwrap();
    header["session_id"] = serde_json::json!("another-session");
    client.state.header = serde_json::from_value(header).unwrap();
    assert!(!view.belongs_to(&client.state));
    client.state.top = None;
    client.scroll(&view, false);
    assert!(client.state.top.is_none());
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn accepted_message_detail_is_bounded_and_keeps_its_cancellation_identity() {
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, AgentMessageSource, MAXIMUM_TURN_TEXT_BYTES,
        MessageOptions,
    };
    let (mut client, _, runtime, surface) = client().await;
    let message = AgentMessage {
        message_id: MessageId::new("large-input").unwrap(),
        source: AgentMessageSource::Human,
        content: vec![AgentMessageContent::Text {
            text: "x".repeat(MAXIMUM_TURN_TEXT_BYTES),
        }],
        options: MessageOptions::default(),
    };
    message.validate().unwrap();
    client.message_detail(message);
    let detail = client.state.detail.as_ref().unwrap();
    assert!(detail.len() < 257 * 1024);
    assert!(detail.contains("JSON display window truncated"));
    assert!(
        matches!(&client.state.detail_actions.as_ref().unwrap().items[0].1, Action::CancelMessage(id) if id.as_str() == "large-input")
    );
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
