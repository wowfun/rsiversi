use super::*;
use rsi_agent_session_protocol::{
    AgentMessageSource, InputMessageSource, MessageAttribution, MessageDelivery,
};
use rsi_session_protocol::SessionHandle;
use serde_json::json;

fn input(id: &str, delivery: MessageDelivery) -> SubmitInput {
    SubmitInput {
        origin: rsi_session_protocol::SubmissionOrigin::User,
        delivery,
        message_id: MessageId::new(id).unwrap(),
        attribution: Some(MessageAttribution {
            id: "original:重复 ID".into(),
            source: json!({"kind":"plugin:question", "data":[1,true,"归属"]}),
        }),
        content: vec![
            SessionInput::Text {
                text: "first block".into(),
            },
            SessionInput::Text {
                text: "第二块".into(),
            },
        ]
        .try_into()
        .unwrap(),
        model: None,
        reasoning_effort: None,
        sandbox: None,
    }
}

async fn complete(handle: &Arc<dyn SessionHandle>, input: SubmitInput) -> MessageReceipt {
    let receipt = handle.submit(input).await.unwrap();
    let (turn, entered) = observe_message_claim(handle, &receipt).await;
    let mut events = handle
        .observe(ObservationCursor {
            control_seq: receipt.accepted_control_seq,
            fact_seq: entered,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(event) = events.next().await {
            if matches!(event.unwrap(), SessionObservation::Fact {fact, ..} if matches!(fact.body(), SessionFactBody::TurnTerminal {turn_id, ..} if turn_id == &turn)) { return; }
        }
        panic!("missing terminal");
    }).await.unwrap();
    receipt
}

