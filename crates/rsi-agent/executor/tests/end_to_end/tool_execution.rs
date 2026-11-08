use super::*;
use rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES as FACT_PAGE_BYTES;

#[derive(Debug)]
struct AuthenticatedEcho(EchoTool);

#[derive(Debug)]
struct RequestEcho {
    replaced: Option<Arc<std::collections::BTreeSet<String>>>,
    middleware: Option<Arc<dyn rsi_tools_protocol::ToolMiddleware>>,
    catalogs: Arc<dyn rsi_tools_protocol::ToolCatalogProvider>,
    echo: Arc<AuthenticatedEcho>,
    captures: AtomicUsize,
    selections: Mutex<Vec<rsi_agent_session_protocol::ModelSelection>>,
}

impl RequestEcho {
    fn new(stack: &BaseStack, calls: Arc<AtomicUsize>) -> Self {
        Self {
            replaced: None,
            middleware: None,
            captures: AtomicUsize::new(0),
            selections: Mutex::new(vec![]),
            catalogs: stack
                .runtime
                .root()
                .lookup_local::<ToolCatalogProviderContract>()
                .unwrap(),
            echo: Arc::new(AuthenticatedEcho(EchoTool {
                store: stack.store.clone(),
                calls,
            })),
        }
    }
}

#[async_trait]
impl rsi_agent_composition_protocol::RequestToolSource for RequestEcho {
    fn replaced_preset_tools(&self) -> Option<Arc<std::collections::BTreeSet<String>>> {
        self.replaced.clone()
    }

    async fn capture(
        &self,
        _header: &SessionHeader,
        selection: &rsi_agent_session_protocol::ModelSelection,
        reserved: &std::collections::BTreeSet<String>,
        _stop: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<rsi_agent_composition_protocol::RequestToolCatalog>
    {
        assert_eq!(reserved.contains("echo"), self.middleware.is_some());
        self.selections.lock().unwrap().push(selection.clone());
        let stage = self.catalogs.begin_stage().unwrap();
        let mut registrations = std::collections::BTreeMap::new();
        let capture = self.captures.fetch_add(1, Ordering::SeqCst);
        let _lease = if capture == 0 && !reserved.contains("echo") {
            registrations.insert("echo".into(), "first-registration".into());
            Some(
                stage
                    .registrar()
                    .register(ToolRegistration {
                        output: None,
                        definition: ToolDefinition::new(
                            "echo",
                            "Only for this request",
                            json!({"type":"object"}),
                        )
                        .unwrap(),
                        timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution {
                            timeout_ms: 2000,
                        },
                        executor: self.echo.clone(),
                    })
                    .unwrap(),
            )
        } else {
            None
        };
        Ok(rsi_agent_composition_protocol::RequestToolCatalog {
            middleware: self.middleware.clone(),
            inputs: None,
            instructions: format!("APPLICATION_SYSTEM_{capture}"),
            generation: "process-generation".into(),
            receipt_source: None,
            registrations,
            runtime: stage.seal().unwrap(),
        })
    }
}

/// The same fixture observes the human broker and the application's parked invocation.
#[derive(Debug)]
struct Review {
    decision: Option<ApprovalDecision>,
    revoked: CancellationToken,
    cards: AtomicUsize,
    answers: Mutex<Vec<bool>>,
}

#[async_trait]
impl Approval for Review {
    async fn ask(
        &self,
        request: ApprovalRequest,
        stop: CancellationToken,
    ) -> rsi_approval_protocol::Result<ApprovalOutcome> {
        assert_eq!(request.reason, "Review native echo");
        assert_eq!(request.review.unwrap().arguments, json!({"value":42}));
        self.cards.fetch_add(1, Ordering::SeqCst);
        if let Some(decision) = self.decision {
            return Ok(ApprovalOutcome {
                decision,
                answerer: "human".into(),
                reason: None,
            });
        }
        self.revoked.cancel();
        stop.cancelled().await;
        Err(rsi_approval_protocol::ApprovalError::Cancelled)
    }
}

#[async_trait]
impl rsi_tools_protocol::ToolMiddleware for Review {
    async fn prepare(
        self: Arc<Self>,
        call: &rsi_tools_protocol::ToolCall,
        _: Option<&rsi_tools_protocol::ToolOutputDeclaration>,
        _: &CancellationToken,
    ) -> ToolResultType<rsi_tools_protocol::ToolGate> {
        assert_eq!(call.name, "echo");
        Ok(rsi_tools_protocol::ToolGate::Ask {
            reason: "Review native echo".into(),
            pending: Box::new(ReviewCall(self)),
        })
    }
}

#[derive(Debug)]
struct ReviewCall(Arc<Review>);
#[async_trait]
impl rsi_tools_protocol::PendingToolApproval for ReviewCall {
    async fn withdrawn(&mut self) {
        self.0.revoked.cancelled().await;
    }
    async fn answer(
        self: Box<Self>,
        allow: bool,
        _: &CancellationToken,
    ) -> ToolResultType<rsi_tools_protocol::ToolDecision> {
        self.0.answers.lock().unwrap().push(allow);
        if allow {
            Ok(rsi_tools_protocol::ToolDecision::Ready(self))
        } else {
            Ok(rsi_tools_protocol::ToolDecision::Denied {
                reason: "Refused".into(),
                additional_contexts: vec![rsi_tools_protocol::ToolContextMessage {
                    id: "review-after-deny".into(),
                    source: json!({"kind":"plugin","id":"fixture.review"}),
                    text: vec!["Use the read-only tool instead.".into()],
                }],
            })
        }
    }
}
#[async_trait]
impl rsi_tools_protocol::AroundTool for ReviewCall {
    async fn execute(
        self: Box<Self>,
        body: rsi_tools_protocol::ToolBody<'_>,
    ) -> ToolResultType<ToolResult> {
        body.run().await
    }
}

fn echo_then_answer(stack: &BaseStack) -> Arc<LanguageFixture> {
    Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::default(),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    })
}

