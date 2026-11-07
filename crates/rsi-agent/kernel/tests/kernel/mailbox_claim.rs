use super::*;
use rsi_agent_session_protocol::{MessageAttribution, MessageDelivery};

fn input(id: &str, application: bool) -> AgentMessage {
    let mut message = mailbox_message(id);
    if application {
        message.source = AgentMessageSource::Application {
            attribution: Some(MessageAttribution {
                id: id.into(),
                source: serde_json::json!({"kind": "plugin:input"}),
            }),
        };
    }
    message.content = vec![AgentMessageContent::Text { text: id.into() }];
    message
}

async fn enqueue(kernel: &AgentKernel, session: &SessionId, message: AgentMessage, step: bool) {
    kernel
        .submit_message(SubmitMessage {
            session: resume(kernel, session.clone()).await,
            message,
            delivery: if step {
                MessageDelivery::NextStep
            } else {
                MessageDelivery::NextTurn
            },
        })
        .await
        .unwrap();
}

async fn seed(store: Arc<dyn SessionStore>, steps: usize, other: bool) -> Vec<AgentMessage> {
    let kernel = AgentKernel::recover_with_clock(store, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_workers();
    let session = SessionId::new("claim-order").unwrap();
    let mut ordered = vec![input("step-0", false)];
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session.as_str())),
            message: ordered[0].clone(),
            delivery: MessageDelivery::NextStep,
        })
        .await
        .unwrap();
    let mut wake = input("wake", true);
    wake.options.sandbox = Some(SandboxMode::ReadOnly);
    enqueue(&kernel, &session, wake.clone(), false).await;
    if other {
        enqueue(&kernel, &session, input("other-turn", false), false).await;
    }
    // Later held input must precede the earlier waking input, across both sources.
    for index in 1..steps {
        let message = input(&format!("step-{index}"), index % 2 == 1);
        enqueue(&kernel, &session, message.clone(), true).await;
        ordered.push(message);
    }
    ordered.push(wake);
    kernel.shutdown(worker).await.unwrap();
    ordered
}

async fn claim_batch(
    kernel: &AgentKernel,
) -> Result<rsi_agent_turn_protocol::SubmittedTurn, TurnError> {
    kernel
        .claim_message(ClaimMessage {
            session: kernel
                .prepare_resume(&SessionId::new("claim-order").unwrap())
                .await
                .unwrap(),
            message_id: MessageId::new("wake").unwrap(),
            activation_id: ActivationId::new("activation-order").unwrap(),
            turn_id: TurnId::new("turn-order").unwrap(),
            step_id: StepId::new("step-order").unwrap(),
            path: AgentPath::root(),
        })
        .await
}

