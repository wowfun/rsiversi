use super::*;
use rsi_agent_session_protocol::{
    QueueMutation, QueueMutationOutcome, QueueMutationReceipt, QueueMutationRequest,
    QueueOperationId, QueueSlotId,
};
fn request() -> QueueMutationRequest {
    QueueMutationRequest {
        operation_id: QueueOperationId::new("edit").unwrap(),
        slot_id: QueueSlotId::new("original").unwrap(),
        expected_message_id: MessageId::new("original").unwrap(),
        mutation: QueueMutation::Replace {
            new_message_id: MessageId::new("successor").unwrap(),
            content: vec![AgentMessageContent::Text {
                text: "complete replacement".into(),
            }],
        },
    }
}
fn receipt() -> QueueMutationReceipt {
    let request = request();
    QueueMutationReceipt {
        operation_id: request.operation_id.clone(),
        request_fingerprint: request.fingerprint().unwrap(),
        slot_id: request.slot_id,
        expected_message_id: request.expected_message_id,
        control_seq: 4,
        outcome: QueueMutationOutcome::Replaced {
            message_id: MessageId::new("successor").unwrap(),
            accepted_control_seq: 3,
        },
    }
}

#[tokio::test]
async fn queue_mutation_rejects_foreign_or_malformed_replies_without_implicit_replay() {
    for change in 0..9 {
        let (remote, _, handle) = fixture().await;
        let mut reply = envelope(serde_json::to_value(receipt()).unwrap());
        match change {
            0 => reply["target"]["session_id"] = json!("foreign"),
            1 => reply["target"]["header_key"] = json!("b".repeat(64)),
            2 => reply["body"]["operation_id"] = json!("foreign"),
            3 => reply["body"]["request_fingerprint"] = json!("b".repeat(64)),
            4 => reply["body"]["expected_message_id"] = json!("foreign"),
            5 => reply["body"]["slot_id"] = json!("foreign"),
            6 => reply["body"]["outcome"]["message_id"] = json!("foreign"),
            7 => reply["body"]["outcome"] = json!({"result":"withdrawn"}),
            8 => reply["body"]["extra"] = json!(true),
            _ => unreachable!(),
        }
        remote.reply(&reply);
        let before = remote.calls.load(Ordering::SeqCst);
        assert!(
            matches!(handle.mutate_queue(request()).await,Err(SessionError::QueueOutcomeUnknown {operation_id}) if operation_id.as_str()=="edit"),
            "case {change}"
        );
        assert_eq!(remote.calls.load(Ordering::SeqCst), before + 1);
        remote.reply(&envelope(json!(receipt())));
        assert_eq!(
            handle
                .queue_mutation_status(&request().operation_id)
                .await
                .unwrap(),
            Some(receipt())
        );
    }
}

#[tokio::test]
async fn queue_status_absence_is_read_only_and_success_binds_the_exact_request() {
    let (remote, _, handle) = fixture().await;
    remote.reply(&envelope(Value::Null));
    assert!(
        handle
            .queue_mutation_status(&request().operation_id)
            .await
            .unwrap()
            .is_none()
    );
    remote.reply(&envelope(json!(receipt())));
    assert_eq!(handle.mutate_queue(request()).await.unwrap(), receipt());
}
