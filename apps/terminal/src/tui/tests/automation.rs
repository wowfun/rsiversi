use super::*;
use rsi_api_protocol::{
    ApiClient, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId, HostEpoch,
    OperationClass, OperationSpec, RetainedBytes,
};
use serde_json::json;
use std::sync::Mutex;
#[derive(Debug)]
struct Controls {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    calls: Mutex<Vec<String>>,
    release: tokio::sync::Semaphore,
}
#[async_trait::async_trait]
impl ApiClient for Controls {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        spec: &OperationSpec,
        _: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        self.calls.lock().unwrap().push(spec.id.name().into());
        if !matches!(spec.id.name(), "resume" | "list") {
            return Err(rsi_api_protocol::ApiError::Unavailable);
        }
        self.release.acquire().await.unwrap().forget();
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default().encode(
                &if spec.id.name() == "list" {
                    json!({"entries":[],"after":"0","watermark":"0","more":false})
                } else {
                    json!({"id":"2","state":"queued"})
                },
                1024,
            )?,
            binary: None,
        }))
    }
}
#[tokio::test]
async fn dismissed_control_survives_new_actions_and_attachment_until_its_receipt() {
    let (mut client, _, runtime, surface) = client().await;
    let api = Arc::new(Controls {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: rsi_automation_api::operations(),
        calls: Mutex::new(vec![]),
        release: tokio::sync::Semaphore::new(0),
    });
    client.automation = Some(rsi_automation_api::Client::new(api.clone()).unwrap());
    client.automation_request(rsi_automation_api::Request::Resume {
        id: "1".into(),
        request_id: "retained-control".into(),
        rule_revision: "1".into(),
    });
    {
        let mut work = Box::pin(client.tasks.next());
        assert!(futures_util::poll!(&mut work).is_pending());
    }
    assert_eq!(*api.calls.lock().unwrap(), ["resume"]);
    client.state.escape();
    client.automation_request(rsi_automation_api::Request::Status);
    assert_eq!(
        client.tasks.len(),
        1,
        "a second action must not overwrite the control waiter"
    );
    client.reset_attachment_tasks();
    assert_eq!(
        client.tasks.len(),
        1,
        "attachment cleanup must retain the control waiter"
    );
    api.release.add_permits(1);
    let work = client.tasks.next().await.unwrap();
    assert!(!work.superseded(&client));
    let Update::Automation(stop, result) = work.result.unwrap() else {
        panic!("control response")
    };
    assert!(
        stop.is_cancelled(),
        "late result must not reopen the dismissed menu"
    );
    let value = result.unwrap();
    assert_eq!(value["mutation_receipt"]["id"], "2");
    assert_eq!(value["id"], "2");
    assert!(value["read_error"].is_string());
    assert!(
        value["browser_readiness"].is_null(),
        "a failed status read must not hide the receipt"
    );
    assert_eq!(*api.calls.lock().unwrap(), ["resume", "get", "status"]);
    assert!(super::super::automation::actions(&value).iter().any(|(_, action)| matches!(action,Action::Automation(request) if matches!(request.as_ref(),rsi_automation_api::Request::Get{id} if id == "2"))));
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn opening_unrelated_detail_retires_the_inflight_automation_read() {
    let (mut client, _, runtime, surface) = client().await;
    let api = Arc::new(Controls {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: rsi_automation_api::operations(),
        calls: Mutex::new(vec![]),
        release: tokio::sync::Semaphore::new(0),
    });
    client.automation = Some(rsi_automation_api::Client::new(api.clone()).unwrap());
    client.automation_request(super::super::automation::list());
    {
        let mut work = Box::pin(client.tasks.next());
        assert!(futures_util::poll!(&mut work).is_pending());
    }
    assert_eq!(*api.calls.lock().unwrap(), ["list"]);
    client.state.open_detail("Other domain evidence".into());
    api.release.add_permits(1);
    let work = client.tasks.next().await.unwrap();
    let Update::Automation(stop, result) = work.result.unwrap() else {
        panic!("read result");
    };
    assert!(stop.is_cancelled());
    assert!(result.is_ok());
    assert_eq!(
        client.state.detail.as_deref(),
        Some("Other domain evidence")
    );
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn protected_queue_has_read_access_without_mutation_or_editor_controls() {
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageSource, AgentMessageSourceKind, QueueMutation, QueueSlot,
        SessionProtectionScope,
    };
    let (mut client, handle, runtime, surface) = client().await;
    client.state.header = client
        .state
        .header
        .clone()
        .with_protection(SessionProtectionScope::new("fixture", "readonly").unwrap())
        .unwrap();
    let message = AgentMessage {
        message_id: MessageId::new("queued-protected").unwrap(),
        source: AgentMessageSource::Human,
        content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
            text: "retained input".into(),
        }],
        options: rsi_agent_session_protocol::MessageOptions::default(),
    };
    let selected = rsi_agent_store_protocol::StorePendingMessage {
        queue_slot: QueueSlot::initial(&message.message_id, 1, 1),
        message_id: message.message_id.clone(),
        source_kind: AgentMessageSourceKind::Human,
        has_turn_options: false,
        delivery: MessageDelivery::NextTurn,
        target: rsi_agent_session_protocol::MessageTarget::NextTurn,
        permits_promotion: false,
        bound_turn_id: None,
        accepted_control_seq: 1,
    };
    client.queue_message_menu(selected.clone());
    assert!(client.state.menu.is_none());
    assert_eq!(
        client.tasks.len(),
        1,
        "the protected queue must still allow reading"
    );
    client.edit_queue_block(selected.clone(), Arc::new(message), 0);
    assert!(client.state.queue_edit.is_none());
    assert!(!client.begin_queue_mutation(selected, QueueMutation::Withdraw));
    assert!(handle.queue_requests.lock().unwrap().is_empty());
    client.tasks.clear();
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn busy_automation_read_preserves_the_existing_detail_instead_of_loading_forever() {
    let (mut client, _, runtime, surface) = client().await;
    let api = Arc::new(Controls {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: rsi_automation_api::operations(),
        calls: Mutex::new(vec![]),
        release: tokio::sync::Semaphore::new(0),
    });
    client.automation = Some(rsi_automation_api::Client::new(api.clone()).unwrap());
    client.state.open_detail("Existing result".into());
    client
        .exports
        .store(10, std::sync::atomic::Ordering::Release);
    client.automation_request(rsi_automation_api::Request::Status);
    assert!(!client.automation_pending);
    assert_eq!(client.state.detail.as_deref(), Some("Existing result"));
    assert!(client.tasks.is_empty() && api.calls.lock().unwrap().is_empty());
    client
        .exports
        .store(0, std::sync::atomic::Ordering::Release);
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn protected_question_draft_remains_local_without_dispatch() {
    let (mut client, handle, runtime, surface) = client().await;
    client.state.header = client
        .state
        .header
        .clone()
        .with_protection(
            rsi_agent_session_protocol::SessionProtectionScope::new("fixture", "readonly").unwrap(),
        )
        .unwrap();
    let request = rsi_user_questions_protocol::QuestionRequest {
        review: None,
        id: "question".into(),
        session_id: client.state.header.session_id().to_string(),
        turn_id: "turn".into(),
        questions: vec![rsi_user_questions_protocol::Question {
            id: "choice".into(),
            prompt: "Choose".into(),
            options: vec!["A".into(), "B".into()],
        }],
    };
    client.interactions = Some(
        rsi_session_protocol::InteractionRetention::default()
            .retain(vec![], vec![request.clone()])
            .unwrap(),
    );
    let mut editor = editor::Editor::default();
    editor.insert("1").unwrap();
    client.state.answer = Some(state::Answer {
        scroll: 0,
        request,
        editor,
        answers: vec![],
    });
    client.answer();
    assert!(client.tasks.is_empty() && handle.answers.lock().unwrap().is_empty());
    assert_eq!(client.state.answer.as_ref().unwrap().editor.text(), "1");
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
