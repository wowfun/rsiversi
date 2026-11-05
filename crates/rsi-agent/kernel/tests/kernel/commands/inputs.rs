use super::*;
use rsi_agent_composition_protocol::{CommandInput, CommandInputDelivery, DraftCommandPreparation};
use rsi_agent_session_protocol::{AgentControlRecord, InputMessageSource, MessageDelivery};
use rsi_agent_store_protocol::MailboxRead;

fn text_input(delivery: CommandInputDelivery, text: &str) -> CommandInput {
    CommandInput {
        delivery,
        content: vec![AgentMessageContent::Text { text: text.into() }],
        attribution: None,
    }
}

#[tokio::test]
async fn inbox_pause_command_injection_preserves_pause_and_receipt_retry_cannot_wake() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let fixture = Fixture::start(store.clone(), false).await;
        let session = &fixture.session_id;
        let read_mailbox = || async {
            store
                .read_agent_mailbox(session, MailboxRead::All)
                .await
                .unwrap()
        };
        let pause = || async {
            let tail = store.read_watermarks(session).await.unwrap();
            store
                .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
                    sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                        session_id: session.clone(),
                        expected_fact_seq: tail.durable_fact_seq,
                        expected_control_seq: tail.durable_control_seq,
                        header: None,
                        facts: vec![],
                        controls: vec![
                            AgentControlRecord::new(
                                tail.durable_control_seq + 1,
                                42,
                                AgentControlRecordBody::InboxPaused,
                            )
                            .unwrap(),
                        ],
                    }],
                    required_active_activations: vec![],
                    quiescent_descendants_of: None,
                })
                .await
                .unwrap();
        };
        pause().await;
        for (delivery, request, paused) in [
            (CommandInputDelivery::Inject, "held-command", true),
            (CommandInputDelivery::Steer, "waking-command", false),
        ] {
            *fixture.callback.inputs.lock().unwrap() = vec![text_input(delivery, request)];
            let invocation = fixture.invocation(request, true).await;
            let receipt = fixture
                .kernel
                .execute(
                    fixture.kernel.prepare_resume(session).await.unwrap(),
                    invocation.clone(),
                )
                .await
                .unwrap();
            let mailbox = read_mailbox().await;
            assert_eq!(mailbox.inbox_paused, paused);
            let input = &receipt.commit().command_inputs()[0];
            assert!(
                mailbox
                    .pending
                    .iter()
                    .any(|entry| entry.message == input.message)
            );
            if !paused {
                let controls = store
                    .read_controls(session, receipt.control_seq(), 2, FACT_PAGE_BYTES)
                    .await
                    .unwrap();
                assert!(matches!(
                    controls.records[0].body(),
                    AgentControlRecordBody::MessageAccepted { .. }
                ));
                assert!(matches!(
                    controls.records[1].body(),
                    AgentControlRecordBody::InboxResumed
                ));
                assert_eq!(controls.fact_cursors[0], controls.fact_cursors[1]);
            }
            pause().await;
            let before = read_mailbox().await;
            assert_eq!(
                fixture
                    .kernel
                    .execute(
                        fixture.kernel.prepare_resume(session).await.unwrap(),
                        invocation
                    )
                    .await
                    .unwrap(),
                receipt
            );
            assert_eq!(read_mailbox().await, before);
        }
        fixture.stop().await;
    }
}

fn input_only_generation(fixture: &Fixture) {
    let mut pin = fixture.composition.pin.write().unwrap();
    *pin = AgentCompositionPin::new(
        pin.preset_id().clone(),
        "d".repeat(64),
        pin.tools(),
        pin.context_builder(),
        DomainCatalog::default(),
        pin.contributions().clone(),
        Arc::new(()),
    )
    .unwrap();
}

