use super::*;
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContributionId, DomainRequestId, SessionCommandInvocation,
    SessionCommandReceipt,
};

fn invocation() -> SessionCommandInvocation {
    SessionCommandInvocation {
        command: ContributionId::new("fixture.plan").unwrap(),
        request_id: DomainRequestId::new("command-request").unwrap(),
        expected_revision: CommandRevision::Draft { revision: 3 },
        arguments: CommandArguments::new(json!(true)).unwrap(),
    }
}

#[tokio::test]
async fn maximum_escaped_goal_detail_fits_registered_status_and_observation_replies() {
    let (remote, _, handle) = fixture().await;
    for detail in ["\n".repeat(4096), "\t".repeat(4096), "\\".repeat(4096)] {
        let state = rsi_goal::GoalLiveState {
            detail: Some(detail),
            message_id: Some(MessageId::new("m".repeat(256)).unwrap()),
            ..Default::default()
        };
        state.validate().unwrap();
        let value = envelope(serde_json::to_value(&state).unwrap());
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(bytes.len() > 8192);
        for operation in [Operation::GoalStatus, Operation::GoalObserve] {
            assert!(bytes.len() <= operation.spec().maximum_response_bytes);
        }
        remote.reply(&value);
        assert_eq!(handle.goal_status().await.unwrap(), state);
        remote.stream(&[value]);
        assert_eq!(
            handle
                .observe_goal()
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap(),
            state
        );
    }
}

