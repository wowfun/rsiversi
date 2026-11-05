use super::*;

#[tokio::test]
async fn fork_create_rejects_missing_or_different_history_even_with_matching_request_echo() {
    use rsi_agent_session_protocol::{ForkPoint, TurnId};
    let turn = TurnId::new("selected-turn").unwrap();
    for point in [ForkPoint::before(&turn), ForkPoint::through(&turn)] {
        check_fork_binding(point).await;
    }
}

async fn check_fork_binding(point: rsi_agent_session_protocol::ForkPoint) {
    use rsi_agent_session_protocol::{ForkPoint, ForkTurnSelection, HistoryOrigin};
    let remote = Remote::new();
    let client = SessionClient::new(remote.clone());
    let parent = header();
    let history = HistoryOrigin {
        parent_session_id: parent.session_id().clone(),
        parent_header_fingerprint: parent.fingerprint().unwrap(),
        point: point.clone(),
        resolved_after_seq: 0,
        resolved_terminal_seq: 0,
        terminal_prefix_sha256: "0".repeat(64),
        resolved_terminal_control_seq: 0,
        terminal_control_prefix_sha256: "0".repeat(64),
        requested_turns: ForkTurnSelection::None,
        effective_turns: 0,
    };
    let child = parent
        .forked_independent(
            rsi_agent_session_protocol::SessionId::new("fork-target").unwrap(),
            2,
            history,
        )
        .unwrap();
    let request = CreateSession {
        workspace_id: rsi_workspace_protocol::WorkspaceId::parse(
            "c52ddf65534b7b46035084358ab7902be4bfef220bdb503ac7039cc861905b05",
        )
        .unwrap(),
        session_id: child.session_id().clone(),
        agent_preset_id: Some(child.agent_preset_id().clone()),
        fork: Some(rsi_session_protocol::ForkDraft {
            source_session_id: parent.session_id().clone(),
            point: point.clone(),
            turns: ForkTurnSelection::None,
        }),
    };
    for kind in ["missing", "different", "opposite", "exact"] {
        let mut value = serde_json::to_value(&child).unwrap();
        if kind == "missing" {
            value.as_object_mut().unwrap().remove("fork_origin");
        }
        if kind == "different" {
            value["fork_origin"]["history"]["point"]["turn_id"] = json!("other-terminal");
        }
        if kind == "opposite" {
            value["fork_origin"]["history"]["point"]["kind"] = json!(match point {
                ForkPoint::BeforeTurn { .. } => "through_turn",
                ForkPoint::ThroughTurn { .. } => "before_turn",
            });
        }
        remote.reply(&wire::Created {
            creation: request.clone(),
            draft: SessionDraftView {
                header: serde_json::from_value(value).unwrap(),
                revision: 0,
                command_inputs: vec![],
            },
        });
        assert_eq!(
            client.create(request.clone()).await.is_ok(),
            kind == "exact"
        );
    }
}
use rsi_session_protocol::{SelectDraftPreset, SessionDraftView};

#[tokio::test]
async fn draft_observation_checks_progress_binding_and_publication_before_eof() {
    let (remote, _, handle) = fixture().await;
    remote.stream(&[
        envelope(json!(0)),
        envelope(json!(3)),
        envelope(Value::Null),
    ]);
    let mut stream = handle.observe_draft().await.unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap(), Some(0));
    assert_eq!(stream.next().await.unwrap().unwrap(), Some(3));
    assert_eq!(stream.next().await.unwrap().unwrap(), None);
    assert!(stream.next().await.is_none());
    for bad in [json!(2), json!(3), json!(-1), json!("4")] {
        remote.stream(&[envelope(json!(3)), envelope(bad)]);
        let mut stream = handle.observe_draft().await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), Some(3));
        assert!(stream.next().await.unwrap().is_err());
    }
    remote.stream(&[envelope(json!(0))]);
    let mut stream = handle.observe_draft().await.unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap(), Some(0));
    assert!(stream.next().await.unwrap().is_err());
    let mut foreign = envelope(json!(1));
    foreign["target"]["session_id"] = json!("foreign");
    remote.stream(&[foreign]);
    assert!(
        handle
            .observe_draft()
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .is_err()
    );
}

fn held_input() -> rsi_agent_session_protocol::CommandMessage {
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, AgentMessageSource, CommandMessage,
        CommandMessageSource, ContributionId, DomainRequestId, MessageDelivery, MessageOptions,
    };
    let source = CommandMessageSource {
        command: ContributionId::new("fixture.command").unwrap(),
        request_id: DomainRequestId::new("draft-input").unwrap(),
        invocation_sha256: "a".repeat(64),
        ordinal: 0,
        attribution: None,
    };
    CommandMessage {
        message: AgentMessage {
            message_id: source.message_id().unwrap(),
            source: AgentMessageSource::Command { source },
            content: vec![AgentMessageContent::Text {
                text: "held input".into(),
            }],
            options: MessageOptions::default(),
        },
        delivery: MessageDelivery::NextStep,
    }
}

