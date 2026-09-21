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
        result: None,
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
    assert_eq!(frozen.metadata.through_seq(), 2);
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
    let rsi_agent_session_protocol::ReferenceSource::Native { binding } =
        &mut modified.metadata.source
    else {
        panic!("native")
    };
    binding.session_id = SessionId::new("forged").unwrap();
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One causal source lifecycle checks old selection, tampering and immutable growth"
)]
async fn exact_old_selection_is_reread_and_frozen_across_growth_with_unicode_and_source_fencing() {
    use sha2::{Digest as _, Sha256};
    let runtime = rsi_meta::Runtime::default();
    let store = Arc::new(MemoryStore::default());
    let source = header("old-source");
    let target = header("selected-target");
    let text = "old 🦀精确 selected evidence";
    append(
        &*store,
        &source,
        0,
        vec![accepted("old", text), terminal("old")],
    )
    .await;
    // The original is over 1024 Facts behind the durable horizon.
    for index in 0..520 {
        let turn = format!("later-{index}");
        append(
            &*store,
            &source,
            2 + index * 2,
            vec![accepted(&turn, "later"), terminal(&turn)],
        )
        .await;
    }
    let window = store
        .read_fact_window(source.session_id(), 0, 1, MAXIMUM_REFERENCE_SCAN_BYTES)
        .await
        .unwrap();
    let selection = ReferenceSelection {
        record: ReferenceRecord {
            sequence: 1,
            kind: ReferenceContentKind::Human,
            content_index: 0,
        },
        through_seq: window.durable_seq,
        start: 4,
        end: 14,
        text_sha256: hex::encode(Sha256::digest(text.as_bytes())),
        scanned_bytes: window.encoded_bytes,
    };
    let binding = ReferenceBinding {
        session_id: source.session_id().clone(),
        header_sha256: source.fingerprint().unwrap(),
    };
    let owner = References::new(store.clone(), runtime.execution().clone());
    let frozen = owner
        .capture_selected(
            binding.clone(),
            target.clone(),
            selection.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(frozen.preview, "🦀精确");
    assert_eq!(frozen.metadata.retained_interval(), (1, 1));
    for changed in [
        ReferenceSelection {
            start: 5,
            ..selection.clone()
        },
        ReferenceSelection {
            text_sha256: "b".repeat(64),
            ..selection.clone()
        },
        ReferenceSelection {
            scanned_bytes: selection.scanned_bytes + 1,
            ..selection.clone()
        },
        ReferenceSelection {
            through_seq: selection.through_seq + 1,
            ..selection.clone()
        },
        ReferenceSelection {
            record: ReferenceRecord {
                kind: ReferenceContentKind::ToolEvidence,
                ..selection.record.clone()
            },
            ..selection.clone()
        },
    ] {
        assert!(
            owner
                .capture_selected(
                    binding.clone(),
                    target.clone(),
                    changed,
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
    let foreign = SessionHeader::new(
        SessionId::new("foreign").unwrap(),
        1,
        "/foreign",
        target.agent_preset_id().clone(),
        target.settings().clone(),
    )
    .unwrap();
    assert!(
        owner
            .capture_selected(
                binding.clone(),
                foreign,
                selection.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    append(
        &*store,
        &source,
        window.durable_seq,
        vec![accepted("growth", "new"), terminal("growth")],
    )
    .await;
    let again = owner
        .capture_selected(binding, target.clone(), selection, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(again, frozen);
    let page = owner
        .preview(target, frozen, 0, 65536, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.text, "🦀精确");
    owner.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn observed_capture_never_claims_native_facts_and_checks_original_bytes() {
    use rsi_agent_references::ObservedReferenceText;
    use sha2::{Digest as _, Sha256};
    let runtime = rsi_meta::Runtime::default();
    let owner = References::new(
        Arc::new(MemoryStore::default()),
        runtime.execution().clone(),
    );
    let selection = ReferenceSelection {
        record: ReferenceRecord {
            sequence: 8,
            kind: ReferenceContentKind::Assistant,
            content_index: 0,
        },
        through_seq: 10,
        start: 0,
        end: 8,
        text_sha256: hex::encode(Sha256::digest(b"observed")),
        scanned_bytes: 128,
    };
    let source = ReferenceSource::Observed {
        owner: "acp".into(),
        id: "external-id".into(),
        epoch: 3,
    };
    let frozen = owner
        .capture_observed(
            ObservedReferenceText {
                source: source.clone(),
                canonical_cwd: "/workspace".into(),
                text: "observed".into(),
                selection: selection.clone(),
            },
            header("target"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(frozen.metadata.source, source);
    assert!(
        owner
            .capture_observed(
                ObservedReferenceText {
                    source,
                    canonical_cwd: "/different".into(),
                    text: "observed".into(),
                    selection
                },
                header("target"),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let mut old_envelope = serde_json::json!({"version":1,"metadata":frozen.metadata,"preview":"observed","text":"observed"});
    assert!(
        serde_json::from_value::<ReferenceSnapshotEnvelope>(old_envelope.clone())
            .unwrap()
            .validate()
            .is_err()
    );
    old_envelope["version"] = 2.into();
    serde_json::from_value::<ReferenceSnapshotEnvelope>(old_envelope)
        .unwrap()
        .validate()
        .unwrap();
    owner.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
