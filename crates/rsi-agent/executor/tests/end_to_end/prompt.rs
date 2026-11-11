use super::*;
use rsi_agent_composition_protocol::{
    ContributionResult, PreparedPrompt, PromptAssembler, PromptInput,
};
use rsi_agent_prompt_assembly::{
    PromptContext, PromptRegistry, PromptSection, PromptText, TOOL_ORDER_REST, TextDeclaration,
    TextProvider,
};
use rsi_agent_session_protocol::{ContributionId, InputMessageSource, StepInstruction, StepPrompt};

#[derive(Debug)]
struct UnreachablePrompt;
#[async_trait]
impl rsi_agent_composition_protocol::PromptAssembler for UnreachablePrompt {
    async fn assemble(
        &self,
        _: PromptInput<'_>,
        _: CancellationToken,
    ) -> ContributionResult<rsi_agent_composition_protocol::PreparedPrompt> {
        panic!("the rejected or application-owned request must not enter native assembly")
    }
}
pub(super) async fn install_unreachable(stack: &BaseStack) -> rsi_meta_scope::ScopeHandle {
    install_prompt(stack, Arc::new(UnreachablePrompt)).await
}

async fn install_prompt(
    stack: &BaseStack,
    assembler: Arc<dyn PromptAssembler>,
) -> rsi_meta_scope::ScopeHandle {
    let root = rsi_meta_scope::ScopeRoot::new(4).unwrap();
    let scope = root.create(&stack.runtime.root()).await.unwrap();
    *stack.composition.prompt.lock().unwrap() = Some((assembler, scope.key().clone()));
    scope
}

fn echo_registration(stack: &BaseStack, name: &str, calls: Arc<AtomicUsize>) -> ToolRegistration {
    ToolRegistration {
        definition: ToolDefinition::new(name, name, json!({"type":"object"})).unwrap(),
        output: None,
        executor: Arc::new(EchoTool {
            store: stack.store.clone(),
            calls,
        }),
        run_policy: rsi_tools_protocol::ToolRunPolicy::execution(std::time::Duration::from_secs(1)),
    }
}

#[derive(Debug, Default)]
struct ClearingContext(AtomicUsize);
#[async_trait]
impl PromptAssembler for ClearingContext {
    async fn assemble(
        &self,
        input: PromptInput<'_>,
        _: CancellationToken,
    ) -> ContributionResult<PreparedPrompt> {
        let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        Ok(PreparedPrompt {
            sections: vec![StepInstruction {
                producer: ContributionId::new("large").unwrap(),
                text: "x".repeat(100 * 1024),
            }],
            runtime_context: if first {
                "initial runtime context".into()
            } else {
                String::new()
            },
            tool_order: input.tools.to_vec(),
        })
    }
}

