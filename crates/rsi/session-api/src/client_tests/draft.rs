use super::*;
use rsi_session_protocol::{SelectDraftPreset, SessionDraftView};

fn select(preset: &str, revision: u64) -> SelectDraftPreset {
    SelectDraftPreset {
        preset_id: AgentPresetId::new(preset).unwrap(),
        expected_revision: revision,
    }
}
fn view(preset: &str, revision: u64) -> SessionDraftView {
    SessionDraftView {
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
    let client = SessionClient::new(remote.clone()).unwrap();
    let request = CreateSession {
        workspace_id: rsi_workspace_protocol::WorkspaceId::parse(
            "c52ddf65534b7b46035084358ab7902be4bfef220bdb503ac7039cc861905b05",
        )
        .unwrap(),
        session_id: header().session_id().clone(),
        agent_preset_id: Some(AgentPresetId::new("standard").unwrap()),
        workspace_trust: header().workspace_trust(),
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
    wrong.workspace_trust = rsi_agent_session_protocol::WorkspaceTrust::Trusted;
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
