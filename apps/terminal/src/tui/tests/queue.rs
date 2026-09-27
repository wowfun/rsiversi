use super::*;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent as Content, AgentMessageSource, AgentMessageSourceKind,
    MessageOptions, MessageTarget, QueueMutation, QueueSlot,
};
use rsi_agent_store_protocol::StorePendingMessage;

#[tokio::test]
// Keep the unknown-outcome and explicit-retry sequence in one lifetime assertion.
#[allow(clippy::too_many_lines)]
async fn modal_queue_editor_preserves_complete_content_and_unknown_identity_after_handle_switch() {
    let (mut client, handle, runtime, surface) = client().await;
    client
        .state
        .editor
        .insert("ordinary untouched composer")
        .unwrap();
    let reference = references::frozen(&client.state.header);
    client.action(Action::AddReference(reference.clone()));
    let image:Content=serde_json::from_value(serde_json::json!({"type":"image","media":{"id":"d".repeat(64),"mime":"image/png","bytes":72,"width":1,"height":1}})).unwrap();
    let message = AgentMessage {
        message_id: MessageId::new("old-queued").unwrap(),
        source: AgentMessageSource::Human,
        content: vec![
            Content::Text {
                text: "first".into(),
            },
            image,
            Content::Reference {
                reference: reference.clone(),
            },
            Content::Text {
                text: "last".into(),
            },
        ],
        options: MessageOptions::default(),
    };
    let selected = StorePendingMessage {
        queue_slot: QueueSlot::initial(&message.message_id, 1, 1),
        message_id: message.message_id.clone(),
        source_kind: AgentMessageSourceKind::Human,
        has_turn_options: false,
        delivery: MessageDelivery::NextTurn,
        target: MessageTarget::NextTurn,
        permits_promotion: false,
        bound_turn_id: None,
        accepted_control_seq: 1,
    };
    client.presented_turn = Some(TurnId::new("acknowledged-turn").unwrap());
    client.state.live_turn = Some(TurnId::new("not-rendered-turn").unwrap());
    client.queue_message_menu(selected.clone());
    assert!(client.state.menu.as_ref().unwrap().items.iter().any(|(_,action)|matches!(action,Action::MutateQueue(_,QueueMutation::ConvertToSteer {expected_turn_id,..}) if expected_turn_id.as_str()=="acknowledged-turn")));
    // Dispose the independent detail read before opening the selected content.
    client.tasks.clear();
    client.edit_queue_block(selected, Arc::new(message.clone()), 0);
    client.state.queue_edit.as_mut().unwrap().editor = editor::Editor::with_text(
        "edited first".into(),
        rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES,
    );
    assert!(client.queue_key(KeyCode::Enter.into()));
    let Update::QueueSettled(operation, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("queue response")
    };
    assert!(matches!(
        result,
        Err(SessionError::QueueOutcomeUnknown { .. })
    ));
    client.queue_settled(&operation, result, initial);
    assert!(client.queue_pending.is_some());
    let frozen = handle.queue_requests.lock().unwrap()[0].clone();
    let QueueMutation::Replace { content, .. } = &frozen.mutation else {
        panic!("replace")
    };
    assert_eq!(&content[1..], &message.content[1..]);
    assert_eq!(
        content[0],
        Content::Text {
            text: "edited first".into()
        }
    );
    assert_eq!(client.state.editor.text(), "ordinary untouched composer");
    assert_eq!(client.state.references, [reference]);
    let foreign = Arc::new(UnknownThenAcceptedHandle::default());
    client.handle = foreign.clone();
    // An absent status retains the request; only explicit retry replays the identical envelope.
    *handle.queue_receipt.lock().unwrap() = None;
    client.run_queue(true, false);
    let Update::QueueSettled(op, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("query")
    };
    assert!(result.as_ref().unwrap().is_none());
    client.queue_settled(&op, result, initial);
    assert!(client.queue_pending.is_some());
    assert_eq!(handle.queue_requests.lock().unwrap().len(), 1);
    client.run_queue(true, true);
    let Update::QueueSettled(op, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("retry")
    };
    assert_eq!(op, operation);
    assert!(result.as_ref().unwrap().is_some());
    client.queue_settled(&op, result, initial);
    assert_eq!(
        *handle.queue_requests.lock().unwrap(),
        [frozen.clone(), frozen]
    );
    assert!(foreign.queue_requests.lock().unwrap().is_empty());
    assert!(client.queue_pending.is_none());
    assert_eq!(client.state.editor.text(), "ordinary untouched composer");
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

fn pending_message(source_kind: AgentMessageSourceKind) -> StorePendingMessage {
    let message_id = MessageId::new("pending-fixture").unwrap();
    StorePendingMessage {
        queue_slot: QueueSlot::initial(&message_id, 1, 1),
        message_id,
        source_kind,
        has_turn_options: false,
        delivery: MessageDelivery::NextTurn,
        target: MessageTarget::NextTurn,
        permits_promotion: false,
        bound_turn_id: None,
        accepted_control_seq: 1,
    }
}

#[tokio::test]
async fn unresolved_queue_operation_fences_attachment_until_receipt_settles() {
    let (mut client, handle, runtime, surface) = client().await;
    assert!(client.can_switch_session());
    assert!(client.begin_queue_mutation(
        pending_message(AgentMessageSourceKind::Human),
        QueueMutation::Withdraw
    ));
    assert!(
        !client.can_switch_session(),
        "unpolled request cannot leave its attachment"
    );
    let Update::QueueSettled(op, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("receipt")
    };
    client.queue_settled(&op, result, initial);
    assert!(
        !client.can_switch_session(),
        "unknown outcome retains its original attachment"
    );
    client.run_queue(true, false);
    let Update::QueueSettled(op, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("reconcile")
    };
    assert!(result.as_ref().unwrap().is_some());
    client.queue_settled(&op, result, initial);
    assert!(client.can_switch_session());
    assert_eq!(handle.queue_requests.lock().unwrap().len(), 1);
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn dropping_unpolled_queue_work_releases_busy_for_explicit_reconciliation() {
    let (mut client, handle, runtime, surface) = client().await;
    assert!(client.begin_queue_mutation(
        pending_message(AgentMessageSourceKind::Human),
        QueueMutation::Withdraw
    ));
    client.tasks.clear();
    assert!(handle.queue_requests.lock().unwrap().is_empty());
    client.run_queue(true, false);
    assert!(
        !client.tasks.is_empty(),
        "query must be admitted after an unpolled task is dropped"
    );
    let Update::QueueSettled(op, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("reconcile")
    };
    assert!(result.as_ref().unwrap().is_none());
    client.queue_settled(&op, result, initial);
    client.run_queue(true, true);
    assert!(!client.tasks.is_empty());
    let Update::QueueSettled(op, result, initial) =
        client.tasks.next().await.unwrap().result.unwrap()
    else {
        panic!("retry")
    };
    client.queue_settled(&op, result, initial);
    assert_eq!(handle.queue_requests.lock().unwrap().len(), 1);
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn pending_menu_uses_shared_labels_and_keeps_nonhuman_inputs_readable() {
    let (mut client, _, runtime, surface) = client().await;
    for kind in [
        AgentMessageSourceKind::Human,
        AgentMessageSourceKind::Program,
        AgentMessageSourceKind::Completion,
        AgentMessageSourceKind::Agent,
    ] {
        let message = pending_message(kind);
        let item = rsi_client::QueueItem::new(&message, None);
        client.queue_message_menu(message.clone());
        let menu = client.state.menu.as_ref().unwrap();
        assert!(menu.title.ends_with(item.delivery_label));
        assert_eq!(menu.items[0].0, item.read_label);
        if item.editable {
            assert!(matches!(&menu.items[0].1, Action::EditQueue(selected) if selected==&message));
            assert_eq!(menu.items[1].0, item.withdraw_label.unwrap());
        } else {
            assert_eq!(menu.items.len(), 1);
            assert!(matches!(&menu.items[0].1, Action::ReadQueue(selected) if selected==&message));
        }
        assert!(
            !client.tasks.is_empty(),
            "accepted content remains readable for every source"
        );
        client.tasks.clear();
    }
    client.cancel(None);
    assert!(client.state.status.contains("No current acknowledged Turn"));
    assert!(client.tasks.is_empty());
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn stale_frame_size_presentation_and_attachment_revoke_queue_and_stop_authority() {
    let (mut client, _, runtime, surface) = client().await;
    let mut frame = terminal::RenderedFrame {
        generation: client.generation,
        presentation: 1,
        revision: 1,
        turn: Some(TurnId::new("shown-turn").unwrap()),
        buffer: ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24)),
        view: render::View::default(),
    };
    client.acknowledged_frame(Some(&frame), 1, (80, 24));
    assert_eq!(client.presented_turn, frame.turn);
    for (generation, presentation, size) in [
        (client.generation, 1, (81, 24)),
        (client.generation, 2, (80, 24)),
        (client.generation + 1, 1, (80, 24)),
    ] {
        frame.generation = generation;
        client.acknowledged_frame(Some(&frame), presentation, size);
        assert!(client.presented_turn.is_none());
        client.queue_message_menu(pending_message(AgentMessageSourceKind::Human));
        assert!(
            !client
                .state
                .menu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|(_, action)| matches!(
                    action,
                    Action::MutateQueue(_, QueueMutation::ConvertToSteer { .. })
                ))
        );
    }
    client.acknowledged_frame(None, 1, (80, 24));
    assert!(client.presented_turn.is_none());
    drop(client);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
