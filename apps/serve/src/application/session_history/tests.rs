use super::super::session_fragments::tests::FragmentReader;
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
async fn large_history_page_read_cost() {
    use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
    use rsi_agent_store_protocol::AppendBatch;
    let reader = FragmentReader::new();
    let store = rsi_agent_testkit::MemoryStore::new();
    let header = metadata_header("fragment-cost");
    let id = header.session_id().clone();
    let fact = Arc::new(
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn").unwrap(),
                text: "界".repeat(300_000),
                model: None,
                reasoning_effort: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: vec![fact.clone()],
        })
        .await
        .unwrap();
    store.take_fact_read_cursors();
    let started = std::time::Instant::now();
    let mut encoded = String::new();
    let mut fragments = 0;
    loop {
        let page = reader
            .facts(&store, &id, 0, 1, encoded.len())
            .await
            .unwrap();
        fragments += 1;
        encoded.push_str(page["json"].as_str().unwrap());
        if page["next_offset"] == page["total_bytes"] {
            break;
        }
    }
    let reads = store.take_fact_read_cursors().len();
    assert_eq!(
        serde_json::from_str::<Vec<SessionFact>>(&encoded).unwrap(),
        vec![fact.as_ref().clone()]
    );
    eprintln!(
        "history_probe fragments={fragments} store_reads={reads} serialized_bytes={} elapsed_ms={}",
        encoded.len(),
        started.elapsed().as_millis()
    );
    assert_eq!(
        reads, 1,
        "a sub-frame logical record needs no capacity probe"
    );
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
    assert_eq!(snapshot["through"], 1);
    assert_eq!(
        snapshot["model_baseline"],
        json!({"model":header.settings().default_model(), "reasoning_effort":null})
    );
    assert!(snapshot.get("model_selection").is_none());
    let catalog = stored_snapshot(&header, 1, 0);
    assert!(catalog.get("model_baseline").is_none());
    assert!(catalog.get("through").is_none());
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

