use rsi_agent_session_protocol::{
    AgentControlRecord, AgentControlRecordBody, AgentMessage, AgentMessageContent,
    AgentMessageSource, ExecutionCoordinates, ExecutionLocation, ExecutionTargetId,
    MessageDelivery, MessageId, MessageOptions, MessageTarget, SessionFact, SessionFactBody,
    SessionHeader, SessionId, StepId, TurnId,
};
use rsi_agent_store_protocol::{
    AppendBatch, AtomicAgentCommit, AtomicSessionAppend, ExecutionLocations, SessionStore,
    StoreOrderSeed,
};
use rsi_sandbox::SandboxMode;
use std::sync::Arc;

async fn seed(
    store: &dyn SessionStore,
    template: &SessionHeader,
    id: &str,
    time: u64,
    coordinates: ExecutionCoordinates,
) -> SessionId {
    let id = SessionId::new(id).unwrap();
    let header = SessionHeader::new(
        id.clone(),
        time,
        coordinates,
        template.agent_preset_id().clone(),
        template.settings().clone(),
    )
    .unwrap();
    let fact = SessionFact::new(
        1,
        time + 1,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("turn").unwrap(),
            text: "input".into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: vec![Arc::new(fact)],
        })
        .await
        .unwrap();
    id
}
async fn append(
    store: &dyn SessionStore,
    id: &SessionId,
    seq: u64,
    time: u64,
    body: SessionFactBody,
) {
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: seq - 1,
            header: None,
            facts: vec![Arc::new(SessionFact::new(seq, time, body).unwrap())],
        })
        .await
        .unwrap();
}
async fn filtered_recent(
    store: &dyn SessionStore,
    location: &ExecutionLocation,
    a: &SessionId,
    b: &SessionId,
) {
    let local_scope =
        ExecutionLocations::only(std::collections::BTreeSet::from([location.clone()])).unwrap();
    let recent = store
        .list_recent_sessions(&local_scope, None, 1)
        .await
        .unwrap();
    assert_eq!(recent.sessions[0].header.session_id(), b);
    assert!(recent.has_more);
    let next = store
        .list_recent_sessions(&local_scope, Some(&recent.sessions[0].cursor()), 1)
        .await
        .unwrap();
    assert_eq!(next.sessions[0].header.session_id(), a);
    assert!(!next.has_more);
    let empty_scope = ExecutionLocations::only(std::collections::BTreeSet::new()).unwrap();
    assert!(
        store
            .list_recent_sessions(&empty_scope, None, 1)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
}
/// Exercises indexed activity isolation, live continuation and atomic projections.
///
/// # Panics
/// Panics when an empty Store violates the shared metadata contract.
pub async fn assert_activity_store_contract(store: &dyn SessionStore, template: &SessionHeader) {
    let local = template.coordinates().clone();
    let remote = ExecutionCoordinates::new(
        ExecutionLocation::Ssh {
            target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        local.path(),
    )
    .unwrap();
    let a = seed(store, template, "activity-a", 10, local.clone()).await;
    let b = seed(store, template, "activity-b", 20, local.clone()).await;
    let remote_id = seed(store, template, "activity-remote", 30, remote.clone()).await;
    filtered_recent(store, local.location(), &a, &b).await;
    filtered_cursors(store, &a, &b, &remote).await;
    exact_summaries(store, &a, &b, &remote_id).await;
    let global = store
        .list_session_activity(&ExecutionLocations::all(), None, None, 3)
        .await
        .unwrap();
    assert_eq!(
        global
            .sessions
            .iter()
            .map(|row| &row.session_id)
            .collect::<Vec<_>>(),
        vec![&remote_id, &b, &a]
    );
    let first = store
        .list_session_activity(&ExecutionLocations::all(), Some(&local), None, 1)
        .await
        .unwrap();
    assert_eq!(first.sessions[0].session_id, b);
    assert!(first.has_more);
    let cursor = first.sessions[0].cursor();
    let turn = TurnId::new("turn").unwrap();
    append(
        store,
        &a,
        2,
        400,
        SessionFactBody::StepStarted {
            turn_id: turn.clone(),
            step_id: StepId::new("step").unwrap(),
        },
    )
    .await;
    let unchanged = store
        .list_session_activity(&ExecutionLocations::all(), Some(&local), None, 1)
        .await
        .unwrap();
    assert_eq!(unchanged.newest, first.newest);
    append(
        store,
        &a,
        3,
        50,
        SessionFactBody::CancelRequested {
            turn_id: turn.clone(),
            reason: None,
        },
    )
    .await;
    let continuation = store
        .list_session_activity(&ExecutionLocations::all(), Some(&local), Some(&cursor), 2)
        .await
        .unwrap();
    assert!(continuation.sessions.is_empty());
    assert!(!continuation.has_more);
    assert_eq!(continuation.newest.as_ref().unwrap().session_id, a);
    assert_eq!(continuation.newest.as_ref().unwrap().last_activity_ms, 50);
    rollback_does_not_advance(store, &a, &local, continuation.newest.clone()).await;
    assert_eq!(
        store
            .session_order_seed(&ExecutionLocations::all(), Some(&local))
            .await
            .unwrap(),
        StoreOrderSeed::from_members(vec![(a.clone(), local.clone(), 50), (b, local.clone(), 21)])
            .unwrap()
    );
    assert_eq!(
        store
            .session_order_seed(&ExecutionLocations::all(), Some(&remote))
            .await
            .unwrap(),
        StoreOrderSeed::from_members(vec![(remote_id, remote.clone(), 31)]).unwrap()
    );
    new_human(store, &a).await;
    let final_page = store
        .list_session_activity(&ExecutionLocations::all(), Some(&local), None, 2)
        .await
        .unwrap();
    assert_eq!(final_page.sessions[0].last_activity_ms, 60);
}

async fn exact_summaries(
    store: &dyn SessionStore,
    a: &SessionId,
    b: &SessionId,
    remote: &SessionId,
) {
    let absent = SessionId::new("absent").unwrap();
    let requested = [b.clone(), absent, remote.clone(), a.clone()];
    let rows = store.session_activity_summaries(&requested).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows[1].is_none());
    for index in [0, 2, 3] {
        assert_eq!(rows[index].as_ref().unwrap().session_id, requested[index]);
    }
    assert!(
        store
            .session_activity_summaries(&[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .session_activity_summaries(&[a.clone(), a.clone()])
            .await
            .is_err()
    );
    let too_many: Vec<_> = (0..65)
        .map(|index| SessionId::new(format!("summary-{index}")).unwrap())
        .collect();
    assert!(store.session_activity_summaries(&too_many).await.is_err());
}
async fn new_human(store: &dyn SessionStore, session: &SessionId) {
    let message = AgentMessage {
        message_id: MessageId::new("new-human").unwrap(),
        source: AgentMessageSource::Human,
        content: vec![AgentMessageContent::Text {
            text: "next".into(),
        }],
        options: MessageOptions::default(),
    };
    let control = AgentControlRecord::new(
        1,
        60,
        AgentControlRecordBody::MessageAccepted {
            message,
            delivery: MessageDelivery::NextTurn,
            bound_turn_id: None,
            root_session_id: session.clone(),
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    )
    .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session.clone(),
                expected_fact_seq: 4,
                expected_control_seq: 0,
                header: None,
                facts: vec![],
                controls: vec![control],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
}

/// Proves complete order membership crosses page boundaries and fails closed at 1025.
///
/// # Panics
/// Panics when membership is truncated or its declared cardinality bound changes.
pub async fn assert_activity_membership_bounds(store: &dyn SessionStore, template: &SessionHeader) {
    for index in 0..1025 {
        seed(
            store,
            template,
            &format!("member-{index:04}"),
            1,
            template.coordinates().clone(),
        )
        .await;
        if [63, 64, 1023].contains(&index) {
            let StoreOrderSeed::Available { members, .. } = store
                .session_order_seed(&ExecutionLocations::all(), Some(template.coordinates()))
                .await
                .unwrap()
            else {
                panic!("bounded membership rejected")
            };
            assert_eq!(members.len(), index + 1);
            assert_eq!(
                members[index].session.as_str(),
                format!("member-{index:04}")
            );
        }
    }
    assert_eq!(
        store
            .session_order_seed(&ExecutionLocations::all(), Some(template.coordinates()))
            .await
            .unwrap(),
        StoreOrderSeed::TooLarge
    );
    assert_eq!(
        store
            .list_session_activity(
                &ExecutionLocations::all(),
                Some(template.coordinates()),
                None,
                64
            )
            .await
            .unwrap()
            .sessions
            .len(),
        64
    );
    let remote = ExecutionCoordinates::new(
        ExecutionLocation::Ssh {
            target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        template.canonical_cwd(),
    )
    .unwrap();
    let remote_id = seed(store, template, "only-visible-member", 2, remote.clone()).await;
    let visible = ExecutionLocations::only(std::collections::BTreeSet::from([remote
        .location()
        .clone()]))
    .unwrap();
    let StoreOrderSeed::Available { members, groups } =
        store.session_order_seed(&visible, None).await.unwrap()
    else {
        panic!("1025 hidden members exhausted visible membership budget");
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].session, remote_id);
    assert_eq!(groups, vec![remote]);
}

async fn rollback_does_not_advance(
    store: &dyn SessionStore,
    a: &SessionId,
    local: &ExecutionCoordinates,
    newest: Option<rsi_agent_store_protocol::StoreActivityCursor>,
) {
    append(
        store,
        a,
        4,
        3,
        SessionFactBody::CancelRequested {
            turn_id: TurnId::new("turn").unwrap(),
            reason: None,
        },
    )
    .await;
    assert_eq!(
        store
            .list_session_activity(&ExecutionLocations::all(), Some(local), None, 2)
            .await
            .unwrap()
            .newest,
        newest
    );
    let conflict = AppendBatch {
        session_id: a.clone(),
        expected_seq: 0,
        header: None,
        facts: vec![Arc::new(
            SessionFact::new(
                1,
                900,
                SessionFactBody::CancelRequested {
                    turn_id: TurnId::new("turn").unwrap(),
                    reason: None,
                },
            )
            .unwrap(),
        )],
    };
    assert!(store.append(conflict).await.is_err());
    assert_eq!(
        store
            .list_session_activity(&ExecutionLocations::all(), Some(local), None, 2)
            .await
            .unwrap()
            .newest,
        newest
    );
}

async fn filtered_cursors(
    store: &dyn SessionStore,
    a: &SessionId,
    b: &SessionId,
    remote: &ExecutionCoordinates,
) {
    let visible =
        ExecutionLocations::only(std::collections::BTreeSet::from([ExecutionLocation::Local]))
            .unwrap();
    let filtered = store
        .list_session_activity(&visible, None, None, 1)
        .await
        .unwrap();
    assert_eq!(&filtered.sessions[0].session_id, b);
    assert_eq!(&filtered.newest.as_ref().unwrap().session_id, b);
    assert!(filtered.has_more);
    let next = store
        .list_session_activity(&visible, None, Some(&filtered.sessions[0].cursor()), 1)
        .await
        .unwrap();
    assert_eq!(&next.sessions[0].session_id, a);
    assert!(!next.has_more);
    assert!(
        store
            .list_session_activity(&visible, Some(remote), None, 1)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
}