async fn inject_only(
    fixture: &Fixture,
    session: &SessionId,
) -> (
    SessionCommandInvocation,
    rsi_agent_turn_protocol::DomainMutationReceipt,
) {
    *fixture.callback.inputs.lock().unwrap() = vec![CommandInput {
        delivery: CommandInputDelivery::Inject,
        content: vec![
            AgentMessageContent::Text {
                text: "Held across cold recovery".into(),
            },
            AgentMessageContent::Text {
                text: "第二块，保留边界".into(),
            },
            AgentMessageContent::File {
                file: rsi_media_protocol::FileRef::new(
                    rsi_media_protocol::MediaId::new("f".repeat(64)).unwrap(),
                    "held.txt",
                    9_000_000_000,
                )
                .unwrap(),
            },
        ],
        attribution: Some(rsi_agent_session_protocol::MessageAttribution {
            id: "original:boundary".into(),
            source: serde_json::json!({"kind":"plugin:sidechat","nested":{"value":3}}),
        }),
    }];
    let catalog = fixture
        .kernel
        .list(fixture.kernel.prepare_resume(session).await.unwrap())
        .await
        .unwrap();
    let invocation = SessionCommandInvocation {
        command: catalog.commands()[0].id().clone(),
        request_id: DomainRequestId::new("input-only-durable").unwrap(),
        expected_revision: catalog.revision(),
        arguments: CommandArguments::new(Value::Null).unwrap(),
    };
    let receipt = fixture
        .kernel
        .execute(
            fixture.kernel.prepare_resume(session).await.unwrap(),
            invocation.clone(),
        )
        .await
        .unwrap();
    assert!(receipt.commit().updates().is_empty());
    let entry = fixture
        .store
        .read_agent_message(
            session,
            &receipt.commit().command_inputs()[0].message.message_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!entry.wake_required);
    assert_eq!(entry.message, receipt.commit().command_inputs()[0].message);
    (invocation, receipt)
}

async fn submit_and_read_context(
    kernel: &AgentKernel,
    session: SubmitSession,
    id: &str,
) -> Vec<String> {
    kernel
        .submit_message(SubmitMessage {
            session,
            delivery: MessageDelivery::NextTurn,
            message: AgentMessage {
                message_id: MessageId::new(id).unwrap(),
                source: AgentMessageSource::Human { attribution: None },
                content: vec![AgentMessageContent::Text {
                    text: "Question".into(),
                }],
                options: MessageOptions::default(),
            },
        })
        .await
        .unwrap();
    let executor = kernel.register("context-executor".into()).unwrap();
    let claim = kernel
        .claim("context-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let page = kernel.read_facts(&claim, 0, 128).await.unwrap();
    let texts = page
        .facts
        .iter()
        .flat_map(|fact| match fact.body() {
            SessionFactBody::InputMessageClaimed {
                source: InputMessageSource::Command { source, .. },
                content,
                ..
            } => {
                if source.request_id.as_str() == "input-only-durable" {
                    let attribution = source.attribution.as_ref().unwrap();
                    assert_eq!(attribution.id, "original:boundary");
                    assert_eq!(
                        attribution.source,
                        serde_json::json!({"kind":"plugin:sidechat","nested":{"value":3}})
                    );
                }
                content
                    .iter()
                    .filter_map(|block| match block {
                        AgentMessageContent::Text { text } => Some(text.clone()),
                        AgentMessageContent::File { file } => {
                            assert_eq!(source.request_id.as_str(), "input-only-durable");
                            assert_eq!(file.name(), "held.txt");
                            assert_eq!(file.bytes(), 9_000_000_000);
                            None
                        }
                        _ => panic!("unexpected command content"),
                    })
                    .collect::<Vec<_>>()
            }
            _ => vec![],
        })
        .collect();
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(executor);
    texts
}

#[tokio::test]
async fn input_only_draft_and_durable_receipts_recover_without_domain_state() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let fixture = Fixture::start(store.clone(), false).await;
        input_only_generation(&fixture);
        let mut draft =
            AgentSessionDraft::new(header("no-domain-input"), fixture.composition.clone())
                .await
                .unwrap();
        *fixture.callback.inputs.lock().unwrap() = vec![text_input(
            CommandInputDelivery::Inject,
            "Side conversation boundary",
        )];
        let invocation = SessionCommandInvocation {
            command: draft.command_descriptors()[0].id().clone(),
            request_id: DomainRequestId::new("draft-boundary").unwrap(),
            expected_revision: draft.revision(),
            arguments: CommandArguments::new(Value::Null).unwrap(),
        };
        let DraftCommandPreparation::Run(prepared) =
            draft.prepare_command(invocation.clone()).unwrap()
        else {
            panic!("new request")
        };
        let receipt = draft
            .apply_command(prepared.execute(CancellationToken::new()).await.unwrap())
            .finish()
            .await
            .unwrap();
        assert!(
            matches!(draft.prepare_command(invocation).unwrap(), DraftCommandPreparation::Completed(retry) if retry == receipt)
        );
        assert!(store.header(draft.header().session_id()).await.is_err());
        assert!(
            draft
                .freeze()
                .initial_commit()
                .unwrap()
                .unwrap()
                .updates()
                .is_empty()
        );
        assert_eq!(
            submit_and_read_context(
                &fixture.kernel,
                SubmitSession::Fresh(draft.freeze()),
                "first-question"
            )
            .await,
            ["Side conversation boundary"]
        );
        let session = draft.header().session_id().clone();
        let (invocation, receipt) = inject_only(&fixture, &session).await;
        let calls = fixture.callback.calls.load(Ordering::SeqCst);
        let composition = fixture.composition.clone();
        fixture.kernel.shutdown(fixture.workers).await.unwrap();
        let kernel = AgentKernel::recover(store.clone(), composition)
            .await
            .unwrap();
        let workers = kernel.start_workers();
        assert_eq!(
            kernel
                .execute(kernel.prepare_resume(&session).await.unwrap(), invocation)
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), calls);
        let resume = SubmitSession::Resume(kernel.prepare_resume(&session).await.unwrap());
        // A distinct human identity starts the second Turn; retrying the first one cannot wake it.
        assert_eq!(
            submit_and_read_context(&kernel, resume, "second-question").await,
            [
                "Side conversation boundary",
                "Held across cold recovery",
                "第二块，保留边界"
            ]
        );
        kernel.shutdown(workers).await.unwrap();
        assert!(fixture.runtime.shutdown().await.is_clean());
    }
}

