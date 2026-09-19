use super::*;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, InitialOutputContract, MessageDelivery,
    MessageId, MessageOptions, OutputContract,
};
use rsi_agent_turn_protocol::SubmitMessage;

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end transcript compares accepted, retryable and missing output outcomes.
async fn invalid_report_retries_valid_report_stops_before_later_tools_and_missing_report_fails() {
    for missing in [false, true] {
        let stack = BaseStack::activate().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let lease = stack
            .tool_registrar
            .register(ToolRegistration {
                definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2000 },
                executor: Arc::new(EchoTool {
                    store: stack.store.clone(),
                    calls: calls.clone(),
                }),
            })
            .unwrap();
        let scripts = if missing {
            vec![StartOutcome::Stream(answer_script())]
        } else {
            vec![
                StartOutcome::Stream(tool_calls_script(&[(
                    "bad",
                    "report_result",
                    r#"{"answer":"wrong"}"#,
                )])),
                StartOutcome::Stream(tool_calls_script(&[
                    ("good", "report_result", r#"{"answer":42}"#),
                    ("later", "echo", "{}"),
                ])),
            ]
        };
        let provider = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from(scripts)),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language = stack
            .activate_language("structured.provider", provider.clone())
            .await;
        let executor = stack.activate_executor("structured-worker").await;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let session = SessionId::new("structured-session").unwrap();
        let message_id = MessageId::new("structured-initial").unwrap();
        let contract = OutputContract::new(json!({"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"],"additionalProperties":false})).unwrap();
        let header = header_for_session(session.as_str(), TurnBudget::default())
            .with_initial_output(Some(InitialOutputContract {
                message_id: message_id.clone(),
                contract: contract.clone(),
            }))
            .unwrap();
        turns
            .submit_message(SubmitMessage {
                session: stack.fresh(header).await,
                delivery: MessageDelivery::NextTurn,
                message: AgentMessage {
                    message_id,
                    source: AgentMessageSource::Human,
                    content: vec![AgentMessageContent::Text {
                        text: "produce the required structured result".into(),
                    }],
                    options: MessageOptions::default(),
                },
            })
            .await
            .unwrap();
        let facts = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let facts = stack
                    .store
                    .read_facts(&session, 0, 256)
                    .await
                    .unwrap()
                    .facts;
                if facts
                    .iter()
                    .any(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { .. }))
                {
                    break facts;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("structured terminal deadline");
        let terminal = facts
            .iter()
            .find(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { .. }))
            .unwrap();
        if missing {
            assert!(
                matches!(terminal.body(), SessionFactBody::TurnTerminal { outcome: TurnOutcome::Failed { code, .. }, result: None, .. } if code == "structured_output.missing")
            );
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
        } else {
            assert!(
                matches!(terminal.body(), SessionFactBody::TurnTerminal { outcome: TurnOutcome::Completed, result: Some(reference), .. } if reference.summary == contract.summarize(&json!({"answer":42})).unwrap())
            );
            let results = facts
                .iter()
                .filter_map(|fact| match fact.body() {
                    SessionFactBody::ToolResult {
                        result, conclusion, ..
                    } => Some((result, conclusion)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(results.len(), 2);
            assert!(results[0].0.is_error);
            assert!(results[0].1.is_none());
            assert_eq!(results[1].0.value, json!({"answer":42}));
            assert!(results[1].1.is_some());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(
                requests[0]
                    .tools()
                    .iter()
                    .find(|tool| tool.name() == "report_result")
                    .unwrap()
                    .input_schema(),
                contract.schema()
            );
        }
        drop(lease);
        stack.dispose(language, executor).await;
    }
}
