use super::*;
use rsi_agent_composition::{AgentCompositionSnapshot, AgentCompositionSource};

#[derive(Debug)]
struct SourceProvider(Arc<dyn AgentCompositionSource>);
#[async_trait::async_trait]
impl PluginFactory for SourceProvider {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_agent_composition::AgentCompositionSourceContract>(
                self.0.clone(),
            )?;
        Ok(())
    }
}

#[tokio::test]
async fn source_contract_blocks_composition_publication_until_its_provider_is_active() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(&temp, &profile("managed"));
    let probe = Arc::new(Probe::default());
    let selected = snapshot(&presets, "managed", &probe);
    let runtime = Runtime::default();
    let root = runtime.root();
    for (id, factory) in [
        ("tools", Arc::new(ToolsFactory) as Arc<dyn PluginFactory>),
        (
            "root",
            Arc::new(rsi_agent_composition::AgentGenerationRootFactory),
        ),
        ("label", Arc::new(LabelFactory)),
    ] {
        root.apply(
            ResolvedFactory::linked(id, "fixture", UpdateMode::RestartRequired, factory),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    }
    let composition = root
        .apply(
            ResolvedFactory::linked(
                "composition",
                "fixture",
                UpdateMode::RestartRequired,
                Arc::new(AgentCompositionFactory::from_source_contract(
                    ScopeRoot::new(128).unwrap(),
                )),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert!(!matches!(composition.snapshot().state, FiberState::Active));
    assert!(root.lookup_local::<AgentCompositionContract>().is_none());
    let owner = root
        .apply(
            ResolvedFactory::linked(
                "source",
                "fixture",
                UpdateMode::RestartRequired,
                Arc::new(SourceProvider(selected)),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let service = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(service) = root.lookup_local::<AgentCompositionContract>() {
                break service;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let pin = service
        .pin(&AgentPresetId::new("default").unwrap())
        .await
        .unwrap();
    assert_eq!(probe.active("managed"), 1);
    drop(pin);
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct Source {
    current: Mutex<Option<Arc<AgentCompositionSnapshot>>>,
    reads: AtomicUsize,
}

impl AgentCompositionSource for Source {
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>> {
        self.reads.fetch_add(1, Ordering::AcqRel);
        self.current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ProfileError::InvalidProgram("fixture source unavailable".into()))
    }
}

fn presets(temp: &TempDir, source: &str) -> AgentPresetCatalog {
    let root = temp.path().join("presets");
    fs::create_dir_all(root.join("default")).unwrap();
    fs::write(root.join("default").join(COMPOSITION_FILE), source).unwrap();
    AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(AgentPresetId::new("default").unwrap())
            .with_configured_root(AgentPresetRoot::new(root, AgentPresetTrust::User).unwrap()),
        test_compiler(temp),
    )
    .unwrap()
}

fn snapshot(
    presets: &AgentPresetCatalog,
    revision: &str,
    probe: &Arc<Probe>,
) -> Arc<AgentCompositionSnapshot> {
    Arc::new(AgentCompositionSnapshot::new(
        presets.clone(),
        AgentContributionCatalog::new([
            context_factory(),
            ResolvedFactory::linked(
                "test.contribution",
                revision,
                UpdateMode::Replayable,
                Arc::new(ProbeFactory {
                    probe: Arc::clone(probe),
                }),
            ),
        ])
        .unwrap(),
    ))
}

#[tokio::test]
async fn unchanged_profile_rebuilds_for_catalog_and_preserves_old_pin() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(&temp, &profile("same"));
    let a_probe = Arc::new(Probe::default());
    let b_probe = Arc::new(Probe::default());
    let a_snapshot = snapshot(&presets, "a", &a_probe);
    let source = Arc::new(Source {
        current: Mutex::new(Some(Arc::clone(&a_snapshot))),
        reads: AtomicUsize::new(0),
    });
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let id = AgentPresetId::new("default").unwrap();
    let a = service.pin(&id).await.unwrap();
    let again = service.pin(&id).await.unwrap();
    assert_eq!(a.source_digest(), again.source_digest());
    assert_eq!(a_probe.activations.load(Ordering::Acquire), 1);
    *source.current.lock().unwrap() = Some(snapshot(&presets, "b", &b_probe));
    let b = service.pin(&id).await.unwrap();
    assert_ne!(a.source_digest(), b.source_digest());
    assert_eq!(a_probe.active("same"), 1);
    assert_eq!(b_probe.active("same"), 1);
    *source.current.lock().unwrap() = None;
    assert!(service.pin(&id).await.is_err());
    assert_eq!(b_probe.active("same"), 1);
    assert_eq!(source.reads.load(Ordering::Acquire), 4);
    drop((a, again, a_snapshot));
    a_probe.wait_active("same", 0).await;
    drop(b);
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn replacement_during_activation_does_not_mix_catalog_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(
        &temp,
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"test.context\"\n[[steps]]\nkind = \"plugin\"\nid = \"block\"\nplugin = \"test.blocking\"\n",
    );
    let gate = Arc::new(BuildGate::default());
    let catalog = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.blocking",
            "a",
            UpdateMode::Replayable,
            Arc::new(BlockingFactory {
                gate: Arc::clone(&gate),
            }),
        ),
    ])
    .unwrap();
    let source = Arc::new(Source {
        current: Mutex::new(Some(Arc::new(AgentCompositionSnapshot::new(
            presets.clone(),
            catalog,
        )))),
        reads: AtomicUsize::new(0),
    });
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let id = AgentPresetId::new("default").unwrap();
    let build = tokio::spawn({
        let service = Arc::clone(&service);
        let id = id.clone();
        async move { service.pin(&id).await }
    });
    gate.wait_entered(1).await;
    *source.current.lock().unwrap() = Some(Arc::new(AgentCompositionSnapshot::new(
        presets,
        AgentContributionCatalog::new([context_factory()]).unwrap(),
    )));
    gate.release.cancel();
    let old = build.await.unwrap().unwrap();
    assert_eq!(source.reads.load(Ordering::Acquire), 1);
    assert!(service.pin(&id).await.is_err());
    assert_eq!(source.reads.load(Ordering::Acquire), 2);
    drop(old);
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct Endpoint;
#[async_trait::async_trait]
impl rsi_meta::ServiceEndpoint for Endpoint {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        _: rsi_meta::ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct PortableFactory;
#[async_trait::async_trait]
impl PluginFactory for PortableFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let prepared = PreparedActivation::new(config.clone());
        Ok(if config["role"] == "consumer" {
            prepared.requiring(rsi_meta::Requirement::new(
                "test.portable",
                "test.portable.v1",
                rsi_meta::ContractVersion(1),
            ))
        } else {
            prepared
        })
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        if plan.config()["role"] == "provider" {
            plan.context().provide(
                "test.portable",
                "test.portable.v1",
                rsi_meta::ContractVersion(1),
                Arc::new(Endpoint),
            )?;
        }
        Ok(())
    }
}