#[tokio::test]
async fn repeated_rounds_store_one_large_prompt_and_one_context_clearing_marker() {
    let stack = BaseStack::activate().await;
    let assembler = Arc::new(ClearingContext::default());
    let scope = install_prompt(&stack, assembler.clone()).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(echo_registration(&stack, "echo", calls.clone()))
        .unwrap();
    let mut outcomes: VecDeque<_> = (0..10)
        .map(|index| {
            StartOutcome::Stream(tool_calls_script(&[(
                &format!("call-{index}"),
                "echo",
                r#"{"value":42}"#,
            )]))
        })
        .collect();
    outcomes.push_back(StartOutcome::Stream(answer_script()));
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(outcomes),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.prompt-growth", language.clone())
        .await;
    let executor = stack.activate_executor("prompt-growth-executor").await;
    let (submitted, outcome) = stack.submit_and_wait("work").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(assembler.0.load(Ordering::SeqCst), 11);
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert!(matches!(
        facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal { .. }
    ));
    let (mut steps, mut preparations, mut payloads, mut prompt_bytes) = (0, 0, 0, 0);
    let mut contexts = Vec::new();
    for fact in &facts {
        match fact.body() {
            SessionFactBody::StepStarted { .. } => steps += 1,
            SessionFactBody::StepPrepared { prompt, .. } => {
                preparations += 1;
                if let Some(prompt) = prompt {
                    payloads += 1;
                    prompt_bytes += prompt
                        .sections()
                        .iter()
                        .map(|section| section.text.len())
                        .sum::<usize>();
                }
            }
            SessionFactBody::InputMessageClaimed {
                source: InputMessageSource::RuntimeContext,
                content,
                ..
            } => contexts.push(content),
            _ => {}
        }
    }
    assert_eq!(
        (steps, preparations, payloads, prompt_bytes),
        (1, 11, 1, 100 * 1024)
    );
    assert_eq!(contexts.len(), 2);
    assert_eq!(
        contexts[1],
        &vec![rsi_agent_session_protocol::AgentMessageContent::Text {
            text:
                "Current runtime context: none. Earlier runtime-context snapshots no longer apply."
                    .into(),
        }]
    );
    let last = serde_json::to_string(language.requests.lock().unwrap().last().unwrap()).unwrap();
    assert_eq!(
        last.matches("Earlier runtime-context snapshots no longer apply.")
            .count(),
        1
    );
    println!(
        "{preparations} model series, {steps} durable Step, {payloads} prompt payload = {prompt_bytes} text bytes, {} runtime context claims",
        contexts.len()
    );
    drop(lease);
    assert!(scope.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

#[derive(Debug)]
struct InvalidPrompt(&'static str);
#[async_trait]
impl PromptAssembler for InvalidPrompt {
    async fn assemble(
        &self,
        input: PromptInput<'_>,
        _: CancellationToken,
    ) -> ContributionResult<PreparedPrompt> {
        let section = StepInstruction {
            producer: ContributionId::new("duplicate").unwrap(),
            text: "invalid prompt".into(),
        };
        Ok(PreparedPrompt {
            sections: if self.0 == "prompt" {
                vec![section.clone(), section]
            } else {
                vec![]
            },
            runtime_context: "must not be published".into(),
            tool_order: match self.0 {
                "missing" => vec![],
                "extra" => vec!["echo".into(), "unregistered".into()],
                "duplicate" => vec!["echo".into(), "echo".into()],
                _ => input.tools.to_vec(),
            },
        })
    }
}

#[tokio::test]
async fn invalid_prompt_and_tool_order_fail_before_publication_or_model_admission() {
    for invalid in ["prompt", "missing", "extra", "duplicate"] {
        let stack = BaseStack::activate().await;
        let scope = install_prompt(&stack, Arc::new(InvalidPrompt(invalid))).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let lease = stack
            .tool_registrar
            .register(echo_registration(&stack, "echo", calls.clone()))
            .unwrap();
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::new()),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack
            .activate_language("test.invalid-prompt", language.clone())
            .await;
        let executor = stack.activate_executor("invalid-prompt-executor").await;
        let (submitted, outcome) = stack.submit_and_wait("work").await;
        let expected = if invalid == "prompt" {
            "duplicate"
        } else {
            "prompt Tool order differs from admitted names"
        };
        assert!(
            matches!(outcome, TurnOutcome::Failed { ref code, ref message } if code == "contribution.failed" && message.contains(expected)),
            "{outcome:?}"
        );
        assert!(language.requests.lock().unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let facts = stack
            .store
            .read_facts(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap()
            .facts;
        assert!(!facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::StepPrepared { .. }
                | SessionFactBody::ModelIntent { .. }
                | SessionFactBody::InputMessageClaimed {
                    source: InputMessageSource::RuntimeContext,
                    ..
                }
        )));
        drop(lease);
        assert!(scope.dispose().await.is_clean());
        stack.dispose(language_fiber, executor).await;
    }
}

#[derive(Debug)]
struct CountText(Arc<AtomicUsize>);
impl TextProvider for CountText {
    fn resolve(&self, _: &PromptInput<'_>) -> ContributionResult<Option<String>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some("Complete native prompt".into()))
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One request/retry history proves assembly ownership, presentation and durable reuse together.
async fn native_prompt_runs_without_listeners_and_retries_reuse_accepted_preparation() {
    let stack = BaseStack::activate().await;
    let root = rsi_meta_scope::ScopeRoot::new(16).unwrap();
    let scope = root.create(&stack.runtime.root()).await.unwrap();
    let registry = Arc::new(
        PromptRegistry::new(
            root,
            4,
            Some(vec!["z-extra".into(), TOOL_ORDER_REST.into()]),
        )
        .unwrap(),
    );
    let samples = Arc::new(AtomicUsize::new(0));
    let _registration_1 = registry
        .section(
            scope.context(),
            PromptSection {
                declaration: TextDeclaration {
                    name: ContributionId::new("complete").unwrap(),
                    order: 0.0,
                    text: PromptText::Provider(Arc::new(CountText(samples.clone()))),
                    interpolate: false,
                },
                complete: true,
            },
        )
        .await
        .unwrap();
    let _registration_2 = registry
        .context(
            scope.context(),
            PromptContext {
                declaration: TextDeclaration {
                    name: ContributionId::new("directory").unwrap(),
                    order: 0.0,
                    text: PromptText::Literal("Directory: /workspace".into()),
                    interpolate: false,
                },
                required: true,
            },
        )
        .await
        .unwrap();
    *stack.composition.prompt.lock().unwrap() = Some((registry, scope.key().clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let leases: Vec<_> = ["echo", "z-extra"]
        .into_iter()
        .map(|name| {
            stack
                .tool_registrar
                .register(echo_registration(&stack, name, calls.clone()))
                .unwrap()
        })
        .collect();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Error(
                AiError::new(
                    ErrorKind::RateLimited,
                    ErrorPhase::Connect,
                    DispatchStatus::NotDispatched,
                    "retry",
                )
                .unwrap(),
            ),
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language_fiber = stack
        .activate_language("test.native-prompt", language.clone())
        .await;
    let executor = stack.activate_executor("native-prompt-executor").await;
    let (submitted, outcome) = stack.submit_and_wait("work").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        samples.load(Ordering::SeqCst),
        2,
        "retry cannot replay accepted providers"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    {
        let requests = language.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests.iter() {
            assert_eq!(
                request
                    .tools()
                    .iter()
                    .map(rsi_ai_protocol::ToolDefinition::name)
                    .collect::<Vec<_>>(),
                ["z-extra", "echo"]
            );
            let system = request
                .messages()
                .iter()
                .filter(|message| message.role() == rsi_ai_protocol::MessageRole::System)
                .collect::<Vec<_>>();
            assert_eq!(system.len(), 1);
            assert_eq!(
                system[0].content(),
                &[rsi_ai_protocol::MessageContent::Text {
                    text: "Complete native prompt".into()
                }]
            );
            let snapshots = request
                .messages()
                .iter()
                .filter(|message| {
                    serde_json::to_string(message)
                        .unwrap()
                        .contains("Directory: /workspace")
                })
                .collect::<Vec<_>>();
            assert_eq!(snapshots.len(), 1);
            assert_eq!(snapshots[0].role(), rsi_ai_protocol::MessageRole::User);
        }
        assert_eq!(requests[0], requests[1]);
    }
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageClaimed {
                    source: InputMessageSource::RuntimeContext,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::StepPrepared {
                    prompt: Some(StepPrompt::Assembled { .. }),
                    ..
                }
            ))
            .count(),
        1
    );
    drop(leases);
    assert!(scope.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}
