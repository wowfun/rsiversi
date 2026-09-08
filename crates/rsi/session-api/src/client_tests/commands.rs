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