#[tokio::test]
async fn draft_input_reads_preserve_identity_and_reject_forged_or_waking_batches() {
    let (remote, _, handle) = fixture().await;
    let mut draft = view("standard", 1);
    draft.command_inputs.push(held_input());
    remote.reply(&envelope(serde_json::to_value(&draft).unwrap()));
    assert_eq!(handle.draft_snapshot().await.unwrap(), draft);
    for mutation in ["source", "identity", "delivery", "duplicate", "ordinal"] {
        let mut changed = serde_json::to_value(&draft).unwrap();
        let input = &mut changed["command_inputs"][0];
        match mutation {
            "source" => input["message"]["source"] = json!({"type":"human"}),
            "identity" => input["message"]["message_id"] = json!("foreign"),
            "delivery" => input["delivery"] = json!("steer"),
            "ordinal" => input["message"]["source"]["source"]["ordinal"] = json!(1),
            "duplicate" => {
                let copied = input.clone();
                changed["command_inputs"]
                    .as_array_mut()
                    .unwrap()
                    .push(copied);
            }
            _ => unreachable!(),
        }
        remote.reply(&envelope(changed));
        assert!(
            handle.draft_snapshot().await.is_err(),
            "accepted {mutation}"
        );
    }
}

fn select(preset: &str, revision: u64) -> SelectDraftPreset {
    SelectDraftPreset {
        preset_id: AgentPresetId::new(preset).unwrap(),
        expected_revision: revision,
    }
}

#[tokio::test]
async fn draft_withdrawal_requires_a_bound_snapshot_without_selected_inputs() {
    let (remote, _, handle) = fixture().await;
    let input = held_input();
    let request = rsi_session_protocol::DiscardDraftInputs {
        message_ids: vec![input.message.message_id.clone()],
    };
    let mut remaining = view("standard", 4);
    remaining.header = header();
    remaining.command_inputs.push(input);
    remote.reply(&envelope(serde_json::to_value(&remaining).unwrap()));
    assert!(handle.discard_draft_inputs(request.clone()).await.is_err());
    remaining.command_inputs.clear();
    remote.reply(&envelope(serde_json::to_value(&remaining).unwrap()));
    assert_eq!(
        handle.discard_draft_inputs(request).await.unwrap(),
        remaining
    );
    let calls = remote.calls.load(Ordering::SeqCst);
    assert!(
        handle
            .discard_draft_inputs(rsi_session_protocol::DiscardDraftInputs {
                message_ids: vec![MessageId::new("too-many").unwrap(); 65],
            })
            .await
            .is_err()
    );
    assert_eq!(remote.calls.load(Ordering::SeqCst), calls);
}
fn view(preset: &str, revision: u64) -> SessionDraftView {
    SessionDraftView {
        command_inputs: vec![],
        header: header()
            .with_agent_preset_id(AgentPresetId::new(preset).unwrap())
            .unwrap(),
        revision,
    }
}
fn envelope_at(header: &SessionHeader, body: &Value) -> Value {
    json!({"target":{"session_id":header.session_id(),"header_key":header.fingerprint().unwrap()},"body":body})
}

#[tokio::test]
async fn selection_updates_one_atomic_binding_and_already_open_streams_keep_the_old_binding() {
    let (remote, _, handle) = fixture().await;
    remote.stream(&[envelope(json!({"approvals":[],"questions":[]}))]);
    let mut stream = handle.observe_interactions().await.unwrap();
    let selected = view("other", 4);
    remote.reply(&envelope(serde_json::to_value(&selected).unwrap()));
    assert_eq!(
        handle.select_preset(select("other", 3)).await.unwrap(),
        selected
    );
    assert_eq!(handle.header().await.unwrap(), selected.header);
    assert!(stream.next().await.unwrap().unwrap().approvals().is_empty());
    remote.reply(&envelope_at(
        &selected.header,
        &serde_json::to_value(&selected).unwrap(),
    ));
    assert_eq!(handle.draft_snapshot().await.unwrap(), selected);
    let requests = remote.requests.lock().unwrap();
    assert_eq!(
        requests.last().unwrap().1["target"]["header_key"],
        selected.header.fingerprint().unwrap()
    );
}