async fn withdrawn_message_fixture(
    text: String,
) -> (
    rsi_agent_testkit::MemoryStore,
    SessionHeader,
    rsi_agent_session_protocol::AgentMessage,
) {
    use rsi_agent_session_protocol::{
        AgentControlRecord, AgentControlRecordBody as Body, AgentMessage, AgentMessageContent,
        AgentMessageSource, MessageDelivery, MessageDiscardReason, MessageId, MessageOptions,
        MessageTarget,
    };
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let store = rsi_agent_testkit::MemoryStore::default();
    let header = metadata_header("exact-message");
    let id = header.session_id().clone();
    let message = AgentMessage {
        message_id: MessageId::new("message").unwrap(),
        source: AgentMessageSource::Human { attribution: None },
        content: vec![AgentMessageContent::Text { text }],
        options: MessageOptions::default(),
    };
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header.clone()),
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        1,
                        1,
                        Body::MessageAccepted {
                            message: message.clone(),
                            delivery: MessageDelivery::NextTurn,
                            bound_turn_id: None,
                            root_session_id: id.clone(),
                            target: MessageTarget::NextTurn,
                            wake_required: true,
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        2,
                        2,
                        Body::MessageDiscarded {
                            message_id: message.message_id.clone(),
                            reason: MessageDiscardReason::Cancelled,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    (store, header, message)
}

#[tokio::test]
async fn control_pages_keep_moderate_acceptances_and_transitions_in_one_read() {
    let reader = FragmentReader::new();
    let (store, header, message) = withdrawn_message_fixture("界".repeat(100_000)).await;
    let page = reader.controls(&store, &header, 0, 2, 0).await.unwrap();
    assert_eq!(page["through_seq"], 2);
    assert_eq!(page["next_offset"], page["total_bytes"]);
    let events: Value = serde_json::from_str(page["json"].as_str().unwrap()).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 2);
    assert_eq!(events[0]["message"], json!(message));
    assert_eq!(events[0]["after_fact_seq"], 0);
    assert_eq!(events[1]["kind"], "discarded");
    let discarded = reader.controls(&store, &header, 1, 2, 0).await.unwrap();
    assert_eq!(discarded["through_seq"], 2);
    assert_eq!(
        serde_json::from_str::<Value>(discarded["json"].as_str().unwrap()).unwrap()[0]["kind"],
        "discarded"
    );
    for (after, through) in [(0, 0), (2, 1), (0, 3)] {
        assert!(
            reader
                .controls(&store, &header, after, through, 0)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn control_pages_pack_escaped_acceptance_and_transition_before_fragmenting() {
    let reader = FragmentReader::new();
    let (store, header, message) = withdrawn_message_fixture("\"".repeat(400_000)).await;
    let mut encoded = String::new();
    let mut fragments = 0;
    loop {
        let page = reader
            .controls(&store, &header, 0, 2, encoded.len())
            .await
            .unwrap();
        assert_eq!(page["through_seq"], 2);
        assert!(serde_json::to_vec(&page).unwrap().len() < rsi_extension::MAXIMUM_FRAME_BYTES);
        encoded.push_str(page["json"].as_str().unwrap());
        fragments += 1;
        if page["next_offset"] == page["total_bytes"] {
            break;
        }
    }
    assert_eq!(fragments, 2);
    let events: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 2);
    assert_eq!(events[0]["message"], json!(message));
    assert_eq!(events[1]["kind"], "discarded");
}

#[tokio::test]
async fn exact_message_reads_preserve_withdrawn_bodies_and_fragment_identity() {
    use rsi_agent_session_protocol::MessageId;
    let reader = FragmentReader::new();
    let (store, header, message) = withdrawn_message_fixture("界".repeat(100_000)).await;
    let id = header.session_id();
    let expected = serde_json::to_string(&message).unwrap();
    let mut restored = String::new();
    while restored.len() < expected.len() {
        let page = reader
            .message(&store, id, &message.message_id, 1, restored.len())
            .await
            .unwrap();
        assert_eq!(page["through_seq"], 1);
        assert_eq!(page["total_bytes"], expected.len());
        assert_eq!(page["offset"], restored.len());
        restored.push_str(page["json"].as_str().unwrap());
        assert_eq!(page["next_offset"], restored.len());
    }
    assert_eq!(restored, expected);
    for seq in [0, 2, 3] {
        assert!(
            reader
                .message(&store, id, &message.message_id, seq, 0)
                .await
                .is_err()
        );
    }
    assert!(
        reader
            .message(&store, id, &MessageId::new("missing").unwrap(), 1, 0)
            .await
            .is_err()
    );
    for offset in [expected.find('界').unwrap() + 1, expected.len(), usize::MAX] {
        assert!(
            reader
                .message(&store, id, &message.message_id, 1, offset)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn stored_pages_inline_normalized_attempts_and_preserve_unicode() {
    use rsi_agent_session_protocol::{
        EffectId, ModelAttemptLog, ModelEventPurpose, SessionFact, SessionFactBody, TurnId,
    };
    let reader = FragmentReader::new();
    let store = rsi_agent_testkit::MemoryStore::default();
    let header = metadata_header("stored-attempt");
    let id = header.session_id().clone();
    let events = [
        (
            0,
            json!({"type":"content_started","index":0,"content":{"type":"text"}}),
        ),
        (
            7,
            json!({"type":"content_delta","index":0,"delta":{"type":"text","value":"中文".repeat(35_000)}}),
        ),
        (11, json!({"type":"content_finished","index":0})),
        (13, json!({"type":"finished","reason":"stop","replay":null})),
    ];
    let fact = Arc::new(
        SessionFact::new(
            2,
            10,
            SessionFactBody::ModelAttemptSettled {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("model").unwrap(),
                attempt: ModelAttemptLog::from_events(
                    events
                        .into_iter()
                        .map(|(time, event)| (time, serde_json::from_value(event).unwrap())),
                    true,
                )
                .unwrap(),
                purpose: ModelEventPurpose::Conversation,
            },
        )
        .unwrap(),
    );
    store
        .append(rsi_agent_store_protocol::AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: vec![
                Arc::new(
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            turn_id: TurnId::new("turn").unwrap(),
                            text: "input".into(),
                            model: None,
                            reasoning_effort: None,
                            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                ),
                fact.clone(),
                Arc::new(
                    SessionFact::new(
                        3,
                        11,
                        SessionFactBody::CancelRequested {
                            turn_id: TurnId::new("turn").unwrap(),
                            reason: None,
                        },
                    )
                    .unwrap(),
                ),
            ],
        })
        .await
        .unwrap();
    let (_, expected) = rsi_extension_agent::project_history_page([fact.as_ref()], 2).unwrap();
    let projected: Value = serde_json::from_str(&expected).unwrap();
    assert!(projected[0]["attempt"].get("chunks").is_none());
    assert!(projected[0]["attempt"]["events"].is_array());
    assert!(serde_json::to_value(&fact).unwrap()["attempt"]["chunks"].is_array());

    let mut restored = String::new();
    while restored.len() < expected.len() {
        let page = reader
            .facts(&store, &id, 1, 2, restored.len())
            .await
            .unwrap();
        assert_eq!(page["through_seq"], 2);
        assert_eq!(page["total_bytes"], expected.len());
        restored.push_str(page["json"].as_str().unwrap());
        assert_eq!(page["next_offset"], restored.len());
    }
    assert_eq!(restored, expected);
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
    let reader = FragmentReader::new();

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
                            text: "\n".repeat(600_000),
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
    let large = reader.facts(&store, &id, 0, 600, 0).await.unwrap();
    assert_eq!(large["through_seq"], 1);
    let offset = usize::try_from(large["next_offset"].as_u64().unwrap()).unwrap();
    assert!(offset < usize::try_from(large["total_bytes"].as_u64().unwrap()).unwrap());
    let tail = reader.facts(&store, &id, 0, 600, offset).await.unwrap();
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
    let cut = reader.facts(&store, &id, 1, 10, 0).await.unwrap();
    let page: Vec<SessionFact> = serde_json::from_str(cut["json"].as_str().unwrap()).unwrap();
    assert_eq!(page.len(), 9);
    assert_eq!(page.first().unwrap().seq(), 2);
    assert_eq!(cut["through_seq"], 10);
    let capped = reader.facts(&store, &id, 1, 600, 0).await.unwrap();
    assert_eq!(capped["through_seq"], 513);
    assert!(reader.facts(&store, &id, 10, 10, 0).await.is_err());
    assert!(reader.facts(&store, &id, 11, 10, 0).await.is_err());
    assert!(reader.facts(&store, &id, 1, 601, 0).await.is_err());
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
fn history_fragments_pack_json_escaped_text_inside_the_process_frame() {
    use rsi_extension::{Frame, MAXIMUM_FRAME_BYTES, Message, Response};
    for unit in ["x", "\"\\\n\r\t\u{0008}\u{000c}\u{0000}", "历史🦀"] {
        let data = unit.repeat(MAXIMUM_FRAME_BYTES / unit.len() + 1);
        let mut offset = 0;
        let mut joined = String::new();
        while offset < data.len() {
            let mut page = fragment(&data, offset, u64::MAX).unwrap();
            let part = page["json"].as_str().unwrap();
            // Fill the existing wire capacity even for heavily escaped text.
            if page["next_offset"] != page["total_bytes"] {
                assert!(serde_json::to_vec(part).unwrap().len() > MAXIMUM_FRAME_BYTES * 9 / 10);
            }
            joined.push_str(part);
            offset = usize::try_from(page["next_offset"].as_u64().unwrap()).unwrap();
            page["reader"] = json!("r".repeat(64));
            let frame = Frame {
                generation: "g".repeat(128),
                message: Message::Reply {
                    id: (1_u64 << 53) - 1,
                    result: Response::Ok(page),
                },
            };
            assert!(serde_json::to_vec(&frame).unwrap().len() <= MAXIMUM_FRAME_BYTES);
        }
        assert_eq!(joined, data);
    }
}

#[test]
fn history_fragments_preserve_multibyte_text_and_reject_invalid_offsets() {
    let prefix = "x".repeat(FRAGMENT_JSON_BYTES - 1);
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