#[tokio::test]
async fn native_and_application_policies_share_one_withdrawable_human_review() {
    for decision in [
        Some(ApprovalDecision::AllowOnce),
        Some(ApprovalDecision::Deny),
        None,
    ] {
        let review = Arc::new(Review {
            decision,
            revoked: CancellationToken::new(),
            cards: AtomicUsize::new(0),
            answers: Mutex::default(),
        });
        let stack = BaseStack::activate_with_answerer(review.clone()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let lease = register_native_echo(&stack, calls.clone());
        let mut source = RequestEcho::new(&stack, calls.clone());
        source.middleware = Some(review.clone());
        let worker = activate_fixture(
            &stack.runtime,
            "test.review-source",
            "app",
            Arc::new(RequestSourceFactory(Arc::new(source))),
        )
        .await;
        let model = echo_then_answer(&stack);
        let provider = stack
            .activate_language("test.review-language", model.clone())
            .await;
        let executor = stack.activate_executor("combined-review").await;
        let (submitted, outcome) = stack
            .submit_and_wait_with_header(
                "call echo",
                None,
                header()
                    .with_execution_policy(SandboxMode::WorkspaceWrite, true)
                    .unwrap(),
            )
            .await;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(review.cards.load(Ordering::SeqCst), 1);
        let allowed = decision == Some(ApprovalDecision::AllowOnce);
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(allowed));
        assert_eq!(
            *review.answers.lock().unwrap(),
            decision
                .map(|d| d == ApprovalDecision::AllowOnce)
                .into_iter()
                .collect::<Vec<_>>()
        );
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
            .await
            .unwrap()
            .facts;
        assert_tool_admissions(&facts, usize::from(allowed));
        let refusal = facts.iter().find_map(|f| match f.body() {
            SessionFactBody::ToolRejected { rejection, .. } => Some(rejection),
            _ => None,
        });
        let claims: Vec<_> = facts
            .iter()
            .filter_map(|f| match f.body() {
                SessionFactBody::ToolContextsClaimed { messages, .. } => Some(messages),
                _ => None,
            })
            .flatten()
            .collect();
        if decision == Some(ApprovalDecision::Deny) {
            let [claim] = claims.as_slice() else {
                panic!("expected one policy message")
            };
            assert_eq!(claim.message.id, "review-after-deny");
            assert!(facts.iter().any(|f| matches!(f.body(),
                SessionFactBody::ToolRejected { effect_id, additional_contexts, .. }
                    if effect_id == &claim.effect && additional_contexts.as_slice() == [claim.message.clone()])));
            let requests = model.requests.lock().unwrap();
            let last = serde_json::to_string(requests.last().unwrap()).unwrap();
            assert_eq!(last.matches("Use the read-only tool instead.").count(), 1);
        } else {
            assert!(claims.is_empty());
        }
        assert!(matches!(
            (decision, refusal),
            (Some(ApprovalDecision::AllowOnce), None)
                | (
                    Some(ApprovalDecision::Deny),
                    Some(rsi_agent_session_protocol::ToolRejection::ApprovalDenied { .. })
                )
                | (
                    None,
                    Some(rsi_agent_session_protocol::ToolRejection::PolicyDenied { .. })
                )
        ));
        assert!(worker.dispose().await.is_clean());
        drop(lease);
        stack.dispose(provider, executor).await;
    }
}

fn assert_tool_admissions(facts: &[SessionFact], expected: usize) {
    let intents = facts
        .iter()
        .filter(|f| matches!(f.body(), SessionFactBody::ToolIntent { .. }));
    let starts = facts
        .iter()
        .filter(|f| matches!(f.body(), SessionFactBody::ToolStarted { .. }));
    assert_eq!(intents.count(), expected);
    assert_eq!(starts.count(), expected);
}

#[derive(Debug)]
pub(super) struct RequestSourceFactory(
    pub(super) Arc<dyn rsi_agent_composition_protocol::RequestToolSource>,
);
#[async_trait]
impl PluginFactory for RequestSourceFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_agent_composition_protocol::RequestToolSourceContract>(
                self.0.clone(),
            )?;
        Ok(())
    }
}

fn register_native_echo(
    stack: &BaseStack,
    calls: Arc<AtomicUsize>,
) -> rsi_tools_protocol::ToolLease {
    stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "native echo", json!({"type":"object"}))
                .unwrap(),
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls,
            }),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
        })
        .unwrap()
}