async fn check_claims(
    kernel: &AgentKernel,
    store: &dyn SessionStore,
    ordered: &[AgentMessage],
    other: bool,
) {
    let session = SessionId::new("claim-order").unwrap();
    let page = store
        .read_facts(
            &session,
            0,
            128,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert_eq!(
        page.facts.len(),
        ordered.len() + 2,
        "one Step without an empty predecessor"
    );
    let SessionFactBody::MessageTurnAccepted {
        message_ids,
        sandbox,
        ..
    } = page.facts[0].body()
    else {
        panic!("missing Turn acceptance");
    };
    assert_eq!(
        message_ids,
        &ordered
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        *sandbox,
        SandboxMode::ReadOnly,
        "waking input owns the policy"
    );
    for (index, message) in ordered.iter().enumerate() {
        let SessionFactBody::InputMessageClaimed {
            source, content, ..
        } = page.facts[index + 2].body()
        else {
            panic!("missing claim");
        };
        assert_eq!(source, &message.claimed_source());
        assert_eq!(content, &message.content);
        assert_eq!(
            kernel
                .message_status(&session, &message.message_id)
                .await
                .unwrap()
                .state,
            MessageState::Claimed {
                activation_id: ActivationId::new("activation-order").unwrap(),
                turn_id: TurnId::new("turn-order").unwrap(),
                step_id: StepId::new("step-order").unwrap(),
                claimed_fact_seq: index as u64 + 3,
            }
        );
    }
    let mailbox = store
        .read_agent_mailbox(&session, MailboxRead::All)
        .await
        .unwrap();
    assert_eq!(mailbox.pending_count, usize::from(other));
    if other {
        assert_eq!(mailbox.pending[0].message.message_id.as_str(), "other-turn");
    }
}

#[tokio::test]
async fn mailbox_initial_claim_orders_both_horizons_and_retries_on_memory_and_sqlite() {
    for sqlite in [false, true] {
        for (steps, other) in [(2, true), (63, false)] {
            let directory = tempfile::tempdir().unwrap();
            let store: Arc<dyn SessionStore> = if sqlite {
                Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
            } else {
                Arc::new(MemoryStore::new())
            };
            let ordered = seed(store.clone(), steps, other).await;
            let kernel =
                AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                    .await
                    .unwrap();
            let worker = kernel.start_workers();
            let submitted = claim_batch(&kernel).await.unwrap();
            check_claims(&kernel, store.as_ref(), &ordered, other).await;
            let before = store.read_watermarks(&submitted.session_id).await.unwrap();
            assert_eq!(claim_batch(&kernel).await.unwrap(), submitted);
            assert_eq!(
                store.read_watermarks(&submitted.session_id).await.unwrap(),
                before
            );
            kernel.shutdown(worker).await.unwrap();
            drop(kernel);
            drop(store);
            if sqlite {
                rsi_agent_store_sqlite::SqliteStore::verify(directory.path()).unwrap();
                let reopened = rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap();
                let mailbox = reopened
                    .read_agent_mailbox(
                        &SessionId::new("claim-order").unwrap(),
                        MailboxRead::WithMessage(MessageId::new("wake").unwrap()),
                    )
                    .await
                    .unwrap();
                assert!(matches!(mailbox.selected.unwrap().state,
                    rsi_agent_store_protocol::StoreAgentMessageState::Claimed { claimed_fact_seq, .. }
                    if claimed_fact_seq == ordered.len() as u64 + 2));
            }
        }
    }
}

#[tokio::test]
async fn mailbox_initial_claim_does_not_let_unrelated_turns_hide_held_input() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let ordered = seed(store.clone(), 2, true).await;
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    // A mixed prefix holds step-0, wake and other-turn, hiding step-1. The
    // selected claim fits this page after unrelated Turn input is excluded.
    store.mailbox.prefix_limit.store(3, Ordering::Relaxed);
    claim_batch(&kernel).await.unwrap();
    check_claims(&kernel, store.as_ref(), &ordered, true).await;
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn mailbox_initial_claim_is_independent_of_payload_page_boundaries() {
    for limit in [1, 2] {
        let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
        let ordered = seed(store.clone(), 2, true).await;
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let worker = kernel.start_workers();
        store.mailbox.prefix_limit.store(limit, Ordering::Relaxed);
        claim_batch(&kernel).await.unwrap();
        check_claims(&kernel, store.as_ref(), &ordered, true).await;
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
async fn mailbox_step_claim_drains_all_selected_inputs_without_payload_pages() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    seed(store.clone(), 2, true).await;
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    claim_batch(&kernel).await.unwrap();
    let _executor = kernel.register("step-claim-executor".into()).unwrap();
    let claim = kernel
        .claim("step-claim-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    for id in ["steer-a", "steer-b"] {
        enqueue(&kernel, claim.session_id(), input(id, false), true).await;
    }
    let before = store.read_watermarks(claim.session_id()).await.unwrap();
    store.mailbox.prefix_limit.store(1, Ordering::Relaxed);
    store
        .mailbox
        .claim_references
        .store(true, Ordering::Relaxed);
    assert_eq!(kernel.claim_pending_step_messages(&claim).await.unwrap(), 2);
    assert!(
        store
            .mailbox
            .claim_references
            .swap(false, Ordering::Relaxed)
    );
    let page = store
        .read_facts(claim.session_id(), before.durable_fact_seq, 4, 64 * 1024)
        .await
        .unwrap();
    assert_eq!(page.facts.len(), 4);
    let mut steps = Vec::new();
    for (fact, id) in page.facts[2..].iter().zip(["steer-a", "steer-b"]) {
        let SessionFactBody::InputMessageClaimed {
            source: rsi_agent_session_protocol::InputMessageSource::Human { message_id, .. },
            step_id,
            ..
        } = fact.body()
        else {
            panic!("missing queued input")
        };
        assert_eq!(message_id.as_str(), id);
        steps.push(step_id);
    }
    assert_eq!(steps[0], steps[1]);
    assert_eq!(
        store
            .read_agent_mailbox(claim.session_id(), MailboxRead::All)
            .await
            .unwrap()
            .pending_count,
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[ignore = "opt-in 68 MiB logical claim and durable verification"]
async fn mailbox_initial_claim_streams_beyond_the_atomic_payload_budget() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap());
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    let session = SessionId::new("claim-order").unwrap();
    for index in 0..34 {
        let mut message = input(&format!("large-{index}"), false);
        message.content = vec![AgentMessageContent::Text {
            text: "\n".repeat(1024 * 1024),
        }];
        let submission = if index == 0 {
            fresh(header(session.as_str()))
        } else {
            resume(&kernel, session.clone()).await
        };
        kernel
            .submit_message(SubmitMessage {
                session: submission,
                message,
                delivery: MessageDelivery::NextStep,
            })
            .await
            .unwrap();
    }
    enqueue(&kernel, &session, input("wake", false), false).await;
    let metadata = store
        .read_mailbox_claim(&session, Some(MessageId::new("wake").unwrap()))
        .await
        .unwrap();
    assert_eq!(metadata.pending.len(), 35);
    claim_batch(&kernel).await.unwrap();
    let marks = store.read_watermarks(&session).await.unwrap();
    assert_eq!(marks.durable_fact_seq, 37);
    let mut encoded = 0;
    for seq in 3..=37 {
        let page = store
            .read_facts(&session, seq - 1, 1, 3 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(page.facts.len(), 1);
        let fact = &page.facts[0];
        encoded += fact.encoded_len();
        let SessionFactBody::InputMessageClaimed {
            content,
            step_id,
            source,
            ..
        } = fact.body()
        else {
            panic!("missing input Fact")
        };
        assert_eq!(step_id.as_str(), "step-order");
        let rsi_agent_session_protocol::InputMessageSource::Human { message_id, .. } = source
        else {
            panic!("wrong input source")
        };
        assert_eq!(
            message_id.as_str(),
            if seq < 37 {
                format!("large-{}", seq - 3)
            } else {
                "wake".into()
            }
        );
        let [AgentMessageContent::Text { text }] = content.as_slice() else {
            panic!("wrong input content")
        };
        if seq < 37 {
            assert_eq!(text.len(), 1024 * 1024);
            assert!(text.bytes().all(|byte| byte == b'\n'));
        } else {
            assert_eq!(text, "wake");
        }
    }
    assert!(encoded > rsi_agent_store_protocol::MAXIMUM_STORE_BATCH_BYTES);
    let remaining = store.read_mailbox_claim(&session, None).await.unwrap();
    assert!(remaining.pending.is_empty());
    kernel.shutdown(worker).await.unwrap();
    drop(kernel);
    drop(store);
    rsi_agent_store_sqlite::SqliteStore::verify(directory.path()).unwrap();
    let reopened = rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap();
    assert!(matches!(
        reopened
            .read_agent_message(&session, &MessageId::new("wake").unwrap())
            .await
            .unwrap()
            .unwrap()
            .state,
        rsi_agent_store_protocol::StoreAgentMessageState::Claimed {
            claimed_fact_seq: 37,
            ..
        }
    ));
}
