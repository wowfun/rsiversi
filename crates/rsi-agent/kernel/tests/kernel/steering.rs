use super::*;
use rsi_agent_session_protocol::MessageDelivery;
use rsi_agent_store_protocol::MailboxRead;
use rsi_agent_store_protocol::StoreAgentMessageState;

fn attributed_message(id: &str) -> AgentMessage {
    let mut message = mailbox_message(id);
    message.source = AgentMessageSource::Human {
        attribution: Some(rsi_agent_session_protocol::MessageAttribution {
            id: format!("原始消息: {id}"),
            source: serde_json::json!({"kind":"plugin:input", "meta":[true, "保留"]}),
        }),
    };
    message
}

async fn submit(
    kernel: &AgentKernel,
    session: &SessionId,
    id: &str,
    delivery: MessageDelivery,
) -> rsi_agent_turn_protocol::MessageReceipt {
    kernel
        .submit_message(SubmitMessage {
            session: resume(kernel, session.clone()).await,
            message: attributed_message(id),
            delivery,
        })
        .await
        .unwrap()
}

#[allow(clippy::too_many_lines)] // Identical terminal/recovery assertions run against both Store backends.
async fn promotion_scenario(store: Arc<dyn SessionStore>, ending: &str) {
    let initial =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = initial.start_workers();
    let session = SessionId::new("steering-root").unwrap();
    let first = initial
        .submit_message(SubmitMessage {
            session: fresh(header(session.as_str())),
            message: mailbox_message("initial"),
            delivery: MessageDelivery::Steer,
        })
        .await
        .unwrap();
    let idle = store
        .read_agent_mailbox(&session, MailboxRead::WithMessage(first.message_id.clone()))
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(idle.target, MessageTarget::NextTurn);
    assert!(idle.bound_turn_id.is_none());
    assert!(matches!(
        initial
            .submit_message(SubmitMessage {
                session: resume(&initial, session.clone()).await,
                message: mailbox_message("initial"),
                delivery: MessageDelivery::NextTurn,
            })
            .await,
        Err(TurnError::MessageConflict { .. })
    ));
    let lease = initial.register("steering-executor".into()).unwrap();
    let claim = initial
        .claim("steering-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    submit(&initial, &session, "before", MessageDelivery::NextTurn).await;
    let steer = submit(&initial, &session, "steer", MessageDelivery::Steer).await;
    submit(&initial, &session, "after", MessageDelivery::NextTurn).await;
    let discarded = submit(&initial, &session, "discarded", MessageDelivery::Steer).await;
    initial
        .cancel_target(
            &session,
            CancelTarget::Message(discarded.message_id.clone()),
            None,
        )
        .await
        .unwrap();
    submit(&initial, &session, "fixed-step", MessageDelivery::NextStep).await;
    let bound = store
        .read_agent_mailbox(&session, MailboxRead::WithMessage(steer.message_id.clone()))
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(bound.target, MessageTarget::NextStep);
    assert_eq!(bound.bound_turn_id.as_ref(), Some(claim.turn_id()));
    assert!(!bound.wake_required);
    assert_eq!(bound.message, attributed_message("steer"));

    if ending == "recovery" {
        initial.shutdown(worker).await.unwrap();
        drop(lease);
    } else {
        let outcome = match ending {
            "completed" => TurnOutcome::Completed,
            "failed" => TurnOutcome::Failed {
                code: "fixture".into(),
                message: "fixture failure".into(),
            },
            "cancelled" => {
                initial
                    .cancel_target(&session, CancelTarget::Turn(claim.turn_id().clone()), None)
                    .await
                    .unwrap();
                TurnOutcome::Cancelled
            }
            _ => unreachable!(),
        };
        initial.finish_turn(&claim, &outcome).await.unwrap();
        initial.shutdown(worker).await.unwrap();
        drop(lease);
    }
    drop(initial);
    let restarted =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let promoted = store
        .read_agent_mailbox(&session, MailboxRead::WithMessage(steer.message_id.clone()))
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(promoted.target, MessageTarget::NextTurn, "ending={ending}");
    assert_eq!(promoted.bound_turn_id, bound.bound_turn_id);
    assert_eq!(promoted.delivery, MessageDelivery::Steer);
    assert_eq!(promoted.message, bound.message);
    let ready = store.list_ready_messages(&session, None, 16).await.unwrap();
    assert_eq!(
        ready
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["before", "steer", "after"]
    );
    assert_eq!(ready.messages[1].control_seq, steer.accepted_control_seq);
    assert_eq!(ready.messages[1].timestamp_ms, bound.accepted_timestamp_ms);
    // Cursor pagination must not reinterpret the promoted entry's identity.
    let remaining = store
        .list_ready_messages(&session, Some(&ready.messages[0].cursor()), 16)
        .await
        .unwrap();
    assert_eq!(remaining.messages[0].message_id, steer.message_id);
    let retry = submit(&restarted, &session, "steer", MessageDelivery::Steer).await;
    assert_eq!(retry.accepted_control_seq, steer.accepted_control_seq);
    assert_eq!(retry.state, MessageState::Pending);
    let fixed = store
        .read_agent_mailbox(
            &session,
            MailboxRead::WithMessage(MessageId::new("fixed-step").unwrap()),
        )
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(fixed.target, MessageTarget::NextStep);
    let discarded = store
        .read_agent_mailbox(
            &session,
            MailboxRead::WithMessage(discarded.message_id.clone()),
        )
        .await
        .unwrap()
        .selected
        .unwrap();
    assert!(matches!(
        discarded.state,
        StoreAgentMessageState::Discarded { .. }
    ));
    let worker = restarted.start_workers();
    restarted.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn human_steering_promotion_and_retry_preserve_fifo_on_memory_and_sqlite() {
    for sqlite in [false, true] {
        for ending in ["completed", "failed", "cancelled", "recovery"] {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("store");
            let store: Arc<dyn SessionStore> = if sqlite {
                Arc::new(rsi_agent_store_sqlite::SqliteStore::open(&path).unwrap())
            } else {
                Arc::new(MemoryStore::new())
            };
            promotion_scenario(store, ending).await;
            if sqlite {
                rsi_agent_store_sqlite::SqliteStore::verify(&path).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn consumed_steer_retries_retain_the_original_turn_and_conflicting_text_is_rejected() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let session = SessionId::new("consumed-steering").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session.as_str())),
            message: mailbox_message("initial"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("steering-executor".into()).unwrap();
    let claim = kernel
        .claim("steering-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let queued = submit(&kernel, &session, "queued", MessageDelivery::NextTurn).await;
    let accepted = submit(&kernel, &session, "steer", MessageDelivery::Steer).await;
    assert_eq!(kernel.claim_pending_step_messages(&claim).await.unwrap(), 1);
    assert!(matches!(
        store
            .read_agent_message(&session, &queued.message_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        StoreAgentMessageState::Pending
    ));
    let page = kernel.read_facts(&claim, 0, 64).await.unwrap();
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageClaimed {
            source: rsi_agent_session_protocol::InputMessageSource::Human { message_id, .. },
            content, ..
        } if message_id == &accepted.message_id && content == &attributed_message("steer").content
    )));
    let retried = submit(&kernel, &session, "steer", MessageDelivery::Steer).await;
    assert_eq!(retried.accepted_control_seq, accepted.accepted_control_seq);
    assert!(
        matches!(retried.state, MessageState::Claimed { ref turn_id, .. } if turn_id == claim.turn_id())
    );
    let mut changed = attributed_message("steer");
    changed.content = vec![AgentMessageContent::Text {
        text: "different text".into(),
    }];
    assert!(matches!(
        kernel
            .submit_message(SubmitMessage {
                session: resume(&kernel, session.clone()).await,
                message: changed,
                delivery: MessageDelivery::Steer,
            })
            .await,
        Err(TurnError::MessageConflict { .. })
    ));
    let mut changed = attributed_message("steer");
    let AgentMessageSource::Human {
        attribution: Some(attribution),
    } = &mut changed.source
    else {
        unreachable!()
    };
    attribution.source = serde_json::json!({"kind":"different"});
    assert!(matches!(
        kernel
            .submit_message(SubmitMessage {
                session: resume(&kernel, session.clone()).await,
                message: changed,
                delivery: MessageDelivery::Steer,
            })
            .await,
        Err(TurnError::MessageConflict { .. })
    ));
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let ready = store.list_ready_messages(&session, None, 8).await.unwrap();
    assert_eq!(ready.messages.len(), 1);
    assert_eq!(ready.messages[0].message_id, queued.message_id);
    kernel.shutdown(worker).await.unwrap();
}