fn inputs(fixture: &Fixture) {
    *fixture.callback.inputs.lock().unwrap() = vec![
        text_input(CommandInputDelivery::Inject, "Mode changed"),
        text_input(CommandInputDelivery::Steer, "Prepare a plan"),
    ];
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One cross-Store scenario follows the atomic receipt through retry, source refusal and ordered model entry."
)]
async fn command_inputs_publish_atomically_keep_provenance_and_retry_once() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let fixture = Fixture::start(store, false).await;
        inputs(&fixture);
        let invocation = fixture.invocation("command-with-inputs", true).await;
        let receipt = fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                invocation.clone(),
            )
            .await
            .unwrap();
        let emitted = receipt.commit().command_inputs();
        assert_eq!(emitted.len(), 2);
        for (ordinal, input) in emitted.iter().enumerate() {
            let entry = fixture
                .store
                .read_agent_message(&fixture.session_id, &input.message.message_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                entry.accepted_control_seq,
                receipt.control_seq() + ordinal as u64 + 1
            );
            assert_eq!(entry.message, input.message);
            assert_eq!(entry.delivery, input.delivery);
            let AgentMessageSource::Command { source } = &entry.message.source else {
                panic!("command provenance")
            };
            assert_eq!(source.invocation_sha256, invocation.digest().unwrap());
            assert_eq!(entry.wake_required, ordinal == 1);
        }
        assert_eq!(
            fixture
                .kernel
                .execute(
                    fixture
                        .kernel
                        .prepare_resume(&fixture.session_id)
                        .await
                        .unwrap(),
                    invocation
                )
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
        // Serialized provenance is not permission to bypass command admission.
        assert!(
            fixture
                .kernel
                .submit_message(SubmitMessage {
                    session: SubmitSession::Resume(
                        fixture
                            .kernel
                            .prepare_resume(&fixture.session_id)
                            .await
                            .unwrap()
                    ),
                    delivery: MessageDelivery::Steer,
                    message: emitted[1].message.clone(),
                })
                .await
                .is_err()
        );
        let executor = fixture
            .kernel
            .register("command-input-executor".into())
            .unwrap();
        let claim = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture
                .kernel
                .claim("command-input-executor", CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(
            fixture
                .kernel
                .claim_pending_step_messages(&claim)
                .await
                .unwrap(),
            0
        );
        let page = fixture.kernel.read_facts(&claim, 0, 128).await.unwrap();
        let entered = page
            .facts
            .iter()
            .filter(|fact| {
                matches!(
                    fact.body(),
                    SessionFactBody::InputMessageClaimed {
                        source: InputMessageSource::Command { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(entered, 2);
        let texts = page
            .facts
            .iter()
            .filter_map(|fact| match fact.body() {
                SessionFactBody::InputMessageClaimed {
                    source: InputMessageSource::Command { .. },
                    content,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            emitted
                .iter()
                .map(|input| input.message.content.clone())
                .collect::<Vec<_>>()
        );
        let initial = rsi_agent_turn_protocol::initial_turn_input(&fixture.kernel, &claim)
            .await
            .unwrap();
        assert!(!initial.human);
        fixture
            .kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(executor);
        fixture.stop().await;
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One draft scenario checks refusal before publication, mailbox acceptance, model entry and preset replacement."
)]
async fn draft_inputs_remain_local_until_first_submit_and_enter_first_step() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let mut draft =
        AgentSessionDraft::new(header("draft-command-inputs"), fixture.composition.clone())
            .await
            .unwrap();
    inputs(&fixture);
    let invocation = SessionCommandInvocation {
        command: draft.command_descriptors()[0].id().clone(),
        request_id: DomainRequestId::new("draft-with-inputs").unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(true.into()).unwrap(),
    };
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(invocation).unwrap() else {
        panic!("new command")
    };
    let draft_receipt = draft
        .apply_command(prepared.execute(CancellationToken::new()).await.unwrap())
        .finish()
        .await
        .unwrap();
    let frozen = draft.freeze();
    assert_eq!(
        draft_receipt.state_sha256(),
        frozen.initial_commit().unwrap().unwrap().request_sha256()
    );
    assert_eq!(frozen.command_inputs().len(), 2);
    assert!(matches!(
        fixture.store.header(frozen.header().session_id()).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        fixture
            .kernel
            .submit(SubmitTurn {
                reasoning_effort: None,
                session: SubmitSession::Fresh(frozen),
                turn_id: TurnId::new("draft-direct-turn").unwrap(),
                text: "Start now".into(),
                model: None,
                sandbox: None
            })
            .await
            .is_err()
    );
    assert!(matches!(
        fixture.store.header(draft.header().session_id()).await,
        Err(StoreError::NotFound(_))
    ));
    fixture
        .kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(draft.freeze()),
            delivery: MessageDelivery::NextTurn,
            message: AgentMessage {
                message_id: MessageId::new("first-human-input").unwrap(),
                source: AgentMessageSource::Human { attribution: None },
                content: vec![AgentMessageContent::Text {
                    text: "Start now".into(),
                }],
                options: MessageOptions::default(),
            },
        })
        .await
        .unwrap();
    let executor = fixture
        .kernel
        .register("draft-input-executor".into())
        .unwrap();
    let claim = fixture
        .kernel
        .claim("draft-input-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.session_id(), draft.header().session_id());
    assert_eq!(
        fixture
            .kernel
            .claim_pending_step_messages(&claim)
            .await
            .unwrap(),
        0
    );
    let page = fixture.kernel.read_facts(&claim, 0, 128).await.unwrap();
    assert_eq!(
        page.facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageClaimed {
                    source: InputMessageSource::Command { .. },
                    ..
                }
            ))
            .count(),
        2
    );
    fixture
        .kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    draft
        .select_preset(AgentPresetId::new("test-agent").unwrap())
        .await
        .unwrap();
    assert!(draft.freeze().command_inputs().is_empty());
    drop(executor);
    fixture.stop().await;
}

#[tokio::test]
async fn invalid_command_input_rolls_back_domain_and_observer() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    *fixture.callback.inputs.lock().unwrap() = vec![text_input(CommandInputDelivery::Steer, "")];
    let invocation = fixture.invocation("invalid-command-input", true).await;
    let before = fixture
        .store
        .read_watermarks(&fixture.session_id)
        .await
        .unwrap();
    assert!(
        fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                invocation.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .read_watermarks(&fixture.session_id)
            .await
            .unwrap(),
        before
    );
    assert!(
        fixture
            .kernel
            .query(&fixture.session_id, &invocation.request_id)
            .await
            .unwrap()
            .is_none()
    );
    fixture.stop().await;
}
