use super::*;
use rsi_agent_composition::{AgentCompositionSnapshot, AgentCompositionSource};
use rsi_agent_composition_protocol::{
    AgentGenerationInputsContract, AgentGenerationSeed, DomainDefinition, DomainRegistrarContract,
};
use rsi_agent_session_protocol::{DomainIdentity, DomainSnapshot, DomainStateValue};
#[derive(Debug, Default)]
struct SeedFactory {
    activated: Mutex<Vec<bool>>,
}
fn identity() -> DomainIdentity {
    DomainIdentity::new("fixture.manifest", 1).unwrap()
}
#[expect(
    clippy::ptr_arg,
    reason = "DomainDefinition requires a validator over its exact Vec state type."
)]
fn validate(names: &Vec<String>) -> Result<(), String> {
    if names.len() != 1 || !matches!(names[0].as_str(), "alpha" | "beta") {
        Err("invalid fixture manifest".into())
    } else {
        Ok(())
    }
}
#[async_trait::async_trait]
impl PluginFactory for SeedFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<AgentGenerationInputsContract>()
            .requiring_local::<DomainRegistrarContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let inputs = plan.local::<AgentGenerationInputsContract>()?;
        // The owner deliberately ignores the mismatch to test the final seal guard.
        if let Err(error) = inputs.seed_state(&identity()) {
            assert!(matches!(
                error,
                AgentCompositionError::UnsupportedSeedCodec { .. }
            ));
            return Ok(());
        }
        let snapshot = inputs
            .seed
            .find(&identity())
            .ok_or_else(|| MetaError::Activation("missing saved manifest".into()))?;
        let names: Vec<String> = serde_json::from_value(snapshot.state().value().clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let definition = DomainDefinition::new(identity(), &names, validate)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let (_, domain) = definition
            .register(
                plan.local::<DomainRegistrarContract>()?.as_ref(),
                &plan.context().registration_context()?,
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let tool = plan
            .local::<ToolRegistrarContract>()?
            .register(ToolRegistration {
                output: None,
                definition: ToolDefinition::new(&names[0], "frozen manifest tool", true.into())
                    .unwrap(),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1000 },
                executor: Arc::new(ProbeTool),
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        self.activated.lock().unwrap().push(inputs.restoring);
        plan.defer(
            "retire seeded definitions",
            Box::new(move || {
                Box::pin(async move {
                    drop((domain, tool));
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Source(Mutex<Arc<AgentCompositionSnapshot>>);
impl AgentCompositionSource for Source {
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>> {
        Ok(self.0.lock().unwrap().clone())
    }
}
fn seed(value: ConfigValue) -> AgentGenerationSeed {
    AgentGenerationSeed::new(vec![DomainSnapshot::new(
        identity(),
        DomainStateValue::new(value).unwrap(),
    )])
    .unwrap()
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One generation lifecycle proves cache reuse and isolation after failed restores."
)]
async fn saved_seed_reconstructs_exact_tools_before_sealing_and_partitions_cache_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("presets");
    let id = AgentPresetId::new("default").unwrap();
    fs::create_dir_all(root.join(id.as_str())).unwrap();
    fs::write(root.join(id.as_str()).join(COMPOSITION_FILE),"format=1\n[[steps]]\nkind='plugin'\nid='context'\nplugin='test.context'\n[[steps]]\nkind='plugin'\nid='manifest'\nplugin='test.domain'\n").unwrap();
    let presets = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(id.clone())
            .with_configured_root(AgentPresetRoot::new(root, AgentPresetTrust::User).unwrap()),
        test_compiler(&temp),
    )
    .unwrap();
    let factory = Arc::new(SeedFactory::default());
    let snapshot = AgentCompositionSnapshot::new(
        presets,
        AgentContributionCatalog::new([
            context_factory(),
            ResolvedFactory::linked("test.domain", "1", UpdateMode::Replayable, factory.clone()),
        ])
        .unwrap(),
    );
    let source = Arc::new(Source(Mutex::new(Arc::new(
        snapshot
            .clone()
            .with_generation_seed(seed(serde_json::json!(["alpha"]))),
    ))));
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let alpha = service.pin(&id, None).await.unwrap();
    let saved = AgentGenerationSeed::new(alpha.domains().baseline().to_vec()).unwrap();
    assert_eq!(alpha.tools().definitions()[0].name(), "alpha");
    *source.0.lock().unwrap() = Arc::new(
        snapshot
            .clone()
            .with_generation_seed(seed(serde_json::json!(["beta"]))),
    );
    let beta = service.pin(&id, None).await.unwrap();
    assert_eq!(beta.tools().definitions()[0].name(), "beta");
    assert_ne!(alpha.source_digest(), beta.source_digest());
    for _ in 0..4 {
        let restored = service.pin(&id, Some(&saved)).await.unwrap();
        assert_eq!(restored.tools().definitions()[0].name(), "alpha");
        assert_ne!(restored.source_digest(), alpha.source_digest());
        let fresh = service.pin(&id, None).await.unwrap();
        assert_eq!(fresh.source_digest(), beta.source_digest());
    }
    assert_eq!(
        *factory.activated.lock().unwrap(),
        [false, false, true],
        "alternating current and saved inputs must reuse their exact generations"
    );
    *source.0.lock().unwrap() =
        Arc::new(snapshot.with_unavailable_generation_seed("fixture integration needs refresh"));
    assert!(
        service
            .pin(&id, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture integration needs refresh"),
        "unavailable current input must report its cause and must not reuse beta"
    );
    let restored = service.pin(&id, Some(&saved)).await.unwrap();
    assert_eq!(restored.tools().definitions()[0].name(), "alpha");
    assert_eq!(restored.domains().baseline(), alpha.domains().baseline());
    let again = service.pin(&id, Some(&saved)).await.unwrap();
    assert_eq!(restored.source_digest(), again.source_digest());
    assert_eq!(*factory.activated.lock().unwrap(), [false, false, true]);
    let invalid = service
        .pin(&id, Some(&seed(serde_json::json!(false))))
        .await
        .unwrap_err();
    assert_eq!(
        invalid.to_string(),
        "Agent preset default is unavailable: activating an Agent contribution failed"
    );
    let wrong_codec = AgentGenerationSeed::new(vec![DomainSnapshot::new(
        DomainIdentity::new("fixture.manifest", 2).unwrap(),
        DomainStateValue::new("private-state".into()).unwrap(),
    )])
    .unwrap();
    let error = service.pin(&id, Some(&wrong_codec)).await.unwrap_err();
    assert_eq!(
        error,
        AgentCompositionError::UnsupportedSeedCodec {
            stored: DomainIdentity::new("fixture.manifest", 2).unwrap(),
            expected: identity(),
        }
    );
    assert!(!error.to_string().contains("private-state"));
    assert!(
        service
            .pin(&id, Some(&saved))
            .await
            .unwrap()
            .same_generation(&restored)
    );
    assert_eq!(beta.tools().definitions()[0].name(), "beta");
    drop((alpha, beta, restored, again));
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
