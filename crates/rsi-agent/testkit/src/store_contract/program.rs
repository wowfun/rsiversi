use super::{
    ActivationId, AgentControlRecord, AgentControlRecordBody, AgentMessage, AgentMessageContent,
    AgentMessageSource, AppendBatch, AtomicAgentCommit, AtomicSessionAppend, ForkTurnSelection,
    MessageId, MessageOptions, MessageTarget, SessionFact, SessionHeader, SessionId, SessionStore,
    TurnId,
};
use rsi_agent_session_protocol::{
    EffectId, ModelSelection, ProgramBlob, ProgramForkBoundary, ProgramOutcome,
    ProgramRunDescriptor, ProgramRunEvent, ProgramRunId,
};
use rsi_agent_store_protocol::Result;
async fn append_program(
    store: &dyn SessionStore,
    id: &SessionId,
    run: &ProgramRunId,
    event: ProgramRunEvent,
) -> Result<()> {
    let tail = store.read_watermarks(id).await?;
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                expected_fact_seq: tail.durable_fact_seq,
                expected_control_seq: tail.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        tail.durable_control_seq + 1,
                        1,
                        AgentControlRecordBody::ProgramRun {
                            run_id: run.clone(),
                            event,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .map(|_| ())
}
/// Exercises Program admission, graph proofs, budget saturation and suffix reads.
/// The supplied template and first accepted Fact seed an isolated Session.
///
/// # Panics
/// Panics if the Store violates the mechanical Program contract.
#[allow(clippy::too_many_lines)] // One parity scenario checks admission, saturation, every closure receipt and restart.
pub async fn assert_program_store_contract(
    store: &dyn SessionStore,
    template: &SessionHeader,
    accepted: &SessionFact,
) -> (SessionId, ProgramRunId) {
    let mut value = serde_json::to_value(template).unwrap();
    value["session_id"] = serde_json::json!("program-store");
    let header: SessionHeader = serde_json::from_value(value).unwrap();
    let id = header.session_id().clone();
    let run = ProgramRunId::new("program-1").unwrap();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![accepted.clone().into()],
        })
        .await
        .unwrap();
    let descriptor = ProgramRunDescriptor {
        run_id: run.clone(),
        session_id: id.clone(),
        creator_turn_id: accepted.body().turn_id().clone(),
        creator_effect_id: EffectId::new("workflow").unwrap(),
        parent_header_sha256: header.fingerprint().unwrap(),
        fork: ProgramForkBoundary {
            requested_turns: ForkTurnSelection::None,
            resolved_after_seq: 0,
            resolved_terminal_seq: 0,
            terminal_prefix_sha256: "0".repeat(64),
            resolved_terminal_control_seq: 0,
            terminal_control_prefix_sha256: "0".repeat(64),
            effective_turns: 0,
        },
        selection: ModelSelection {
            model: header.settings().default_model().clone(),
            reasoning_effort: None,
        },
        sandbox: header.settings().sandbox(),
        require_approval: header.settings().require_approval(),
        script: ProgramBlob {
            sha256: "a".repeat(64),
            bytes: 10,
        },
        guard: None,
    };
    assert!(
        store
            .read_program_records(&id, &run)
            .await
            .unwrap()
            .is_none()
    );
    assert_ordered_admission(store, &id, &run, &descriptor).await;
    let active = store.list_active_program_runs(None, 1).await.unwrap();
    assert_eq!(active.runs.len(), 1);
    assert!(!active.has_more);
    assert!(
        store
            .read_agent_subtree_snapshot(&id)
            .await
            .unwrap()
            .session
            .has_active_program
    );
    let tail = store.read_watermarks(&id).await.unwrap();
    let idle = AtomicAgentCommit {
        sessions: vec![AtomicSessionAppend {
            session_id: id.clone(),
            expected_fact_seq: tail.durable_fact_seq,
            expected_control_seq: tail.durable_control_seq,
            header: None,
            facts: vec![],
            controls: vec![
                AgentControlRecord::new(
                    tail.durable_control_seq + 1,
                    1,
                    AgentControlRecordBody::ProgramRun {
                        run_id: run.clone(),
                        event: ProgramRunEvent::Progress {
                            phase: None,
                            message: "guarded mutation".into(),
                        },
                    },
                )
                .unwrap(),
            ],
        }],
        required_active_activations: vec![],
        quiescent_descendants_of: Some(id.clone().into()),
    };
    assert!(matches!(
        store.commit_agent(idle).await,
        Err(rsi_agent_store_protocol::StoreError::SessionNotQuiescent { .. })
    ));
    assert_eq!(store.read_watermarks(&id).await.unwrap(), tail);
    reject_nonwaking_or_retired_initial_children(store, &header, &descriptor).await;
    let mut other = descriptor.clone();
    other.run_id = ProgramRunId::new("overlap").unwrap();
    other.creator_turn_id = TurnId::new("turn-2").unwrap();
    assert!(
        append_program(
            store,
            &id,
            &other.run_id,
            ProgramRunEvent::Accepted {
                descriptor: Box::new(other.clone())
            }
        )
        .await
        .is_err()
    );
    for event in [
        ProgramRunEvent::ChildAdmitted {
            ordinal: 1,
            child_session_id: SessionId::new("missing-child").unwrap(),
            message_id: MessageId::new("missing-input").unwrap(),
        },
        ProgramRunEvent::ChildStarted {
            ordinal: 1,
            activation_id: ActivationId::new("missing-activation").unwrap(),
        },
        ProgramRunEvent::ChildSettled {
            receipt: rsi_agent_session_protocol::ProgramChildReceipt {
                ordinal: 1,
                child_session_id: SessionId::new("missing-child").unwrap(),
                activation_id: None,
                outcome: ProgramOutcome::Cancelled,
                result: None,
            },
        },
    ] {
        let before = store.read_watermarks(&id).await.unwrap();
        assert!(append_program(store, &id, &run, event).await.is_err());
        assert_eq!(
            store.read_watermarks(&id).await.unwrap(),
            before,
            "unpaired graph must roll back"
        );
    }
    let mut admitted = 0;
    loop {
        let event = ProgramRunEvent::Progress {
            phase: None,
            message: "\u{1}".repeat(16 * 1024),
        };
        if append_program(store, &id, &run, event).await.is_err() {
            break;
        }
        admitted += 1;
        assert!(admitted < 100);
    }
    assert!(admitted > 1);
    let page = store
        .read_program_records(&id, &run)
        .await
        .unwrap()
        .unwrap();
    page.validate(&id, &run).unwrap();
    assert_eq!(page.records.len(), admitted + 1);
    // Progress exhaustion leaves closure capacity and failed append does not consume a cursor.
    append_program(store, &id, &run, ProgramRunEvent::CancellationRequested)
        .await
        .unwrap();
    // Exercise the closure byte budget directly: synthetic receipts have no child graph.
    // Online Store admission separately requires actual paired child receipts.
    let mut closure = store
        .read_program_records(&id, &run)
        .await
        .unwrap()
        .unwrap()
        .head;
    for ordinal in 1..=128 {
        let record = AgentControlRecord::new(
            closure.last_control_seq + 1,
            1,
            AgentControlRecordBody::ProgramRun {
                run_id: run.clone(),
                event: ProgramRunEvent::ChildSettled {
                    receipt: rsi_agent_session_protocol::ProgramChildReceipt {
                        ordinal,
                        child_session_id: SessionId::new("c".repeat(256)).unwrap(),
                        activation_id: Some(
                            rsi_agent_session_protocol::ActivationId::new("a".repeat(256)).unwrap(),
                        ),
                        outcome: ProgramOutcome::Failed {
                            code: "x".repeat(256),
                            message: "\u{1}".repeat(4096),
                        },
                        result: None,
                    },
                },
            },
        )
        .unwrap();
        closure =
            rsi_agent_store_protocol::program_head_after(&id, Some(&closure), &record).unwrap();
    }
    assert!(
        closure.encoded_bytes - page.head.encoded_bytes
            <= rsi_agent_session_protocol::PROGRAM_TERMINAL_RESERVE_BYTES
    );
    let suffix = store
        .read_program_records_after(&id, &run, page.head.last_control_seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(suffix.records.len(), 1);
    suffix.validate_after(&id, &run, Some(&page.head)).unwrap();
    let unchanged = store
        .read_program_records_after(&id, &run, suffix.head.last_control_seq)
        .await
        .unwrap()
        .unwrap();
    assert!(unchanged.records.is_empty());
    unchanged
        .validate_after(&id, &run, Some(&suffix.head))
        .unwrap();

    append_program(
        store,
        &id,
        &run,
        ProgramRunEvent::Terminal {
            outcome: ProgramOutcome::Cancelled,
            result: None,
        },
    )
    .await
    .unwrap();
    assert!(
        store
            .list_active_program_runs(None, 1)
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(
        !store
            .read_agent_subtree_snapshot(&id)
            .await
            .unwrap()
            .session
            .has_active_program
    );
    assert!(
        append_program(store, &id, &run, ProgramRunEvent::Started)
            .await
            .is_err()
    );
    let mut repeat = descriptor;
    repeat.run_id = ProgramRunId::new("repeat-turn").unwrap();
    let before_repeat = store.read_watermarks(&id).await.unwrap();
    let repeated = append_program(
        store,
        &id,
        &repeat.run_id,
        ProgramRunEvent::Accepted {
            descriptor: Box::new(repeat.clone()),
        },
    )
    .await;
    assert!(
        matches!(
            repeated,
            Err(rsi_agent_store_protocol::StoreError::Invalid(_))
        ),
        "{repeated:?}"
    );
    assert_eq!(store.read_watermarks(&id).await.unwrap(), before_repeat);
    program_notice_promotion(store, &id, &run).await;
    (id, run)
}

async fn program_notice_promotion(store: &dyn SessionStore, id: &SessionId, run: &ProgramRunId) {
    let tail = store.read_watermarks(id).await.unwrap();
    let terminal_control_seq = store
        .read_program_records(id, run)
        .await
        .unwrap()
        .unwrap()
        .head
        .last_control_seq;
    let message_id = rsi_agent_session_protocol::MessageId::new("workflow-notice").unwrap();
    let commit = AtomicAgentCommit {
        sessions: vec![AtomicSessionAppend {
            session_id: id.clone(),
            expected_fact_seq: tail.durable_fact_seq,
            expected_control_seq: tail.durable_control_seq,
            header: None,
            facts: vec![],
            controls: vec![
                AgentControlRecord::new(
                    tail.durable_control_seq + 1,
                    1,
                    AgentControlRecordBody::MessageAccepted {
                        message: rsi_agent_session_protocol::AgentMessage {
                            message_id: message_id.clone(),
                            source: rsi_agent_session_protocol::AgentMessageSource::Program {
                                source: rsi_agent_session_protocol::ProgramCompletionSource {
                                    run_id: run.clone(),
                                    generation: "a".repeat(64),
                                    terminal_control_seq,
                                },
                            },
                            content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                                text: "completed".into(),
                            }],
                            options: rsi_agent_session_protocol::MessageOptions::default(),
                        },
                        delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                        bound_turn_id: None,
                        root_session_id: id.clone(),
                        target: rsi_agent_session_protocol::MessageTarget::NextStep,
                        wake_required: false,
                    },
                )
                .unwrap(),
            ],
        }],
        required_active_activations: vec![],
        quiescent_descendants_of: None,
    };
    let mut invalid = serde_json::to_value(&commit.sessions[0].controls[0]).unwrap();
    invalid["message"]["source"]["source"]["terminal_control_seq"] =
        serde_json::json!(terminal_control_seq - 1);
    let mut bad = commit.clone();
    bad.sessions[0].controls[0] = serde_json::from_value(invalid).unwrap();
    assert!(store.commit_agent(bad).await.is_err());
    assert_eq!(store.read_watermarks(id).await.unwrap(), tail);
    store.commit_agent(commit).await.unwrap();
    assert_eq!(
        store
            .read_agent_mailbox_summary(id)
            .await
            .unwrap()
            .pending_promotable_message_ids,
        vec![message_id.clone()]
    );
    let snapshot = store.inspect_session(id).await.unwrap();
    assert!(
        snapshot
            .pending
            .iter()
            .any(|pending| pending.message_id == message_id && pending.permits_promotion)
    );
}

