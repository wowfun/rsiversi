//! Shared Session behavioral assertions for native and browser adapters.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    AgentControlRecordBody, MessageId, SessionFactBody, SessionId, TurnId,
};
use rsi_agent_turn_protocol::{
    MessageReceipt, MessageState, ObservationCursor, SessionObservation, SessionObservationStream,
};
use rsi_meta_execution::Execution;
use rsi_session_protocol::{
    CreateSession, SessionError, SessionInput, SessionService, SubmitInput,
};
use std::sync::Arc;

/// Waits for the exact receipt's durable claim using the supplied clock.
///
/// # Panics
/// Panics when the fixture cannot produce a matching durable claim within ten seconds.
pub async fn observe_message_claim(
    handle: &Arc<dyn rsi_session_protocol::SessionHandle>,
    receipt: &MessageReceipt,
    execution: &Execution,
) -> (TurnId, u64) {
    let mut observation: SessionObservationStream = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: receipt.observed_fact_seq,
        })
        .await
        .unwrap();
    execution
        .deadline_after(std::time::Duration::from_secs(10))
        .timeout(async {
            loop {
                if let SessionObservation::Control { record, .. } =
                    observation.next().await.unwrap().unwrap()
                    && let AgentControlRecordBody::MessageClaimed {
                        message_id,
                        turn_id,
                        entered_fact_seq,
                        ..
                    } = record.body()
                    && message_id == &receipt.message_id
                {
                    return (turn_id.clone(), *entered_fact_seq);
                }
            }
        })
        .await
        .expect("message reached its durable claim boundary")
}

/// Exercises one complete Session domain scenario against an isolated adapter.
///
/// # Panics
/// Panics when an adapter violates the Session contract or the fixture does not complete.
#[allow(clippy::too_many_lines)] // One ordered scenario preserves shared preconditions across adapters.
pub async fn assert_session_contract(
    application: Arc<dyn SessionService>,
    create: CreateSession,
    canonical_cwd: &str,
    execution: &Execution,
) {
    let session_id = create.session_id.clone();
    let session = session_id.as_str().to_owned();
    let handle = application.create(create.clone()).await.unwrap();
    let header = handle.header().await.unwrap();
    assert_eq!(header.session_id(), &session_id);
    assert_eq!(header.canonical_cwd(), canonical_cwd);
    assert!(
        handle
            .history_before(None, 8)
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    assert!(matches!(
        handle.history_before(None, 0).await,
        Err(SessionError::Invalid(_))
    ));

    let message_id = MessageId::new(format!("{session}-message")).unwrap();
    let submission = SubmitInput {
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        message_id: message_id.clone(),
        content: vec![SessionInput::Text {
            text: "hello contract".into(),
        }],
        model: None,
        sandbox: None,
    };
    let first = handle.submit(submission.clone()).await.unwrap();
    let message = handle
        .read_message(&message_id, first.accepted_control_seq)
        .await
        .unwrap();
    assert_eq!(message.message_id, message_id);
    assert_eq!(
        message.content,
        vec![rsi_agent_session_protocol::AgentMessageContent::Text {
            text: "hello contract".into()
        }]
    );
    assert!(handle.read_message(&message_id, 0).await.is_err());
    assert!(
        handle
            .read_message(
                &MessageId::new("wrong-message").unwrap(),
                first.accepted_control_seq
            )
            .await
            .is_err()
    );
    assert!(
        handle
            .read_message(&message_id, first.accepted_control_seq + 1)
            .await
            .is_err()
    );
    let retried = handle.submit(submission.clone()).await.unwrap();
    assert_eq!(first.session_id, retried.session_id);
    assert_eq!(first.message_id, retried.message_id);
    assert_eq!(first.accepted_control_seq, retried.accepted_control_seq);
    assert!(retried.observed_fact_seq >= first.observed_fact_seq);
    if first.state != MessageState::Pending {
        assert_eq!(first.state, retried.state);
    }
    assert!(matches!(
        handle
            .submit(SubmitInput {
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
                message_id: message_id.clone(),
                content: vec![SessionInput::Text {
                    text: "changed body".into(),
                }],
                model: None,
                sandbox: None,
            })
            .await,
        Err(SessionError::MessageConflict { .. })
    ));

    let (turn_id, entered_fact_seq) = observe_message_claim(&handle, &first, execution).await;
    let claimed = handle.message_status(&message_id).await.unwrap();
    assert_eq!(claimed.session_id, first.session_id);
    assert_eq!(claimed.message_id, first.message_id);
    assert_eq!(claimed.accepted_control_seq, first.accepted_control_seq);
    assert!(matches!(
        &claimed.state,
        MessageState::Claimed { turn_id: observed, entered_fact_seq: seq, .. }
            if observed == &turn_id && *seq == entered_fact_seq
    ));
    let claimed_retry = handle.submit(submission).await.unwrap();
    assert_eq!(claimed_retry.state, claimed.state);
    assert_eq!(
        claimed_retry.accepted_control_seq,
        claimed.accepted_control_seq
    );
    assert!(claimed_retry.observed_fact_seq >= claimed.observed_fact_seq);
    let mut observation = handle
        .observe(ObservationCursor {
            control_seq: first.accepted_control_seq,
            fact_seq: entered_fact_seq,
        })
        .await
        .unwrap();

    execution.deadline_after(std::time::Duration::from_secs(10)).timeout(async {
        loop {
            let update = observation.next().await.unwrap().unwrap();
            if matches!(
                update,
                SessionObservation::Fact { ref fact, .. }
                    if matches!(fact.body(), SessionFactBody::TurnTerminal { turn_id: observed, .. } if observed == &turn_id)
            ) {
                break;
            }
        }
    })
    .await
    .expect("turn reached a durable terminal Fact");

    let history = handle.history_before(None, 64).await.unwrap();
    assert!(history.durable_seq >= entered_fact_seq);
    assert!(history.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::TurnTerminal { turn_id: observed, .. } if observed == &turn_id
    )));
    let attached = application.attach(&session_id).await.unwrap();
    assert_eq!(attached.header().await.unwrap(), header);
    assert!(
        application
            .list_recent(None, 64)
            .await
            .unwrap()
            .sessions
            .iter()
            .any(|summary| summary.header.session_id() == &session_id)
    );
    assert!(matches!(
        application
            .attach(&SessionId::new(format!("{session}-missing")).unwrap())
            .await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        application
            .create(create)
            .await,
        Err(SessionError::Invalid(message))
            if message.contains("already exists") && message.contains("durable Store")
    ));
}