#[tokio::test]
async fn selection_rejects_changed_immutable_fields_wrong_revision_and_wrong_error_identity() {
    let (remote, _, handle) = fixture().await;
    for (field, replacement) in [
        ("revision", json!(5)),
        ("header", serde_json::to_value(header()).unwrap()),
    ] {
        let mut changed = serde_json::to_value(view("other", 4)).unwrap();
        changed[field] = replacement;
        remote.reply(&envelope(changed));
        assert!(matches!(
            handle.select_preset(select("other", 3)).await,
            Err(SessionError::Api(ApiError::OutcomeUnknown))
        ));
        assert_eq!(handle.header().await.unwrap(), header());
    }
    let mut changed = serde_json::to_value(view("other", 4)).unwrap();
    changed["header"]["canonical_cwd"] = json!("/foreign");
    remote.reply(&envelope(changed));
    assert!(matches!(
        handle.select_preset(select("other", 3)).await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
    remote.domain(&json!({"code":"command_revision_conflict","expected":{"kind":"draft","revision":2},"actual":{"kind":"draft","revision":4}}));
    assert!(matches!(
        handle.select_preset(select("other", 3)).await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
    remote.domain(&json!({"code":"command_revision_conflict","expected":{"kind":"draft","revision":3},"actual":{"kind":"draft","revision":4}}));
    assert!(matches!(
        handle.select_preset(select("other", 3)).await,
        Err(SessionError::CommandRevisionConflict { .. })
    ));
    assert_eq!(remote.calls.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn create_retry_correlates_original_input_while_returning_the_current_selected_draft() {
    let remote = Remote::new();
    let client = SessionClient::new(remote.clone());
    let request = CreateSession {
        fork: None,
        workspace_id: rsi_workspace_protocol::WorkspaceId::parse(
            "c52ddf65534b7b46035084358ab7902be4bfef220bdb503ac7039cc861905b05",
        )
        .unwrap(),
        session_id: header().session_id().clone(),
        agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
    };
    let selected = view("other", 4);
    remote.reply(&wire::Created {
        creation: request.clone(),
        draft: selected.clone(),
    });
    let handle = client.create(request.clone()).await.unwrap();
    assert_eq!(handle.header().await.unwrap(), selected.header);
    let stale = SessionDraftView {
        header: selected.header.clone(),
        revision: 1,
        command_inputs: vec![],
    };
    remote.reply(&envelope_at(
        &selected.header,
        &serde_json::to_value(stale).unwrap(),
    ));
    assert!(matches!(
        handle.select_preset(select("other", 0)).await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
    remote.reply(&wire::Created {
        creation: request.clone(),
        draft: view("other", 0),
    });
    assert!(matches!(
        client.create(request.clone()).await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
    let mut wrong = request.clone();
    wrong.agent_preset_id =
        Some(rsi_agent_session_protocol::AgentPresetId::new("other-agent").unwrap());
    remote.reply(&wire::Created {
        creation: wrong,
        draft: selected,
    });
    assert!(matches!(
        client.create(request).await,
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
}

#[tokio::test]
async fn out_of_order_selection_responses_cannot_regress_the_same_header_revision() {
    let (remote, _, handle) = fixture().await;
    let gate = Gate::new();
    remote.gates.lock().unwrap().push_back(gate.clone());
    remote.reply(&envelope(
        serde_json::to_value(view("standard", 1)).unwrap(),
    ));
    let earlier = tokio::spawn({
        let handle = handle.clone();
        async move { handle.select_preset(select("standard", 0)).await }
    });
    gate.entered.acquire().await.unwrap().forget();
    remote.reply(&envelope(
        serde_json::to_value(view("standard", 2)).unwrap(),
    ));
    assert_eq!(
        handle
            .select_preset(select("standard", 1))
            .await
            .unwrap()
            .revision,
        2
    );
    gate.release.add_permits(1);
    assert!(matches!(
        earlier.await.unwrap(),
        Err(SessionError::Api(ApiError::OutcomeUnknown))
    ));
}

#[tokio::test]
async fn delayed_finite_reply_uses_its_admitted_binding_after_a_preset_switch() {
    let (remote, _, handle) = fixture().await;
    let gate = Gate::new();
    remote.gates.lock().unwrap().push_back(gate.clone());
    remote.reply(&envelope(Value::Null));
    let earlier = tokio::spawn({
        let handle = handle.clone();
        async move {
            handle
                .command_status(
                    &rsi_agent_session_protocol::DomainRequestId::new("original").unwrap(),
                )
                .await
        }
    });
    gate.entered.acquire().await.unwrap().forget();
    remote.reply(&envelope(serde_json::to_value(view("other", 1)).unwrap()));
    handle.select_preset(select("other", 0)).await.unwrap();
    gate.release.add_permits(1);
    assert!(earlier.await.unwrap().unwrap().is_none());
}