async fn assert_ordered_admission(
    store: &dyn SessionStore,
    id: &SessionId,
    run: &ProgramRunId,
    descriptor: &ProgramRunDescriptor,
) {
    use rsi_agent_session_protocol::{MAXIMUM_PENDING_AGENT_MESSAGES, MessageDelivery};
    let controls = (1..=MAXIMUM_PENDING_AGENT_MESSAGES as u64)
        .map(|seq| {
            AgentControlRecord::new(
                seq,
                1,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: MessageId::new(format!("pending-{seq}")).unwrap(),
                        source: AgentMessageSource::Human,
                        content: vec![AgentMessageContent::Text {
                            text: "pending".into(),
                        }],
                        options: MessageOptions::default(),
                    },
                    delivery: MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    root_session_id: id.clone(),
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
            .unwrap()
        })
        .collect();
    let mut append = AtomicSessionAppend {
        session_id: id.clone(),
        expected_fact_seq: 1,
        expected_control_seq: 0,
        header: None,
        facts: vec![],
        controls,
    };
    let commit = |append| AtomicAgentCommit {
        sessions: vec![append],
        required_active_activations: vec![],
        quiescent_descendants_of: None,
    };
    store.commit_agent(commit(append.clone())).await.unwrap();
    let tail = store.read_watermarks(id).await.unwrap();
    append.expected_control_seq = tail.durable_control_seq;
    let accept = AgentControlRecordBody::ProgramRun {
        run_id: run.clone(),
        event: ProgramRunEvent::Accepted {
            descriptor: Box::new(descriptor.clone()),
        },
    };
    let discard = AgentControlRecordBody::MessageDiscarded {
        message_id: MessageId::new("pending-1").unwrap(),
        reason: rsi_agent_session_protocol::MessageDiscardReason::Cancelled,
    };
    // A later release cannot retroactively authorize acceptance at a full mailbox.
    append.controls = vec![
        AgentControlRecord::new(tail.durable_control_seq + 1, 1, accept.clone()).unwrap(),
        AgentControlRecord::new(tail.durable_control_seq + 2, 1, discard.clone()).unwrap(),
    ];
    assert!(store.commit_agent(commit(append.clone())).await.is_err());
    assert_eq!(store.read_watermarks(id).await.unwrap(), tail);
    assert!(store.read_program_records(id, run).await.unwrap().is_none());
    // Earlier releases are visible at the exact acceptance position.
    append.controls = vec![
        AgentControlRecord::new(tail.durable_control_seq + 1, 1, discard).unwrap(),
        AgentControlRecord::new(tail.durable_control_seq + 2, 1, accept).unwrap(),
    ];
    store
        .commit_agent(commit(append))
        .await
        .expect("discard frees Program notice slot in the same append");
}

