use super::*;
use rsi_agent_composition_protocol::{
    AgentPromptScopeContract, ContributionResult, PreparedPrompt, PromptAssembler,
    PromptAssemblerContract, PromptInput,
};
use rsi_agent_session_protocol::{ContributionId, StepInstruction};

#[tokio::test]
async fn real_prompt_registry_is_private_to_overlapping_generations() {
    use rsi_agent_prompt_assembly::{PromptAssemblyFactory, PromptRegistryContract};
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("presets/default");
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join(COMPOSITION_FILE);
    let profile = |text| {
        format!(
            "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'context'\nplugin = 'test.context'\n[[steps]]\nkind = 'plugin'\nid = 'prompt'\nplugin = 'test.prompt'\nconfig = {{ sections = [{{name = 'complete', order = 0, text = '{text}', complete = true}}] }}\n"
        )
    };
    fs::write(&source, profile("old prompt")).unwrap();
    let id = AgentPresetId::new("default").unwrap();
    let presets = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(id.clone()).with_configured_root(
            AgentPresetRoot::new(temp.path().join("presets"), AgentPresetTrust::User).unwrap(),
        ),
        test_compiler(&temp),
    )
    .unwrap();
    let mut catalog = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.prompt",
            "1",
            UpdateMode::RestartRequired,
            Arc::new(PromptAssemblyFactory),
        ),
    ])
    .unwrap();
    catalog
        .register_local_contract::<AgentPromptScopeContract>()
        .unwrap();
    catalog
        .isolate_local_contract::<PromptRegistryContract>()
        .unwrap();
    let (runtime, tools, composition, service) = activate_composition(presets, catalog).await;
    let old = service.pin(&id, None).await.unwrap();
    fs::write(&source, profile("new prompt")).unwrap();
    let new = service.pin(&id, None).await.unwrap();
    assert!(!old.same_generation(&new));
    let header = rsi_agent_session_protocol::SessionHeader::new_local(
        rsi_agent_session_protocol::SessionId::new("overlap").unwrap(),
        1,
        "/project",
        id,
        rsi_agent_session_protocol::FrozenAgentSettings::new(
            "default",
            "header",
            rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            rsi_sandbox::SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let model = rsi_agent_session_protocol::ModelSelection::baseline(header.settings());
    for (pin, expected) in [(&old, "old prompt"), (&new, "new prompt")] {
        let (assembler, scope) = pin.prompt_assembler().unwrap();
        let result = assembler
            .assemble(
                PromptInput {
                    header: &header,
                    model: &model,
                    scope: Some(scope),
                    instructions: &[],
                    tools: &[],
                    known_tools: &[],
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.sections[0].text, expected);
        assert!(
            result
                .runtime_context
                .ends_with("Working directory: /project")
        );
    }
    drop((old, new));
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct Gate {
    entered: Notify,
    release: CancellationToken,
    dropped: AtomicUsize,
}
#[derive(Debug)]
struct Assembler {
    text: String,
    gate: Arc<Gate>,
}
#[async_trait::async_trait]
impl PromptAssembler for Assembler {
    async fn assemble(
        &self,
        input: PromptInput<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<PreparedPrompt> {
        self.gate.entered.notify_one();
        tokio::select! {
            () = self.gate.release.cancelled() => {},
            () = cancellation.cancelled() => return Err(rsi_agent_composition_protocol::ContributionError::Closed),
        }
        Ok(PreparedPrompt {
            sections: vec![StepInstruction {
                producer: ContributionId::new("fixture.prompt").unwrap(),
                text: self.text.clone(),
            }],
            runtime_context: String::new(),
            tool_order: input.tools.to_vec(),
        })
    }
}
#[derive(Debug)]
struct Factory(Arc<Gate>);
#[async_trait::async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()).requiring_local::<AgentPromptScopeContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let _scope = plan.local::<AgentPromptScopeContract>()?;
        let supply = plan
            .context()
            .provide_local::<PromptAssemblerContract>(Arc::new(Assembler {
                text: plan.config()["text"].as_str().unwrap().into(),
                gate: self.0.clone(),
            }))?;
        let gate = self.0.clone();
        plan.defer(
            "withdraw prompt",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    gate.dropped.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the awaited request and both generation transitions in one lifetime scenario.
async fn awaited_prompt_keeps_its_generation_and_scope_across_replacement_and_removal() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("presets/default");
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join(COMPOSITION_FILE);
    let profile = |text| {
        format!(
            "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'context'\nplugin = 'test.context'\n[[steps]]\nkind = 'plugin'\nid = 'prompt'\nplugin = 'test.prompt'\nconfig = {{ text = '{text}' }}\n"
        )
    };
    fs::write(&source, profile("old")).unwrap();
    let id = AgentPresetId::new("default").unwrap();
    let presets = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(id.clone()).with_configured_root(
            AgentPresetRoot::new(temp.path().join("presets"), AgentPresetTrust::User).unwrap(),
        ),
        test_compiler(&temp),
    )
    .unwrap();
    let gate = Arc::new(Gate::default());
    let mut catalog = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.prompt",
            "1",
            UpdateMode::Replayable,
            Arc::new(Factory(gate.clone())),
        ),
    ])
    .unwrap();
    catalog
        .register_local_contract::<AgentPromptScopeContract>()
        .unwrap();
    let (runtime, tools, composition, service) = activate_composition(presets, catalog).await;
    let old = service.pin(&id, None).await.unwrap();
    let old_scope = old.prompt_assembler().unwrap().1.clone();
    let cancel = CancellationToken::new();
    let pending = {
        let pin = old.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let header = rsi_agent_session_protocol::SessionHeader::new_local(
                rsi_agent_session_protocol::SessionId::new("prompt").unwrap(),
                1,
                "/project",
                AgentPresetId::new("default").unwrap(),
                rsi_agent_session_protocol::FrozenAgentSettings::new(
                    "default",
                    "header",
                    rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
                    rsi_sandbox::SandboxMode::WorkspaceWrite,
                    false,
                )
                .unwrap(),
            )
            .unwrap();
            let model = rsi_agent_session_protocol::ModelSelection::baseline(header.settings());
            let (assembler, scope) = pin.prompt_assembler().unwrap();
            assembler
                .assemble(
                    PromptInput {
                        header: &header,
                        model: &model,
                        scope: Some(scope),
                        instructions: &[],
                        tools: &[],
                        known_tools: &[],
                    },
                    cancel,
                )
                .await
        })
    };
    gate.entered.notified().await;
    fs::write(&source, profile("new")).unwrap();
    let new = service.pin(&id, None).await.unwrap();
    assert!(!old.same_generation(&new));
    assert_ne!(old_scope, *new.prompt_assembler().unwrap().1);
    drop(old);
    assert_eq!(
        gate.dropped.load(Ordering::SeqCst),
        0,
        "awaited work retains its exact old generation"
    );
    gate.release.cancel();
    assert_eq!(pending.await.unwrap().unwrap().sections[0].text, "old");
    fs::write(
        &source,
        "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'context'\nplugin = 'test.context'\n",
    )
    .unwrap();
    let removed = service.pin(&id, None).await.unwrap();
    assert!(removed.prompt_assembler().is_none());
    assert!(
        new.prompt_assembler().is_some(),
        "retained generation remains usable"
    );
    drop((removed, new));
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert_eq!(gate.dropped.load(Ordering::SeqCst), 2);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
