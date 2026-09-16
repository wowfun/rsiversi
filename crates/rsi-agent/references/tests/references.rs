use rsi_agent_references::{ReferenceError, References};
use rsi_agent_session_protocol::*;
use rsi_agent_store_protocol::{AppendBatch, SessionStore};
use rsi_agent_testkit::{MemoryStore, append_history_fixture};
use rsi_ai_protocol::ModelRef;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn header(id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(id).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "",
            ModelRef::new("test", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
fn accepted(turn: &str, text: &str) -> SessionFactBody {
    SessionFactBody::TurnAccepted {
        turn_id: TurnId::new(turn).unwrap(),
        text: text.into(),
        model: None,
        reasoning_effort: None,
        sandbox: rsi_sandbox::SandboxMode::ReadOnly,
        require_approval: false,
    }
}
fn terminal(turn: &str) -> SessionFactBody {
    SessionFactBody::TurnTerminal {
        turn_id: TurnId::new(turn).unwrap(),
        outcome: TurnOutcome::Completed,
    }
}
async fn append(
    store: &dyn SessionStore,
    header: &SessionHeader,
    after: u64,
    bodies: Vec<SessionFactBody>,
) {
    let facts = bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| Arc::new(SessionFact::new(after + index as u64 + 1, 1, body).unwrap()))
        .collect();
    append_history_fixture(
        store,
        AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: after,
            header: (after == 0).then(|| header.clone()),
            facts,
        },
    )
    .await
    .unwrap();
}
fn inherited(parent: &SessionHeader, after: u64, terminal: u64) -> SessionHeader {
    parent
        .forked_child(
            SessionId::new("child").unwrap(),
            2,
            ForkOrigin {
                parent_session_id: parent.session_id().clone(),
                root_session_id: parent.session_id().clone(),
                path: AgentPath::new(vec![1]).unwrap(),
                task_name: "child".into(),
                parent_header_fingerprint: parent.fingerprint().unwrap(),
                invoking_turn_id: TurnId::new("spawn").unwrap(),
                resolved_after_seq: after,
                resolved_terminal_seq: terminal,
                terminal_prefix_sha256: "a".repeat(64),
                resolved_terminal_control_seq: 1,
                terminal_control_prefix_sha256: "b".repeat(64),
                requested_turns: ForkTurnSelection::All,
                effective_turns: 1,
            },
            ModelSelection::baseline(parent.settings()),
        )
        .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One owner scenario follows capture, mutation, lineage and cancellation."
)]
async fn capture_is_immutable_target_bound_and_only_recorded_references_cross_the_actual_fork_interval()
 {
    let runtime = rsi_meta::Runtime::default();
    let store = Arc::new(MemoryStore::default());
    let source = header("source");
    let target = header("target");
    append(
        &*store,
        &source,
        0,
        vec![accepted("first", "最初的材料"), terminal("first")],
    )
    .await;
    let owner = References::new(store.clone(), runtime.execution().clone());
    let frozen = owner
        .capture(
            source.session_id().clone(),
            target.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(frozen.metadata.through_seq, 2);
    assert!(frozen.preview.contains("最初的材料"));
    owner
        .verify(target.clone(), frozen.clone(), CancellationToken::new())
        .await
        .unwrap();
    append(
        &*store,
        &source,
        2,
        vec![accepted("later", "later-new-text"), terminal("later")],
    )
    .await;
    let again = owner
        .capture(
            source.session_id().clone(),
            target.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(again.snapshot, frozen.snapshot);
    let original = owner
        .preview(
            target.clone(),
            frozen.clone(),
            0,
            65536,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!original.text.contains("later-new-text"));
    assert_eq!(original.reference, frozen);
    let mut modified = frozen.clone();
    modified.preview.push('x');
    assert!(
        owner
            .verify(target.clone(), modified, CancellationToken::new())
            .await
            .is_err()
    );
    let mut modified = frozen.clone();
    modified.metadata.source.session_id = SessionId::new("forged").unwrap();
    assert!(
        owner
            .verify(target.clone(), modified, CancellationToken::new())
            .await
            .is_err()
    );
    let mut modified = frozen.clone();
    modified.snapshot.byte_len += 1;
    assert!(
        owner
            .verify(target.clone(), modified, CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        owner
            .verify(
                header("foreign-target"),
                frozen.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    append(
        &*store,
        &target,
        0,
        vec![
            accepted("target-turn", "read selected material"),
            SessionFactBody::InputMessageEntered {
                turn_id: TurnId::new("target-turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::Human {
                    message_id: MessageId::new("message").unwrap(),
                },
                content: vec![AgentMessageContent::Reference {
                    reference: frozen.clone(),
                }],
            },
            terminal("target-turn"),
        ],
    )
    .await;
    let request = ReferenceReadRequest {
        recorded_session_id: target.session_id().clone(),
        fact_seq: 2,
        content_index: 0,
        offset: 0,
        maximum: 4,
    };
    let page = owner
        .read_recorded(target.clone(), request.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.recorded, Some(request.clone()));
    assert!(page.has_more);
    let child = inherited(&target, 0, 3);
    assert_eq!(
        owner
            .read_recorded(child, request.clone(), CancellationToken::new())
            .await
            .unwrap()
            .text,
        page.text
    );
    let excluded = inherited(&target, 2, 3);
    assert!(matches!(
        owner
            .read_recorded(excluded, request.clone(), CancellationToken::new())
            .await,
        Err(ReferenceError::Invalid(_))
    ));
    let foreign = ReferenceReadRequest {
        recorded_session_id: source.session_id().clone(),
        ..request.clone()
    };
    assert!(
        owner
            .read_recorded(target.clone(), foreign, CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        owner
            .read_recorded(
                target.clone(),
                ReferenceReadRequest {
                    content_index: 1,
                    ..request
                },
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        owner
            .capture(source.session_id().clone(), target, cancelled)
            .await,
        Err(ReferenceError::Cancelled)
    ));
    owner.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
