use async_trait::async_trait;
use rsi_agent_composition_protocol::{ContributionResult, PromptInput};
use rsi_agent_prompt_assembly::*;
use rsi_agent_session_protocol::{
    AgentPresetId, ContributionId, FrozenAgentSettings, ModelSelection, SessionHeader, SessionId,
};
use rsi_ai_protocol::ModelRef;
use rsi_meta::Runtime;
use rsi_meta_scope::ScopeRoot;
use rsi_sandbox::SandboxMode;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

fn header() -> SessionHeader {
    SessionHeader::new_local(
        SessionId::new("prompt-test").unwrap(),
        1,
        "/project",
        AgentPresetId::new("standard").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "",
            ModelRef::new("fixture", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
fn text(name: &str, order: f64, text: &str) -> TextDeclaration {
    TextDeclaration {
        name: ContributionId::new(name).unwrap(),
        order,
        text: PromptText::Literal(text.into()),
        interpolate: true,
    }
}
fn input<'a>(
    header: &'a SessionHeader,
    model: &'a ModelSelection,
    scope: Option<&'a rsi_meta_scope::ScopeKey>,
) -> PromptInput<'a> {
    PromptInput {
        header,
        model,
        scope,
        instructions: &[],
        tools: &[],
        known_tools: &[],
    }
}
fn prompt(assembly: PromptAssembly) -> String {
    render(assembly)
        .unwrap()
        .sections
        .into_iter()
        .map(|section| section.text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[tokio::test]
async fn scoped_shadowing_exact_removal_and_duplicate_registration() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(16).unwrap();
    let global = root.create(&runtime.root()).await.unwrap();
    let parent = root.create(&runtime.root()).await.unwrap();
    let (child, _parent) = root
        .create_child(&runtime.root(), parent.key())
        .await
        .unwrap();
    let registry = PromptRegistry::new(root, 8, None).unwrap();
    let _registration_1 = registry
        .variable(
            global.context().meta(),
            "place".into(),
            PromptText::Literal("global".into()),
        )
        .await
        .unwrap();
    let _registration_2 = registry
        .section(
            global.context().meta(),
            PromptSection {
                declaration: text("greeting", 1.0, "Hello {{place}}"),
                complete: false,
            },
        )
        .await
        .unwrap();
    let _registration_3 = registry
        .variable(
            parent.context(),
            "place".into(),
            PromptText::Literal("parent".into()),
        )
        .await
        .unwrap();
    let removal = registry
        .variable(
            child.context(),
            "place".into(),
            PromptText::Literal("child".into()),
        )
        .await
        .unwrap();
    assert!(
        registry
            .variable(
                child.context(),
                "place".into(),
                PromptText::Literal("duplicate".into())
            )
            .await
            .is_err()
    );
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let input = input(&header, &model, Some(child.key()));
    let token = CancellationToken::new();
    assert_eq!(
        prompt(registry.presentation(&input, &token).await.unwrap()),
        "Hello child"
    );
    assert!(removal.dispose().await.is_clean());
    assert_eq!(
        prompt(registry.presentation(&input, &token).await.unwrap()),
        "Hello parent"
    );
    assert!(child.dispose().await.is_clean());
    assert!(parent.dispose().await.is_clean());
    assert!(global.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct Around {
    name: &'static str,
    trace: Arc<Mutex<Vec<String>>>,
    add_section: bool,
}
#[async_trait]
impl PresentationTransform for Around {
    async fn transform(
        &self,
        presentation: PromptPresentation,
        input: &PromptInput<'_>,
        next: PromptContinuation<'_>,
    ) -> ContributionResult<PromptPresentation> {
        self.trace
            .lock()
            .unwrap()
            .push(format!("{}:enter", self.name));
        tokio::task::yield_now().await;
        let mut result = next.run(presentation, input).await?;
        self.trace
            .lock()
            .unwrap()
            .push(format!("{}:exit", self.name));
        if self.add_section {
            result.sections.push(PresentedText {
                name: "unwanted".into(),
                text: "discard transform".into(),
                interpolate: false,
            });
        }
        Ok(result)
    }
}

#[tokio::test]
async fn complete_section_wins_after_awaited_transforms_in_actual_registration_order() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(16).unwrap();
    let global = root.create(&runtime.root()).await.unwrap();
    let scoped = root.create(&runtime.root()).await.unwrap();
    let registry = PromptRegistry::new(root, 8, None).unwrap();
    let _registration_5 = registry
        .variable(
            global.context().meta(),
            "name".into(),
            PromptText::Literal("native".into()),
        )
        .await
        .unwrap();
    let _registration_6 = registry
        .section(
            global.context().meta(),
            PromptSection {
                declaration: text("ordinary", 0.0, "discard ordinary"),
                complete: false,
            },
        )
        .await
        .unwrap();
    let _registration_7 = registry
        .section(
            scoped.context(),
            PromptSection {
                declaration: text("complete", 1.0, "Only {{name}}"),
                complete: true,
            },
        )
        .await
        .unwrap();
    let trace = Arc::new(Mutex::new(vec![]));
    // A scoped callback registered first must remain outside a later global one.
    let _registration_8 = registry
        .transform(
            scoped.context(),
            Arc::new(Around {
                name: "outer",
                trace: trace.clone(),
                add_section: true,
            }),
        )
        .await
        .unwrap();
    let _registration_9 = registry
        .transform(
            global.context().meta(),
            Arc::new(Around {
                name: "inner",
                trace: trace.clone(),
                add_section: false,
            }),
        )
        .await
        .unwrap();
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let input = input(&header, &model, Some(scoped.key()));
    assert_eq!(
        prompt(
            registry
                .presentation(&input, &CancellationToken::new())
                .await
                .unwrap()
        ),
        "Only native"
    );
    assert_eq!(
        *trace.lock().unwrap(),
        ["outer:enter", "inner:enter", "inner:exit", "outer:exit"]
    );
    let _registration_10 = registry
        .section(
            scoped.context(),
            PromptSection {
                declaration: text("second-complete", 2.0, "invalid"),
                complete: true,
            },
        )
        .await
        .unwrap();
    assert!(
        registry
            .presentation(&input, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct Sample {
    value: Arc<Mutex<String>>,
    calls: Arc<AtomicUsize>,
}
impl TextProvider for Sample {
    fn resolve(&self, _: &PromptInput<'_>) -> ContributionResult<Option<String>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some(self.value.lock().unwrap().clone()))
    }
}

#[derive(Debug)]
struct ChangeContext(Arc<Mutex<String>>);
#[async_trait]
impl PresentationTransform for ChangeContext {
    async fn transform(
        &self,
        presentation: PromptPresentation,
        input: &PromptInput<'_>,
        next: PromptContinuation<'_>,
    ) -> ContributionResult<PromptPresentation> {
        let mut result = next.run(presentation, input).await?;
        tokio::task::yield_now().await;
        *self.0.lock().unwrap() = "fresh provider value".into();
        result.contexts[0].text = "overwritten edit".into();
        result.contexts.push(PresentedText {
            name: "transform-only".into(),
            text: "retained edit".into(),
            interpolate: false,
        });
        Ok(result)
    }
}

#[tokio::test]
async fn native_admission_samples_once_unless_a_transform_can_change_context() {
    use rsi_agent_composition_protocol::PromptAssembler;
    for transformed in [false, true] {
        let runtime = Runtime::default();
        let root = ScopeRoot::new(4).unwrap();
        let owner = root.create(&runtime.root()).await.unwrap();
        let registry = PromptRegistry::new(root, 4, None).unwrap();
        let value = Arc::new(Mutex::new("initial provider value".into()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut declaration = text("live", 0.0, "");
        declaration.text = PromptText::Provider(Arc::new(Sample {
            value: value.clone(),
            calls: calls.clone(),
        }));
        let _context = registry
            .context(
                owner.context(),
                PromptContext {
                    declaration,
                    required: true,
                },
            )
            .await
            .unwrap();
        let _transform = if transformed {
            Some(
                registry
                    .transform(owner.context(), Arc::new(ChangeContext(value)))
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let header = header();
        let model = ModelSelection::baseline(header.settings());
        let prepared = registry
            .assemble(
                input(&header, &model, Some(owner.key())),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            if transformed { 2 } else { 1 }
        );
        assert!(!prepared.runtime_context.contains("overwritten edit"));
        if transformed {
            assert!(prepared.runtime_context.contains("fresh provider value"));
            assert!(prepared.runtime_context.contains("retained edit"));
            assert!(!prepared.runtime_context.contains("initial provider value"));
        } else {
            assert!(prepared.runtime_context.contains("initial provider value"));
        }
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn refresh_preserves_accepted_sections_variables_and_tool_order() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(16).unwrap();
    let owner = root.create(&runtime.root()).await.unwrap();
    let registry = PromptRegistry::new(root, 8, None).unwrap();
    let value = Arc::new(Mutex::new("before".into()));
    let fixed = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let _registration_11 = registry
        .variable(
            owner.context(),
            "fixed".into(),
            PromptText::Provider(Arc::new(Sample {
                value: value.clone(),
                calls: fixed.clone(),
            })),
        )
        .await
        .unwrap();
    let _registration_12 = registry
        .section(
            owner.context(),
            PromptSection {
                declaration: text("fixed", 0.0, "{{fixed}}"),
                complete: false,
            },
        )
        .await
        .unwrap();
    let mut declaration = text("live", 0.0, "");
    declaration.text = PromptText::Provider(Arc::new(Sample {
        value: value.clone(),
        calls: live.clone(),
    }));
    let _registration_13 = registry
        .context(
            owner.context(),
            PromptContext {
                declaration,
                required: true,
            },
        )
        .await
        .unwrap();
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let input = input(&header, &model, Some(owner.key()));
    let mut assembly = registry
        .presentation(&input, &CancellationToken::new())
        .await
        .unwrap();
    *value.lock().unwrap() = "after".into();
    registry.refresh_context(&mut assembly, &input).unwrap();
    assert_eq!(assembly.presentation.contexts[0].text, "after");
    assert_eq!(prompt(assembly), "before");
    assert_eq!(fixed.load(Ordering::Relaxed), 1);
    assert_eq!(live.load(Ordering::Relaxed), 2);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn suppression_restores_required_context_and_never_runs_optional_provider() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(16).unwrap();
    let owner = root.create(&runtime.root()).await.unwrap();
    let registry = PromptRegistry::new(root, 8, None).unwrap();
    let optional = Arc::new(AtomicUsize::new(0));
    let _registration_14 = registry
        .context(
            owner.context(),
            PromptContext {
                declaration: text("directory", 0.0, "Directory: /project"),
                required: true,
            },
        )
        .await
        .unwrap();
    let mut declaration = text("optional", 1.0, "");
    declaration.text = PromptText::Provider(Arc::new(Sample {
        value: Arc::new(Mutex::new("unused".into())),
        calls: optional.clone(),
    }));
    let _registration_15 = registry
        .context(
            owner.context(),
            PromptContext {
                declaration,
                required: false,
            },
        )
        .await
        .unwrap();
    let _registration_16 = registry
        .suppress_runtime_context(owner.context())
        .await
        .unwrap();
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let input = input(&header, &model, Some(owner.key()));
    let mut assembly = registry
        .presentation(&input, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(assembly.presentation.contexts.len(), 1);
    assembly.presentation.contexts.clear();
    registry.refresh_context(&mut assembly, &input).unwrap();
    assert_eq!(
        assembly.presentation.contexts[0].text,
        "Directory: /project"
    );
    assert_eq!(optional.load(Ordering::Relaxed), 0);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn order_uses_pre_restriction_names_without_granting_authority() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(16).unwrap();
    let registry = PromptRegistry::new(
        root.clone(),
        8,
        Some(vec!["z".into(), "hidden".into(), TOOL_ORDER_REST.into()]),
    )
    .unwrap();
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let tools = vec!["z".into(), "a".into(), "m".into()];
    let known = vec!["a".into(), "hidden".into(), "m".into(), "z".into()];
    let mut input = input(&header, &model, None);
    input.tools = &tools;
    input.known_tools = &known;
    assert_eq!(
        registry
            .presentation(&input, &CancellationToken::new())
            .await
            .unwrap()
            .tool_order,
        ["z", "a", "m"]
    );
    input.known_tools = &[];
    assert!(
        registry
            .presentation(&input, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(PromptRegistry::new(root.clone(), 8, Some(vec![])).is_err());
    assert!(
        PromptRegistry::new(
            root,
            8,
            Some(vec![TOOL_ORDER_REST.into(), TOOL_ORDER_REST.into()])
        )
        .is_err()
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[test]
fn strict_interpolation_preserves_literals_and_never_rescans_values() {
    let assemble = |text: &str, interpolate| PromptAssembly {
        presentation: PromptPresentation {
            sections: vec![PresentedText {
                name: "probe".into(),
                text: text.into(),
                interpolate,
            }],
            contexts: vec![],
            variables: [
                ("literal".into(), Some("{{unknown}}".into())),
                ("unset".into(), None),
            ]
            .into_iter()
            .collect(),
        },
        tool_order: vec![],
    };
    assert_eq!(prompt(assemble("{{literal}}", true)), "{{unknown}}");
    assert_eq!(prompt(assemble("a {{unclosed", true)), "a {{unclosed");
    assert_eq!(prompt(assemble("{{raw}}", false)), "{{raw}}");
    for text in ["{{missing}}", "{{unset}}", "{{ bad }}", "{{x{bad}}", "{{}}"] {
        assert!(render(assemble(text, true)).is_err(), "{text}");
    }
}

#[test]
fn plugin_configuration_accepts_complete_sections_and_rejects_unknown_fields() {
    use rsi_meta::PluginFactory as _;
    let factory = PromptAssemblyFactory;
    assert!(factory.prepare(&serde_json::json!({"sections":[{"name":"complete", "order":0, "text":"{{value}}", "complete":true}],
        "variables":{"value":"configured"}, "contexts":[{"name":"required", "order":0, "text":"runtime", "required":true}]})).is_ok());
    assert!(factory.prepare(&serde_json::json!({"sections":[{"name":"complete", "order":0, "text":"text", "typo":true}]})).is_err());
}

#[tokio::test]
async fn ordinary_plugin_activates_and_supplies_default_runtime_context() {
    use rsi_agent_composition_protocol::{
        AgentPromptScope, AgentPromptScopeContract, PromptAssemblerContract,
    };
    use rsi_meta::{
        ActivationPlan, ConfigValue, FiberState, PluginFactory, PreparedActivation,
        ResolvedFactory, UpdateMode,
    };
    #[derive(Debug)]
    struct Supply(Arc<AgentPromptScope>);
    #[async_trait]
    impl PluginFactory for Supply {
        fn prepare(&self, value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(value.clone()))
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            let lease = plan
                .context()
                .provide_local::<AgentPromptScopeContract>(self.0.clone())?;
            plan.defer(
                "scope supply",
                Box::new(move || {
                    Box::pin(async move {
                        drop(lease);
                        Ok(())
                    })
                }),
            )
        }
    }
    let runtime = Runtime::default();
    let root = ScopeRoot::new(4).unwrap();
    let scope = root.create(&runtime.root()).await.unwrap();
    let owner = scope.context().meta();
    let supply = owner
        .apply(
            ResolvedFactory::linked(
                "scope",
                "1",
                UpdateMode::RestartRequired,
                Arc::new(Supply(Arc::new(AgentPromptScope {
                    root,
                    key: scope.key().clone(),
                }))),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        supply.snapshot().state,
        FiberState::Active,
        "{:?}",
        supply.snapshot()
    );
    let plugin = owner
        .apply(
            ResolvedFactory::linked(
                "prompt",
                "1",
                UpdateMode::RestartRequired,
                Arc::new(PromptAssemblyFactory),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        plugin.snapshot().state,
        FiberState::Active,
        "{:?}",
        plugin.snapshot()
    );
    let assembler = owner.lookup_local::<PromptAssemblerContract>().unwrap();
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let prepared = assembler
        .assemble(
            input(&header, &model, Some(scope.key())),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        prepared
            .runtime_context
            .contains("Working directory: /project")
    );
    assert!(scope.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct WaitForCancellation(Arc<tokio::sync::Notify>);
#[async_trait]
impl PresentationTransform for WaitForCancellation {
    async fn transform(
        &self,
        presentation: PromptPresentation,
        input: &PromptInput<'_>,
        next: PromptContinuation<'_>,
    ) -> ContributionResult<PromptPresentation> {
        self.0.notify_one();
        next.cancellation().cancelled().await;
        next.run(presentation, input).await
    }
}

#[tokio::test]
async fn cancellation_during_await_does_not_replay_providers_or_keep_effects_alive() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(4).unwrap();
    let owner = root.create(&runtime.root()).await.unwrap();
    let registry = PromptRegistry::new(root, 4, None).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let _registration_17 = registry
        .transform(
            owner.context(),
            Arc::new(WaitForCancellation(entered.clone())),
        )
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let _registration_18 = registry
        .variable(
            owner.context(),
            "value".into(),
            PromptText::Provider(Arc::new(Sample {
                value: Arc::new(Mutex::new("one".into())),
                calls: calls.clone(),
            })),
        )
        .await
        .unwrap();
    let header = header();
    let model = ModelSelection::baseline(header.settings());
    let input = input(&header, &model, Some(owner.key()));
    let token = CancellationToken::new();
    let (result, ()) = tokio::join!(registry.presentation(&input, &token), async {
        entered.notified().await;
        token.cancel();
    });
    assert!(matches!(
        result,
        Err(rsi_agent_composition_protocol::ContributionError::Closed)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
