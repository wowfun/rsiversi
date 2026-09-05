use super::*;

#[tokio::test]
async fn fresh_empty_workspace_snapshot_publishes_no_replacement_or_tombstone() {
    let store = Arc::new(MemoryStore::new());
    let context = Arc::new(QueuedWorkspaceContext {
        snapshots: Mutex::new(VecDeque::from([WorkspaceContextSnapshot {
            complete: true,
            instructions_sha256: "a".repeat(64),
            instructions: None,
            skill_catalog_sha256: "b".repeat(64),
            skill_catalog: None,
            invocations: Vec::new(),
        }])),
        calls: AtomicUsize::new(0),
    });
    let kernel = SessionKernel::recover_with_context_clock_and_limits(
        store.clone(),
        composition(),
        context.clone(),
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-empty-workspace").unwrap();
    let message_id = MessageId::new("message-empty-workspace").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    kernel
        .claim_message(ClaimMessage {
            session: kernel.prepare_resume(&session_id).await.unwrap(),
            message_id,
            activation_id: ActivationId::new("activation-empty-workspace").unwrap(),
            path: AgentPath::root(),
            turn_id: TurnId::new("turn-empty-workspace").unwrap(),
            step_id: StepId::new("step-empty-workspace").unwrap(),
        })
        .await
        .unwrap();

    let page = store.read_facts(&session_id, 0, 16).await.unwrap();
    assert_eq!(context.calls.load(Ordering::SeqCst), 1);
    assert!(!page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageEntered {
            source: rsi_agent_session_protocol::InputMessageSource::AgentInstructions { .. }
                | rsi_agent_session_protocol::InputMessageSource::SkillCatalog { .. },
            ..
        }
    )));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The refresh regression shows the complete replacement, tombstone, and deduplication sequence.
