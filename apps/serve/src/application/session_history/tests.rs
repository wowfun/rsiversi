use super::*;
use rsi_session_protocol::{CreateSession, RecentSessionCursor, RecentSessionPage, SessionError};
use std::sync::Arc;

#[derive(Debug)]
struct MetadataService(Vec<SessionHeader>);

#[async_trait::async_trait]
impl SessionService for MetadataService {
    async fn read_header(&self, id: &SessionId) -> rsi_session_protocol::Result<SessionHeader> {
        match id.as_str() {
            "denied" => Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized)),
            "failed" => Err(SessionError::Backend("read failed".into())),
            _ => self
                .0
                .iter()
                .find(|header| header.session_id() == id)
                .cloned()
                .ok_or_else(|| SessionError::NotFound(id.to_string())),
        }
    }
    async fn create(
        &self,
        _: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        panic!("metadata must not create a Session")
    }
    async fn attach(&self, _: &SessionId) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        panic!("metadata must not attach a Session or open runtime activity")
    }
    async fn list_recent(
        &self,
        _: Option<&RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<RecentSessionPage> {
        panic!("metadata must not enumerate Sessions")
    }
}

fn metadata_header(id: &str) -> SessionHeader {
    use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings};
    SessionHeader::new_local(
        SessionId::new(id).unwrap(),
        1,
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
    .unwrap()
}