#[tokio::test]
async fn goal_control_authenticates_receipt_and_live_state_without_replay() {
    let (remote, _, handle) = fixture().await;
    let request: rsi_goal::GoalControl = serde_json::from_value(json!({
        "request_id":"goal-control", "expected_revision":{"kind":"draft","revision":3},
        "action":{"action":"resume","id":"goal"}
    }))
    .unwrap();
    let invocation = request.invocation().unwrap();
    let receipt = rsi_goal::GoalControlReceipt {
        command: SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap(),
        live: rsi_goal::GoalLiveState::default(),
    };
    let valid = serde_json::to_value(&receipt).unwrap();
    for path in [
        "/command/request_id",
        "/command/command",
        "/command/invocation_sha256",
        "/command/outcome",
        "/live/detail",
        "/live/armed",
    ] {
        let mut changed = valid.clone();
        *changed.pointer_mut(path).unwrap() = match path {
            "/command/outcome" => json!({"kind":"draft_changed","revision":5}),
            "/live/detail" => json!("x".repeat(4097)),
            "/live/armed" => json!(true),
            _ => json!("foreign"),
        };
        remote.reply(&envelope(changed));
        let before = remote.calls.load(Ordering::SeqCst);
        assert!(
            matches!(handle.control_goal(request.clone()).await, Err(SessionError::CommandOutcomeUnknown { request_id }) if request_id == request.request_id)
        );
        assert_eq!(remote.calls.load(Ordering::SeqCst), before + 1);
    }
    remote.reply(&envelope(valid));
    assert_eq!(handle.control_goal(request).await.unwrap(), receipt);
    remote.reply(&envelope(
        json!({"available":false,"armed":true,"stage":"waiting","message_id":null,"detail":null}),
    ));
    assert!(matches!(
        handle.goal_status().await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    remote.stream(&[envelope(json!({"available":true,"armed":false,"stage":"disarmed","message_id":null,"detail":"x".repeat(4097)}))]);
    assert!(matches!(
        handle.observe_goal().await.unwrap().next().await.unwrap(),
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
}

#[tokio::test]
async fn execution_rejects_wrong_identity_digest_and_successor_without_replaying() {
    let (remote, _, handle) = fixture().await;
    let invocation = invocation();
    let receipt = SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap();
    let valid = serde_json::to_value(&receipt).unwrap();
    for (field, replacement) in [
        ("command", json!("foreign.command")),
        ("request_id", json!("foreign-request")),
        ("invocation_sha256", json!("b".repeat(64))),
        ("state_sha256", json!("invalid-sha")),
        ("outcome", json!({"kind":"draft_changed","revision":5})),
        ("outcome", json!({"kind":"committed","control_seq":4})),
    ] {
        let mut changed = valid.clone();
        changed[field] = replacement;
        remote.reply(&envelope(changed));
        let before = remote.calls.load(Ordering::SeqCst);
        assert!(matches!(handle.execute_command(invocation.clone()).await,
            Err(SessionError::CommandOutcomeUnknown { request_id }) if request_id == invocation.request_id));
        assert_eq!(remote.calls.load(Ordering::SeqCst), before + 1);
    }
    remote.reply(&envelope(valid.clone()));
    assert_eq!(
        handle.execute_command(invocation.clone()).await.unwrap(),
        receipt
    );
    remote.reply(&envelope(valid));
    assert_eq!(
        handle.command_status(&invocation.request_id).await.unwrap(),
        Some(receipt)
    );
    remote.reply(&envelope(Value::Null));
    assert!(
        handle
            .command_status(&invocation.request_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn command_failures_are_correlated_to_the_original_request_and_operation() {
    let (remote, _, handle) = fixture().await;
    let invocation = invocation();
    for code in ["command_conflict", "command_outcome_unknown"] {
        remote.domain(&json!({"code":code,"request_id":"foreign"}));
        assert!(matches!(handle.execute_command(invocation.clone()).await,
            Err(SessionError::CommandOutcomeUnknown { request_id }) if request_id == invocation.request_id));
    }
    remote.domain(&json!({"code":"command_revision_conflict","expected":{"kind":"draft","revision":2},"actual":{"kind":"draft","revision":4}}));
    assert!(matches!(
        handle.execute_command(invocation.clone()).await,
        Err(SessionError::CommandOutcomeUnknown { .. })
    ));
    remote.domain(&json!({"code":"command_revision_conflict","expected":invocation.expected_revision,"actual":{"kind":"draft","revision":4}}));
    assert!(
        matches!(handle.execute_command(invocation.clone()).await, Err(SessionError::CommandRevisionConflict { expected, .. }) if expected == invocation.expected_revision)
    );
    remote.domain(&json!({"code":"command_conflict","request_id":"foreign"}));
    assert!(matches!(
        handle.command_status(&invocation.request_id).await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    remote.domain(&json!({"code":"command_outcome_unknown","request_id":invocation.request_id}));
    assert!(matches!(
        handle.commands().await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    remote
        .replies
        .lock()
        .unwrap()
        .push_back(Err(ApiError::OutcomeUnknown));
    assert!(
        matches!(handle.execute_command(invocation.clone()).await, Err(SessionError::CommandOutcomeUnknown { request_id }) if request_id == invocation.request_id)
    );
    assert_eq!(remote.calls.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn command_discovery_and_status_reject_invalid_bounded_values() {
    let (remote, _, handle) = fixture().await;
    let invocation = invocation();
    remote.reply(&envelope(
        json!({"revision":invocation.expected_revision,"commands":[]}),
    ));
    assert!(handle.commands().await.unwrap().commands().is_empty());
    let descriptor =
        json!({"id":"fixture.plan","name":"plan","description":"plan help","draft_safe":true});
    remote.reply(&envelope(
        json!({"revision":invocation.expected_revision,"commands":[descriptor.clone(),descriptor]}),
    ));
    assert!(matches!(
        handle.commands().await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
    let mut receipt = serde_json::to_value(
        SessionCommandReceipt::draft_changed(&invocation, "a".repeat(64)).unwrap(),
    )
    .unwrap();
    receipt["request_id"] = json!("foreign");
    remote.reply(&envelope(receipt));
    assert!(matches!(
        handle.command_status(&invocation.request_id).await,
        Err(SessionError::Api(ApiError::Invalid(_)))
    ));
}
