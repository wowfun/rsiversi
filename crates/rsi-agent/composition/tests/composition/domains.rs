use super::*;
use rsi_agent_composition_protocol::{
    DomainDefinition, DomainError, DomainHandle, DomainRegistrar, DomainRegistrarContract,
};
use rsi_agent_session_protocol::{DomainIdentity, DomainRevision};

#[derive(Debug)]
struct Captured {
    registrar: Arc<dyn DomainRegistrar>,
    context: rsi_meta::RegistrationContext,
    definition: DomainDefinition<bool>,
    handle: DomainHandle<bool>,
}

#[derive(Debug, Default)]
struct DomainsFactory {
    captured: Mutex<Vec<Captured>>,
}

#[async_trait::async_trait]
impl PluginFactory for DomainsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()).requiring_local::<DomainRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<DomainRegistrarContract>()?;
        let context = plan.context().registration_context()?;
        let initial = plan
            .config()
            .get("enabled")
            .and_then(ConfigValue::as_bool)
            .unwrap_or(false);
        let definition = DomainDefinition::new(
            DomainIdentity::new("fixture.plan", 1).unwrap(),
            &initial,
            |_| Ok(()),
        )
        .unwrap();
        let (handle, lease) = definition
            .register(registrar.as_ref(), &context)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        self.captured.lock().unwrap().push(Captured {
            registrar: registrar.clone(),
            context: context.clone(),
            definition: definition.clone(),
            handle,
        });
        if plan
            .config()
            .get("duplicate")
            .and_then(ConfigValue::as_bool)
            == Some(true)
        {
            let error = definition
                .register(registrar.as_ref(), &context)
                .unwrap_err();
            assert!(matches!(error, DomainError::Duplicate(_)));
            return Err(MetaError::Activation(error.to_string()));
        }
        if plan.config().get("withdraw").and_then(ConfigValue::as_bool) == Some(true) {
            drop(lease);
        } else {
            plan.defer(
                "withdraw fixture domain",
                Box::new(move || {
                    Box::pin(async move {
                        drop(lease);
                        Ok(())
                    })
                }),
            )?;
        }
        Ok(())
    }
}

fn source(config: &str) -> String {
    format!(
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"test.context\"\n[[steps]]\nkind = \"plugin\"\nid = \"domain\"\nplugin = \"test.domain\"\nconfig = {{ {config} }}\n"
    )
}

#[tokio::test]
async fn domain_stage_seals_exact_definitions_and_closes_failed_and_withdrawn_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("presets");
    let id = AgentPresetId::new("default").unwrap();
    let directory = root.join(id.as_str());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(COMPOSITION_FILE);
    fs::write(&path, source("enabled = false")).unwrap();
    let presets = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(id.clone())
            .with_configured_root(AgentPresetRoot::new(root, AgentPresetTrust::User).unwrap()),
        test_compiler(&temp),
    )
    .unwrap();
    let factory = Arc::new(DomainsFactory::default());
    let contributions = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked("test.domain", "v1", UpdateMode::Replayable, factory.clone()),
    ])
    .unwrap();
    let (runtime, tools, composition, service) = activate_composition(presets, contributions).await;
    let first = service.pin(&id).await.unwrap();
    assert_eq!(
        first.domains().baseline()[0].state().value(),
        &serde_json::json!(false)
    );
    let old_proposal = factory.captured.lock().unwrap()[0]
        .handle
        .propose(DomainRevision::new(1), &true)
        .unwrap();
    fs::write(&path, source("enabled = true")).unwrap();
    let second = service.pin(&id).await.unwrap();
    assert_eq!(
        second.domains().baseline()[0].state().value(),
        &serde_json::json!(true)
    );
    first.domains().validate_proposal(&old_proposal).unwrap();
    assert!(matches!(
        second.domains().validate_proposal(&old_proposal),
        Err(DomainError::WrongGeneration)
    ));

    fs::write(&path, source("enabled = true, duplicate = true")).unwrap();
    assert!(service.pin(&id).await.is_err());
    {
        let captured = factory.captured.lock().unwrap();
        for entry in captured.iter() {
            assert!(matches!(
                entry
                    .definition
                    .register(entry.registrar.as_ref(), &entry.context),
                Err(DomainError::Closed)
            ));
        }
        let accepted = captured[1]
            .handle
            .propose(DomainRevision::new(2), &false)
            .unwrap();
        second.domains().validate_proposal(&accepted).unwrap();
    }
    fs::write(&path, source("withdraw = true")).unwrap();
    let withdrawn = service.pin(&id).await.unwrap();
    assert!(withdrawn.domains().baseline().is_empty());
    let stale = factory
        .captured
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .handle
        .propose(DomainRevision::new(0), &true)
        .unwrap();
    assert!(matches!(
        withdrawn.domains().validate_proposal(&stale),
        Err(DomainError::WrongGeneration)
    ));
    drop((first, second, withdrawn, old_proposal, stale));
    factory.captured.lock().unwrap().clear();
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
