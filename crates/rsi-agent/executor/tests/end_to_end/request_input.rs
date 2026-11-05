use super::*;
use rsi_agent_composition_protocol::{
    RequestInputContext, RequestInputPreparer, RequestToolCatalog, RequestToolSource,
};
use rsi_agent_session_protocol::{
    InputClaimRef, MessageAttribution, PreparedInput, PreparedInputMessage, PreparedInputRole,
    StepInputDecision,
};

#[derive(Clone, Copy, Debug)]
enum Decision {
    Rewrite,
    Reject,
    Empty,
    ResponseLost,
    CaptureLost,
    ColdReject,
    ColdCancel,
    PresetReject,
    Mixed,
    MixedFailure,
}

#[derive(Debug)]
struct Preparation {
    decision: Decision,
    calls: AtomicUsize,
}

#[async_trait]
impl RequestInputPreparer for Preparation {
    async fn prepare(
        &self,
        context: &RequestInputContext<'_>,
        _: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<StepInputDecision> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(!context.claims.is_empty());
        assert!(
            context
                .claims
                .iter()
                .all(|fact| fact.seq() <= context.fact_horizon)
        );
        Ok(match self.decision {
            Decision::CaptureLost | Decision::ColdCancel => {
                unreachable!("capture failed before preparation")
            }
            Decision::PresetReject => panic!("application cannot revive rejected preset input"),
            Decision::Mixed => {
                let [PreparedInput::Added { message }] =
                    context.previous.unwrap().messages().unwrap()
                else {
                    panic!("missing preset edit")
                };
                assert_eq!(message.attribution().id, "preset-edit");
                let wire = serde_json::to_value(context).unwrap();
                assert_eq!(wire["claims"], json!([]));
                assert!(
                    wire["previous"].is_object()
                        && wire["contributed"].as_array().unwrap().len() == 1
                );
                assert!(matches!(
                    context.contributed,
                    [SessionFactBody::InputMessageClaimed { .. }]
                ));
                StepInputDecision::enter(vec![
                    PreparedInput::Claimed {
                        claim: InputClaimRef::preceding(1 + context.trailing_facts).unwrap(),
                    },
                    PreparedInput::Added {
                        message: message.clone(),
                    },
                ])
                .unwrap()
            }
            Decision::ResponseLost | Decision::MixedFailure => {
                return Err(
                    rsi_agent_composition_protocol::AgentCompositionError::InvalidInput(
                        "response lost after preparation".into(),
                    ),
                );
            }
            Decision::Reject | Decision::ColdReject => StepInputDecision::reject(),
            Decision::Empty => StepInputDecision::enter(vec![]).unwrap(),
            Decision::Rewrite => StepInputDecision::enter(vec![
                PreparedInput::Replacement {
                    claim: InputClaimRef::new(context.claims[0].seq(), 0).unwrap(),
                    message: PreparedInputMessage::new(
                        PreparedInputRole::User,
                        MessageAttribution {
                            id: "original-human".into(),
                            source: json!({"kind":"user"}),
                        },
                        vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                            text: "rewritten by preparation".into(),
                        }],
                    )
                    .unwrap(),
                },
                PreparedInput::Added {
                    message: PreparedInputMessage::new(
                        PreparedInputRole::User,
                        MessageAttribution {
                            id: "empty-rewrite".into(),
                            source: json!({"kind":"context"}),
                        },
                        vec![],
                    )
                    .unwrap(),
                },
            ])
            .unwrap(),
        })
    }
}

#[derive(Debug)]
struct Source {
    catalogs: Arc<dyn rsi_tools_protocol::ToolCatalogProvider>,
    preparation: Arc<Preparation>,
    captures: AtomicUsize,
    startup: Option<(tokio::sync::Notify, tokio::sync::Notify)>,
}
#[async_trait]
impl RequestToolSource for Source {
    async fn await_input_preparer(
        &self,
        _: &SessionHeader,
    ) -> rsi_agent_composition_protocol::Result<bool> {
        if let Some((waiting, release)) = &self.startup {
            waiting.notify_one();
            release.notified().await;
        }
        Ok(true)
    }
    async fn capture(
        &self,
        _: &SessionHeader,
        _: &rsi_agent_session_protocol::ModelSelection,
        _: &std::collections::BTreeSet<String>,
        _: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<RequestToolCatalog> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        if matches!(self.preparation.decision, Decision::CaptureLost) {
            return Err(rsi_agent_composition_protocol::AgentCompositionError::ShuttingDown);
        }
        Ok(RequestToolCatalog {
            middleware: None,
            generation: "prepared-input".into(),
            receipt_source: None,
            registrations: std::collections::BTreeMap::default(),
            runtime: self.catalogs.begin_stage().unwrap().seal().unwrap(),
            instructions: String::new(),
            inputs: Some(self.preparation.clone()),
        })
    }
}

