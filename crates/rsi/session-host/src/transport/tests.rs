use super::client::{
    ObservationStreamContract, SequenceContract, admit_sequence_item, decode_message_receipt,
    decode_subscription_frame,
};
use super::server::{
    peer_belongs_to_effective_user, read_message_uploads, validate_wire_operation,
};

use super::*;

fn test_fact() -> SessionFact {
    SessionFact::new(
        1,
        1,
        rsi_agent_session_protocol::SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("turn-one").unwrap(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn internally_tagged_wire_enums_reject_unknown_fields() {
    let client_body = br#"{"type":"request","request_id":"request-unknown-field","operation":{"type":"probe","extra":true}}"#;
    let client_budget = FrameReadBudget::new(client_body.len());
    let (mut client_writer, mut client_reader) = tokio::io::duplex(128);
    client_writer
        .write_u32(u32::try_from(client_body.len()).unwrap())
        .await
        .unwrap();
    client_writer.write_all(client_body).await.unwrap();

    assert!(matches!(
        read_frame::<_, ClientFrame>(&mut client_reader, client_body.len(), &client_budget).await,
        Err(SessionHostError::Invalid(message)) if message.contains("extra")
    ));

    let server_body = br#"{"type":"response","request_id":"request-unknown-field","response":{"type":"ready","extra":true},"error":null}"#;
    let server_budget = FrameReadBudget::new(server_body.len());
    let (mut server_writer, mut server_reader) = tokio::io::duplex(128);
    server_writer
        .write_u32(u32::try_from(server_body.len()).unwrap())
        .await
        .unwrap();
    server_writer.write_all(server_body).await.unwrap();

    assert!(matches!(
        read_frame::<_, ServerFrame>(&mut server_reader, server_body.len(), &server_budget).await,
        Err(SessionHostError::Invalid(message)) if message.contains("extra")
    ));
}

#[test]
fn peer_admission_rejects_a_foreign_effective_user() {
    assert!(peer_belongs_to_effective_user(1000, 1000));
    assert!(!peer_belongs_to_effective_user(1001, 1000));
}

#[test]
fn wire_operation_bounds_cancellation_reason_and_approval_id() {
    let session_id = SessionId::new("session-wire-bound").unwrap();
    assert!(
        validate_wire_operation(&WireOperation::Cancel {
            session_id: session_id.clone(),
            target: WireCancelTarget::Turn {
                turn_id: TurnId::new("turn-wire-bound").unwrap(),
            },
            reason: Some("x".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES + 1)),
        })
        .is_err()
    );
    assert!(
        validate_wire_operation(&WireOperation::AnswerApproval {
            session_id,
            approval_id: "x".repeat(MAXIMUM_APPROVAL_FIELD_BYTES + 1),
            decision: ApprovalDecision::AllowOnce,
        })
        .is_err()
    );
}

#[test]
fn wire_operation_bounds_message_content_before_reading_uploads() {
    let operation = WireOperation::SubmitInput {
        session_id: SessionId::new("session-content-bound").unwrap(),
        message_id: MessageId::new("message-content-bound").unwrap(),
        content: vec![
            WireInputBlock::Text { text: "x".into() };
            MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS + 1
        ],
        model: None,
        sandbox: None,
    };
    assert!(matches!(
        validate_wire_operation(&operation),
        Err(SessionHostError::Invalid(message)) if message.contains("content blocks")
    ));
}

#[tokio::test]
async fn frame_budget_is_acquired_before_reading_the_declared_body() {
    let budget = FrameReadBudget::new(4);
    let (mut first_writer, mut first_reader) = tokio::io::duplex(32);
    let (mut second_writer, mut second_reader) = tokio::io::duplex(32);
    first_writer.write_u32(4).await.unwrap();
    second_writer.write_u32(4).await.unwrap();
    second_writer.write_all(b"null").await.unwrap();

    let first = tokio::spawn({
        let budget = budget.clone();
        async move { read_frame::<_, Option<()>>(&mut first_reader, 4, &budget).await }
    });
    tokio::task::yield_now().await;
    let second = tokio::spawn({
        let budget = budget.clone();
        async move { read_frame::<_, Option<()>>(&mut second_reader, 4, &budget).await }
    });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    first_writer.write_all(b"null").await.unwrap();
    assert_eq!(first.await.unwrap().unwrap(), None);
    assert_eq!(second.await.unwrap().unwrap(), None);
}

#[tokio::test]
async fn frame_budget_is_released_after_decode_error_and_read_timeout() {
    let budget = FrameReadBudget::new(4);

    let (mut invalid_writer, mut invalid_reader) = tokio::io::duplex(32);
    invalid_writer.write_u32(4).await.unwrap();
    invalid_writer.write_all(b"xxxx").await.unwrap();
    assert!(
        read_frame::<_, Option<()>>(&mut invalid_reader, 4, &budget)
            .await
            .is_err()
    );

    let (mut stalled_writer, mut stalled_reader) = tokio::io::duplex(32);
    stalled_writer.write_u32(4).await.unwrap();
    assert!(
        read_frame_with_timeout::<_, Option<()>>(
            &mut stalled_reader,
            4,
            &budget,
            Duration::from_millis(1),
            "test",
        )
        .await
        .is_err()
    );

    let (mut valid_writer, mut valid_reader) = tokio::io::duplex(32);
    valid_writer.write_u32(4).await.unwrap();
    valid_writer.write_all(b"null").await.unwrap();
    let decoded = tokio::time::timeout(
        Duration::from_secs(1),
        read_frame::<_, Option<()>>(&mut valid_reader, 4, &budget),
    )
    .await
    .expect("failed reads retained the complete frame ledger")
    .unwrap();
    assert_eq!(decoded, None);
}

#[tokio::test]
async fn decoded_request_can_retain_its_frame_budget_through_dispatch() {
    let budget = FrameReadBudget::new(4);
    let (mut first_writer, mut first_reader) = tokio::io::duplex(32);
    first_writer.write_u32(4).await.unwrap();
    first_writer.write_all(b"null").await.unwrap();
    let (decoded, admission) = read_frame_with_retained_budget::<_, Option<()>>(
        &mut first_reader,
        4,
        &budget,
        Duration::from_secs(1),
        "test request",
    )
    .await
    .unwrap();
    assert_eq!(decoded, None);

    let blocked = tokio::time::timeout(Duration::from_millis(1), budget.acquire(4)).await;
    assert!(blocked.is_err());
    drop(admission);
    let _released = tokio::time::timeout(Duration::from_secs(1), budget.acquire(4))
        .await
        .expect("dispatch completion must release the decoded request budget")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn upload_uses_one_absolute_deadline_across_progressing_frames() {
    let request_id = "request-upload-deadline";
    let body = vec![b'a'; MAXIMUM_UPLOAD_CHUNK_BYTES + 1];
    let operation = WireOperation::SubmitInput {
        session_id: SessionId::new("session-upload-deadline").unwrap(),
        message_id: MessageId::new("message-upload-deadline").unwrap(),
        content: vec![WireInputBlock::Image {
            upload_id: 0,
            bytes: u64::try_from(body.len()).unwrap(),
            sha256: hex::encode(sha2::Sha256::digest(&body)),
        }],
        model: None,
        sandbox: None,
    };
    let budget = FrameReadBudget::new(MAXIMUM_UPLOAD_FRAME_BYTES);
    let upload_budget = Arc::new(Semaphore::new(MAXIMUM_SESSION_INPUT_IMAGE_BYTES));
    let (mut writer, mut reader) = tokio::io::duplex(MAXIMUM_UPLOAD_FRAME_BYTES);
    let read = tokio::spawn({
        let budget = budget.clone();
        let upload_budget = Arc::clone(&upload_budget);
        async move {
            read_message_uploads(&mut reader, request_id, &operation, &budget, &upload_budget).await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(UPLOAD_READ_TIMEOUT.saturating_sub(Duration::from_secs(1))).await;
    write_frame(
        &mut writer,
        &ClientFrame::UploadChunk {
            request_id: request_id.into(),
            upload_id: 0,
            index: 0,
            data: base64::engine::general_purpose::STANDARD
                .encode(vec![b'a'; MAXIMUM_UPLOAD_CHUNK_BYTES]),
        },
    )
    .await
    .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;

    assert!(
        read.is_finished(),
        "frame progress reset the upload deadline"
    );
    assert!(matches!(
        read.await.unwrap(),
        Err(SessionHostError::Io(message)) if message.contains("image upload timed out")
    ));
}

#[tokio::test(start_paused = true)]
async fn subscription_frame_body_is_bounded_after_its_length_arrives() {
    let budget = FrameReadBudget::new(4);
    let (mut writer, mut reader) = tokio::io::duplex(32);
    writer.write_u32(4).await.unwrap();
    let read_budget = budget.clone();
    let read = tokio::spawn(async move {
        read_subscription_frame::<_, Option<()>>(&mut reader, 4, &read_budget).await
    });

    tokio::task::yield_now().await;
    tokio::time::advance(RESPONSE_READ_TIMEOUT + Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(
        read.is_finished(),
        "a declared subscription frame retained decoder admission without a deadline"
    );
    assert!(matches!(
        read.await.unwrap(),
        Err(SessionHostError::Io(message)) if message.contains("subscription frame body read timed out")
    ));

    let (mut valid_writer, mut valid_reader) = tokio::io::duplex(32);
    valid_writer.write_u32(4).await.unwrap();
    valid_writer.write_all(b"null").await.unwrap();
    assert_eq!(
        read_subscription_frame::<_, Option<()>>(&mut valid_reader, 4, &budget)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn subscription_idle_wait_starts_no_body_deadline() {
    let budget = FrameReadBudget::new(4);
    let (mut writer, mut reader) = tokio::io::duplex(32);
    let read = tokio::spawn(async move {
        read_subscription_frame::<_, Option<()>>(&mut reader, 4, &budget).await
    });

    tokio::time::advance(RESPONSE_READ_TIMEOUT + Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(!read.is_finished());
    writer.write_u32(4).await.unwrap();
    writer.write_all(b"null").await.unwrap();
    assert_eq!(read.await.unwrap().unwrap(), None);
}

#[test]
fn history_sequence_rejects_a_fact_for_another_session() {
    let expected = SessionId::new("expected-session").unwrap();
    let contract = SequenceContract::History {
        session_id: expected,
        limit: 1,
    };
    let item = WireItem::Fact {
        session_id: SessionId::new("other-session").unwrap(),
        fact: test_fact(),
    };

    assert!(contract.admit_item(0, 0, &item).is_err());
}

#[test]
fn subscription_rejects_an_event_for_another_session() {
    let expected = SessionId::new("expected-session").unwrap();
    let frame = ServerFrame::Event {
        request_id: "request-one".into(),
        session_id: SessionId::new("other-session").unwrap(),
        update: WireUpdate::Fact {
            fact: Box::new(test_fact()),
            durable_fact_seq: 1,
        },
    };

    assert!(decode_subscription_frame(frame, "request-one", &expected).is_err());
}

#[test]
fn message_receipt_rejects_zero_and_reversed_sequence_contracts() {
    let session_id = SessionId::new("session-receipt-sequence").unwrap();
    let message_id = MessageId::new("message-receipt-sequence").unwrap();
    assert!(
        decode_message_receipt(
            WireResponse::MessageReceipt {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                accepted_control_seq: 0,
                observed_fact_seq: 0,
                state: WireMessageState::Pending,
            },
            &session_id,
            &message_id,
        )
        .is_err()
    );
    assert!(
        decode_message_receipt(
            WireResponse::MessageReceipt {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                accepted_control_seq: 4,
                observed_fact_seq: 0,
                state: WireMessageState::Discarded {
                    reason: rsi_agent_session_protocol::MessageDiscardReason::Cancelled,
                    control_seq: 3,
                },
            },
            &session_id,
            &message_id,
        )
        .is_err()
    );
    assert!(
        decode_message_receipt(
            WireResponse::MessageReceipt {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                accepted_control_seq: 1,
                observed_fact_seq: 4,
                state: WireMessageState::Claimed {
                    activation_id: rsi_agent_session_protocol::ActivationId::new(
                        "activation-receipt-sequence",
                    )
                    .unwrap(),
                    turn_id: TurnId::new("turn-receipt-sequence").unwrap(),
                    step_id: rsi_agent_session_protocol::StepId::new("step-receipt-sequence",)
                        .unwrap(),
                    entered_fact_seq: 5,
                },
            },
            &session_id,
            &message_id,
        )
        .is_err()
    );
}

#[test]
fn subscription_contract_rejects_skipped_and_regressing_fact_updates() {
    let fact = test_fact();
    let mut skipped = ObservationStreamContract::new(ObservationCursor {
        control_seq: 0,
        fact_seq: 1,
    });
    assert!(
        skipped
            .admit(&SessionObservation::Fact {
                fact: Arc::new(fact.clone()),
                durable_fact_seq: 1,
            })
            .is_err()
    );

    let mut regressing = ObservationStreamContract::new(ObservationCursor::default());
    regressing
        .admit(&SessionObservation::Fact {
            fact: Arc::new(fact),
            durable_fact_seq: 2,
        })
        .unwrap();
    let second = SessionFact::new(
        2,
        2,
        rsi_agent_session_protocol::SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("turn-one").unwrap(),
            outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
        },
    )
    .unwrap();
    assert!(
        regressing
            .admit(&SessionObservation::Fact {
                fact: Arc::new(second),
                durable_fact_seq: 1,
            })
            .is_err()
    );
}

#[test]
fn sequence_item_count_is_bounded_independently_of_frame_size() {
    assert!(admit_sequence_item(MAXIMUM_SEQUENCE_ITEMS - 1).is_ok());
    assert!(matches!(
        admit_sequence_item(MAXIMUM_SEQUENCE_ITEMS),
        Err(SessionApplicationError::Backend(message))
            if message.contains("1024-item bound")
    ));
}

#[tokio::test]
async fn upload_rejects_short_nonfinal_chunks_before_admission() {
    let operation = WireOperation::SubmitInput {
        session_id: SessionId::new("chunk-session").unwrap(),
        message_id: MessageId::new("chunk-message").unwrap(),
        content: vec![WireInputBlock::Image {
            upload_id: 0,
            bytes: 2,
            sha256: hex::encode(sha2::Sha256::digest(b"ab")),
        }],
        model: None,
        sandbox: None,
    };
    let (mut writer, mut reader) = tokio::io::duplex(MAXIMUM_UPLOAD_FRAME_BYTES);
    for (index, byte) in [b"a", b"b"].into_iter().enumerate() {
        write_frame(
            &mut writer,
            &ClientFrame::UploadChunk {
                request_id: "chunks".into(),
                upload_id: 0,
                index: u32::try_from(index).unwrap(),
                data: base64::engine::general_purpose::STANDARD.encode(byte),
            },
        )
        .await
        .unwrap();
    }
    write_frame(
        &mut writer,
        &ClientFrame::UploadEnd {
            request_id: "chunks".into(),
        },
    )
    .await
    .unwrap();
    let result = read_message_uploads(
        &mut reader,
        "chunks",
        &operation,
        &FrameReadBudget::new(MAXIMUM_UPLOAD_FRAME_BYTES),
        &Arc::new(Semaphore::new(MAXIMUM_SESSION_INPUT_IMAGE_BYTES)),
    )
    .await;
    assert!(
        matches!(result, Err(SessionHostError::Invalid(message)) if message.contains("chunk length"))
    );
}
