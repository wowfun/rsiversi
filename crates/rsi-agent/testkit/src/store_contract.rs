use super::{
    ActivationId, ActivationOutcome, AgentActivationGuard, AgentControlRecord,
    AgentControlRecordBody, AgentMessage, AgentMessageContent, AgentMessageSource, AgentPath,
    AppendBatch, Arc, AtomicAgentCommit, AtomicSessionAppend, ForkOrigin, ForkTurnSelection,
    InputMessageSource, MAXIMUM_STORE_MAILBOX_PAGE_BYTES, MessageId, MessageOptions, MessageTarget,
    SessionFact, SessionFactBody, SessionHeader, SessionId, SessionStore, StepId,
    StoreAgentMailboxSummary, StoreError, StoredContextCheckpoint, TurnId, TurnOutcome,
    WriteContextCheckpoint, fact_prefix_sha256,
};

/// Exercises the backend-independent observable Store contract against one
/// empty implementation.
///
/// # Panics
///
/// Panics when the supplied fixture is internally inconsistent or the backend
/// violates any observable part of the mechanical Store contract.
#[allow(clippy::too_many_lines)]
pub async fn assert_mechanical_store_contract(
    store: &dyn SessionStore,
    header: SessionHeader,
    accepted: SessionFact,
    event: SessionFact,
    terminal: SessionFact,
) {
    let session_id = header.session_id().clone();
    let turn_id = accepted.body().turn_id().clone();
    assert_eq!(accepted.seq(), 1);
    assert_eq!(event.seq(), 2);
    assert_eq!(terminal.seq(), 3);
    assert_eq!(event.body().turn_id(), &turn_id);
    assert_eq!(terminal.body().turn_id(), &turn_id);
    assert_missing_session_atomic_append_is_not_found(store).await;
    assert!(matches!(
        store.validate_session(&session_id).await,
        Err(StoreError::NotFound(_))
    ));

    let commit = store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: (vec![accepted.clone()])
                .into_iter()
                .map(Into::into)
                .collect(),
        })
        .await
        .expect("create session");
    assert_eq!(commit.durable_seq, 1);
    assert_eq!(store.header(&session_id).await.unwrap(), header);
    store
        .validate_session(&session_id)
        .await
        .expect("validate an open session");
    let inspected = store.inspect_session(&session_id).await.unwrap();
    inspected.validate().unwrap();
    assert_eq!(inspected.header, header);
    assert_eq!(inspected.durable_fact_seq, 1);
    assert_eq!(inspected.durable_control_seq, 0);
    assert_eq!(inspected.active_turn_id.as_ref(), Some(&turn_id));
    assert!(inspected.pending.is_empty());
    assert!(matches!(
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: None,
                facts: (vec![accepted.clone()])
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            })
            .await,
        Err(StoreError::Conflict {
            expected: 0,
            actual: 1
        })
    ));

    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 1,
            header: None,
            facts: (vec![event.clone()]).into_iter().map(Into::into).collect(),
        })
        .await
        .expect("append open-turn event");
    let first = store.read_facts(&session_id, 0, 1).await.unwrap();
    assert_eq!(first.facts, vec![accepted.clone()]);
    assert!(!first.caught_up());
    let second = store.read_facts(&session_id, 1, 1).await.unwrap();
    assert_eq!(second.facts, vec![event.clone()]);
    assert!(second.caught_up());
    let newest = store.read_facts_before(&session_id, 0, 1).await.unwrap();
    assert_eq!(newest.before_seq, 3);
    assert_eq!(newest.facts, vec![event.clone()]);
    assert!(newest.has_more);
    let oldest = store
        .read_facts_before(&session_id, event.seq(), 1)
        .await
        .unwrap();
    assert_eq!(oldest.facts, vec![accepted.clone()]);
    assert!(!oldest.has_more);
    let turn = store
        .read_turn_facts(&session_id, &turn_id, 0, 8)
        .await
        .unwrap();
    assert_eq!(turn.facts, vec![accepted.clone(), event.clone()]);
    assert!(!turn.has_more);
    let open_boundary = store
        .read_turn_boundary(&session_id, &turn_id)
        .await
        .unwrap();
    assert_eq!(open_boundary.turn_id(), &turn_id);
    assert_eq!(open_boundary.accepted(), &accepted);
    assert_eq!(open_boundary.terminal(), None);
    assert_eq!(open_boundary.durable_seq(), 2);
    assert!(matches!(
        store
            .read_turn_boundary(&session_id, &TurnId::new("turn-absent").unwrap())
            .await,
        Err(StoreError::TurnNotFound { .. })
    ));
    let open = store.list_open_turns(&session_id, 0, 8).await.unwrap();
    assert_eq!(open.turns.len(), 1);
    assert_eq!(open.turns[0].turn_id, turn_id);
    let open_sessions = store.list_open_sessions(None, 8).await.unwrap();
    assert_eq!(open_sessions.sessions, vec![session_id.clone()]);
    assert!(!open_sessions.has_more);

    let first_marker = AgentControlRecord::new(
        1,
        terminal.timestamp_ms(),
        AgentControlRecordBody::TurnBoundaryRecorded {
            turn_id: turn_id.clone(),
            terminal_fact_seq: terminal.seq(),
        },
    )
    .unwrap();
    let terminal_commit = AtomicAgentCommit {
        sessions: vec![AtomicSessionAppend {
            session_id: session_id.clone(),
            expected_fact_seq: 2,
            expected_control_seq: 0,
            header: None,
            facts: vec![Arc::new(terminal.clone())],
            controls: vec![first_marker.clone()],
        }],
        required_active_activations: Vec::new(),
        quiescent_descendants_of: None,
    };
    let mut missing_marker = terminal_commit.clone();
    missing_marker.sessions[0].controls.clear();
    assert!(matches!(
        store.commit_agent(missing_marker).await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 2,
                header: None,
                facts: vec![Arc::new(terminal.clone())],
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .read_facts(&session_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        2
    );
    assert_eq!(
        store
            .read_controls(&session_id, 0, 8)
            .await
            .unwrap()
            .durable_seq,
        0
    );
    store
        .commit_agent(terminal_commit)
        .await
        .expect("close Turn with its exact control horizon");
    assert!(
        store
            .list_open_turns(&session_id, 0, 8)
            .await
            .unwrap()
            .turns
            .is_empty()
    );
    let closed_boundary = store
        .read_turn_boundary(&session_id, &turn_id)
        .await
        .unwrap();
    assert_eq!(closed_boundary.turn_id(), &turn_id);
    assert_eq!(closed_boundary.accepted(), &accepted);
    assert_eq!(closed_boundary.terminal(), Some(&terminal));
    assert_eq!(closed_boundary.durable_seq(), 3);
    assert!(matches!(
        store
            .write_context_checkpoint(WriteContextCheckpoint {
                session_id: session_id.clone(),
                expected_durable_seq: 3,
                checkpoint: StoredContextCheckpoint {
                    header_fingerprint: header.fingerprint().unwrap(),
                    through_seq: 3,
                    fact_prefix_sha256: "b".repeat(64),
                    bytes: Arc::from(b"self-consistent-forged-checkpoint".as_slice()),
                },
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    let checkpoint = StoredContextCheckpoint {
        header_fingerprint: header.fingerprint().unwrap(),
        through_seq: 3,
        fact_prefix_sha256: fact_prefix_sha256([&accepted, &event, &terminal]).unwrap(),
        bytes: Arc::from(b"context-checkpoint-v2".as_slice()),
    };
    store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: session_id.clone(),
            expected_durable_seq: 3,
            checkpoint: checkpoint.clone(),
        })
        .await
        .expect("write terminal-tail checkpoint");
    assert_eq!(
        store.read_context_checkpoint(&session_id).await.unwrap(),
        Some(checkpoint)
    );
    let replacement = StoredContextCheckpoint {
        header_fingerprint: header.fingerprint().unwrap(),
        through_seq: 3,
        fact_prefix_sha256: fact_prefix_sha256([&accepted, &event, &terminal]).unwrap(),
        bytes: Arc::from(b"context-checkpoint-v2-replacement".as_slice()),
    };
    store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: session_id.clone(),
            expected_durable_seq: 3,
            checkpoint: replacement.clone(),
        })
        .await
        .expect("replace terminal-tail checkpoint");
    assert_eq!(
        store.read_context_checkpoint(&session_id).await.unwrap(),
        Some(replacement)
    );
    assert!(matches!(
        store
            .write_context_checkpoint(WriteContextCheckpoint {
                session_id: session_id.clone(),
                expected_durable_seq: 2,
                checkpoint: StoredContextCheckpoint {
                    header_fingerprint: header.fingerprint().unwrap(),
                    through_seq: 2,
                    fact_prefix_sha256: "c".repeat(64),
                    bytes: Arc::from(b"stale-checkpoint-v2".as_slice()),
                },
            })
            .await,
        Err(StoreError::Conflict {
            expected: 2,
            actual: 3
        })
    ));
    let sessions = store.list_sessions(None, 8).await.unwrap();
    assert_eq!(sessions.sessions, vec![session_id.clone()]);
    assert!(!sessions.has_more);
    let recent = store.list_recent_sessions(None, 8).await.unwrap();
    assert_eq!(recent.sessions.len(), 1);
    assert_eq!(recent.sessions[0].header, header);
    assert!(
        store
            .list_recent_sessions(Some(&recent.sessions[0].cursor()), 8)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    let closed_sessions = store.list_open_sessions(None, 8).await.unwrap();
    assert!(closed_sessions.sessions.is_empty());
    assert!(!closed_sessions.has_more);
    store
        .validate_session(&session_id)
        .await
        .expect("validate a closed session");

    let message_id = MessageId::new("message-control-contract").unwrap();
    let accepted_control = AgentControlRecord::new(
        2,
        10,
        AgentControlRecordBody::MessageAccepted {
            message: AgentMessage {
                message_id: message_id.clone(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "durable queued input".into(),
                }],
                options: MessageOptions::default(),
            },
            root_session_id: session_id.clone(),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            bound_turn_id: None,
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    )
    .unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 3,
                    expected_control_seq: 1,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![accepted_control.clone()],
                }],
                required_active_activations: vec![AgentActivationGuard {
                    session_id: session_id.clone(),
                    activation_id: ActivationId::new("missing-activation").unwrap(),
                }],
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::ActivationGuardConflict { session })
            if session == session_id.as_str()
    ));
    let committed = store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 3,
                expected_control_seq: 1,
                header: None,
                facts: Vec::new(),
                controls: vec![accepted_control.clone()],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("append a ready Agent message");
    assert_eq!(committed.sessions[0].durable_fact_seq, 3);
    assert_eq!(committed.sessions[0].durable_control_seq, 2);
    assert_eq!(
        store
            .read_controls(&session_id, 0, 8)
            .await
            .unwrap()
            .records,
        vec![first_marker, accepted_control]
    );
    assert_eq!(
        store
            .list_ready_messages(&session_id, None, 8)
            .await
            .unwrap()
            .messages[0]
            .message_id,
        message_id
    );
    for (root, parent, path) in [
        (
            SessionId::new("wrong-root").unwrap(),
            None,
            AgentPath::root(),
        ),
        (
            session_id.clone(),
            Some(SessionId::new("wrong-parent").unwrap()),
            AgentPath::new(vec![1]).unwrap(),
        ),
    ] {
        assert!(matches!(store.commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(), expected_fact_seq: 3, expected_control_seq: 2,
                header: None, facts: Vec::new(),
                controls: vec![AgentControlRecord::new(3, 11, AgentControlRecordBody::ActivationStarted {
                    activation_id: ActivationId::new("wrong-lineage").unwrap(),
                    root_session_id: root, parent_session_id: parent, path,
                }).unwrap()],
            }], required_active_activations: Vec::new(), quiescent_descendants_of: None,
        }).await, Err(StoreError::Invalid(message)) if message.contains("lineage")));
    }
    let activation_id = ActivationId::new("activation-contract").unwrap();
    let message_turn_id = TurnId::new("turn-message-contract").unwrap();
    let message_step_id = StepId::new("step-contract").unwrap();
    let step_message_id = MessageId::new("message-step-contract").unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 3,
                    expected_control_seq: 2,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![
                        AgentControlRecord::new(
                            3,
                            11,
                            AgentControlRecordBody::ActivationStarted {
                                activation_id: activation_id.clone(),
                                root_session_id: session_id.clone(),
                                parent_session_id: None,
                                path: rsi_agent_session_protocol::AgentPath::root(),
                            },
                        )
                        .unwrap(),
                        AgentControlRecord::new(
                            4,
                            11,
                            AgentControlRecordBody::MessageClaimed {
                                message_id: message_id.clone(),
                                activation_id: activation_id.clone(),
                                turn_id: message_turn_id.clone(),
                                step_id: message_step_id.clone(),
                                entered_fact_seq: 3,
                            },
                        )
                        .unwrap(),
                    ],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("newly appended input Fact")
    ));
    let message_facts = vec![
        SessionFact::new(
            4,
            11,
            SessionFactBody::MessageTurnAccepted {
                turn_id: message_turn_id.clone(),
                activation_id: activation_id.clone(),
                message_ids: vec![message_id.clone()],
                model: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
        SessionFact::new(
            5,
            11,
            SessionFactBody::StepStarted {
                turn_id: message_turn_id.clone(),
                step_id: message_step_id.clone(),
            },
        )
        .unwrap(),
        SessionFact::new(
            6,
            11,
            SessionFactBody::InputMessageEntered {
                turn_id: message_turn_id.clone(),
                step_id: message_step_id.clone(),
                source: InputMessageSource::Human {
                    message_id: message_id.clone(),
                },
                content: vec![AgentMessageContent::Text {
                    text: "durable queued input".into(),
                }],
            },
        )
        .unwrap(),
        SessionFact::new(
            7,
            13,
            SessionFactBody::TurnTerminal {
                turn_id: message_turn_id.clone(),
                outcome: TurnOutcome::Completed,
            },
        )
        .unwrap(),
    ];
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 3,
                expected_control_seq: 2,
                header: None,
                facts: (message_facts.clone())
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                controls: vec![
                    AgentControlRecord::new(
                        3,
                        11,
                        AgentControlRecordBody::ActivationStarted {
                            activation_id: activation_id.clone(),
                            root_session_id: session_id.clone(),
                            parent_session_id: None,
                            path: rsi_agent_session_protocol::AgentPath::root(),
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        4,
                        11,
                        AgentControlRecordBody::MessageClaimed {
                            message_id: message_id.clone(),
                            activation_id: activation_id.clone(),
                            turn_id: message_turn_id.clone(),
                            step_id: message_step_id.clone(),
                            entered_fact_seq: 6,
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        5,
                        12,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: step_message_id.clone(),
                                source: AgentMessageSource::Human,
                                content: vec![AgentMessageContent::Text {
                                    text: "enter the next Step".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: session_id.clone(),
                            delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                            bound_turn_id: None,
                            target: MessageTarget::NextStep,
                            wake_required: false,
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        6,
                        12,
                        AgentControlRecordBody::MessageDiscarded {
                            message_id: step_message_id,
                            reason: rsi_agent_session_protocol::MessageDiscardReason::Cancelled,
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        7,
                        13,
                        AgentControlRecordBody::WaitParked {
                            activation_id: activation_id.clone(),
                            turn_id: message_turn_id.clone(),
                            step_id: message_step_id.clone(),
                            kind: rsi_agent_session_protocol::WaitKind::Agent,
                            deadline_ms: Some(100),
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        8,
                        13,
                        AgentControlRecordBody::TurnBoundaryRecorded {
                            turn_id: message_turn_id.clone(),
                            terminal_fact_seq: 7,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("claim the ready Agent message");
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 7,
                    expected_control_seq: 8,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![AgentControlRecord::new(
                        9,
                        13,
                        AgentControlRecordBody::ActivationWaitingForDescendants {
                            activation_id: activation_id.clone(),
                        },
                    )
                    .unwrap()],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Corrupt(message)) if message.contains("running activation")
    ));
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 7,
                expected_control_seq: 8,
                header: None,
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        9,
                        13,
                        AgentControlRecordBody::WaitResumed {
                            activation_id: activation_id.clone(),
                            turn_id: message_turn_id,
                            step_id: message_step_id,
                            cause: rsi_agent_session_protocol::WaitResumeCause::Cancel,
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        10,
                        13,
                        AgentControlRecordBody::ActivationSettled {
                            activation_id,
                            outcome: ActivationOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("resume the parked wait before settling its Activation");
    assert!(
        store
            .list_ready_messages(&session_id, None, 8)
            .await
            .unwrap()
            .messages
            .is_empty()
    );

    let invoking_turn = TurnId::new("turn-fork-invoking").unwrap();
    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 7,
            header: None,
            facts: (vec![
                SessionFact::new(
                    8,
                    13,
                    SessionFactBody::TurnAccepted {
                        turn_id: invoking_turn.clone(),
                        text: "invoke a child".into(),
                        model: None,
                        sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
            ])
            .into_iter()
            .map(Into::into)
            .collect(),
        })
        .await
        .unwrap();
    let none = store
        .resolve_fork_boundary(&session_id, &invoking_turn, ForkTurnSelection::None)
        .await
        .unwrap();
    assert_eq!(
        (none.resolved_after_seq, none.resolved_terminal_seq),
        (0, 0)
    );
    assert_eq!(none.effective_turns, 0);
    assert_eq!(none.resolved_terminal_control_seq, 0);
    assert_eq!(none.terminal_control_prefix_sha256, "0".repeat(64));
    let all = store
        .resolve_fork_boundary(&session_id, &invoking_turn, ForkTurnSelection::All)
        .await
        .unwrap();
    let last = store
        .resolve_fork_boundary(&session_id, &invoking_turn, ForkTurnSelection::Last(8))
        .await
        .unwrap();
    assert_eq!(all, last);
    assert_eq!((all.resolved_after_seq, all.resolved_terminal_seq), (0, 7));
    assert_eq!(all.effective_turns, 2);
    assert_eq!(all.resolved_terminal_control_seq, 8);
    let controls_at_terminal = store.read_controls(&session_id, 0, 8).await.unwrap();
    assert_eq!(
        controls_at_terminal.durable_seq, 10,
        "later idle controls exist"
    );
    assert_eq!(
        all.terminal_control_prefix_sha256,
        rsi_agent_session_protocol::control_prefix_sha256(controls_at_terminal.records.iter())
            .unwrap()
    );
    assert_eq!(
        all.terminal_prefix_sha256,
        fact_prefix_sha256(
            [accepted.clone(), event.clone(), terminal.clone()]
                .iter()
                .chain(message_facts.iter())
        )
        .unwrap()
    );

    let child_origin = |task_name: &str| ForkOrigin {
        parent_session_id: session_id.clone(),
        root_session_id: session_id.clone(),
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: task_name.into(),
        parent_header_fingerprint: header.fingerprint().unwrap(),
        invoking_turn_id: invoking_turn.clone(),
        resolved_after_seq: all.resolved_after_seq,
        resolved_terminal_seq: all.resolved_terminal_seq,
        terminal_prefix_sha256: all.terminal_prefix_sha256.clone(),
        resolved_terminal_control_seq: all.resolved_terminal_control_seq,
        terminal_control_prefix_sha256: all.terminal_control_prefix_sha256.clone(),
        requested_turns: ForkTurnSelection::All,
        effective_turns: all.effective_turns,
    };
    let first_child_id = SessionId::new("shared-contract-child-one").unwrap();
    let first_child_header = header
        .forked_child(first_child_id.clone(), 30, child_origin("first-child"))
        .unwrap();
    let first_child_fingerprint = first_child_header.fingerprint().unwrap();
    let child_control = |session_id: &SessionId, message_id: &str| {
        AgentControlRecord::new(
            1,
            30,
            AgentControlRecordBody::MessageAccepted {
                message: AgentMessage {
                    message_id: MessageId::new(message_id).unwrap(),
                    source: AgentMessageSource::Agent {
                        source_session_id: session_id.clone(),
                    },
                    content: vec![AgentMessageContent::Text {
                        text: "start child".into(),
                    }],
                    options: MessageOptions::default(),
                },
                root_session_id: session_id.clone(),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                bound_turn_id: None,
                target: MessageTarget::NextTurn,
                wake_required: true,
            },
        )
        .unwrap()
    };
    let busy_child_commit = AtomicAgentCommit {
        sessions: vec![AtomicSessionAppend {
            session_id: first_child_id.clone(),
            expected_fact_seq: 0,
            expected_control_seq: 0,
            header: Some(first_child_header.clone()),
            facts: Vec::new(),
            controls: vec![child_control(&session_id, "first-child-message")],
        }],
        required_active_activations: Vec::new(),
        quiescent_descendants_of: Some(session_id.clone()),
    };
    assert!(
        matches!(
            store.commit_agent(busy_child_commit).await,
            Err(StoreError::SessionNotQuiescent { session }) if session == first_child_id.as_str()
        ),
        "a guarded transaction cannot introduce its own busy descendant"
    );
    assert!(matches!(
        store.header(&first_child_id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        store
            .read_agent_subtree_snapshot(&session_id)
            .await
            .unwrap()
            .descendants
            .is_empty()
    );
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: first_child_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(first_child_header.clone()),
                facts: Vec::new(),
                controls: vec![child_control(&session_id, "first-child-message")],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: Some(first_child_id.clone()),
        })
        .await
        .expect("create a new guard root with no strict descendants");
    let before_new_child = store
        .read_agent_subtree_snapshot(&first_child_id)
        .await
        .unwrap();
    assert!(before_new_child.descendants.is_empty());
    let grandchild_id = SessionId::new("shared-contract-grandchild").unwrap();
    let grandchild_header = first_child_header
        .forked_child(
            grandchild_id.clone(),
            31,
            ForkOrigin {
                parent_session_id: first_child_id.clone(),
                root_session_id: session_id.clone(),
                path: AgentPath::new(vec![1, 1]).unwrap(),
                task_name: "grandchild".into(),
                parent_header_fingerprint: first_child_fingerprint,
                invoking_turn_id: invoking_turn.clone(),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: "0".repeat(64),
                resolved_terminal_control_seq: 0,
                terminal_control_prefix_sha256: "0".repeat(64),
                requested_turns: ForkTurnSelection::None,
                effective_turns: 0,
            },
        )
        .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: grandchild_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(grandchild_header),
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        1,
                        31,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: MessageId::new("grandchild-message").unwrap(),
                                source: AgentMessageSource::Agent {
                                    source_session_id: first_child_id.clone(),
                                },
                                content: vec![AgentMessageContent::Text {
                                    text: "start grandchild".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: session_id.clone(),
                            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                            bound_turn_id: None,
                            target: MessageTarget::NextTurn,
                            wake_required: true,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("create a grandchild path");
    let snapshot = store
        .read_agent_subtree_snapshot(&session_id)
        .await
        .unwrap();
    assert_eq!(snapshot.session.session_id, session_id);
    assert_eq!(
        snapshot
            .descendants
            .iter()
            .map(|child| (&child.status.session_id, child.status.durable_control_seq))
            .collect::<Vec<_>>(),
        vec![(&first_child_id, 1), (&grandchild_id, 1)]
    );
    assert!(
        snapshot
            .descendants
            .iter()
            .all(|child| child.status.has_waking_message
                && !child.status.has_open_turn
                && !child.status.has_active_activation)
    );
    assert_eq!(snapshot.descendants[0].parent_session_id, session_id);
    assert_eq!(snapshot.descendants[1].parent_session_id, first_child_id);
    let child_snapshot = store
        .read_agent_subtree_snapshot(&first_child_id)
        .await
        .unwrap();
    assert_eq!(child_snapshot.descendants.len(), 1);
    assert_eq!(
        child_snapshot.descendants[0].status.session_id,
        grandchild_id
    );
    assert!(matches!(store.commit_agent(AtomicAgentCommit {
        sessions: vec![AtomicSessionAppend { session_id: first_child_id.clone(), expected_fact_seq: 0, expected_control_seq: 1, header: None, facts: Vec::new(), controls: vec![AgentControlRecord::new(2, 32, child_control(&session_id, "guarded-message").body().clone()).unwrap()] }],
        required_active_activations: Vec::new(), quiescent_descendants_of: Some(first_child_id.clone()),
    }).await, Err(StoreError::SessionNotQuiescent { session }) if session == grandchild_id.as_str()));
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: grandchild_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 1,
                header: None,
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        2,
                        33,
                        child_control(&session_id, "quiescent-root-excluded")
                            .body()
                            .clone(),
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: Some(grandchild_id.clone()),
        })
        .await
        .expect("a busy root is excluded from its strict-descendant guard");
    let second_child_id = SessionId::new("shared-contract-child-two").unwrap();
    let second_child_header = header
        .forked_child(second_child_id.clone(), 31, child_origin("second-child"))
        .unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: second_child_id.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(second_child_header),
                    facts: Vec::new(),
                    controls: vec![child_control(&session_id, "second-child-message")],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("tree path")
    ));
    assert!(matches!(
        store.header(&second_child_id).await,
        Err(StoreError::NotFound(_))
    ));
    let duplicate_task_id = SessionId::new("shared-contract-child-duplicate-task").unwrap();
    let mut duplicate_task_origin = child_origin("first-child");
    duplicate_task_origin.path = AgentPath::new(vec![2]).unwrap();
    let duplicate_task_header = header
        .forked_child(duplicate_task_id.clone(), 31, duplicate_task_origin)
        .unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: duplicate_task_id.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(duplicate_task_header),
                    facts: Vec::new(),
                    controls: vec![child_control(&session_id, "duplicate-task-message")],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("task name")
    ));
    assert!(matches!(
        store.header(&duplicate_task_id).await,
        Err(StoreError::NotFound(_))
    ));

    let unrelated_root_id = SessionId::new("shared-contract-unrelated-root").unwrap();
    let unrelated_root_header = SessionHeader::new(
        unrelated_root_id.clone(),
        32,
        header.canonical_cwd(),
        header.agent_preset_id().clone(),
        header.settings().clone(),
    )
    .unwrap()
    .with_workspace_trust(header.workspace_trust())
    .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: unrelated_root_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(unrelated_root_header),
                facts: Vec::new(),
                controls: vec![child_control(&unrelated_root_id, "unrelated-root-message")],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("create an unrelated root for lineage rejection");

    let malformed_child_id = SessionId::new("shared-contract-malformed-child").unwrap();
    let mut malformed_origin = child_origin("malformed-child");
    malformed_origin.root_session_id = unrelated_root_id.clone();
    malformed_origin.path = AgentPath::new(vec![3]).unwrap();
    let malformed_child_header = header
        .forked_child(malformed_child_id.clone(), 33, malformed_origin)
        .unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: malformed_child_id.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 0,
                    header: Some(malformed_child_header),
                    facts: Vec::new(),
                    controls: vec![child_control(
                        &unrelated_root_id,
                        "malformed-child-message",
                    )],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("root")
    ));
    assert!(matches!(
        store.header(&malformed_child_id).await,
        Err(StoreError::NotFound(_))
    ));

    let wrong_root_control = AgentControlRecord::new(
        11,
        20,
        AgentControlRecordBody::MessageAccepted {
            message: AgentMessage {
                message_id: MessageId::new("wrong-root-message").unwrap(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "must not enter a foreign ready lane".into(),
                }],
                options: MessageOptions::default(),
            },
            root_session_id: unrelated_root_id,
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            bound_turn_id: None,
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    )
    .unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 8,
                    expected_control_seq: 10,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![wrong_root_control],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("root")
    ));

    let duplicate_control = AgentControlRecord::new(
        11,
        20,
        AgentControlRecordBody::MessageAccepted {
            message: AgentMessage {
                message_id: message_id.clone(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "duplicate identity".into(),
                }],
                options: MessageOptions::default(),
            },
            root_session_id: session_id.clone(),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            bound_turn_id: None,
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    )
    .unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 8,
                    expected_control_seq: 10,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![duplicate_control],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Corrupt(message)) if message.contains("message identity")
    ));
    let duplicate_activation = |seq, activation: &str| {
        AgentControlRecord::new(
            seq,
            20,
            AgentControlRecordBody::ActivationStarted {
                activation_id: ActivationId::new(activation).unwrap(),
                root_session_id: session_id.clone(),
                parent_session_id: None,
                path: AgentPath::root(),
            },
        )
        .unwrap()
    };
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 8,
                    expected_control_seq: 10,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![
                        duplicate_activation(11, "duplicate-activation-one"),
                        duplicate_activation(12, "duplicate-activation-two"),
                    ],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Corrupt(message)) if message.contains("unsettled activation")
    ));

    let reservation_activation = ActivationId::new("bounded-reservation-activation").unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: first_child_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 1,
                header: None,
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        2,
                        20,
                        AgentControlRecordBody::ActivationStarted {
                            activation_id: reservation_activation.clone(),
                            root_session_id: session_id.clone(),
                            parent_session_id: Some(session_id.clone()),
                            path: AgentPath::new(vec![1]).unwrap(),
                        },
                    )
                    .unwrap(),
                    AgentControlRecord::new(
                        3,
                        20,
                        AgentControlRecordBody::CompletionReserved {
                            activation_id: reservation_activation.clone(),
                            parent_session_id: session_id.clone(),
                            maximum_bytes: u64::try_from(
                                rsi_agent_session_protocol::MAXIMUM_AGENT_MESSAGE_BYTES,
                            )
                            .unwrap(),
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("reserve one exact parent mailbox slot for child completion");
    let bounded_controls = (0..rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES - 1)
        .map(|offset| {
            AgentControlRecord::new(
                11 + u64::try_from(offset).unwrap(),
                20 + u64::try_from(offset).unwrap(),
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: MessageId::new(format!("bounded-message-{offset}")).unwrap(),
                        source: if offset == 0 {
                            AgentMessageSource::Completion {
                                child_session_id: first_child_id.clone(),
                                activation_id: reservation_activation.clone(),
                            }
                        } else {
                            AgentMessageSource::Human
                        },
                        content: vec![AgentMessageContent::Text {
                            text: if offset == 0 {
                                "child completed".into()
                            } else {
                                "x".repeat(
                                    MAXIMUM_STORE_MAILBOX_PAGE_BYTES
                                        / (rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
                                            - 1),
                                )
                            },
                        }],
                        options: MessageOptions::default(),
                    },
                    root_session_id: session_id.clone(),
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                    bound_turn_id: None,
                    target: MessageTarget::NextStep,
                    wake_required: false,
                },
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 8,
                expected_control_seq: 10,
                header: None,
                facts: Vec::new(),
                controls: bounded_controls,
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("fill every parent mailbox slot not reserved for child completion");
    let reserved_full_control_seq =
        10 + u64::try_from(rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES - 1).unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 8,
                    expected_control_seq: reserved_full_control_seq,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![AgentControlRecord::new(
                        reserved_full_control_seq + 1,
                        99,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: MessageId::new("message-blocked-by-reservation")
                                    .unwrap(),
                                source: AgentMessageSource::Human,
                                content: vec![AgentMessageContent::Text {
                                    text: "must preserve completion capacity".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: session_id.clone(),
                            delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                            bound_turn_id: None,
                            target: MessageTarget::NextStep,
                            wake_required: false,
                        },
                    )
                    .unwrap()],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("pending-message bound")
    ));
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: first_child_id,
                expected_fact_seq: 0,
                expected_control_seq: 3,
                header: None,
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        4,
                        100,
                        AgentControlRecordBody::ActivationSettled {
                            activation_id: reservation_activation,
                            outcome: ActivationOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("release the exact completion reservation");
    let final_bounded_offset = rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES - 1;
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 8,
                expected_control_seq: reserved_full_control_seq,
                header: None,
                facts: Vec::new(),
                controls: vec![AgentControlRecord::new(
                    reserved_full_control_seq + 1,
                    100,
                    AgentControlRecordBody::MessageAccepted {
                        message: AgentMessage {
                            message_id: MessageId::new(format!(
                                "bounded-message-{final_bounded_offset}"
                            ))
                            .unwrap(),
                            source: AgentMessageSource::Human,
                            content: vec![AgentMessageContent::Text {
                                text: "x".repeat(
                                    MAXIMUM_STORE_MAILBOX_PAGE_BYTES
                                        / (rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
                                            - 1),
                                ),
                            }],
                            options: MessageOptions::default(),
                        },
                        root_session_id: session_id.clone(),
                        delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                        bound_turn_id: None,
                        target: MessageTarget::NextStep,
                        wake_required: false,
                    },
                )
                .unwrap()],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("use the released mailbox slot");
    let full_control_seq = reserved_full_control_seq + 1;
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 8,
                    expected_control_seq: full_control_seq,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![AgentControlRecord::new(
                        full_control_seq + 1,
                        100,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: MessageId::new("message-beyond-bound").unwrap(),
                                source: AgentMessageSource::Human,
                                content: vec![AgentMessageContent::Text {
                                    text: "must not enter the mailbox".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: session_id.clone(),
                            delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                            bound_turn_id: None,
                            target: MessageTarget::NextStep,
                            wake_required: false,
                        },
                    )
                    .unwrap()],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Invalid(message)) if message.contains("pending-message bound")
    ));
    let mailbox = store.read_agent_mailbox(&session_id, None).await.unwrap();
    assert_eq!(
        mailbox.pending_count,
        rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES
    );
    assert!(
        mailbox.pending.len() < mailbox.pending_count,
        "the mailbox read must stop before materializing its valid worst case"
    );
    assert_eq!(
        store.read_agent_mailbox_summary(&session_id).await.unwrap(),
        StoreAgentMailboxSummary {
            pending_count: rsi_agent_session_protocol::MAXIMUM_PENDING_AGENT_MESSAGES,
            pending_promotable_message_ids: vec![MessageId::new("bounded-message-0").unwrap(),],
            durable_control_seq: full_control_seq,
            durable_fact_seq: 8,
        }
    );

    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 8,
                    expected_control_seq: full_control_seq,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![AgentControlRecord::new(
                        full_control_seq + 1,
                        101,
                        AgentControlRecordBody::MessagePromoted {
                            message_id: MessageId::new("bounded-message-1").unwrap(),
                        },
                    )
                    .unwrap()],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::Corrupt(message)) if message.contains("next-Step input")
    ));
    let promoted_message_id = MessageId::new("bounded-message-0").unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                expected_fact_seq: 8,
                expected_control_seq: full_control_seq,
                header: None,
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        full_control_seq + 1,
                        101,
                        AgentControlRecordBody::MessagePromoted {
                            message_id: promoted_message_id.clone(),
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .expect("promote a pending next-Step completion into the ready index");
    let promoted = store
        .read_agent_mailbox(&session_id, Some(&promoted_message_id))
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(promoted.target, MessageTarget::NextTurn);
    assert!(promoted.wake_required);
    let inspection = store.inspect_session(&session_id).await.unwrap();
    inspection.validate().unwrap();
    assert!(
        inspection
            .pending
            .iter()
            .any(|entry| entry.message_id == promoted_message_id
                && entry.target == MessageTarget::NextTurn)
    );
    let ready = store
        .list_ready_messages(&session_id, None, 8)
        .await
        .unwrap();
    assert!(ready.messages.iter().any(|message| {
        message.message_id == promoted_message_id
            && message.control_seq == full_control_seq + 1
            && message.target == MessageTarget::NextTurn
    }));

    let bytes: Arc<[u8]> = Arc::from(b"shared Store contract".as_slice());
    let object = store.put_cas(Arc::clone(&bytes)).await.unwrap();
    assert_eq!(store.put_cas(Arc::clone(&bytes)).await.unwrap(), object);
    assert_eq!(store.read_cas(&object).await.unwrap(), bytes);
    assert_unbound_steer_inspection(store, &header).await;
}