#[tokio::test]
async fn application_tool_conclusion_waits_for_siblings_and_their_pending_inputs() {
    for defer in [false, true] {
        let stack = BaseStack::activate_with_approval(ApprovalDecision::Deny).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(RequestEcho::new(&stack, calls.clone()));
        let worker = activate_fixture(
            &stack.runtime,
            "test.request-tools",
            "app",
            Arc::new(RequestSourceFactory(source.clone())),
        )
        .await;
        let model = echo_then_answer(&stack);
        let second = json!({"defer":defer}).to_string();
        model.outcomes.lock().unwrap()[0] = StartOutcome::Stream(tool_calls_script(&[
            ("first", "echo", r#"{"finish":true,"defer":false}"#),
            ("second", "echo", &second),
        ]));
        let provider = stack
            .activate_language("test.language", model.clone())
            .await;
        let executor = stack.activate_executor("executor-tool-conclusion").await;
        let (submitted, outcome) = stack.submit_and_wait("finish after both calls").await;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(model.requests.lock().unwrap().len(), 1 + usize::from(defer));
        assert_eq!(
            source.captures.load(Ordering::SeqCst),
            1 + usize::from(defer)
        );
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
            .await
            .unwrap()
            .facts;
        assert_tool_admissions(&facts, 2);
        let markers: Vec<_> = facts
            .iter()
            .filter_map(|f| match f.body() {
                SessionFactBody::ToolResult { result, .. } => Some(result.concludes_turn),
                _ => None,
            })
            .collect();
        assert_eq!(markers, [true, false]);
        if defer {
            assert_entered_tool_contexts(&facts, &submitted.session_id);
        }
        assert!(worker.dispose().await.is_clean());
        stack.dispose(provider, executor).await;
    }
}

#[tokio::test]
async fn model_retry_reuses_application_registration_and_next_round_drops_it() {
    // Any unexpected approval would refuse the Tool and make its body count fail.
    let stack = BaseStack::activate_with_approval(ApprovalDecision::Deny).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let native_calls = Arc::new(AtomicUsize::new(0));
    let _native = register_native_echo(&stack, native_calls.clone());
    let mut source = RequestEcho::new(&stack, calls.clone());
    source.replaced = Some(Arc::new(["echo".to_owned()].into()));
    let source = Arc::new(source);
    let provider = Arc::new(LanguageFixture {
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
            StartOutcome::Error(
                AiError::new(
                    ErrorKind::RateLimited,
                    ErrorPhase::Connect,
                    DispatchStatus::NotDispatched,
                    "retry after context",
                )
                .unwrap(),
            ),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language = stack
        .activate_language("test.language", provider.clone())
        .await;
    let executor = stack.activate_executor("executor-request-tools").await;
    let worker = activate_fixture(
        &stack.runtime,
        "test.request-tools",
        "app",
        Arc::new(RequestSourceFactory(source.clone())),
    )
    .await;
    let (submitted, outcome) = stack.submit_and_wait("echo then complete").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(source.captures.load(Ordering::SeqCst), 2);
    assert_eq!(source.selections.lock().unwrap().len(), 2);
    assert_eq!(native_calls.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_retry_tools_and_inputs(&provider.requests.lock().unwrap());
    for (index, request) in provider.requests.lock().unwrap().iter().enumerate() {
        let encoded = serde_json::to_string(request).unwrap();
        let capture = index / 2;
        assert!(encoded.contains(&format!("APPLICATION_SYSTEM_{capture}")));
        assert!(!encoded.contains(&format!("APPLICATION_SYSTEM_{}", 1 - capture)));
    }
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let views = page
        .facts
        .iter()
        .filter_map(|f| match f.body() {
            SessionFactBody::StepPrepared {
                application: Some(view),
                ..
            } => Some(view.bindings()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(views.len(), 2);
    assert!(views[1].is_empty());
    let binding = &views[0][0];
    assert_eq!(binding.name, "echo");
    assert_eq!(binding.registration, "first-registration");
    assert_request_tool_evidence(&page.facts, &provider.requests.lock().unwrap());
    assert!(page.facts.iter().any(|f|matches!(f.body(),
        SessionFactBody::ToolIntent{identity,approval:None,..} if identity.owner_id()==binding.owner)));
    assert!(page.facts.iter().any(|f|matches!(f.body(),
        SessionFactBody::ToolResult{identity,result,..} if identity.owner_id()==binding.owner && result.value==json!({"value":42}))));
    assert_entered_tool_contexts(&page.facts, &submitted.session_id);
    assert!(worker.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}

fn assert_request_tool_evidence(facts: &[SessionFact], requests: &[LanguageRequest]) {
    // The model request owns schema inspection; preparation retains only routing identities.
    let mut intents = 0;
    for fact in facts {
        let SessionFactBody::ModelIntent { evidence, .. } = fact.body() else {
            continue;
        };
        let rsi_agent_session_protocol::RequestEvidence::Available { tools, .. } = evidence else {
            panic!("request evidence missing");
        };
        let original = match tools {
            rsi_agent_session_protocol::EvidencePart::Inline { .. } => tools,
            rsi_agent_session_protocol::EvidencePart::Reference { seq, .. } => {
                let original = facts.iter().find(|f| f.seq() == *seq).unwrap();
                let SessionFactBody::ModelIntent {
                    evidence: rsi_agent_session_protocol::RequestEvidence::Available { tools, .. },
                    ..
                } = original.body()
                else {
                    panic!("reference must target original request evidence");
                };
                tools
            }
        };
        let rsi_agent_session_protocol::EvidencePart::Inline { text, .. } = original else {
            panic!("evidence reference must be direct");
        };
        let schema: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            schema["definitions"],
            serde_json::to_value(requests[intents].tools()).unwrap()
        );
        intents += 1;
    }
    assert_eq!(intents, requests.len());
}

fn assert_retry_tools_and_inputs(requests: &[LanguageRequest]) {
    assert_eq!(
        requests
            .iter()
            .map(|request| request.tools().len())
            .collect::<Vec<_>>(),
        [1, 1, 0, 0]
    );
    let entered=requests.iter().map(|request|{
        request.messages().windows(2).filter(|pair|{
            let first=&pair[0];let second=&pair[1];
            first.role()==rsi_ai_protocol::MessageRole::User && second.role()==first.role() &&
                matches!(first.content(),[rsi_ai_protocol::MessageContent::Text{text:a},rsi_ai_protocol::MessageContent::Text{text:b}]
                    if a=="tool guidance" && b=="second block") &&
                matches!(second.content(),[rsi_ai_protocol::MessageContent::Text{text}] if text=="last guidance")
        }).count()
    }).collect::<Vec<_>>();
    assert_eq!(entered, [0, 0, 1, 1]);
}

#[derive(Debug)]
struct ApplicationPolicy(rsi_agent_composition_protocol::ToolPolicyDecision);

#[derive(Debug)]
struct GatedCapture {
    inner: RequestEcho,
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl rsi_agent_composition_protocol::RequestToolSource for GatedCapture {
    async fn capture(
        &self,
        header: &SessionHeader,
        selection: &rsi_agent_session_protocol::ModelSelection,
        reserved: &std::collections::BTreeSet<String>,
        stop: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<rsi_agent_composition_protocol::RequestToolCatalog>
    {
        if self.inner.captures.load(Ordering::SeqCst) == 0 {
            self.entered.notify_one();
            self.release.notified().await;
            assert!(!stop.is_cancelled());
        }
        self.inner.capture(header, selection, reserved, stop).await
    }
}

#[tokio::test(start_paused = true)]
async fn application_prompt_capture_uses_the_preparation_stage_budget() {
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(GatedCapture {
        inner: RequestEcho::new(&stack, calls.clone()),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let worker = activate_fixture(
        &stack.runtime,
        "test.request-tools",
        "app",
        Arc::new(RequestSourceFactory(source.clone())),
    )
    .await;
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let provider = stack
        .activate_language("test.language", language.clone())
        .await;
    let executor = stack.activate_executor("prompt-budget").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "prompt-budget", "call echo")
        .await;
    source.entered.notified().await;
    tokio::time::advance(std::time::Duration::from_secs(12)).await;
    assert_eq!(language.starts.load(Ordering::SeqCst), 0);
    source.release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &submitted).await,
        TurnOutcome::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        serde_json::to_string(&language.requests.lock().unwrap()[0])
            .unwrap()
            .contains("APPLICATION_SYSTEM_0")
    );
    assert!(worker.dispose().await.is_clean());
    stack.dispose(provider, executor).await;
}

#[async_trait]
impl rsi_agent_composition_protocol::ToolPolicy for ApplicationPolicy {
    async fn decide(
        &self,
        _: &rsi_agent_composition_protocol::ContributionContext,
        request: &rsi_agent_composition_protocol::ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> rsi_agent_composition_protocol::ContributionResult<
        rsi_agent_composition_protocol::ToolPolicyDecision,
    > {
        assert_eq!(request.name, "echo");
        assert_eq!(request.sandbox, SandboxMode::DangerFullAccess);
        assert!(!request.require_approval);
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn application_tools_keep_explicit_approval_and_native_policy_constraints() {
    use rsi_agent_composition_protocol::{
        ContributionKind, ContributionRegistration, ToolPolicyDecision,
    };
    use rsi_agent_session_protocol::{ContributionId, ToolRejection};
    for decision in [
        None,
        Some(ToolPolicyDecision::RequireApproval),
        Some(ToolPolicyDecision::Deny {
            reason: "plugin policy".into(),
        }),
    ] {
        let stack = BaseStack::activate_with_approval(ApprovalDecision::Deny).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(RequestEcho::new(&stack, calls.clone()));
        let worker = activate_fixture(
            &stack.runtime,
            "test.request-tools",
            "app",
            Arc::new(RequestSourceFactory(source)),
        )
        .await;
        let callbacks = if let Some(decision) = &decision {
            Some(
                super::contributions::install(
                    &stack,
                    vec![ContributionRegistration::new(
                        ContributionId::new("fixture.application-policy").unwrap(),
                        0,
                        ContributionKind::ToolPolicy(Arc::new(ApplicationPolicy(decision.clone()))),
                    )],
                )
                .await,
            )
        } else {
            None
        };
        let language = echo_then_answer(&stack);
        let provider = stack.activate_language("test.language", language).await;
        let executor = stack.activate_executor("application-policy").await;
        let header = header()
            .with_execution_policy(SandboxMode::WorkspaceWrite, decision.is_none())
            .unwrap();
        let (submitted, outcome) = stack
            .submit_and_wait_with_header("call echo", None, header)
            .await;
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
            .await
            .unwrap()
            .facts;
        let rejections: Vec<_> = facts
            .iter()
            .filter_map(|fact| match fact.body() {
                SessionFactBody::ToolRejected { rejection, .. } => Some(rejection),
                _ => None,
            })
            .collect();
        assert_eq!(rejections.len(), 1);
        assert_eq!(
            matches!(rejections[0], ToolRejection::PolicyDenied { .. }),
            matches!(decision, Some(ToolPolicyDecision::Deny { .. }))
        );
        assert!(!facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolIntent { .. }
                | SessionFactBody::ToolStarted { .. }
                | SessionFactBody::ToolResult { .. }
        )));
        if let Some(callbacks) = callbacks {
            assert!(callbacks.dispose().await.is_clean());
        }
        assert!(worker.dispose().await.is_clean());
        stack.dispose(provider, executor).await;
    }
}

fn assert_entered_tool_contexts(facts: &[SessionFact], session: &SessionId) {
    let contexts: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolContextsClaimed { messages, .. } => Some((fact.seq(), messages)),
            _ => None,
        })
        .collect();
    assert_eq!(contexts.len(), 1);
    let (seq, inputs) = contexts[0];
    assert_eq!(inputs.len(), 2);
    assert!(
        inputs
            .iter()
            .enumerate()
            .all(|(i, input)| input.session == *session
                && usize::from(input.ordinal) == i
                && input.effect == inputs[0].effect)
    );
    assert_eq!(inputs[1].message.id, "second-hook");
    assert!(facts.iter().any(|fact|fact.seq()<seq && matches!(fact.body(),
        SessionFactBody::ToolResult{effect_id,result,..} if effect_id==&inputs[0].effect&&result.additional_contexts[0]==inputs[0].message)));
}

#[async_trait]
impl ToolExecutor for AuthenticatedEcho {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        let caller = execution
            .extension::<rsi_agent_turn_protocol::AgentCallerAuthority>()
            .expect("external Tool execution requires authenticated origin authority");
        assert!(caller.tool_effect_id().is_some());
        assert!(caller.source_selection().is_some());
        let mut result = self.0.execute(arguments, execution).await?;
        result.concludes_turn = result.value.get("finish") == Some(&Value::Bool(true));
        if result.value.get("defer") == Some(&Value::Bool(false)) {
            return Ok(result);
        }
        result
            .additional_contexts
            .push(rsi_tools_protocol::ToolContextMessage {
                id: "hook".into(),
                source: json!({"kind":"post-tool"}),
                text: vec!["tool guidance".into(), "second block".into()],
            });
        result
            .additional_contexts
            .push(rsi_tools_protocol::ToolContextMessage {
                id: "second-hook".into(),
                source: json!({"kind":"another-hook"}),
                text: vec!["last guidance".into()],
            });
        Ok(result)
    }
}

#[tokio::test]
async fn frozen_role_hides_tools_before_any_intent_and_does_not_mutate_other_claims() {
    use rsi_agent_session_protocol::{DelegationPolicy, DelegationRole};
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.language.roles", provider.clone())
        .await;
    let executor = stack.activate_executor("role-policy").await;
    let role = DelegationRole {
        name: "reason-only".into(),
        persona: Some("ROLE_PERSONA_FROZEN".into()),
        allow: Some(std::collections::BTreeSet::default()),
        deny: std::collections::BTreeSet::default(),
    };
    let policy =
        DelegationPolicy::freeze(Some(&role), ["echo".into()].into_iter().collect(), None).unwrap();
    let restricted = header_for_session("restricted")
        .with_delegation_policy(Some(policy))
        .unwrap();
    let (submitted, outcome) = stack
        .submit_and_wait_with_header("try echo", None, restricted)
        .await;
    assert!(matches!(outcome, TurnOutcome::Failed { ref code, .. } if code == "tool.not_found"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    assert!(!facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolRejected { .. }
            | SessionFactBody::ToolResult { .. }
    )));
    {
        let requests = provider.requests.lock().unwrap();
        assert!(requests[0].tools().is_empty());
        assert!(
            serde_json::to_string(&requests[0])
                .unwrap()
                .contains("ROLE_PERSONA_FROZEN")
        );
    }
    // A second Session sharing the sealed generation still discovers echo.
    assert_eq!(stack.tool_runtime().definitions().len(), 1);
    let (_, outcome) = stack.submit_and_wait("ordinary independent task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        provider.requests.lock().unwrap()[1].tools()[0].name(),
        "echo"
    );
    drop(lease);
    stack.dispose(language, executor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_approval_persists_the_prepared_call_without_starting_the_tool() {
    let stack = BaseStack::activate_with_approval(ApprovalDecision::Deny).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let tool_lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.language", fixture.clone())
        .await;
    let executor = stack.activate_executor("executor-deny").await;
    let (submitted, outcome) = stack
        .submit_and_wait_with_sandbox("call echo", Some(SandboxMode::DangerFullAccess))
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    {
        let requests = fixture.requests.lock().unwrap();
        assert_complete_tool_groups(&requests[1]);
        assert!(
            serde_json::to_string(&requests[1])
                .unwrap()
                .contains("denied")
        );
    }
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let rejected = page
        .facts
        .iter()
        .filter(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
        .collect::<Vec<_>>();
    assert_eq!(rejected.len(), 1);
    assert!(
        matches!(rejected[0].body(), SessionFactBody::ToolRejected { identity, name, arguments, rejection: rsi_agent_session_protocol::ToolRejection::ApprovalDenied { outcome }, .. }
        if identity.call_id() == "tool-call-1" && name == "echo" && arguments == &json!({"value":42}) && outcome.decision == ApprovalDecision::Deny)
    );
    assert!(!page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolResult { .. }
    )));
    assert!(matches!(
        page.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal { .. }
    ));
    drop(tool_lease);
    stack.dispose(language, executor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_persists_intent_and_start_before_model_and_tool_io() {
    let stack = BaseStack::activate().await;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(AuthenticatedEcho(EchoTool {
                store: stack.store.clone(),
                calls: tool_calls.clone(),
            })),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-1").await;
    let (submitted, outcome) = stack
        .submit_and_wait_with_sandbox("call echo", Some(SandboxMode::DangerFullAccess))
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    assert_eq!(tool_calls.load(Ordering::Acquire), 1);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let kinds = page
        .facts
        .iter()
        .map(|fact| fact_kind(fact.body()))
        .collect::<Vec<_>>();
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent {
            approval: Some(approval),
            ..
        } if approval.decision == ApprovalDecision::AllowOnce
    )));
    assert!(page.facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolResult { result, .. } if result.enforcement.len() == 1
    )));
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["model_intent", "model_started"])
    );
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["tool_intent", "tool_started"])
    );
    assert!(
        kinds
            .windows(2)
            .any(|pair| pair == ["tool_started", "tool_result"])
    );
    assert_eq!(kinds.last(), Some(&"terminal"));
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .messages()
                .iter()
                .any(|message| { message.role() == rsi_ai_protocol::MessageRole::Tool })
        );
    }
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adjacent_parallel_safe_tools_overlap_but_publish_results_in_source_order() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let rendezvous = Arc::new(Barrier::new(2));
    let release_first = Arc::new(Notify::new());
    let observed_lane_parking_authority = Arc::new(AtomicBool::new(false));
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new(
                "parallel",
                "parallel fixture",
                json!({"type":"object"}),
            )
            .unwrap()
            .with_scheduling(ToolScheduling::ParallelSafe),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(ParallelTool {
                store: stack.store.clone(),
                rendezvous,
                release_first,
                observed_lane_parking_authority: Arc::clone(&observed_lane_parking_authority),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[
                ("parallel-call-1", "parallel", r#"{"position":"first"}"#),
                ("parallel-call-2", "parallel", r#"{"position":"second"}"#),
            ])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parallel-tools", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-parallel-tools").await;

    let (submitted, outcome) = stack.submit_and_wait("run both tools").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let intents = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent {
                parallel_safe,
                approval,
                ..
            } => {
                assert!(
                    approval.is_none(),
                    "restricted calls do not request approval"
                );
                Some(*parallel_safe)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(intents, vec![true, true]);
    let effect_markers = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent { effect_id, .. } => Some(("intent", effect_id.as_str())),
            SessionFactBody::ToolStarted { effect_id, .. } => Some(("started", effect_id.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        effect_markers
            .iter()
            .map(|(marker, _)| *marker)
            .collect::<Vec<_>>(),
        vec!["intent", "intent", "started", "started"]
    );
    assert_eq!(effect_markers[0].1, effect_markers[2].1);
    assert_eq!(effect_markers[1].1, effect_markers[3].1);
    let results = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolResult { result, .. } => {
                result.value.get("position").and_then(Value::as_str)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results, vec!["first", "second"]);
    assert!(!observed_lane_parking_authority.load(Ordering::Acquire));

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_batch_publishes_successful_siblings_before_propagating_a_failure() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let failed_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("fail", "fail", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(FailingTool {
                store: stack.store.clone(),
            }),
        })
        .unwrap();
    let successful_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[
                ("failed-call", "fail", r#"{"position":"failed"}"#),
                ("successful-call", "echo", r#"{"position":"successful"}"#),
            ],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parallel-partial-failure", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor("executor-parallel-partial-failure")
        .await;

    let (submitted, outcome) = stack.submit_and_wait("run both tools").await;
    assert!(matches!(outcome, TurnOutcome::Failed { .. }));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    let successful_result = facts.iter().position(|fact| {
        matches!(
            fact.body(),
            SessionFactBody::ToolResult { result, .. }
                if result.value.get("position") == Some(&Value::String("successful".into()))
        )
    });
    let terminal = facts
        .iter()
        .position(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { .. }));
    assert!(
        successful_result
            .zip(terminal)
            .is_some_and(|(result, terminal)| result < terminal)
    );

    drop((failed_lease, successful_lease));
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_results_above_the_old_cumulative_byte_budget_both_publish() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("uneven", "uneven", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(UnevenParallelResultTool),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[
                ("large-call", "uneven", r#"{"position":"first"}"#),
                ("small-call", "uneven", r#"{"position":"second"}"#),
            ])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.parallel-publication-failure", fixture)
        .await;
    let executor_fiber = stack
        .activate_executor("executor-parallel-publication-failure")
        .await;

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("run uneven tools", None, header())
        .await;

    assert!(matches!(outcome, TurnOutcome::Completed));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    assert!(facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolResult { result, .. }
            if result.value.get("position") == Some(&Value::String("second".into()))
    )));

    assert!(facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::ToolResult { result, .. } if result.value.get("position") == Some(&json!("first")))));
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_tool_is_a_durable_barrier_between_parallel_safe_runs() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let calls = Arc::new(AtomicUsize::new(0));
    let parallel_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("read", "read", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ParallelSafe),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let exclusive_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("write", "write", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[
                ("read-call", "read", r#"{"position":"read"}"#),
                ("write-call", "write", r#"{"position":"write"}"#),
                ("read-call-2", "read", r#"{"position":"read-2"}"#),
            ])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-barrier", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-barrier").await;

    let (submitted, outcome) = stack.submit_and_wait("read write read").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::Acquire), 3);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let relevant = page
        .facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent { name, .. } => Some(format!("intent:{name}")),
            SessionFactBody::ToolResult { result, .. } => result
                .value
                .get("position")
                .and_then(Value::as_str)
                .map(|position| format!("result:{position}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relevant,
        vec![
            "intent:read",
            "result:read",
            "intent:write",
            "result:write",
            "intent:read",
            "result:read-2",
        ]
    );

    drop((parallel_lease, exclusive_lease));
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_final_tool_is_rejected_before_effects_when_not_last_in_source_order() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let calls = Arc::new(AtomicUsize::new(0));
    let wait_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("wait_agent", "wait", json!({"type":"object"}))
                .unwrap()
                .with_scheduling(ToolScheduling::ExclusiveFinal),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let echo_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&calls),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[
                ("wait-call", "wait_agent", r"{}"),
                ("echo-call", "echo", r"{}"),
            ],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.wait-order", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-wait-order").await;

    let (submitted, outcome) = stack.submit_and_wait("invalid wait ordering").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "tool.exclusive_final_not_last"
    ));
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    assert!(
        !page
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent { .. }))
    );

    drop((wait_lease, echo_lease));
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tool_is_rejected_before_any_effect_is_published() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[("missing-call", "missing_tool", r"{}")],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.missing-tool", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-missing-tool").await;

    let (submitted, outcome) = stack.submit_and_wait("call a missing tool").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "tool.not_found"
    ));
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    assert!(
        !page
            .facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent { .. }))
    );

    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_tool_loop_keeps_independent_attempt_settlements() {
    let stack = BaseStack::activate().await;
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::clone(&tool_calls),
            }),
        })
        .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::clone(&starts),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.budget", fixture.clone())
        .await;
    let executor_fiber = stack.activate_executor("executor-budget").await;
    let (submitted, outcome) = stack
        .submit_and_wait_with_header("keep calling tools", None, header())
        .await;

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(starts.load(Ordering::Acquire), 2);
    assert_eq!(tool_calls.load(Ordering::Acquire), 1);
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let page = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let kinds = page
        .facts
        .iter()
        .map(|fact| fact_kind(fact.body()))
        .collect::<Vec<_>>();
    assert!(kinds.ends_with(&["step_ended", "terminal"]));
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "model_attempt_settled")
            .count(),
        2
    );

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalizer_failure_replaces_success_with_one_durable_terminal() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.finalizer-budget", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let (finalizer_owner, finalizer_context) =
        rsi_agent_testkit::activate_contribution_owner(&stack.runtime.root())
            .await
            .unwrap();
    let credential = finalizer_context.registration_context().unwrap();
    let finalizer_lease = finalization
        .register(
            &credential,
            "failing-test-finalizer".into(),
            Arc::new(FailingFinalizer),
        )
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-finalizer-budget").await;

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("fail finalization", None, header())
        .await;

    assert!(matches!(
        outcome,
        TurnOutcome::Failed { code, .. } if code == "test.finalization"
    ));
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::TurnTerminal { .. }))
            .count(),
        1
    );

    drop(finalizer_lease);
    assert!(finalizer_owner.dispose().await.is_clean());
    drop(finalization);
    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_blocker_replaces_only_an_otherwise_successful_outcome() {
    let stack = BaseStack::activate().await;
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.completion-blocker", fixture)
        .await;
    let finalization = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap();
    let (finalizer_owner, finalizer_context) =
        rsi_agent_testkit::activate_contribution_owner(&stack.runtime.root())
            .await
            .unwrap();
    let credential = finalizer_context.registration_context().unwrap();
    let finalizer_lease = finalization
        .register(
            &credential,
            "completion-blocker".into(),
            Arc::new(CompletionBlockerFinalizer),
        )
        .unwrap();
    let executor_fiber = stack.activate_executor("executor-completion-blocker").await;

    let (_, outcome) = stack.submit_and_wait("finish successfully").await;
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            code: "jobs.unreported".into(),
            message: "background output was not collected".into(),
        }
    );

    drop(finalizer_lease);
    assert!(finalizer_owner.dispose().await.is_clean());
    drop(finalization);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_tool_result_retires_the_retained_identity_after_terminal_durability() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-result-budget", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-result-budget").await;

    let (submitted, outcome) = stack
        .submit_and_wait_with_header("budget the result", None, header())
        .await;

    assert_eq!(outcome, TurnOutcome::Completed);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("durable ToolStarted identity");
    // Terminal publication precedes the independent retirement task. Observe
    // that task's public result without relying on which worker runs next.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while stack.tool_runtime().query(&identity).unwrap() != RetainedToolResult::Absent {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the durable successful result must retire its retained Tool identity");

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

