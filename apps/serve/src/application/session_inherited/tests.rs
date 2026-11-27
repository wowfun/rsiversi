use super::*;
use rsi_agent_session_protocol::{
    AgentPresetId, ForkPoint, FrozenAgentSettings, HistoryOrigin, SessionFact, SessionFactBody,
    TurnId,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore};

#[tokio::test]
async fn captured_intervals_preserve_distinct_parent_and_child_metadata() {
    let store = rsi_agent_testkit::MemoryStore::new();
    let parent = SessionHeader::new_local(
        SessionId::new("parent").unwrap(),
        11,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
            rsi_sandbox::SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let turn = TurnId::new("turn").unwrap();
    store
        .append(AppendBatch {
            session_id: parent.session_id().clone(),
            expected_seq: 0,
            header: Some(parent.clone()),
            facts: vec![Arc::new(
                SessionFact::new(
                    1,
                    12,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn.clone(),
                        text: "history".into(),
                        model: None,
                        reasoning_effort: None,
                        sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            )],
        })
        .await
        .unwrap();
    for (turns, full) in [
        (ForkTurnSelection::None, false),
        (ForkTurnSelection::All, true),
    ] {
        let point = ForkPoint::before(&turn);
        let cut = store
            .resolve_fork_boundary(parent.session_id(), &point, turns.clone())
            .await
            .unwrap();
        let child = parent
            .forked_independent(
                SessionId::new("child").unwrap(),
                22,
                HistoryOrigin {
                    parent_session_id: parent.session_id().clone(),
                    parent_header_fingerprint: parent.fingerprint().unwrap(),
                    point,
                    resolved_after_seq: cut.resolved_after_seq,
                    resolved_terminal_seq: cut.resolved_terminal_seq,
                    terminal_prefix_sha256: cut.terminal_prefix_sha256,
                    resolved_terminal_control_seq: cut.resolved_terminal_control_seq,
                    requested_turns: turns,
                    effective_turns: cut.effective_turns,
                },
            )
            .unwrap();
        let intervals = capture_inherited_intervals(&store, &child).await.unwrap();
        assert_eq!(intervals.len(), 1);
        assert_eq!(
            interval_metadata(&intervals[0]),
            json!({
                "parent": "parent", "parent_time": 11, "child": "child", "child_time": 22,
                "after": 0, "through": 0, "full": full,
            })
        );
    }
}
