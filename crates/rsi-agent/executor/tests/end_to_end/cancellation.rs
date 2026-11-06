use super::*;
use std::time::Duration;

#[derive(Debug)]
struct CancellationReceipt {
    entered: Semaphore,
    cancelled: Semaphore,
    release: CancellationToken,
}

#[async_trait]
impl ToolExecutor for CancellationReceipt {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        self.entered.add_permits(1);
        execution.cancellation.cancelled().await;
        self.cancelled.add_permits(1);
        self.release.cancelled().await;
        let mut result = ToolResult::new(arguments, vec![], true)?;
        result.metadata = Some(Box::new(json!({"fixture":{"error":"ABORTED"}})));
        Ok(result)
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Follow admission, cancellation, ordered publication and retirement in one scenario.
async fn cancelled_serial_and_parallel_calls_publish_their_settled_receipts() {
    for (scheduling, count) in [
        (ToolScheduling::default(), 1),
        (ToolScheduling::ParallelSafe, 2),
    ] {
        let stack = BaseStack::activate().await;
        let body = Arc::new(CancellationReceipt {
            entered: Semaphore::new(0),
            cancelled: Semaphore::new(0),
            release: CancellationToken::new(),
        });
        let lease = stack
            .tool_registrar
            .register(ToolRegistration {
                output: None,
                definition: ToolDefinition::new(
                    "echo",
                    "cooperative cancellation",
                    json!({"type":"object"}),
                )
                .unwrap()
                .with_scheduling(scheduling),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 60_000 },
                executor: body.clone(),
            })
            .unwrap();
        let calls = [
            ("first", "echo", r#"{"n":1}"#),
            ("second", "echo", r#"{"n":2}"#),
        ];
        let model = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
                &calls,
            ))])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language = stack
            .activate_language("test.language.cancel-receipt", model.clone())
            .await;
        let executor = stack.activate_executor("executor-cancel-receipt").await;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = stack
            .submit_fresh(&turns, "session-e2e", "cancel actual bodies")
            .await;
        let permits = u32::try_from(count).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            body.entered.acquire_many(permits).await.unwrap().forget();
            stack.cancel_current().await;
            body.cancelled.acquire_many(permits).await.unwrap().forget();
        })
        .await
        .expect("all admitted bodies must receive cancellation");
        body.release.cancel();
        assert_eq!(
            wait_for_outcome(&turns, &submitted).await,
            TurnOutcome::Cancelled
        );
        let page = stack
            .store
            .read_facts(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap();
        let receipts = page
            .facts
            .iter()
            .filter_map(|fact| match fact.body() {
                SessionFactBody::ToolResult {
                    identity, result, ..
                } => Some((fact.seq(), identity, result)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            page.facts
                .iter()
                .filter(|fact| matches!(fact.body(), SessionFactBody::ToolStarted { .. }))
                .count(),
            count,
            "cancellation cannot start another serial Tool call"
        );
        assert_eq!(
            receipts.len(),
            count,
            "confirmed cancellation results must precede the terminal"
        );
        let terminal = page
            .facts
            .iter()
            .find(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { .. }))
            .unwrap();
        for (index, (seq, identity, result)) in receipts.into_iter().enumerate() {
            assert!(seq < terminal.seq());
            assert_eq!(identity.call_id(), calls[index].0);
            assert_eq!(result.value, json!({"n":index + 1}));
            assert!(result.is_error);
            assert_eq!(
                result.metadata.as_deref(),
                Some(&json!({"fixture":{"error":"ABORTED"}}))
            );
            assert_eq!(
                stack.tool_runtime().query(identity).unwrap(),
                RetainedToolResult::Absent
            );
        }
        assert_eq!(
            model.starts.load(Ordering::SeqCst),
            1,
            "cancellation cannot start another model request"
        );
        drop(lease);
        stack.dispose(language, executor).await;
    }
}