fn portable_catalog(revision: &str) -> AgentContributionCatalog {
    let mut catalog = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.contribution",
            revision,
            UpdateMode::Replayable,
            Arc::new(PortableFactory),
        ),
    ])
    .unwrap();
    catalog.isolate_portable("test.portable").unwrap();
    catalog
}

#[tokio::test]
async fn portable_bindings_are_private_to_each_overlapping_generation() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(
        &temp,
        r#"format = 1
[[steps]]
kind = "plugin"
id = "context"
plugin = "test.context"
[[steps]]
kind = "plugin"
id = "provider"
plugin = "test.contribution"
config = { role = "provider" }
[[steps]]
kind = "plugin"
id = "consumer"
plugin = "test.contribution"
config = { role = "consumer" }
"#,
    );
    let source = Arc::new(Source {
        current: Mutex::new(Some(Arc::new(AgentCompositionSnapshot::new(
            presets.clone(),
            portable_catalog("a"),
        )))),
        reads: AtomicUsize::new(0),
    });
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let id = AgentPresetId::new("default").unwrap();
    let a = service.pin(&id).await.unwrap();
    *source.current.lock().unwrap() = Some(Arc::new(AgentCompositionSnapshot::new(
        presets,
        portable_catalog("b"),
    )));
    let b = service.pin(&id).await.unwrap();
    let inspected = runtime
        .inspect(rsi_meta::InspectionRequest::default())
        .unwrap();
    let consumers = inspected
        .fibers
        .iter()
        .filter(|fiber| {
            fiber.dependencies.items.iter().any(|dependency| {
                matches!(&dependency.service,
            rsi_meta::InspectedService::Portable { key, .. } if key.as_str() == "test.portable")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(consumers.len(), 2);
    let mut providers = Vec::new();
    let mut isolations = Vec::new();
    for consumer in consumers {
        let dependency = &consumer.dependencies.items[0];
        let provider = dependency.provider.as_ref().unwrap();
        let provider_fiber = inspected
            .fibers
            .iter()
            .find(|fiber| fiber.id == provider.owner.fiber)
            .unwrap();
        assert_eq!(consumer.parent, provider_fiber.parent);
        assert_eq!(consumer.factory, provider_fiber.factory);
        providers.push(provider.owner.fiber);
        if let rsi_meta::InspectedService::Portable { isolation, .. } = dependency.service {
            isolations.push(isolation);
        }
    }
    assert_ne!(providers[0], providers[1]);
    assert_ne!(isolations[0], isolations[1]);
    drop((a, b));
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[test]
fn catalog_admission_is_bounded_and_duplicate_portable_keys_are_idempotent() {
    use rsi_agent_composition::{
        MAXIMUM_CATALOG_FACTORIES, MAXIMUM_CATALOG_KEY_BYTES, MAXIMUM_CATALOG_MARKERS,
    };
    assert!(
        AgentContributionCatalog::new((0..MAXIMUM_CATALOG_FACTORIES).map(|index| {
            ResolvedFactory::linked(
                format!("test.{index}"),
                "v1",
                UpdateMode::Replayable,
                Arc::new(NoopFactory),
            )
        }))
        .is_ok()
    );
    AgentContributionCatalog::new([])
        .unwrap()
        .isolate_portable("x".repeat(MAXIMUM_CATALOG_KEY_BYTES))
        .unwrap();
    assert!(
        AgentContributionCatalog::new((0..=MAXIMUM_CATALOG_FACTORIES).map(|index| {
            ResolvedFactory::linked(
                format!("test.{index}"),
                "v1",
                UpdateMode::Replayable,
                Arc::new(NoopFactory),
            )
        }))
        .is_err()
    );
    let mut catalog = AgentContributionCatalog::new([]).unwrap();
    assert!(catalog.isolate_portable("").is_err());
    assert!(
        catalog
            .isolate_portable("x".repeat(MAXIMUM_CATALOG_KEY_BYTES + 1))
            .is_err()
    );
    for index in 0..MAXIMUM_CATALOG_MARKERS {
        catalog
            .isolate_portable(format!("service.{index}"))
            .unwrap();
    }
    catalog.isolate_portable("service.0").unwrap();
    assert!(catalog.isolate_portable("extra").is_err());
}

struct MarkerA;
impl rsi_meta::LocalContract for MarkerA {
    const KEY: &'static str = "test.nominal";
    type Service = usize;
}
struct MarkerB;
impl rsi_meta::LocalContract for MarkerB {
    const KEY: &'static str = "test.nominal";
    type Service = usize;
}

fn changed_catalog(probe: &Arc<Probe>, step: usize) -> AgentContributionCatalog {
    let mode = if step == 4 {
        UpdateMode::RestartRequired
    } else {
        UpdateMode::Replayable
    };
    let mut catalog = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.contribution",
            "same-revision",
            mode,
            Arc::new(ProbeFactory {
                probe: Arc::clone(probe),
            }),
        ),
    ])
    .unwrap();
    if step == 1 {
        catalog.register_local_contract::<MarkerA>().unwrap();
    }
    if step >= 2 {
        catalog.register_local_contract::<MarkerB>().unwrap();
    }
    if step >= 3 {
        catalog.isolate_portable("test.unused-portable").unwrap();
    }
    catalog
}

#[tokio::test]
async fn cache_identity_includes_nominal_bindings_portable_keys_and_update_mode() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(&temp, &profile("same"));
    let probe = Arc::new(Probe::default());
    let source = Arc::new(Source {
        current: Mutex::new(None),
        reads: AtomicUsize::new(0),
    });
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let id = AgentPresetId::new("default").unwrap();
    let mut pins = Vec::new();
    for step in 0..5 {
        *source.current.lock().unwrap() = Some(Arc::new(AgentCompositionSnapshot::new(
            presets.clone(),
            changed_catalog(&probe, step),
        )));
        let pin = service.pin(&id).await.unwrap();
        for old in &pins {
            assert_ne!(pin.source_digest(), old);
        }
        pins.push(pin.source_digest().to_owned());
        let again = service.pin(&id).await.unwrap();
        assert_eq!(again.source_digest(), pin.source_digest());
        assert_eq!(probe.activations.load(Ordering::Acquire), step + 1);
    }
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn last_pin_retains_the_entire_selected_catalog_through_scope_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(&temp, &profile("same"));
    let probe = Arc::new(Probe::default());
    let unused = Arc::new(NoopFactory);
    let unused_weak = Arc::downgrade(&unused);
    let catalog = AgentContributionCatalog::new([
        context_factory(),
        ResolvedFactory::linked(
            "test.contribution",
            "a",
            UpdateMode::Replayable,
            Arc::new(ProbeFactory {
                probe: Arc::clone(&probe),
            }),
        ),
        ResolvedFactory::linked("test.unselected", "a", UpdateMode::Replayable, unused),
    ])
    .unwrap();
    let source = Arc::new(Source {
        current: Mutex::new(Some(Arc::new(AgentCompositionSnapshot::new(
            presets.clone(),
            catalog,
        )))),
        reads: AtomicUsize::new(0),
    });
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let id = AgentPresetId::new("default").unwrap();
    let old = service.pin(&id).await.unwrap();
    *source.current.lock().unwrap() = Some(snapshot(&presets, "b", &probe));
    let new = service.pin(&id).await.unwrap();
    assert!(unused_weak.upgrade().is_some());
    drop(old);
    tokio::time::timeout(Duration::from_secs(2), async {
        while unused_weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(new);
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn refreshed_compiler_allowlist_is_checked_before_a_possible_cache_hit() {
    let temp = tempfile::tempdir().unwrap();
    let presets = presets(&temp, &profile("same"));
    let probe = Arc::new(Probe::default());
    let healthy = snapshot(&presets, "a", &probe);
    let source = Arc::new(Source {
        current: Mutex::new(Some(Arc::clone(&healthy))),
        reads: AtomicUsize::new(0),
    });
    let (runtime, tools, composition, service) = activate_composition_factory(
        AgentCompositionFactory::with_source(source.clone(), ScopeRoot::new(128).unwrap()),
    )
    .await;
    let id = AgentPresetId::new("default").unwrap();
    let old = service.pin(&id).await.unwrap();
    let environment = ProfileEnvironment::new(
        temp.path().join("config"),
        temp.path().join("state"),
        temp.path().join("cache"),
        "test",
        BTreeMap::new(),
    )
    .unwrap();
    let restricted = AgentPresetCatalog::new(
        AgentPresetCatalogConfig::new(id.clone()).with_configured_root(
            AgentPresetRoot::new(temp.path().join("presets"), AgentPresetTrust::User).unwrap(),
        ),
        AgentPresetProfileCompiler::new(
            ProfileCompiler::new(environment, ProfileLimits::default()),
            ["test.context"],
        ),
    )
    .unwrap();
    *source.current.lock().unwrap() = Some(snapshot(&restricted, "a", &probe));
    assert!(service.pin(&id).await.is_err());
    assert_eq!(probe.activations.load(Ordering::Acquire), 1);
    *source.current.lock().unwrap() = Some(healthy);
    let restored = service.pin(&id).await.unwrap();
    assert_eq!(old.source_digest(), restored.source_digest());
    assert_eq!(probe.activations.load(Ordering::Acquire), 1);
    drop((old, restored));
    assert!(composition.dispose().await.is_clean());
    drop(service);
    assert!(tools.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