#[tokio::test]
async fn application_preparation_rewrites_before_the_request_and_reuses_the_decision_on_retry() {
    exercise(Decision::Rewrite, TurnOutcome::Completed, 2).await;
}

#[tokio::test]
async fn rejected_or_initially_empty_application_input_spends_no_model_request() {
    exercise(Decision::Reject, TurnOutcome::Blocked, 0).await;
    exercise(Decision::Empty, TurnOutcome::Completed, 0).await;
}

#[tokio::test]
async fn uncertain_application_preparation_is_not_replayed_and_spends_no_model_request() {
    for decision in [Decision::ResponseLost, Decision::MixedFailure] {
        exercise(
            decision,
            TurnOutcome::Failed {
                code: "input.failed".into(),
                message: "invalid Agent composition input: response lost after preparation".into(),
            },
            0,
        )
        .await;
    }
}

#[tokio::test]
async fn lost_required_input_capture_cannot_send_the_unprepared_prompt() {
    exercise(
        Decision::CaptureLost,
        TurnOutcome::Failed {
            code: "application.input_unavailable".into(),
            message: rsi_agent_composition_protocol::AgentCompositionError::ShuttingDown
                .to_string(),
        },
        0,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn cold_input_owner_can_reject_after_startup_outlasts_the_step_deadline() {
    exercise(Decision::ColdReject, TurnOutcome::Blocked, 0).await;
}

#[tokio::test(start_paused = true)]
async fn cancelling_cold_input_does_not_wait_for_the_application_or_dispatch_to_a_provider() {
    exercise(Decision::ColdCancel, TurnOutcome::Cancelled, 0).await;
}

#[derive(Debug)]
struct Preset(Decision);

#[async_trait]
impl rsi_agent_composition_protocol::BeforeStepContributor for Preset {
    async fn prepare(
        &self,
        context: &rsi_agent_composition_protocol::BeforeStepContext<'_>,
        _: CancellationToken,
    ) -> rsi_agent_composition_protocol::ContributionResult<
        rsi_agent_composition_protocol::BeforeStepOutput,
    > {
        assert_eq!(
            &context.selection.model,
            context.captured.header.settings().default_model()
        );
        let (decision, inputs) = if matches!(self.0, Decision::Mixed | Decision::MixedFailure) {
            let message = PreparedInputMessage::new(
                PreparedInputRole::User,
                MessageAttribution {
                    id: "preset-edit".into(),
                    source: json!({"kind":"user"}),
                },
                vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                    text: "preset edit".into(),
                }],
            )
            .unwrap();
            (
                StepInputDecision::enter(vec![PreparedInput::Added { message }]).unwrap(),
                vec![rsi_agent_composition_protocol::ContributionInput::context(
                    "mixed native context",
                )],
            )
        } else {
            (StepInputDecision::reject(), vec![])
        };
        Ok(rsi_agent_composition_protocol::BeforeStepOutput {
            decision: Some(decision),
            contribution: rsi_agent_composition_protocol::ContributionOutput {
                inputs,
                domains: vec![],
            },
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn preset_rejection_precedes_application_input_and_provider_dispatch() {
    exercise(Decision::PresetReject, TurnOutcome::Blocked, 0).await;
}

#[tokio::test]
async fn application_composes_preset_edit_and_same_batch_context_once_across_retry() {
    exercise(Decision::Mixed, TurnOutcome::Completed, 2).await;
}

async fn install_preset(stack: &BaseStack, decision: Decision) -> FiberHandle {
    use rsi_agent_composition_protocol::{ContributionKind, ContributionRegistration};
    contributions::install(
        stack,
        vec![ContributionRegistration::new(
            rsi_agent_session_protocol::ContributionId::new("fixture.reject").unwrap(),
            0,
            ContributionKind::BeforeStep(Arc::new(Preset(decision))),
        )],
    )
    .await
}

async fn submit_when_ready(
    stack: &BaseStack,
    source: &Source,
    provider: &LanguageFixture,
) -> (SubmittedTurn, TurnOutcome) {
    if let Some((waiting, release)) = &source.startup {
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = stack
            .submit_fresh(&turns, "cold-input", "UNENTERED original input")
            .await;
        waiting.notified().await;
        tokio::time::advance(std::time::Duration::from_secs(45)).await;
        assert_eq!(provider.starts.load(Ordering::SeqCst), 0);
        assert_eq!(source.captures.load(Ordering::SeqCst), 0);
        if matches!(source.preparation.decision, Decision::ColdCancel) {
            turns
                .cancel(&submitted.session_id, &submitted.turn_id, None)
                .await
                .unwrap();
        } else {
            release.notify_one();
        }
        let outcome = wait_for_outcome(&turns, &submitted).await;
        (submitted, outcome)
    } else {
        stack.submit_and_wait("UNENTERED original input").await
    }
}

fn assert_entered_model_requests(provider: &LanguageFixture, starts: usize, decision: Decision) {
    let requests = provider.requests.lock().unwrap();
    for request in requests.iter() {
        let encoded = serde_json::to_string(request).unwrap();
        if matches!(decision, Decision::Mixed) {
            assert!(
                encoded.find("mixed native context").unwrap()
                    < encoded.find("preset edit").unwrap()
            );
        } else {
            assert!(encoded.contains("rewritten by preparation"));
        }
        assert!(!encoded.contains("UNENTERED"));
    }
    if starts > 0 {
        assert_eq!(requests[0], requests[1]);
    }
}

async fn exercise(decision: Decision, expected: TurnOutcome, starts: usize) {
    let stack = BaseStack::activate().await;
    let preset = if matches!(
        decision,
        Decision::PresetReject | Decision::Mixed | Decision::MixedFailure
    ) {
        Some(install_preset(&stack, decision).await)
    } else {
        None
    };
    let preparation = Arc::new(Preparation {
        decision,
        calls: AtomicUsize::new(0),
    });
    let source = Arc::new(Source {
        catalogs: stack
            .runtime
            .root()
            .lookup_local::<ToolCatalogProviderContract>()
            .unwrap(),
        preparation: preparation.clone(),
        captures: AtomicUsize::new(0),
        startup: matches!(decision, Decision::ColdReject | Decision::ColdCancel)
            .then(Default::default),
    });
    let provider = retrying_provider(&stack);
    let language = stack
        .activate_language("test.language", provider.clone())
        .await;
    let executor = stack.activate_executor("executor-input-preparation").await;
    let worker = activate_fixture(
        &stack.runtime,
        "test.request-input",
        "app",
        Arc::new(super::tool_execution::RequestSourceFactory(source.clone())),
    )
    .await;
    let (submitted, outcome) = submit_when_ready(&stack, &source, &provider).await;
    assert_eq!(outcome, expected);
    assert_eq!(provider.starts.load(Ordering::SeqCst), starts);
    assert_eq!(
        preparation.calls.load(Ordering::SeqCst),
        usize::from(!matches!(
            decision,
            Decision::PresetReject | Decision::CaptureLost | Decision::ColdCancel
        ))
    );
    assert_eq!(
        source.captures.load(Ordering::SeqCst),
        usize::from(!matches!(decision, Decision::ColdCancel))
    );
    assert_entered_model_requests(&provider, starts, decision);
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
    assert_eq!(
        page.facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::StepInputsPrepared { .. }))
            .count(),
        usize::from(!matches!(
            decision,
            Decision::ResponseLost
                | Decision::MixedFailure
                | Decision::CaptureLost
                | Decision::ColdCancel
        ))
    );
    assert_eq!(
        page.facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
            .count(),
        starts
    );
    if matches!(decision, Decision::MixedFailure) {
        assert!(!page.facts.iter().any(|f| matches!(
            f.body(),
            SessionFactBody::StepPrepared { .. }
                | SessionFactBody::InputMessageClaimed { .. }
                | SessionFactBody::StepInputsPrepared { .. }
        )));
    }
    assert!(worker.dispose().await.is_clean());
    if let Some(preset) = preset {
        assert!(preset.dispose().await.is_clean());
    }
    stack.dispose(language, executor).await;
}

fn retrying_provider(stack: &BaseStack) -> Arc<LanguageFixture> {
    let retry = AiError::new(
        ErrorKind::RateLimited,
        ErrorPhase::Connect,
        DispatchStatus::NotDispatched,
        "retry",
    )
    .unwrap();
    Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Error(retry),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    })
}
