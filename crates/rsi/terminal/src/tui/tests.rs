use super::*;
use crate::tests::{UnknownThenAcceptedHandle, UnusedWorkspace};

mod ui;

#[tokio::test]
async fn command_completion_is_discovered_undoable_and_inert_after_draft_change() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.editor.insert("/pl").unwrap();
    assert!(client.complete_command());
    let work = client.tasks.next().await.unwrap();
    let Update::Completions { prefix, names } = work.result.unwrap() else {
        panic!("discovered completions")
    };
    client.command_completions(&prefix, names);
    assert_eq!(client.state.editor.text, "/plan ");
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    let mut undo: termina::event::KeyEvent = KeyCode::Char('z').into();
    undo.modifiers = Modifiers::CONTROL;
    client.state.editor.key(undo).unwrap();
    assert_eq!(client.state.editor.text, "/pl");
    assert!(client.complete_command());
    client.state.editor.insert("changed").unwrap();
    let work = client.tasks.next().await.unwrap();
    let Update::Completions { prefix, names } = work.result.unwrap() else {
        panic!("discovered completions")
    };
    client.command_completions(&prefix, names);
    assert_eq!(client.state.editor.text, "/plchanged");
    assert!(client.state.menu.is_none());
    let notice = client.state.status.clone();
    client.command_completions("/pl", Err(error("stale discovery error")));
    assert_eq!(client.state.status, notice);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn recalling_only_this_sessions_prompt_is_an_edit_without_submission() {
    let (mut client, handle, runtime, surface) = client().await;
    client
        .prompts
        .remember(client.state.header.session_id(), "first\nsecond");
    client
        .prompts
        .remember(&SessionId::new("another-session").unwrap(), "foreign text");
    client.state.editor.insert("unfinished draft").unwrap();
    client.prompt_menu();
    let menu = client.state.menu.take().unwrap();
    assert_eq!(menu.items.len(), 1);
    client.action(menu.items[0].1.clone());
    assert_eq!(client.state.editor.text, "first\nsecond");
    let mut undo: termina::event::KeyEvent = KeyCode::Char('z').into();
    undo.modifiers = Modifiers::CONTROL;
    client.state.editor.key(undo).unwrap();
    assert_eq!(client.state.editor.text, "unfinished draft");
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn registered_slash_command_retains_unknown_identity_without_submitting_a_message() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.editor.insert("/plan on").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    let work = client.tasks.next().await.unwrap();
    let Update::Command(result) = work.result.unwrap() else {
        panic!("Session command path")
    };
    assert!(result.is_err());
    client.command_finished(result);
    assert_eq!(client.state.editor.text, "/plan on");
    client.prompt_menu();
    assert_eq!(client.state.menu.take().unwrap().items[0].0, "/plan on");
    assert!(client.submission.request.is_none());
    assert!(client.owned.is_empty());
    let original = client.command.view().pending.unwrap();
    assert_eq!(original.arguments.value(), "on");
    client.submit(MessageDelivery::NextTurn, false);
    assert!(client.tasks.is_empty());
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    assert_eq!(handle.commands.lock().unwrap().len(), 1);
    *handle.command_receipt.lock().unwrap() = Some(
        rsi_agent_session_protocol::SessionCommandReceipt::draft_changed(&original, "a".repeat(64))
            .unwrap(),
    );
    client.action(Action::CommandResult);
    let work = client.tasks.next().await.unwrap();
    assert!(matches!(work.result, Ok(Update::Notice(_))));
    assert!(client.command.view().pending.is_none());
    assert_eq!(
        client.command.view().receipt.unwrap().request_id(),
        &original.request_id
    );
    assert_eq!(handle.commands.lock().unwrap().len(), 1);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn extension_inspection_tracks_replacements_without_reopening_a_closed_detail() {
    use rsi_agent_session_protocol::{
        ContributionId, ProjectionCursor, ProjectionEntry, ProjectionValue,
        SessionProjectionSnapshot,
    };
    let (mut client, _, runtime, surface) = client().await;
    let pool = rsi_session_protocol::ProjectionRetention::default();
    let snapshot = |revision| {
        pool.reserve_capture()
            .unwrap()
            .retain(
                SessionProjectionSnapshot::new(
                    client.state.header.session_id().clone(),
                    "a".repeat(64),
                    "b".repeat(64),
                    ProjectionCursor::Draft { revision },
                    vec![
                        ProjectionEntry::value(
                            ContributionId::new("fixture.good").unwrap(),
                            ProjectionValue::new(serde_json::json!({"revision":revision})).unwrap(),
                        ),
                        ProjectionEntry::failed(
                            ContributionId::new("fixture.failed").unwrap(),
                            "isolated failure",
                        )
                        .unwrap(),
                    ],
                )
                .unwrap(),
            )
            .unwrap()
    };
    let first = snapshot(0);
    let second = snapshot(1);
    client.projections = Some(first);
    client.action(Action::Extensions);
    assert_eq!(client.state.menu.as_ref().unwrap().items.len(), 2);
    client.action(Action::Extension("fixture.good".into()));
    assert!(
        client
            .state
            .detail
            .as_ref()
            .unwrap()
            .contains("revision: 0")
    );
    client.projections = Some(second);
    client.refresh_extensions();
    assert!(
        client
            .state
            .detail
            .as_ref()
            .unwrap()
            .contains("revision: 1")
    );
    assert_eq!(
        pool.retained_bytes(),
        client
            .projections
            .as_ref()
            .unwrap()
            .snapshot()
            .encoded_len()
            .unwrap()
    );
    client.state.detail = None;
    client.refresh_extensions();
    assert!(client.state.detail.is_none());
    assert!(client.extension_view.is_none());
    client.action(Action::Extension("fixture.failed".into()));
    assert!(
        client
            .state
            .detail
            .as_ref()
            .unwrap()
            .contains("Producer failed: isolated failure")
    );
    client.action(Action::CommandResult);
    assert!(client.extension_view.is_none());
    client.projections = None;
    assert_eq!(pool.retained_bytes(), 0);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn closing_source_detail_cancels_its_owned_io_and_fences_the_late_error() {
    let fixture = Arc::new(UnknownThenAcceptedHandle {
        read_gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
        ..UnknownThenAcceptedHandle::default()
    });
    let (mut client, handle, runtime, surface) = client_with(fixture).await;
    client.state.open_detail("old detail".into());
    client.action(Action::Window(
        rsi_conversation::SourceRef {
            seq: 9,
            field: rsi_conversation::FactField::TurnOutcome,
        },
        0,
    ));
    assert!(futures_util::poll!(client.tasks.next()).is_pending());
    tokio::time::timeout(Duration::from_secs(1), async {
        while handle.read_active.load(std::sync::atomic::Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    client.state.escape();
    let work = tokio::time::timeout(Duration::from_secs(1), client.tasks.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        handle.read_active.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(work.result.is_err());
    assert!(work.superseded(&client));
    assert!(client.state.detail.is_none());
    assert!(handle.cancellations.lock().unwrap().is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn closing_other_details_drops_io_and_fences_errors_without_cancelling_mutations() {
    for action in [
        Action::Output("fixture-output".into(), 0),
        Action::Child(SessionId::new("fixture-child").unwrap()),
        Action::Message(rsi_agent_store_protocol::StorePendingMessage {
            message_id: MessageId::new("fixture-message").unwrap(),
            delivery: MessageDelivery::NextTurn,
            target: rsi_agent_session_protocol::MessageTarget::NextTurn,
            permits_promotion: false,
            bound_turn_id: None,
            accepted_control_seq: 1,
        }),
    ] {
        let fixture = Arc::new(UnknownThenAcceptedHandle {
            read_gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
            ..Default::default()
        });
        let (mut client, handle, runtime, surface) = client_with(fixture).await;
        client.state.open_detail("previous detail".into());
        client.action(action);
        assert!(futures_util::poll!(client.tasks.next()).is_pending());
        assert_eq!(
            handle.read_active.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        client.state.escape();
        let work = tokio::time::timeout(Duration::from_secs(1), client.tasks.next())
            .await
            .expect("closed detail kept its I/O alive")
            .unwrap();
        assert_eq!(
            handle.read_active.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(work.result.is_err());
        assert!(work.superseded(&client));
        assert!(handle.cancellations.lock().unwrap().is_empty());
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

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
    let (runtime, surfaces) = crate::surfaces::fixture(handle.clone(), &events, true).await;
    let surface = surfaces
        .open(attached.header.session_id(), None, 0)
        .await
        .unwrap();
    (
        Client::new(
            Services {
                application: Arc::new(Application(handle.clone())),
                output_cache: handle.clone(),
                model_catalog: handle.clone(),
                workspace: Arc::new(UnusedWorkspace),
                lifetime: rsi_client::ConnectionLifetime::Embedded,
                ui: runtime.root().lookup_local::<rsi_ui::UiContract>().unwrap(),
                ui_target: runtime
                    .root()
                    .lookup_local::<rsi_ui::UiTargetContract>()
                    .unwrap(),
            },
            attached,
            surface.controller.clone(),
            surface.ui_target.clone().unwrap(),
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