#[allow(clippy::too_many_lines)] // Canonical child setup and both paired-commit rollback assertions share one fixture.
async fn reject_nonwaking_or_retired_initial_children(
    store: &dyn SessionStore,
    parent: &SessionHeader,
    descriptor: &ProgramRunDescriptor,
) {
    use rsi_agent_session_protocol::{
        AgentPath, ExecutionOwner, ForkOrigin, MessageDelivery, MessageDiscardReason,
    };
    let id = parent.session_id();
    let tail = store.read_watermarks(id).await.unwrap();
    let child = parent
        .forked_child(
            SessionId::new("invalid-program-child").unwrap(),
            2,
            ForkOrigin {
                parent_session_id: id.clone(),
                root_session_id: id.clone(),
                path: AgentPath::new(vec![1]).unwrap(),
                task_name: "invalid-child".into(),
                parent_header_fingerprint: parent.fingerprint().unwrap(),
                invoking_turn_id: descriptor.creator_turn_id.clone(),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: "0".repeat(64),
                resolved_terminal_control_seq: 0,
                terminal_control_prefix_sha256: "0".repeat(64),
                requested_turns: ForkTurnSelection::None,
                effective_turns: 0,
            },
            descriptor.selection.clone(),
        )
        .unwrap()
        .with_execution_owner(ExecutionOwner::ProgramRun {
            session_id: id.clone(),
            run_id: descriptor.run_id.clone(),
            ordinal: 1,
        })
        .unwrap();
    let message_id = MessageId::new("initial").unwrap();
    for retired in [false, true] {
        let mut controls = vec![
            AgentControlRecord::new(
                1,
                1,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: message_id.clone(),
                        source: AgentMessageSource::Agent {
                            source_session_id: id.clone(),
                        },
                        content: vec![AgentMessageContent::Text {
                            text: "work".into(),
                        }],
                        options: MessageOptions::default(),
                    },
                    delivery: if retired {
                        MessageDelivery::NextTurn
                    } else {
                        MessageDelivery::NextStep
                    },
                    bound_turn_id: None,
                    root_session_id: id.clone(),
                    target: if retired {
                        MessageTarget::NextTurn
                    } else {
                        MessageTarget::NextStep
                    },
                    wake_required: retired,
                },
            )
            .unwrap(),
        ];
        if retired {
            controls.push(
                AgentControlRecord::new(
                    2,
                    1,
                    AgentControlRecordBody::MessageDiscarded {
                        message_id: message_id.clone(),
                        reason: MessageDiscardReason::Cancelled,
                    },
                )
                .unwrap(),
            );
        }
        let commit = AtomicAgentCommit {
            sessions: vec![
                AtomicSessionAppend {
                    session_id: id.clone(),
                    expected_fact_seq: tail.durable_fact_seq,
                    expected_control_seq: tail.durable_control_seq,
                    header: None,
                    facts: vec![],
                    controls: vec![
                        AgentControlRecord::new(
                            tail.durable_control_seq + 1,
                            1,
                            AgentControlRecordBody::ProgramRun {
                                run_id: descriptor.run_id.clone(),
                                event: ProgramRunEvent::ChildAdmitted {
                                    ordinal: 1,
                                    child_session_id: child.session_id().clone(),
                                    message_id: message_id.clone(),
                                },
                            },
                        )
                        .unwrap(),
                    ],
                },
                AtomicSessionAppend {
                    session_id: child.session_id().clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(child.clone()),
                    facts: vec![],
                    controls,
                },
            ],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        };
        assert!(store.commit_agent(commit).await.is_err());
        assert_eq!(store.read_watermarks(id).await.unwrap(), tail);
        assert!(store.header(child.session_id()).await.is_err());
    }
}