async fn workspace_refresh_durably_tombstones_removed_instructions_and_suppresses_repeats() {
    let store = Arc::new(MemoryStore::new());
    let empty_catalog_digest = "c".repeat(64);
    let context = Arc::new(QueuedWorkspaceContext {
        snapshots: Mutex::new(VecDeque::from([
            WorkspaceContextSnapshot {
                complete: true,
                instructions_sha256: "a".repeat(64),
                instructions: Some("ACTIVE WORKSPACE INSTRUCTIONS".into()),
                skill_catalog_sha256: empty_catalog_digest.clone(),
                skill_catalog: None,
                invocations: Vec::new(),
            },
            WorkspaceContextSnapshot {
                complete: true,
                instructions_sha256: "b".repeat(64),
                instructions: None,
                skill_catalog_sha256: empty_catalog_digest.clone(),
                skill_catalog: None,
                invocations: Vec::new(),
            },
            WorkspaceContextSnapshot {
                complete: true,
                instructions_sha256: "b".repeat(64),
                instructions: None,
                skill_catalog_sha256: empty_catalog_digest,
                skill_catalog: None,
                invocations: Vec::new(),
            },
        ])),
        calls: AtomicUsize::new(0),
    });
    let kernel = SessionKernel::recover_with_context_clock_and_limits(
        store.clone(),
        composition(),
        context.clone(),
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let session_id = SessionId::new("session-workspace-refresh").unwrap();
    let message_id = MessageId::new("message-workspace-refresh").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    kernel
        .claim_message(ClaimMessage {
            session: kernel.prepare_resume(&session_id).await.unwrap(),
            message_id,
            activation_id: ActivationId::new("activation-workspace-refresh").unwrap(),
            path: AgentPath::root(),
            turn_id: TurnId::new("turn-workspace-refresh").unwrap(),
            step_id: StepId::new("step-workspace-refresh").unwrap(),
        })
        .await
        .unwrap();
    let _lease = kernel
        .register("executor-workspace-refresh".into())
        .unwrap();
    let claim = kernel
        .claim("executor-workspace-refresh", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(kernel.refresh_workspace_context(&claim).await.unwrap(), 1);
    assert_eq!(kernel.refresh_workspace_context(&claim).await.unwrap(), 0);
    assert_eq!(context.calls.load(Ordering::SeqCst), 3);

    let page = store.read_facts(&session_id, 0, 16).await.unwrap();
    let instruction_facts = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::InputMessageEntered {
                source:
                    rsi_agent_session_protocol::InputMessageSource::AgentInstructions {
                        sha256,
                        tombstone,
                        ..
                    },
                content,
                ..
            } => Some((sha256, tombstone, content)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(instruction_facts.len(), 2);
    assert_eq!(instruction_facts[0].0, &"a".repeat(64));
    assert!(!instruction_facts[0].1);
    assert_eq!(instruction_facts[1].0, &"b".repeat(64));
    assert!(instruction_facts[1].1);
    assert!(matches!(
        instruction_facts[1].2.as_slice(),
        [AgentMessageContent::Text { text }]
            if text.contains("earlier workspace instructions no longer apply")
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One restart sequence compares the complete persisted workspace projection.
async fn cold_resume_restores_workspace_digests_without_duplicate_replacements() {
    let store = Arc::new(MemoryStore::new());
    let snapshot = WorkspaceContextSnapshot {
        complete: true,
        instructions_sha256: "a".repeat(64),
        instructions: Some("STABLE WORKSPACE INSTRUCTIONS".into()),
        skill_catalog_sha256: "b".repeat(64),
        skill_catalog: Some("<available_skills>stable</available_skills>".into()),
        invocations: Vec::new(),
    };
    let first_context = Arc::new(QueuedWorkspaceContext {
        snapshots: Mutex::new(VecDeque::from([snapshot.clone()])),
        calls: AtomicUsize::new(0),
    });
    let first = SessionKernel::recover_with_context_clock_and_limits(
        store.clone(),
        composition(),
        first_context,
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let first_worker = first.start_write_behind();
    let session_id = SessionId::new("session-workspace-cold-digests").unwrap();
    first
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-workspace-cold-first"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _first_lease = first
        .register("executor-workspace-cold-first".into())
        .unwrap();
    let first_claim = first
        .claim("executor-workspace-cold-first", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    first
        .finish_activation_turn(&first_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    first.shutdown(first_worker).await.unwrap();

    let second_context = Arc::new(QueuedWorkspaceContext {
        snapshots: Mutex::new(VecDeque::from([snapshot])),
        calls: AtomicUsize::new(0),
    });
    let second = SessionKernel::recover_with_context_clock_and_limits(
        store.clone(),
        composition(),
        second_context,
        Arc::new(FixedClock),
        KernelLimits::default(),
    )
    .await
    .unwrap();
    let second_worker = second.start_write_behind();
    second
        .submit_message(SubmitMessage {
            session: resume(&second, session_id.clone()).await,
            message: mailbox_message("message-workspace-cold-second"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _second_lease = second
        .register("executor-workspace-cold-second".into())
        .unwrap();
    let second_claim = second
        .claim("executor-workspace-cold-second", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    second
        .finish_activation_turn(&second_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();

    let facts = store.read_facts(&session_id, 0, 64).await.unwrap().facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageEntered {
                    source: rsi_agent_session_protocol::InputMessageSource::AgentInstructions { .. }
                        | rsi_agent_session_protocol::InputMessageSource::SkillCatalog { .. },
                    ..
                }
            ))
            .count(),
        2
    );
    second.shutdown(second_worker).await.unwrap();
}

#[tokio::test]
async fn mailbox_admission_creates_a_zero_fact_session_and_survives_restart() {
    let store = Arc::new(MemoryStore::new());
    let first = kernel(store.clone()).await;
    let first_worker = first.start_write_behind();
    let session_id = SessionId::new("session-mailbox-zero-fact").unwrap();
    let receipt = first
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-zero-fact"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    assert_eq!(receipt.state, MessageState::Pending);
    let facts = store.read_facts(&session_id, 0, 1).await.unwrap();
    assert_eq!(facts.durable_seq, 0);
    assert!(facts.facts.is_empty());
    let controls = store.read_controls(&session_id, 0, 8).await.unwrap();
    assert_eq!(controls.durable_seq, 1);
    assert_eq!(controls.records.len(), 1);
    let recent = store.list_recent_sessions(None, 8).await.unwrap();
    assert_eq!(recent.sessions.len(), 1);
    assert_eq!(recent.sessions[0].header.session_id(), &session_id);
    let history = store.read_facts_before(&session_id, 0, 8).await.unwrap();
    assert_eq!(history.before_seq, 1);
    assert_eq!(history.durable_seq, 0);
    assert!(history.facts.is_empty());
    assert!(!history.has_more);
    let retried = first
        .submit_message(SubmitMessage {
            session: resume(&first, session_id.clone()).await,
            message: mailbox_message("message-zero-fact"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    assert_eq!(retried, receipt);
    let mut changed = mailbox_message("message-zero-fact");
    changed.content = vec![AgentMessageContent::Text {
        text: "different input".into(),
    }];
    assert!(matches!(
        first
            .submit_message(SubmitMessage {
                session: resume(&first, session_id.clone()).await,
                message: changed,
                target: MessageTarget::NextTurn,
                wake_required: true,
            })
            .await,
        Err(TurnError::MessageConflict { session, message })
            if session == session_id.as_str() && message == "message-zero-fact"
    ));

    first.shutdown(first_worker).await.unwrap();
    let restarted = kernel(store).await;
    let restarted_worker = restarted.start_write_behind();
    assert_eq!(
        restarted
            .message_status(&session_id, &MessageId::new("message-zero-fact").unwrap())
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        restarted
            .outcome(&session_id, &TurnId::new("turn-never-created").unwrap())
            .await,
        Err(TurnError::TurnNotFound { session, turn })
            if session == session_id.as_str() && turn == "turn-never-created"
    ));
    restarted.shutdown(restarted_worker).await.unwrap();
}

#[tokio::test]
async fn message_claim_atomically_enters_activation_turn_step_and_input() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let initial_worker = initial.start_write_behind();
    let session_id = SessionId::new("session-message-claim").unwrap();
    let message_id = MessageId::new("message-claim").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    initial.shutdown(initial_worker).await.unwrap();

    let restarted = kernel(store.clone()).await;
    let restarted_worker = restarted.start_write_behind();
    let turn_id = TurnId::new("turn-message-claim").unwrap();
    let activation_id = ActivationId::new("activation-message-claim").unwrap();
    let step_id = StepId::new("step-message-claim").unwrap();
    let submitted = restarted
        .claim_message(ClaimMessage {
            session: restarted.prepare_resume(&session_id).await.unwrap(),
            message_id: message_id.clone(),
            activation_id: activation_id.clone(),
            path: AgentPath::root(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(submitted.accepted_seq, 1);
    assert_eq!(
        restarted
            .message_status(&session_id, &message_id)
            .await
            .unwrap()
            .state,
        MessageState::Claimed {
            activation_id,
            turn_id: turn_id.clone(),
            step_id,
            entered_fact_seq: 3,
        }
    );
    let facts = store.read_facts(&session_id, 0, 8).await.unwrap();
    assert_eq!(facts.durable_seq, 3);
    assert!(matches!(
        facts.facts[0].body(),
        SessionFactBody::MessageTurnAccepted { .. }
    ));
    assert!(matches!(
        facts.facts[1].body(),
        SessionFactBody::StepStarted { .. }
    ));
    assert!(matches!(
        facts.facts[2].body(),
        SessionFactBody::InputMessageEntered { .. }
    ));
    let controls = store.read_controls(&session_id, 0, 8).await.unwrap();
    assert_eq!(controls.durable_seq, 3);

    let mut resumed = restarted
        .observe_session(
            &session_id,
            ObservationCursor {
                control_seq: 1,
                fact_seq: 2,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        resumed.next().await.unwrap().unwrap(),
        SessionObservation::Control { .. }
    ));
    assert!(matches!(
        resumed.next().await.unwrap().unwrap(),
        SessionObservation::Control { .. }
    ));
    assert!(matches!(
        resumed.next().await.unwrap().unwrap(),
        SessionObservation::Fact { .. }
    ));
    restarted.shutdown(restarted_worker).await.unwrap();
}

#[tokio::test]
async fn durable_tree_membership_for_approval_routing_survives_a_cold_restart() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let initial_worker = initial.start_write_behind();
    let root_id = SessionId::new("session-cold-tree-root").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-cold-tree-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = initial.register("executor-cold-tree-root".into()).unwrap();
    let root_claim = initial
        .claim("executor-cold-tree-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child_id = SessionId::new("session-cold-tree-child").unwrap();
    initial
        .spawn_agent(SpawnAgentRequest {
            caller: initial.agent_caller(&root_claim).unwrap(),
            child_session_id: child_id.clone(),
            task_name: "cold-child".into(),
            message_id: MessageId::new("message-cold-tree-child").unwrap(),
            message: "complete before restart".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = initial.register("executor-cold-tree-child".into()).unwrap();
    let child_claim = initial
        .claim("executor-cold-tree-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    initial
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        initial
            .enter_pending_step_messages(&root_claim)
            .await
            .unwrap(),
        1
    );
    initial
        .finish_activation_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    initial.shutdown(initial_worker).await.unwrap();

    let restarted = kernel(store).await;
    let restarted_worker = restarted.start_write_behind();
    assert_eq!(
        restarted.tree_sessions(&root_id).await.unwrap(),
        [root_id, child_id]
    );
    restarted.shutdown(restarted_worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live depth-three chain pins both lineage authorization and the spawn ceiling.
async fn only_a_live_ancestor_can_interrupt_a_descendant_turn() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-interrupt-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-interrupt-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-interrupt-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-interrupt-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let root_caller = kernel.agent_caller(&root_claim).unwrap();

    let child_id = SessionId::new("session-interrupt-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: root_caller.clone(),
            child_session_id: child_id.clone(),
            task_name: "interrupt-child".into(),
            message_id: MessageId::new("message-interrupt-child").unwrap(),
            message: "spawn the grandchild".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _child_lease = kernel.register("executor-interrupt-child".into()).unwrap();
    let child_claim = kernel
        .claim("executor-interrupt-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child_caller = kernel.agent_caller(&child_claim).unwrap();

    let grandchild_id = SessionId::new("session-interrupt-grandchild").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: child_caller.clone(),
            child_session_id: grandchild_id.clone(),
            task_name: "interrupt-grandchild".into(),
            message_id: MessageId::new("message-interrupt-grandchild").unwrap(),
            message: "spawn the depth-three leaf".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let _grandchild_lease = kernel
        .register("executor-interrupt-grandchild".into())
        .unwrap();
    let grandchild_claim = kernel
        .claim("executor-interrupt-grandchild", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let grandchild_caller = kernel.agent_caller(&grandchild_claim).unwrap();

    let leaf_id = SessionId::new("session-interrupt-leaf").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            caller: grandchild_caller.clone(),
            child_session_id: leaf_id.clone(),
            task_name: "interrupt-leaf".into(),
            message_id: MessageId::new("message-interrupt-leaf").unwrap(),
            message: "remain live".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();

    assert!(matches!(
        kernel.interrupt_agent(&child_caller, &root_id).await,
        Err(TurnError::Invalid(message)) if message.contains("live ancestor caller")
    ));
    kernel
        .finish_activation_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap()
        .unwrap();
    let _leaf_lease = kernel.register("executor-interrupt-leaf".into()).unwrap();
    let leaf_claim = kernel
        .claim("executor-interrupt-leaf", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let leaf_caller = kernel.agent_caller(&leaf_claim).unwrap();
    assert!(matches!(
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller: leaf_caller,
                child_session_id: SessionId::new("session-too-deep").unwrap(),
                task_name: "too-deep".into(),
                message_id: MessageId::new("message-too-deep").unwrap(),
                message: "must not be admitted".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await,
        Err(TurnError::Capacity)
    ));
    let queued = kernel
        .submit(SubmitTurn {
            session: resume(&kernel, leaf_id.clone()).await,
            turn_id: TurnId::new("turn-interrupt-queued").unwrap(),
            text: "queued work remains accepted".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    kernel
        .flush(&leaf_claim, queued.accepted_seq)
        .await
        .unwrap();
    assert_eq!(
        kernel
            .interrupt_agent(&root_caller, &leaf_id)
            .await
            .unwrap(),
        rsi_agent_turn_protocol::CancelResult {
            accepted: true,
            already_terminal: false,
        }
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The exact 256-node boundary requires constructing every durable child identity.
async fn spawn_rejects_the_two_hundred_fifty_seventh_tree_session() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let root_id = SessionId::new("session-tree-capacity-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-tree-capacity-root"),
            target: MessageTarget::NextTurn,
            wake_required: true,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-tree-capacity-root".into())
        .unwrap();
    let root_claim = kernel
        .claim("executor-tree-capacity-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = kernel.agent_caller(&root_claim).unwrap();
    let parent_header = root_claim.header().clone();
    let parent_fingerprint = parent_header.fingerprint().unwrap();
    let empty_fact_prefix = fact_prefix_sha256(std::iter::empty::<&SessionFact>()).unwrap();
    let sessions = (1..rsi_agent_session_protocol::MAXIMUM_DURABLE_AGENT_TREE_NODES)
        .map(|index| {
            let child_id = SessionId::new(format!("session-tree-capacity-child-{index}")).unwrap();
            let origin = rsi_agent_session_protocol::ForkOrigin {
                parent_session_id: root_id.clone(),
                root_session_id: root_id.clone(),
                path: AgentPath::new(vec![u16::try_from(index).unwrap()]).unwrap(),
                task_name: format!("capacity-child-{index}"),
                parent_header_fingerprint: parent_fingerprint.clone(),
                invoking_turn_id: root_claim.turn_id().clone(),
                resolved_after_seq: 0,
                resolved_terminal_seq: 0,
                terminal_prefix_sha256: empty_fact_prefix.clone(),
                requested_turns: ForkTurnSelection::None,
                effective_turns: 0,
            };
            let child_header = parent_header
                .forked_child(child_id.clone(), 50, origin)
                .unwrap();
            rsi_agent_store_protocol::AtomicSessionAppend {
                session_id: child_id.clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(child_header),
                facts: Vec::new(),
                controls: vec![
                    rsi_agent_session_protocol::AgentControlRecord::new(
                        1,
                        50,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: MessageId::new(format!(
                                    "message-tree-capacity-child-{index}"
                                ))
                                .unwrap(),
                                source: AgentMessageSource::Human,
                                content: vec![AgentMessageContent::Text {
                                    text: "remain ready".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: root_id.clone(),
                            target: MessageTarget::NextTurn,
                            wake_required: true,
                        },
                    )
                    .unwrap(),
                ],
            }
        })
        .collect::<Vec<_>>();
    for session in sessions {
        store
            .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
                sessions: vec![session],
                required_active_activations: Vec::new(),
                quiescent_sessions: Vec::new(),
            })
            .await
            .unwrap();
    }

    assert!(matches!(
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller,
                child_session_id: SessionId::new("session-tree-capacity-overflow").unwrap(),
                task_name: "capacity-overflow".into(),
                message_id: MessageId::new("message-tree-capacity-overflow").unwrap(),
                message: "must not be admitted".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await,
        Err(TurnError::Capacity)
    ));
    kernel.shutdown(worker).await.unwrap();
}