fn fact_kind(body: &SessionFactBody) -> &'static str {
    match body {
        SessionFactBody::TurnAccepted { .. } => "accepted",
        SessionFactBody::MessageTurnAccepted { .. } => "message_accepted",
        SessionFactBody::StepStarted { .. } => "step_started",
        SessionFactBody::InputMessageClaimed { .. } => "input_message_claimed",
        SessionFactBody::ToolContextsClaimed { .. } => "tool_contexts_claimed",
        SessionFactBody::StepEnded { .. } => "step_ended",
        SessionFactBody::WorkspaceTouched { .. } => "workspace_touched",
        SessionFactBody::ImageRequested { .. } => "image_requested",
        SessionFactBody::ModelIntent { .. } => "model_intent",
        SessionFactBody::ModelStarted { .. } => "model_started",
        SessionFactBody::ModelAttemptSettled { .. } => "model_attempt_settled",
        SessionFactBody::ImageIntent { .. } => "image_intent",
        SessionFactBody::ImageStarted { .. } => "image_started",
        SessionFactBody::ImageOutput { .. } => "image_output",
        SessionFactBody::ToolIntent { .. } => "tool_intent",
        SessionFactBody::ToolRejected { .. } => "tool_rejected",
        SessionFactBody::ToolStarted { .. } => "tool_started",
        SessionFactBody::ToolResult { .. } => "tool_result",
        SessionFactBody::TurnTerminal { .. } => "terminal",
        SessionFactBody::CancelRequested { .. } => "cancel",
        SessionFactBody::ToolCallsSuperseded { .. } => "tool_calls_superseded",
        SessionFactBody::StepPrepared { .. } => "step_tools_selected",
        SessionFactBody::StepInputsPrepared { .. } => "step_inputs_prepared",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_tool_result_is_retired_after_the_terminal_fact_is_durable() {
    let stack = BaseStack::activate().await;
    let tools = Arc::clone(&stack.tool_registrar);
    let tool_lease = tools
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "fail", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(FailingTool {
                store: stack.store.clone(),
            }),
        })
        .unwrap();
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language.tool-failure", fixture)
        .await;
    let executor_fiber = stack.activate_executor("executor-tool-failure").await;
    let (submitted, outcome) = stack.submit_and_wait("fail the tool").await;
    assert!(matches!(outcome, TurnOutcome::Failed { .. }));

    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("durable ToolStarted identity");
    // The terminal observer can run before the driver's owned retirement task.
    // Observe actual slot release, without treating durable visibility as a join.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stack.tool_runtime().query(&identity).unwrap() != RetainedToolResult::Absent {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned retirement must release the failed Tool slot after durability");

    drop(tool_lease);
    drop(tools);
    stack.dispose(language_fiber, executor_fiber).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One gated public scenario checks dispatch, durable order, replay and checkpoint equivalence.
