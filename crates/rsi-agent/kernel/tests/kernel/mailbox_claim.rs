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
async fn mailbox_initial_claim_cannot_omit_its_waking_input_at_a_page_boundary() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    seed(store.clone(), 2, true).await;
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    let session = SessionId::new("claim-order").unwrap();
    let before = store.read_watermarks(&session).await.unwrap();
    // Model a valid byte-bounded prefix without allocating tens of megabytes.
    for limit in [1, 2] {
        store.mailbox.prefix_limit.store(limit, Ordering::Relaxed);
        assert!(matches!(
            claim_batch(&kernel).await,
            Err(TurnError::Capacity)
        ));
        assert_eq!(store.read_watermarks(&session).await.unwrap(), before);
        assert_eq!(
            kernel
                .message_status(&session, &MessageId::new("wake").unwrap())
                .await
                .unwrap()
                .state,
            MessageState::Pending
        );
    }
    kernel.shutdown(worker).await.unwrap();
}
