use super::*;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionContext, ContributionError, ContributionKind,
    ContributionOutput, ContributionRegistrar, ContributionRegistrarContract,
    ContributionRegistration, ContributionResult,
};
use rsi_agent_session_protocol::ContributionId;

#[derive(Debug)]
struct ContextProbe;
#[async_trait::async_trait]
impl ContextContributor for ContextProbe {
    async fn contribute(
        &self,
        _: &ContributionContext,
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        Ok(ContributionOutput::default())
    }
}

fn registration(id: &str, priority: i32) -> ContributionRegistration {
    ContributionRegistration::new(
        ContributionId::new(id).unwrap(),
        priority,
        ContributionKind::Context(Arc::new(ContextProbe)),
    )
}

#[derive(Debug, Default)]
struct CallbacksFactory {
    captured: Mutex<
        Vec<(
            Arc<dyn ContributionRegistrar>,
            rsi_meta::RegistrationContext,
        )>,
    >,
}

#[async_trait::async_trait]
impl PluginFactory for CallbacksFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<ContributionRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ContributionRegistrarContract>()?;
        let context = plan.context().registration_context()?;
        self.captured
            .lock()
            .unwrap()
            .push((registrar.clone(), context.clone()));
        let prefix = plan
            .config()
            .get("prefix")
            .and_then(ConfigValue::as_str)
            .unwrap_or("first");
        let mut leases = Vec::new();
        // The final business tie break must ignore this reversed local registration order.
        for (suffix, priority) in [("z", 0), ("a", 0), ("priority", -1)] {
            leases.push(
                registrar
                    .register(
                        &context,
                        registration(&format!("{prefix}.{suffix}"), priority),
                    )
                    .map_err(|error| MetaError::Activation(error.to_string()))?,
            );
        }
        if plan
            .config()
            .get("duplicate")
            .and_then(ConfigValue::as_bool)
            == Some(true)
        {
            let error = registrar
                .register(&context, registration(&format!("{prefix}.a"), 0))
                .unwrap_err();
            assert!(matches!(error, ContributionError::Duplicate(_)));
            return Err(MetaError::Activation(error.to_string()));
        }
        if plan.config().get("withdraw").and_then(ConfigValue::as_bool) == Some(true) {
            drop(leases);
        } else {
            plan.defer(
                "withdraw callbacks",
                Box::new(move || {
                    Box::pin(async move {
                        drop(leases);
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
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"test.context\"\n[[steps]]\nkind = \"plugin\"\nid = \"callbacks\"\nplugin = \"test.callbacks\"\nconfig = {{ {config} }}\n[[steps]]\nkind = \"plugin\"\nid = \"second\"\nplugin = \"test.callbacks\"\nconfig = {{ prefix = \"second\" }}\n"
    )
}

#[tokio::test]
async fn execution_catalog_freezes_business_order_and_rolls_back_failed_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("presets");
    let id = AgentPresetId::new("default").unwrap();
    let directory = root.join(id.as_str());
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(COMPOSITION_FILE);
    fs::write(&path, source("")).unwrap();
    let presets = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(id.clone())
            .with_configured_root(AgentPresetRoot::new(root, AgentPresetTrust::User).unwrap()),
        test_compiler(&temp),
    )
    .unwrap();
    let factory = Arc::new(CallbacksFactory::default());
    let contributions = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.callbacks",
            "v1",
            UpdateMode::Replayable,
            factory.clone(),
        ),
    ])
    .unwrap();
    let (runtime, tools, composition, service) = activate_composition(presets, contributions).await;
    let first = service.pin(&id).await.unwrap();
    let names: Vec<_> = first
        .contributions()
        .entries()
        .iter()
        .map(|entry| entry.id().as_str())
        .collect();
    assert_eq!(
        names,
        [
            "first.priority",
            "second.priority",
            "first.a",
            "first.z",
            "second.a",
            "second.z"
        ]
    );
    fs::write(&path, source("duplicate = true")).unwrap();
    assert!(service.pin(&id).await.is_err());
    assert_eq!(first.contributions().entries().len(), 6);
    fs::write(&path, source("withdraw = true")).unwrap();
    let second = service.pin(&id).await.unwrap();
    assert_eq!(second.contributions().entries().len(), 3);
    for (registrar, context) in factory.captured.lock().unwrap().iter() {
        assert!(matches!(
            registrar.register(context, registration("late", 0)),
            Err(ContributionError::Closed)
        ));
    }
    drop((first, second));
    factory.captured.lock().unwrap().clear();
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