async fn assert_inputs(handle: &Arc<dyn SessionHandle>, receipts: &[MessageReceipt]) {
    let history = handle.history_before(None, 128).await.unwrap();
    let entered: Vec<_> = history
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::InputMessageClaimed {
                source:
                    InputMessageSource::Human {
                        message_id,
                        attribution,
                    }
                    | InputMessageSource::Application {
                        message_id,
                        attribution,
                    },
                content,
                ..
            } => Some((message_id, attribution, content)),
            _ => None,
        })
        .collect();
    assert_eq!(entered.len(), receipts.len());
    for (receipt, (id, attribution, content)) in receipts.iter().zip(entered) {
        let accepted = handle
            .read_message(&receipt.message_id, receipt.accepted_control_seq)
            .await
            .unwrap();
        assert_eq!(id, &receipt.message_id);
        assert_eq!(
            attribution,
            &input("unused", MessageDelivery::NextTurn).attribution
        );
        assert_eq!(&accepted.content, content);
        assert_eq!(
            accepted.source,
            if id.as_str() == "native-second" {
                AgentMessageSource::Application {
                    attribution: attribution.clone(),
                }
            } else {
                AgentMessageSource::Human {
                    attribution: attribution.clone(),
                }
            }
        );
        assert_ne!(id.as_str(), attribution.as_ref().unwrap().id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_input_attribution_survives_uds_draft_publication_retry_and_restart() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    seen.lock().unwrap().push(body);
                    chat()
                }),
            ),
        )
        .await
        .unwrap();
    });
    let daemon = DaemonFixture::new(&fixture).await;
    let workspace = daemon
        .connection
        .workspace_registry()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let mut completed = Vec::new();
    for (id, delivery) in [
        ("attributed-followup", MessageDelivery::NextTurn),
        ("attributed-steer", MessageDelivery::Steer),
    ] {
        let handle = daemon
            .connection
            .session_service()
            .create(CreateSession {
                fork: None,
                workspace_id: workspace.id.clone(),
                session_id: SessionId::new(id).unwrap(),
                agent_preset_id: None,
            })
            .await
            .unwrap();
        let request = input("native-first", delivery);
        let first = complete(&handle, request.clone()).await;
        let retry = handle.submit(request.clone()).await.unwrap();
        assert_eq!(retry.accepted_control_seq, first.accepted_control_seq);
        let mut conflict = request.clone();
        conflict.origin = rsi_session_protocol::SubmissionOrigin::Application;
        assert!(matches!(
            handle.submit(conflict).await,
            Err(SessionError::MessageConflict { .. })
        ));
        let mut conflict = request;
        conflict.attribution.as_mut().unwrap().source = json!({"kind":"changed"});
        assert!(matches!(
            handle.submit(conflict).await,
            Err(SessionError::MessageConflict { .. })
        ));
        // Reusing presentation identity is a distinct submission when its native ID changes.
        let mut unattended = input("native-second", delivery);
        unattended.origin = rsi_session_protocol::SubmissionOrigin::Application;
        let second = complete(&handle, unattended).await;
        assert_ne!(first.accepted_control_seq, second.accepted_control_seq);
        let receipts = vec![first, second];
        assert_inputs(&handle, &receipts).await;
        completed.push((id, delivery, receipts));
    }
    assert_eq!(
        requests.lock().unwrap().len(),
        4,
        "exact retries must not call the provider again"
    );
    daemon.shutdown().await;
    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    for (id, delivery, receipts) in completed {
        let handle = running
            .session_service()
            .unwrap()
            .attach(&SessionId::new(id).unwrap())
            .await
            .unwrap();
        assert_inputs(&handle, &receipts).await;
        assert_eq!(
            handle
                .submit(input("native-first", delivery))
                .await
                .unwrap()
                .accepted_control_seq,
            receipts[0].accepted_control_seq
        );
    }
    assert_eq!(requests.lock().unwrap().len(), 4);
    assert!(running.shutdown().await.is_clean());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_waking_input_publishes_once_and_survives_restart_before_a_question() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = fixture(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    seen.lock().unwrap().push(body);
                    chat()
                }),
            ),
        )
        .await
        .unwrap();
    });
    let daemon = DaemonFixture::new(&fixture).await;
    let workspace = daemon
        .connection
        .workspace_registry()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let id = SessionId::new("held-input").unwrap();
    let handle = daemon
        .connection
        .session_service()
        .create(CreateSession {
            fork: None,
            workspace_id: workspace.id,
            session_id: id.clone(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let text = format!("native-held-context-{}", "字".repeat(100_000));
    let mut held = input("held-message", MessageDelivery::NextStep);
    held.origin = rsi_session_protocol::SubmissionOrigin::Application;
    held.content = vec![SessionInput::Text { text: text.clone() }]
        .try_into()
        .unwrap();
    let mut invalid = held.clone();
    invalid.reasoning_effort = Some(rsi_ai_protocol::ReasoningEffortId::new("high").unwrap());
    assert!(matches!(
        handle.submit(invalid).await,
        Err(SessionError::Invalid(_))
    ));
    let receipt = handle.submit(held.clone()).await.unwrap();
    assert_eq!(
        handle
            .submit(held.clone())
            .await
            .unwrap()
            .accepted_control_seq,
        receipt.accepted_control_seq
    );
    handle.wait_idle().await.unwrap();
    assert!(
        handle
            .history_before(None, 128)
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    assert!(requests.lock().unwrap().is_empty());
    let mut conflict = held.clone();
    conflict.delivery = MessageDelivery::NextTurn;
    assert!(matches!(
        handle.submit(conflict).await,
        Err(SessionError::MessageConflict { .. })
    ));
    drop(handle);
    daemon.shutdown().await;

    let running = RunningRsi::boot(composition(fixture.paths.clone()), &fixture.profile)
        .await
        .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .attach(&id)
        .await
        .unwrap();
    handle.wait_idle().await.unwrap();
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        handle
            .history_before(None, 128)
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    assert_eq!(
        handle.submit(held).await.unwrap().accepted_control_seq,
        receipt.accepted_control_seq
    );
    let accepted = handle
        .read_message(&receipt.message_id, receipt.accepted_control_seq)
        .await
        .unwrap();
    assert_eq!(
        accepted.content,
        vec![rsi_agent_session_protocol::AgentMessageContent::Text { text: text.clone() }]
    );
    complete(&handle, input("question", MessageDelivery::NextTurn)).await;
    let history = handle.history_before(None, 128).await.unwrap();
    let claimed: Vec<_> = history
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::InputMessageClaimed {
                source: InputMessageSource::Application { message_id, .. },
                content,
                ..
            } if message_id == &receipt.message_id => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(claimed, vec![&accepted.content]);
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(
        requests.lock().unwrap()[0]["messages"]
            .to_string()
            .matches(&text)
            .count(),
        1
    );
    assert!(running.shutdown().await.is_clean());
    server.abort();
}
