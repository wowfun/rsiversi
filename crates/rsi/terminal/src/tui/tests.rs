use super::*;
use crate::tests::{UnknownThenAcceptedHandle, UnusedWorkspace};

mod references;
mod ui;
mod workspace;

#[tokio::test]
async fn effort_command_completes_and_dispatches_without_a_model_request() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.editor.insert("/eff").unwrap();
    client
        .state
        .slash
        .update(&client.state.editor, Some(&client.controller));
    client.state.slash.next().await;
    assert_eq!(
        client.state.slash.popup.as_ref().unwrap().items[0].0,
        "/effort"
    );
    assert!(
        client
            .state
            .slash
            .key(KeyCode::Tab.into(), &mut client.state.editor)
    );
    assert_eq!(client.state.editor.text(), "/effort");
    client.submit(MessageDelivery::NextTurn, false);
    assert!(matches!(client.setup_command, Some(setup::Command::Effort)));
    assert!(client.state.editor.text().is_empty());
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn local_status_has_no_source_or_model_submission_and_is_bounded() {
    let (mut client, handle, runtime, surface) = client().await;
    for i in 0..40 {
        client.state.info(format!("Switched to model-{i}"));
    }
    assert_eq!(client.state.transcript.blocks.len(), 32);
    assert!(
        client
            .state
            .transcript
            .blocks
            .iter()
            .all(|block| block.role == transcript::Role::Notice
                && block.pieces.is_empty()
                && block.anchor(0).is_none())
    );
    client.state.notice("Authentication failed; use /login");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            render::draw(frame, &client.state);
        })
        .unwrap();
    let cells = &terminal.backend().buffer().content;
    let row = cells
        .chunks(80)
        .find(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("Authentication failed")
        })
        .unwrap();
    assert!(
        row.iter()
            .take(37)
            .all(|cell| cell.fg == ratatui::style::Color::LightRed)
    );
    client.state.editor.insert("only this prompt").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    client.tasks.next().await.unwrap();
    let requests = handle.submitted_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "the fixture retries one unknown receipt");
    for request in requests {
        assert!(
            matches!(request.content.as_slice(), [MessageInput::Text { text }] if text == "only this prompt")
        );
    }
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn internal_model_selection_is_rejected_before_dispatch() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.editor.insert("/model-selection").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    assert!(client.state.status.contains("/model or /effort"));
    assert_eq!(client.state.editor.text(), "/model-selection");
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn visible_timer_requires_confirmed_activity_and_drops_other_turn_start() {
    let (mut client, _, runtime, surface) = client().await;
    let turn = rsi_agent_session_protocol::TurnId::new("clock").unwrap();
    client.state.model_fact(
        &SessionFact::new(
            1,
            1000,
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "start".into(),
                model: None,
                reasoning_effort: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    assert!(!client.state.tick_activity(2000));
    client.state.active = true;
    client.state.live_turn = Some(turn);
    assert!(client.state.tick_activity(2000));
    assert_eq!(
        client.state.activity.as_ref().unwrap().elapsed_ms(),
        Some(1000)
    );
    client.state.live_turn = Some(rsi_agent_session_protocol::TurnId::new("foreign").unwrap());
    client.state.tick_activity(2001);
    assert_eq!(client.state.activity.as_ref().unwrap().elapsed_ms(), None);
    client.state.active = false;
    assert!(client.state.tick_activity(2002));
    assert!(client.state.activity.is_none());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn beginning_of_history_is_silent_and_preserves_existing_feedback() {
    let (mut client, _, runtime, surface) = client().await;
    client.history.before = None;
    client.history(true);
    assert!(client.state.status.is_empty());
    client.state.notice("A real failure");
    client.history(true);
    assert_eq!(client.state.status, "A real failure");
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Source-less and durable prefixes cross narrow/wide serialized fold frames.
async fn expanding_a_process_preserves_its_visible_row_even_when_content_overflows() {
    let output = format!("{}OUTPUT-END", "expanded line\n".repeat(80));
    for (width, height) in [(80, 24), (28, 9)] {
        for notice in [false, true] {
            for role in [transcript::Role::Reasoning, transcript::Role::Tool] {
                let (mut client, _, runtime, surface) = client().await;
                if notice {
                    client
                        .state
                        .transcript
                        .push_notice(1, "Local status before process");
                } else {
                    client.state.transcript.apply(&history_fact(1));
                }
                if role == transcript::Role::Tool {
                    let turn_id = rsi_agent_session_protocol::TurnId::new("fold").unwrap();
                    let effect_id = rsi_agent_session_protocol::EffectId::new("tool").unwrap();
                    let identity = rsi_tools_protocol::ToolResultIdentity::new(
                        "owner",
                        "invoke",
                        "call",
                        "a".repeat(64),
                    )
                    .unwrap();
                    client.state.transcript.apply(
                        &SessionFact::new(
                            2,
                            2,
                            SessionFactBody::ToolIntent {
                                turn_id: turn_id.clone(),
                                effect_id: effect_id.clone(),
                                identity: identity.clone(),
                                source_model_effect_id: rsi_agent_session_protocol::EffectId::new(
                                    "model",
                                )
                                .unwrap(),
                                name: "bash".into(),
                                arguments: serde_json::json!({"command":"true"}),
                                approval: None,
                                parallel_safe: false,
                            },
                        )
                        .unwrap(),
                    );
                    client.state.transcript.apply(
                        &SessionFact::new(
                            3,
                            3,
                            SessionFactBody::ToolResult {
                                turn_id,
                                effect_id,
                                identity,
                                result: rsi_tools_protocol::ToolResult::new(
                                    serde_json::json!({"exit_code":0}),
                                    vec![rsi_tools_protocol::ToolContent::Text {
                                        text: output.clone(),
                                    }],
                                    false,
                                )
                                .unwrap(),
                            },
                        )
                        .unwrap(),
                    );
                } else {
                    client.state.transcript.apply(
                        &SessionFact::new(
                            2,
                            2,
                            SessionFactBody::ModelEvent {
                                turn_id: rsi_agent_session_protocol::TurnId::new("fold").unwrap(),
                                effect_id: rsi_agent_session_protocol::EffectId::new("request")
                                    .unwrap(),
                                purpose:
                                    rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                                event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                                    index: 0,
                                    delta: rsi_ai_protocol::ContentDelta::Reasoning(output.clone()),
                                },
                            },
                        )
                        .unwrap(),
                    );
                }
                let block = client.state.transcript.blocks.last_mut().unwrap();
                block.role = role;
                block.title = "Process to expand".into();
                block.completed = true;
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                let mut view = render::View::default();
                terminal
                    .draw(|frame| view = render::draw(frame, &client.state))
                    .unwrap();
                let row = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .chunks(usize::from(width))
                        .position(|cells| {
                            cells
                                .iter()
                                .map(ratatui::buffer::Cell::symbol)
                                .collect::<String>()
                                .contains("Process to expand")
                        })
                        .unwrap_or_else(|| panic!("missing summary at {width}x{height} notice={notice} role={role:?}: {:?}", terminal.backend().buffer().content.chunks(usize::from(width)).map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect::<String>()).collect::<Vec<_>>()))
                };
                let before = row(&terminal);
                assert!(before > 0);
                assert!(client.toggle_fold_at(&view, 2, u16::try_from(before).unwrap()));
                terminal
                    .draw(|frame| view = render::draw(frame, &client.state))
                    .unwrap();
                assert_eq!(
                    row(&terminal),
                    before,
                    "expansion must retain the clicked row"
                );
                assert!(client.toggle_fold_at(&view, 2, u16::try_from(before).unwrap()));
                terminal
                    .draw(|frame| view = render::draw(frame, &client.state))
                    .unwrap();
                assert_eq!(
                    row(&terminal),
                    before,
                    "collapse must retain the clicked row"
                );
                assert!(client.toggle_fold_at(&view, 2, u16::try_from(before).unwrap()));
                terminal
                    .draw(|frame| view = render::draw(frame, &client.state))
                    .unwrap();
                let contains = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
                                needle: &str| {
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .chunks(usize::from(width))
                        .any(|cells| {
                            cells
                                .iter()
                                .map(ratatui::buffer::Cell::symbol)
                                .collect::<String>()
                                .contains(needle)
                        })
                };
                for _ in 0..40 {
                    client.scroll(&view, false);
                    terminal
                        .draw(|frame| view = render::draw(frame, &client.state))
                        .unwrap();
                    if contains(&terminal, "OUTPUT-END") {
                        break;
                    }
                }
                assert!(
                    contains(&terminal, "OUTPUT-END"),
                    "expanded output must remain reachable: {width}x{height} notice={notice} role={role:?}, top={:?}, first={:?}, fourth={:?}",
                    client.state.top,
                    view.location(&client.state, 0),
                    view.location(&client.state, 3)
                );
                for _ in 0..40 {
                    client.scroll(&view, true);
                    terminal
                        .draw(|frame| view = render::draw(frame, &client.state))
                        .unwrap();
                    if contains(&terminal, "Process to expand") {
                        break;
                    }
                }
                assert!(
                    contains(&terminal, "Process to expand"),
                    "scrolling up must return to the expanded summary: {width}x{height} notice={notice} role={role:?}, top={:?}, first={:?}",
                    client.state.top,
                    view.location(&client.state, 0)
                );
                surface.stop().await;
                assert!(runtime.shutdown().await.is_clean());
            }
        }
    }
}

#[tokio::test]
async fn thinking_click_uses_acknowledged_sources_and_never_copies_the_header() {
    let (mut client, _, runtime, surface) = client().await;
    client.state.transcript.apply(
        &SessionFact::new(
            1,
            1,
            SessionFactBody::ModelEvent {
                turn_id: rsi_agent_session_protocol::TurnId::new("thinking").unwrap(),
                effect_id: rsi_agent_session_protocol::EffectId::new("model").unwrap(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event: rsi_ai_protocol::LanguageEvent::ContentDelta {
                    index: 0,
                    delta: rsi_ai_protocol::ContentDelta::Reasoning(
                        "first row\nsecond row\nthird row".into(),
                    ),
                },
            },
        )
        .unwrap(),
    );
    client.state.transcript.blocks[0].completed = true;
    let mut screen = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let mut view = render::View::default();
    screen
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    assert!(view.hit(2, 0, false).is_none());
    assert!(client.toggle_fold_at(&view, 2, 0));
    assert!(!client.state.transcript.blocks[0].collapsed);
    assert!(
        !client.toggle_fold_at(&view, 2, 0),
        "stale revision cannot toggle again"
    );
    client.copy(&view);
    assert!(client.tasks.is_empty());
    screen
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    assert!(
        screen
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
            .contains("second row")
    );
    assert!(
        !client.toggle_fold_at(&view, 2, 1),
        "content selection stays text selection"
    );
    assert!(client.toggle_fold_at(&view, 7, 0));
    assert!(client.state.transcript.blocks[0].collapsed);
    screen
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    client.state.transcript = transcript::Transcript::default();
    assert!(
        !client.toggle_fold_at(&view, 2, 0),
        "evicted source cannot be toggled"
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn live_completion_is_display_only_and_keeps_session_commands_out_of_actions() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.editor.insert("/pl").unwrap();
    client
        .state
        .slash
        .update(&client.state.editor, Some(&client.controller));
    client.state.slash.next().await;
    assert_eq!(
        client.state.slash.popup.as_ref().unwrap().items[0].0,
        "/plan"
    );
    assert!(
        client
            .state
            .slash
            .key(KeyCode::Enter.into(), &mut client.state.editor)
    );
    assert_eq!(client.state.editor.text(), "/plan");
    assert!(client.state.menu.is_none());
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
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
    assert_eq!(client.state.editor.text(), "first\nsecond");
    assert!(client.state.status.is_empty());
    let mut undo: termina::event::KeyEvent = KeyCode::Char('z').into();
    undo.modifiers = Modifiers::CONTROL;
    client.state.editor.key(undo).unwrap();
    assert_eq!(client.state.editor.text(), "unfinished draft");
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn copying_without_a_selection_preserves_draft_and_existing_error_without_work() {
    let (mut client, _, runtime, surface) = client().await;
    client.state.editor.insert("unfinished draft").unwrap();
    client.copy(&render::View::default());
    assert!(client.state.status.is_empty());
    client.state.notice("A real failure");
    client.copy(&render::View::default());
    assert_eq!(client.state.status, "A real failure");
    assert_eq!(client.state.editor.text(), "unfinished draft");
    assert!(client.tasks.is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn copy_uses_the_focused_detail_or_session_entry_without_falling_through_a_menu() {
    let (mut client, _, runtime, surface) = client().await;
    client.state.open_detail("Visible detail".into());
    client.action_menu();
    client.copy(&render::View::default());
    assert!(
        client.tasks.is_empty(),
        "an action menu must not copy its hidden detail"
    );
    client.state.escape();
    client.copy(&render::View::default());
    assert_eq!(client.tasks.len(), 1, "visible detail retains copy support");
    client.tasks.clear();
    client.state.menu = Some(Menu {
        title: "Recent sessions".into(),
        items: vec![(
            "saved-session".into(),
            Action::Attach(SessionId::new("saved-session").unwrap()),
        )],
        selected: 0,
    });
    client.copy(&render::View::default());
    assert_eq!(
        client.tasks.len(),
        1,
        "the selected session ID retains copy support"
    );
    client.tasks.clear();
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
    assert_eq!(client.state.editor.text(), "/plan on");
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

#[tokio::test]
async fn closing_preview_drops_its_only_pending_read_and_stops_future_ticks() {
    let fixture = Arc::new(UnknownThenAcceptedHandle {
        read_gate: Some(Arc::new(tokio::sync::Semaphore::new(0))),
        ..Default::default()
    });
    let (mut client, handle, runtime, surface) = client_with(fixture).await;
    client.action(Action::Preview(
        rsi_agent_turn_protocol::JobPreviewRequest {
            turn_id: TurnId::new("preview-turn").unwrap(),
            generation: 1,
            job_id: "job-1".into(),
            effect_id: rsi_agent_session_protocol::EffectId::new("effect").unwrap(),
            stdout_bytes: 1024,
            stderr_bytes: 1024,
        },
    ));
    assert!(futures_util::poll!(client.tasks.next()).is_pending());
    assert_eq!(
        handle.read_active.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    for index in 1..20 {
        client.poll_preview(tokio::time::Instant::now() + Duration::from_secs(index));
    }
    assert_eq!(client.tasks.len(), 1);
    client.state.escape();
    let work = tokio::time::timeout(Duration::from_secs(1), client.tasks.next())
        .await
        .unwrap()
        .unwrap();
    assert!(work.superseded(&client));
    assert!(work.result.is_err());
    assert_eq!(
        handle.read_active.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(client.state.preview.is_none());
    assert!(client.state.detail.is_none());
    client.poll_preview(tokio::time::Instant::now() + Duration::from_mins(1));
    assert!(client.tasks.is_empty());
    assert!(handle.cancellations.lock().unwrap().is_empty());
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn fold_retention_charges_current_keys_and_allocation_capacity_without_discarding_drafts() {
    let (mut client, _, runtime, surface) = client().await;
    client.state.editor.insert("current draft").unwrap();
    client.state.folds = (0..512)
        .map(|index| (format!("current-{index}"), false))
        .collect();
    for session in 0..8 {
        let mut editor = editor::Editor::default();
        editor.insert("saved draft").unwrap();
        client.drafts.insert(
            SessionId::new(format!("saved-{session}")).unwrap(),
            SavedSession {
                references: Vec::new(),
                reference_bytes: 0,
                editor,
                command: Arc::new(rsi_client::CommandSubmission::default()),
                model: None,
                reasoning_effort: None,
                top: None,
                folds: (0..512)
                    .map(|index| (format!("{session}-{index}-{}", "x".repeat(700)), false))
                    .collect(),
                last_used: session,
                owned: BTreeSet::new(),
            },
        );
    }
    assert!(client.fold_retention().0 > 4096);
    assert!(client.fold_retention().1 > 1024 * 1024);
    retention::SCANNED_KEYS.set(0);
    client.enforce_fold_budget();
    assert!(
        retention::SCANNED_KEYS.get() <= 3 * 4608,
        "eviction repeatedly rescanned retained keys"
    );
    let (count, bytes) = client.fold_retention();
    assert!(count <= 4096);
    assert!(bytes <= 1024 * 1024);
    assert_eq!(client.state.folds.len(), 512);
    assert_eq!(client.state.editor.text(), "current draft");
    assert!(
        client
            .drafts
            .values()
            .all(|saved| saved.editor.text() == "saved draft")
    );
    assert!(
        client.drafts[&SessionId::new("saved-0").unwrap()]
            .folds
            .is_empty()
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
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
    let attached = attachment(handle.clone(), false, None).await.unwrap();
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
                last_settled_control_seq: 0,
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
    assert_eq!(client.state.editor.text(), "preserved draft");
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
        assert_eq!(client.state.editor.text(), "next draft");
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn ordinary_submission_reconciliation_freezes_id_and_content_without_a_model_override() {
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
        assert_eq!(request.model, None);
        assert_eq!(request.reasoning_effort, None);
        assert!(
            matches!(&request.content[0], MessageInput::Text { text } if text == "original input")
        );
    }
    assert_eq!(client.state.editor.text(), "next draft");
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
    assert_eq!(client.state.editor.text(), "steering");
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
        last_settled_control_seq: 0,
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
    assert_eq!(client.state.editor.text(), "preserved draft");
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
            reasoning_effort: None,
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
async fn restoring_a_saved_anchor_reads_its_neighborhood_and_a_separate_live_tail() {
    use rsi_agent_store_protocol::{StoreAgentSessionStatus, StoreAgentSubtreeSnapshot};
    let (mut client, handle, runtime, surface) = client().await;
    *handle.history_facts.lock().unwrap() = Some((1..=2000).map(history_fact).collect());
    *handle.inspection.lock().unwrap() = Some(StoreSessionInspection {
        header: client.state.header.clone(),
        durable_fact_seq: 2000,
        durable_control_seq: 0,
        pending: vec![],
        active_turn_id: None,
        activation_phase: None,
        tree: StoreAgentSubtreeSnapshot {
            session: StoreAgentSessionStatus {
                session_id: client.state.header.session_id().clone(),
                durable_control_seq: 0,
                last_settled_control_seq: 0,
                has_open_turn: false,
                has_active_activation: false,
                has_waking_message: false,
            },
            descendants: vec![],
        },
    });
    let anchor = transcript::Anchor {
        source: transcript::Source {
            seq: 25,
            field: rsi_conversation::FactField::TurnInput,
        },
        offset: 0,
    };
    let attached = attachment(handle.clone(), true, Some(anchor))
        .await
        .unwrap();
    assert_eq!(
        *handle.history_requests.lock().unwrap(),
        vec![Some(26), Some(2001)]
    );
    client.state.top = Some(anchor);
    client.live_transcript = attached.live_page.map(live_window);
    client.page(attached.page.unwrap());
    assert!(client.state.transcript.locate(anchor).is_some());
    assert_eq!(client.state.transcript.blocks.last().unwrap().first, 25);
    client.live_fact(&history_fact(2001));
    assert_eq!(client.state.transcript.blocks.last().unwrap().first, 25);
    client.follow_live();
    assert!(client.state.top.is_none());
    assert_eq!(client.state.transcript.blocks.first().unwrap().first, 1873);
    assert_eq!(client.state.transcript.blocks.last().unwrap().first, 2001);
    assert!(client.state.transcript.earlier);
    assert_eq!(
        handle.submissions.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn expanded_long_command_preserves_its_full_source_and_copy_bytes() {
    let (mut client, _, runtime, surface) = client().await;
    let command = format!(
        "printf 'quote: \"界\"\\n'\n# {}COMMAND-TAIL",
        "long ".repeat(160)
    );
    let fact = SessionFact::new(
        1,
        1,
        SessionFactBody::ToolIntent {
            turn_id: rsi_agent_session_protocol::TurnId::new("command").unwrap(),
            effect_id: rsi_agent_session_protocol::EffectId::new("tool").unwrap(),
            source_model_effect_id: rsi_agent_session_protocol::EffectId::new("model").unwrap(),
            identity: rsi_tools_protocol::ToolResultIdentity::new(
                "owner",
                "invoke",
                "call",
                "a".repeat(64),
            )
            .unwrap(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": command}),
            approval: None,
            parallel_safe: false,
        },
    )
    .unwrap();
    client.state.transcript.apply(&fact);
    assert_eq!(
        client.state.transcript.blocks[0].text(),
        command,
        "a bounded summary must not replace the command source"
    );
    let block = &client.state.transcript.blocks[0];
    let first = block.anchor(0).unwrap();
    let last = block.anchor(command.len()).unwrap();
    assert_eq!(
        rsi_conversation::select_field(&fact, first.source)
            .unwrap()
            .window(0, 4096)
            .unwrap()
            .text,
        command
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(28, 9)).unwrap();
    let mut view = render::View::default();
    terminal
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    assert!(
        client.toggle_fold_at(&view, 2, 0),
        "even an intent without output must expand"
    );
    terminal
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    let visible = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
            .contains("COMMAND-TAIL")
    };
    for _ in 0..40 {
        if visible(&terminal) {
            break;
        }
        client.scroll(&view, false);
        terminal
            .draw(|frame| view = render::draw(frame, &client.state))
            .unwrap();
    }
    assert!(
        visible(&terminal),
        "the complete tail must remain visible across wrapped rows"
    );
    assert_eq!(
        client.state.transcript.selected(first, last).unwrap(),
        command
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn scrolling_stops_at_the_bottom_without_moving_content_or_blocking_up() {
    for (width, height) in [(80, 24), (28, 9)] {
        for count in [0, 1, 40] {
            let (mut client, _, runtime, surface) = client().await;
            for seq in 1..=count {
                client.state.transcript.apply(&history_fact(seq));
            }
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let mut view = render::View::default();
            terminal
                .draw(|frame| view = render::draw(frame, &client.state))
                .unwrap();
            let tail = terminal.backend().buffer().clone();
            for _ in 0..5 {
                client.scroll(&view, false);
                terminal
                    .draw(|frame| view = render::draw(frame, &client.state))
                    .unwrap();
                assert_eq!(
                    terminal.backend().buffer(),
                    &tail,
                    "bottom scroll must be a no-op: {width}x{height}, {count} messages"
                );
            }
            assert!(
                client.tasks.is_empty(),
                "bottom scrolling does not query history"
            );
            if count > 1 {
                client.scroll(&view, true);
                terminal
                    .draw(|frame| view = render::draw(frame, &client.state))
                    .unwrap();
                assert_ne!(
                    terminal.backend().buffer(),
                    &tail,
                    "up must leave the bottom immediately"
                );
                for _ in 0..10 {
                    client.scroll(&view, false);
                    terminal
                        .draw(|frame| view = render::draw(frame, &client.state))
                        .unwrap();
                }
                assert_eq!(
                    terminal.backend().buffer(),
                    &tail,
                    "returning to the tail must not overscroll"
                );
            }
            surface.stop().await;
            assert!(runtime.shutdown().await.is_clean());
        }
    }
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
    client.state.top = client.state.transcript.blocks[0].anchor(0);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let mut view = render::View::default();
    terminal
        .draw(|frame| view = render::draw(frame, &client.state))
        .unwrap();
    let expected = view.scroll_anchor(
        client.state.header.session_id(),
        &client.state.transcript,
        false,
    );
    assert!(expected.is_some());
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

#[tokio::test]
async fn output_close_failure_still_awaits_presentation_disposal() {
    use std::{future::Future as _, task::Poll};
    let (started, mut entered) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let disposed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = disposed.clone();
    let mut closing = std::pin::pin!(super::close_rendering(
        async { Err(std::io::Error::other("fixture output restoration failure")) },
        async move {
            started.send(()).unwrap();
            released.await.unwrap();
            finished.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    ));
    std::future::poll_fn(|cx| {
        assert!(closing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    entered
        .try_recv()
        .expect("presentation cleanup started despite output failure");
    assert!(!disposed.load(std::sync::atomic::Ordering::SeqCst));
    release.send(()).unwrap();
    let error = closing.await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture output restoration failure")
    );
    assert!(disposed.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn setup_commands_bypass_message_history_and_unresolved_session_submissions() {
    let (mut client, handle, runtime, surface) = client().await;
    client.submission.busy = true;
    for text in [
        "/login",
        "/model",
        "/login openai",
        "/help",
        "/new",
        "/resume",
        "/quit",
        "/exit",
    ] {
        client.state.editor.insert(text).unwrap();
        client.submit(MessageDelivery::NextTurn, false);
        assert!(client.setup_command.take().is_some());
        assert!(client.state.editor.text().is_empty());
        assert!(client.tasks.is_empty());
        assert!(client.submission.request.is_none());
    }
    assert!(handle.commands.lock().unwrap().is_empty());
    assert!(handle.submitted_requests.lock().unwrap().is_empty());
    client
        .state
        .editor
        .insert("/login openai key-not-accepted")
        .unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    assert!(client.setup_command.is_none());
    assert_eq!(client.state.editor.text(), "/login openai key-not-accepted");
    client.prompt_menu();
    assert!(
        client
            .state
            .menu
            .as_ref()
            .is_none_or(|menu| menu.items.is_empty())
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn multiline_reserved_and_double_slash_reach_human_submission_exactly() {
    for text in [
        "/model\nexplain this literal",
        "//plan on",
        "/login\ndeepseek",
    ] {
        let (mut client, handle, runtime, surface) = client().await;
        client.state.editor.insert(text).unwrap();
        client.submit(MessageDelivery::NextTurn, false);
        let work = client.tasks.next().await.unwrap();
        assert!(matches!(work.result, Ok(Update::Submitted(_))));
        assert!(client.setup_command.is_none());
        assert!(handle.commands.lock().unwrap().is_empty());
        assert!(
            matches!(handle.submitted_requests.lock().unwrap()[0].content.as_slice(),[MessageInput::Text{text:body}] if body==text)
        );
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn gated_attachment_keeps_pretransition_input_with_its_original_session() {
    for remote in [false, true] {
        let (mut client, handle, runtime, surface) = client().await;
        client.state.remote = remote;
        let original = client.state.header.clone();
        let mut wire = serde_json::to_value(&original).unwrap();
        wire["session_id"] = serde_json::json!("new-attachment");
        let next: SessionHeader = serde_json::from_value(wire).unwrap();
        let (release, admitted) = tokio::sync::oneshot::channel();
        let attachment = async move {
            admitted.await.unwrap();
            next
        };
        tokio::pin!(attachment);
        assert!(futures_util::poll!(&mut attachment).is_pending());
        client.state.editor.insert("before attachment").unwrap();
        assert_eq!(client.state.header.session_id(), original.session_id());
        release.send(()).unwrap();
        client.switch_session_draft(attachment.await);
        assert!(client.state.editor.text().is_empty());
        assert_eq!(
            client.drafts[original.session_id()].editor.text(),
            "before attachment"
        );
        client.state.editor.insert("after attachment").unwrap();
        assert_eq!(client.state.header.session_id().as_str(), "new-attachment");
        assert_eq!(client.state.editor.text(), "after attachment");
        client.switch_session_draft(original);
        assert_eq!(client.state.editor.text(), "before attachment");
        assert_eq!(
            client.drafts[&SessionId::new("new-attachment").unwrap()]
                .editor
                .text(),
            "after attachment"
        );
        assert!(handle.submitted_requests.lock().unwrap().is_empty());
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}