#[tokio::test]
async fn metadata_reads_do_not_attach_and_preserve_visibility_and_failures() {
    use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
    use rsi_agent_store_protocol::AppendBatch;
    use rsi_agent_testkit::MemoryStore;
    let header = metadata_header("metadata");
    let id = header.session_id();
    let service = MetadataService(vec![header.clone()]);
    let store = MemoryStore::default();
    // No durable row, including an unpublished draft, is not an error.
    assert_eq!(
        metadata_snapshot(&service, &store, id, None).await.unwrap(),
        Value::Null
    );
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![Arc::new(
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("turn").unwrap(),
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
    let snapshot = metadata_snapshot(&service, &store, id, None).await.unwrap();
    assert_eq!(snapshot["id"], id.as_str());
    assert_eq!(snapshot["created_at"], 1);
    assert_eq!(
        snapshot["selection_cursor"],
        json!({"kind":"durable", "fact_seq":1, "control_seq":0})
    );
    assert!(store.take_fact_read_cursors().is_empty());
    for absent in ["missing", "denied"] {
        assert_eq!(
            metadata_snapshot(&service, &store, &SessionId::new(absent).unwrap(), None)
                .await
                .unwrap(),
            Value::Null
        );
    }
    assert!(matches!(
        metadata_snapshot(&service, &store, &SessionId::new("failed").unwrap(), None).await,
        Err(Error::Remote(_))
    ));
}

#[tokio::test]
async fn metadata_parent_address_requires_visible_direct_parent() {
    use rsi_agent_session_protocol::{
        AgentPath, EMPTY_CONTROL_PREFIX_DIGEST, EMPTY_FACT_PREFIX_DIGEST, ForkOrigin, ForkPoint,
        ForkTurnSelection, HistoryOrigin, TurnId,
    };
    let parent = metadata_header("parent");
    let child = metadata_header("child")
        .with_fork_origin(ForkOrigin {
            history: HistoryOrigin {
                parent_session_id: parent.session_id().clone(),
                parent_header_fingerprint: parent.fingerprint().unwrap(),
                point: ForkPoint::before(&TurnId::new("turn").unwrap()),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: hex::encode(EMPTY_FACT_PREFIX_DIGEST),
                resolved_terminal_control_seq: 0,
                terminal_control_prefix_sha256: hex::encode(EMPTY_CONTROL_PREFIX_DIGEST),
                requested_turns: ForkTurnSelection::None,
                effective_turns: 0,
            },
            root_session_id: parent.session_id().clone(),
            path: AgentPath::new(vec![1]).unwrap(),
            task_name: "child".into(),
        })
        .unwrap();
    let other = metadata_header("other");
    let service = MetadataService(vec![parent.clone(), child.clone(), other.clone()]);
    let store = rsi_agent_testkit::MemoryStore::default();
    // The metadata path checks parent access and lineage before querying the Store.
    assert_eq!(
        metadata_snapshot(
            &service,
            &store,
            child.session_id(),
            Some(parent.session_id())
        )
        .await
        .unwrap(),
        Value::Null
    );
    let hidden_parent = MetadataService(vec![child.clone()]);
    assert!(
        metadata_snapshot(
            &hidden_parent,
            &store,
            child.session_id(),
            Some(parent.session_id()),
        )
        .await
        .is_err()
    );
    for invalid in ["denied", "missing", "other"] {
        assert!(matches!(
            metadata_snapshot(
                &service,
                &store,
                child.session_id(),
                Some(&SessionId::new(invalid).unwrap())
            )
            .await,
            Err(Error::Remote(_))
        ));
    }
    assert!(
        metadata_snapshot(
            &service,
            &store,
            other.session_id(),
            Some(parent.session_id())
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn fact_pages_bound_the_captured_horizon_and_read_large_records_alone() {
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, TurnId,
    };
    use rsi_agent_store_protocol::AppendBatch;
    use rsi_agent_testkit::MemoryStore;
    use rsi_ai_protocol::ModelRef;
    use rsi_sandbox::SandboxMode;
    use std::sync::Arc;

    let store = MemoryStore::default();
    let id = SessionId::new("page-test").unwrap();
    let turn = TurnId::new("turn").unwrap();
    let header = SessionHeader::new_local(
        id.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("test", "test").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let facts: Vec<_> = (1..=600)
        .map(|seq| {
            Arc::new(
                SessionFact::new(
                    seq,
                    seq,
                    if seq == 1 {
                        SessionFactBody::TurnAccepted {
                            turn_id: turn.clone(),
                            text: "history".repeat(24_000),
                            model: None,
                            reasoning_effort: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        }
                    } else {
                        SessionFactBody::CancelRequested {
                            turn_id: turn.clone(),
                            reason: None,
                        }
                    },
                )
                .unwrap(),
            )
        })
        .collect();
    for (index, chunk) in facts.chunks(64).enumerate() {
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: (index * 64) as u64,
                header: (index == 0).then(|| header.clone()),
                facts: chunk.to_vec(),
            })
            .await
            .unwrap();
    }
    let large = read_facts(&store, &id, 0, 600, 0).await.unwrap();
    assert_eq!(large["through_seq"], 1);
    assert_eq!(large["next_offset"], 128 * 1024);
    let tail = read_facts(&store, &id, 0, 600, 128 * 1024).await.unwrap();
    assert_eq!(tail["through_seq"], 1);
    assert_eq!(large["total_bytes"], tail["total_bytes"]);
    let encoded = format!(
        "{}{}",
        large["json"].as_str().unwrap(),
        tail["json"].as_str().unwrap()
    );
    assert_eq!(
        serde_json::from_str::<Vec<SessionFact>>(&encoded).unwrap(),
        vec![facts[0].as_ref().clone()]
    );
    let cut = read_facts(&store, &id, 1, 10, 0).await.unwrap();
    let page: Vec<SessionFact> = serde_json::from_str(cut["json"].as_str().unwrap()).unwrap();
    assert_eq!(page.len(), 9);
    assert_eq!(page.first().unwrap().seq(), 2);
    assert_eq!(cut["through_seq"], 10);
    let capped = read_facts(&store, &id, 1, 600, 0).await.unwrap();
    assert_eq!(capped["through_seq"], 513);
    assert!(read_facts(&store, &id, 10, 10, 0).await.is_err());
    assert!(read_facts(&store, &id, 11, 10, 0).await.is_err());
    assert!(read_facts(&store, &id, 1, 601, 0).await.is_err());
}

#[test]
fn model_projection_retains_effort_absence_and_does_not_default_a_failed_producer() {
    use rsi_agent_session_protocol::{ContributionId, ProjectionEntry, ProjectionValue};
    let producer = ContributionId::new(rsi_agent_model_selection::VIEW).unwrap();
    assert!(projected_selection(&[]).unwrap().is_none());
    let selection = json!({"model":{"deployment":"native","model":"selected"}});
    let row = ProjectionEntry::value(
        producer.clone(),
        ProjectionValue::encode(&selection).unwrap(),
    );
    let parsed = projected_selection(&[row]).unwrap().unwrap();
    assert_eq!(parsed.model.model(), "selected");
    assert!(parsed.reasoning_effort.is_none());
    let failed = ProjectionEntry::failed(producer, "state unavailable").unwrap();
    assert!(
        matches!(projected_selection(&[failed]), Err(Error::Remote(message)) if message == "state unavailable")
    );
}

#[test]
fn history_fragments_preserve_multibyte_text_and_reject_invalid_offsets() {
    let prefix = "x".repeat(128 * 1024 - 1);
    let data = format!("{prefix}历史");
    let first = fragment(&data, 0, 7).unwrap();
    assert_eq!(first["json"], prefix);
    let next = prefix.len();
    let last = fragment(&data, next, 7).unwrap();
    assert_eq!(last["json"], "历史");
    assert_eq!(last["next_offset"], data.len());
    assert!(fragment(&data, next + 1, 7).is_err());
    assert!(fragment(&data, data.len(), 7).is_err());
}