async fn next_step_supersedes_two_unadmitted_calls_before_the_next_provider_request() {
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, AgentMessageSource, MessageDelivery, MessageId,
        MessageOptions,
    };
    use rsi_agent_turn_protocol::SubmitMessage;
    use rsi_ai_protocol::{MessageContent, MessageRole};
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let _tool = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::GatedStream {
                events: tool_calls_script(&[
                    ("obsolete-a", "echo", "{}"),
                    ("obsolete-b", "echo", "{}"),
                ]),
                waiting_after_first: entered.clone(),
                release: release.clone(),
            },
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.language.supersession", provider.clone())
        .await;
    let executor = stack.activate_executor("supersession").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let id = SessionId::new("supersession").unwrap();
    let header = header_for_session("supersession");
    turns
        .submit_message(SubmitMessage {
            session: stack.fresh(header.clone()).await,
            delivery: MessageDelivery::NextTurn,
            message: AgentMessage {
                message_id: MessageId::new("initial").unwrap(),
                source: AgentMessageSource::Human { attribution: None },
                content: vec![AgentMessageContent::Text {
                    text: "original".into(),
                }],
                options: MessageOptions::default(),
            },
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let turn = stack
        .store
        .inspect_session(&id)
        .await
        .unwrap()
        .active_turn_id
        .unwrap();
    turns
        .submit_message(SubmitMessage {
            session: SubmitSession::Resume(turns.prepare_resume(&id).await.unwrap()),
            delivery: MessageDelivery::NextStep,
            message: AgentMessage {
                message_id: MessageId::new("steer").unwrap(),
                source: AgentMessageSource::Human { attribution: None },
                content: vec![AgentMessageContent::Text {
                    text: "new direction".into(),
                }],
                options: MessageOptions::default(),
            },
        })
        .await
        .unwrap();
    release.notify_one();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(outcome) = turns.outcome(&id, &turn).await.unwrap() {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let mut pending = std::collections::BTreeSet::new();
    for message in requests[1].messages() {
        if message.role() != MessageRole::Tool {
            assert!(pending.is_empty());
        }
        for content in message.content() {
            match content {
                MessageContent::ToolCall(call) => {
                    assert!(pending.insert(call.id.clone()));
                }
                MessageContent::ToolResult {
                    call_id,
                    content,
                    is_error,
                } => {
                    assert!(pending.remove(call_id));
                    assert!(is_error);
                    assert!(
                        serde_json::to_string(content)
                            .unwrap()
                            .contains("newer input superseded")
                    );
                }
                _ => {}
            }
        }
    }
    assert!(pending.is_empty());
    let facts = stack
        .store
        .read_facts(&id, 0, 128, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ToolCallsSuperseded { .. }))
            .count(),
        1
    );
    assert!(!facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolRejected { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolResult { .. }
    )));
    let marker = facts
        .iter()
        .position(|fact| matches!(fact.body(), SessionFactBody::ToolCallsSuperseded { .. }))
        .unwrap();
    assert!(matches!(
        facts[marker + 1].body(),
        SessionFactBody::StepEnded { .. }
    ));
    assert!(matches!(
        facts[marker + 2].body(),
        SessionFactBody::StepStarted { .. }
    ));
    assert!(matches!(
        facts[marker + 3].body(),
        SessionFactBody::InputMessageClaimed { .. }
    ));
    let mut replay = ModelContextState::open(
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        header,
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    // Stop before the final answer: compare the exact request sent after steering.
    let last_model = facts
        .iter()
        .rposition(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
        .unwrap();
    let prefix = facts[..last_model]
        .iter()
        .cloned()
        .map(Arc::new)
        .collect::<Vec<_>>();
    replay.ingest(ContextPage::Canonical(&prefix)).unwrap();
    let options = rsi_ai_protocol::LanguageRequestOptions::new(
        requests[1].tools().to_vec(),
        requests[1].tool_choice().clone(),
        requests[1].hosted_tools().to_vec(),
        requests[1].response_format().clone(),
        requests[1].settings().clone(),
        requests[1].extensions().to_vec(),
    )
    .unwrap();
    let rebuilt = replay
        .build(options.clone(), &context_test_profile())
        .unwrap();
    assert_eq!(rebuilt, requests[1]);
    assert_eq!(
        replay
            .restored(&replay.checkpoint().unwrap())
            .unwrap()
            .build(options, &context_test_profile())
            .unwrap(),
        requests[1]
    );
    stack.dispose(language, executor).await;
}

#[derive(Debug)]
pub(super) struct UncertainTool(pub(super) Arc<AtomicUsize>);
#[async_trait]
impl ToolExecutor for UncertainTool {
    async fn execute(&self, _: Value, _: ToolExecution) -> ToolResultType<ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ToolError::OutcomeUnknown)
    }
}

#[tokio::test]
async fn uncertain_tool_interrupts_without_model_result_or_replay_and_retires_after_durability() {
    let stack = BaseStack::activate().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "uncertain", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(UncertainTool(calls.clone())),
        })
        .unwrap();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.language.uncertain", provider.clone())
        .await;
    let executor = stack.activate_executor("executor-uncertain").await;
    let (submitted, outcome) = stack.submit_and_wait("run once").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Interrupted {
            effect: Some(rsi_agent_session_protocol::EffectKind::Tool),
            ..
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64, FACT_PAGE_BYTES)
        .await
        .unwrap()
        .facts;
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
    );
    let identity = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolStarted { identity, .. } => Some(identity.clone()),
            _ => None,
        })
        .expect("uncertain effect has a durable start");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stack.tool_runtime().query(&identity).unwrap() != RetainedToolResult::Absent {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(lease);
    stack.dispose(language, executor).await;
}