async fn assert_unbound_steer_inspection(store: &dyn SessionStore, template: &SessionHeader) {
    let mut value = serde_json::to_value(template).unwrap();
    value["session_id"] = serde_json::json!("unbound-steer-inspection");
    let header: SessionHeader = serde_json::from_value(value).unwrap();
    let session_id = header.session_id().clone();
    let message_id = MessageId::new("unbound-steer").unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: session_id.clone(),
                header: Some(header),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                facts: Vec::new(),
                controls: vec![
                    AgentControlRecord::new(
                        1,
                        1,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: message_id.clone(),
                                source: AgentMessageSource::Human,
                                content: vec![AgentMessageContent::Text {
                                    text: "idle steering".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: session_id.clone(),
                            delivery: rsi_agent_session_protocol::MessageDelivery::Steer,
                            bound_turn_id: None,
                            target: MessageTarget::NextTurn,
                            wake_required: true,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: Vec::new(),
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    let entry = store
        .read_agent_mailbox(&session_id, Some(&message_id))
        .await
        .unwrap()
        .selected
        .unwrap();
    let inspection = store.inspect_session(&session_id).await.unwrap();
    assert_eq!(inspection.pending.len(), 1);
    assert!(!entry.permits_promotion());
    assert_eq!(
        inspection.pending[0].permits_promotion,
        entry.permits_promotion(),
        "inspection must use the typed promotion rule for an unbound steer"
    );
    assert!(
        store
            .read_agent_mailbox_summary(&session_id)
            .await
            .unwrap()
            .pending_promotable_message_ids
            .is_empty()
    );
}

async fn assert_missing_session_atomic_append_is_not_found(store: &dyn SessionStore) {
    let session_id = SessionId::new("missing-atomic-control-tail").unwrap();
    assert!(matches!(
        store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq: 0,
                    expected_control_seq: 1,
                    header: None,
                    facts: Vec::new(),
                    controls: vec![AgentControlRecord::new(
                        2,
                        1,
                        AgentControlRecordBody::MessagePromoted {
                            message_id: MessageId::new("missing-atomic-message").unwrap(),
                        },
                    )
                    .unwrap()],
                }],
                required_active_activations: Vec::new(),
                quiescent_descendants_of: None,
            })
            .await,
        Err(StoreError::NotFound(missing)) if missing == session_id.as_str()
    ));
}
