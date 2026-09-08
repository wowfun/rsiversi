use async_trait::async_trait;
use rsi::{
    AddonScope, AgentPresetManager, StandardAddon, StandardAddonBuilder, StandardAddonSet,
    StandardComposition,
};
use rsi_host::{HostBuilder, HostPaths, Profile, ProfileEntry, ProfileFragment};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
    UpdateMode,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Counter;
impl LocalContract for Counter {
    const KEY: &'static str = "fixture.addon.counter";
    type Service = AtomicUsize;
}
struct ConflictingCounter;
impl LocalContract for ConflictingCounter {
    const KEY: &'static str = Counter::KEY;
    type Service = AtomicUsize;
}

#[derive(Debug, Default)]
struct CounterFactory {
    prepared: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
}
#[async_trait]
impl PluginFactory for CounterFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        let value = desired
            .get("value")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| MetaError::InvalidInput("value must be a natural number".into()))?;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            value,
            std::mem::size_of::<usize>(),
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let value = plan.take_state::<usize>()?;
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = self.live.clone();
        plan.defer(
            "release addon",
            Box::new(move || {
                Box::pin(async move {
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        plan.context()
            .provide_local::<Counter>(Arc::new(AtomicUsize::new(value)))?;
        Ok(())
    }
}

fn addon(factory: Arc<CounterFactory>, scope: AddonScope, enabled: bool) -> StandardAddon {
    let mut builder = StandardAddonBuilder::new("fixture.addon");
    builder
        .register_factory(
            scope,
            "fixture.counter",
            "one",
            UpdateMode::Replayable,
            factory,
        )
        .unwrap();
    builder
        .register_local_contract_at::<Counter>(scope)
        .unwrap();
    // Intentionally disagree with prepare: descriptions never veto valid config.
    builder
        .describe_factory(
            "fixture.counter",
            "Independent counter",
            Some(json!({"type":"string"})),
        )
        .unwrap();
    if enabled {
        builder
            .register_fragment_at(
                scope,
                ProfileFragment::new(
                    "fixture.fragment",
                    [ProfileEntry::new(
                        "fixture.counter",
                        "fixture.counter",
                        json!({"value":41}),
                    )],
                ),
            )
            .unwrap();
    }
    builder.build().unwrap()
}

fn composition(root: &std::path::Path) -> StandardComposition {
    StandardComposition::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap(),
        BTreeMap::new(),
        None,
    )
}

#[tokio::test]
async fn independent_addon_configures_executes_and_unloads_through_standard_composition() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/settings.json"),
        serde_json::to_vec(
            &json!({"rsi.agent":{"default_model":{"deployment":"test", "model":"test"}}}),
        )
        .unwrap(),
    )
    .unwrap();
    let factory = Arc::new(CounterFactory::default());
    let set = StandardAddonSet::new([addon(factory.clone(), AddonScope::Service, true)]).unwrap();
    let composition = composition(root.path()).with_addons(set);
    let profile = rsi::ProfileCatalog::new(composition.paths().clone())
        .host(&rsi::HostProfileId::new("standard").unwrap())
        .unwrap();
    let preview = composition.preview_host(&profile).unwrap();
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    assert!(
        preview
            .factories
            .iter()
            .any(|description| description.plugin == "fixture.counter"
                && description.configuration_schema == Some(json!({"type":"string"})))
    );
    let host = composition
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let counter = host.lookup_local::<Counter>().unwrap();
    assert_eq!(counter.fetch_add(1, Ordering::SeqCst), 41);
    assert_eq!(counter.load(Ordering::SeqCst), 42);
    assert_eq!(factory.live.load(Ordering::SeqCst), 1);
    assert!(host.shutdown().await.is_clean());
    assert_eq!(factory.live.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registration_and_descriptions_do_not_prepare_or_enable_a_factory() {
    let factory = Arc::new(CounterFactory::default());
    let set = StandardAddonSet::new([addon(factory.clone(), AddonScope::Service, false)]).unwrap();
    let mut builder = HostBuilder::without_paths("test");
    set.register_into(&mut builder, AddonScope::Service)
        .unwrap();
    let host = builder.build().unwrap();
    assert!(
        host.preview(Profile::new([ProfileEntry::new(
            "counter",
            "fixture.counter",
            Value::Null
        )]))
        .is_ok()
    );
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    let running = host.start(Profile::default()).await.unwrap();
    assert!(running.lookup_local::<Counter>().is_none());
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    assert!(running.shutdown().await.is_clean());
}

#[test]
fn duplicate_factories_builtin_collisions_marker_conflicts_and_oversize_descriptions_are_rejected()
{
    let factory = Arc::new(CounterFactory::default());
    let one = addon(factory.clone(), AddonScope::Service, false);
    assert!(StandardAddonSet::new([one.clone(), one]).is_err());
    let mut second = StandardAddonBuilder::new("second");
    second
        .register_linked(
            "fixture.counter",
            "different",
            UpdateMode::Replayable,
            factory.clone(),
        )
        .unwrap();
    assert!(
        StandardAddonSet::new([
            addon(factory.clone(), AddonScope::Service, false),
            second.build().unwrap()
        ])
        .is_err()
    );
    let mut conflicting = StandardAddonBuilder::new("conflict");
    conflicting
        .register_local_contract::<ConflictingCounter>()
        .unwrap();
    assert!(
        StandardAddonSet::new([
            addon(factory.clone(), AddonScope::Service, false),
            conflicting.build().unwrap()
        ])
        .is_err()
    );
    let mut shared = StandardAddonBuilder::new("shared");
    shared.register_local_contract::<Counter>().unwrap();
    assert!(
        StandardAddonSet::new([
            addon(factory.clone(), AddonScope::Service, false),
            shared.build().unwrap()
        ])
        .is_ok()
    );
    let mut builtin = StandardAddonBuilder::new("builtin-collision");
    builtin
        .register_linked(
            "rsi.tools",
            "override",
            UpdateMode::Replayable,
            factory.clone(),
        )
        .unwrap();
    assert!(
        builtin
            .describe_factory(
                "rsi.tools",
                "x".repeat(rsi::MAXIMUM_FACTORY_DESCRIPTION_BYTES),
                None
            )
            .is_err()
    );
    let root = tempfile::tempdir().unwrap();
    assert!(
        composition(root.path())
            .with_addons(StandardAddonSet::new([builtin.build().unwrap()]).unwrap())
            .build()
            .is_err()
    );
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
}

#[test]
fn platform_and_declaration_identity_are_frozen_before_activation() {
    let mut unsupported = StandardAddonBuilder::new("unsupported");
    unsupported
        .platforms(["not-this-platform".to_owned()])
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    assert!(
        composition(root.path())
            .with_addons(StandardAddonSet::new([unsupported.build().unwrap()]).unwrap())
            .build()
            .is_err()
    );
    let bare = composition(root.path());
    let profile = rsi::ProfileCatalog::new(bare.paths().clone())
        .host(&rsi::HostProfileId::new("standard").unwrap())
        .unwrap();
    let original = bare.preview_host(&profile).unwrap().launch_key;
    let factory = Arc::new(CounterFactory::default());
    let extended = bare.with_addons(
        StandardAddonSet::new([addon(factory.clone(), AddonScope::Agent, false)]).unwrap(),
    );
    let changed = extended.preview_host(&profile).unwrap().launch_key;
    assert_ne!(original, changed);
    assert_eq!(changed, extended.preview_host(&profile).unwrap().launch_key);
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn preset_management_uses_actual_agent_declarations_and_rejects_a_mismatched_manager() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let factory = Arc::new(CounterFactory::default());
    let plain = composition(root.path());
    let extended = plain.clone().with_addons(
        StandardAddonSet::new([addon(factory.clone(), AddonScope::Agent, false)]).unwrap(),
    );
    let system = root.path().join("presets");
    std::fs::create_dir_all(system.join("standard")).unwrap();
    std::fs::write(system.join("standard/agent.profile.toml"), "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'fixture'\nplugin = 'fixture.counter'\nconfig = { value = 7 }\n").unwrap();
    let manager = AgentPresetManager::open_standard(&extended, &system)
        .await
        .unwrap();
    let id = rsi_agent_presets::AgentPresetId::new("standard").unwrap();
    assert!(manager.catalog().compile(&id).is_ok());
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
    assert!(plain.clone().with_agent_presets(&manager).is_err());
    let other_root = tempfile::tempdir().unwrap();
    assert!(
        composition(other_root.path())
            .with_addons(extended.addons().clone())
            .with_agent_presets(&manager)
            .is_err()
    );
    let attached = extended.with_agent_presets(&manager).unwrap();
    // Replacing addon inputs after attachment cannot silently reuse a stale compiler.
    assert!(
        attached
            .clone()
            .with_addons(StandardAddonSet::default())
            .build()
            .is_err()
    );
    let host = attached
        .build()
        .unwrap()
        .start(Profile::default())
        .await
        .unwrap();
    let resolver = host
        .lookup_local::<rsi_agent_composition_protocol::AgentCompositionContract>()
        .unwrap();
    let pin = resolver.pin(&id).await.unwrap();
    assert_eq!(factory.live.load(Ordering::SeqCst), 1);
    assert!(
        host.lookup_local::<Counter>().is_none(),
        "Agent-only contribution leaked to the service catalog"
    );
    drop(pin);
    assert!(host.shutdown().await.is_clean());
    assert_eq!(factory.live.load(Ordering::SeqCst), 0);
    assert!(manager.shutdown().await.is_clean());
    let plain_manager = AgentPresetManager::open_standard(&plain, system)
        .await
        .unwrap();
    assert!(plain_manager.catalog().compile(&id).is_err());
    assert!(plain_manager.shutdown().await.is_clean());
}

#[cfg(target_os = "linux")]
mod domains {
    use super::*;
    use rsi_api_protocol::{
        ApiClient, ApiClientContract, ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput,
        ApiRegistrarContract, ApiResponseCapacity, OperationAccess, OperationClass,
        OperationEffect, OperationId, OperationSpec, RequestEncoding, RetainedBytes,
    };

    #[async_trait]
    trait ReadValue: Send + Sync {
        async fn read(&self) -> rsi_api_protocol::Result<usize>;
    }
    enum ValueDomain {}
    impl LocalContract for ValueDomain {
        const KEY: &'static str = "fixture.addon.value";
        type Service = dyn ReadValue;
    }
    struct Direct(Arc<AtomicUsize>);
    #[async_trait]
    impl ReadValue for Direct {
        async fn read(&self) -> rsi_api_protocol::Result<usize> {
            Ok(self.0.load(Ordering::SeqCst))
        }
    }
    fn operation() -> OperationSpec {
        OperationSpec {
            id: OperationId::new("fixture", "value", 1).unwrap(),
            class: OperationClass::Data,
            effect: OperationEffect::Read,
            access: OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 16,
            maximum_response_bytes: 64,
        }
    }
    #[derive(Debug)]
    struct Handler(Arc<AtomicUsize>);
    #[async_trait]
    impl ApiHandler for Handler {
        async fn invoke(
            &self,
            _: ApiContext,
            input: RetainedBytes,
            capacity: ApiResponseCapacity,
        ) -> rsi_api_protocol::Result<ApiOutput> {
            if input.as_bytes() != b"null" {
                return Err(ApiError::Invalid("expected null".into()));
            }
            let ApiResponseCapacity::Finite(capacity) = capacity else {
                return Err(ApiError::Unavailable);
            };
            Ok(ApiOutput::Reply(ApiMessage {
                json: capacity.encode(&self.0.load(Ordering::SeqCst))?,
                binary: None,
            }))
        }
    }
    #[derive(Debug)]
    struct Endpoint;
    #[async_trait]
    impl PluginFactory for Endpoint {
        fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone())
                .requiring_local::<Counter>()
                .requiring_local::<ApiRegistrarContract>())
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            let counter = plan.local::<Counter>()?;
            plan.context()
                .provide_local::<ValueDomain>(Arc::new(Direct(counter.clone())))?;
            let registration = plan
                .local::<ApiRegistrarContract>()?
                .register(operation(), Arc::new(Handler(counter)))
                .map_err(|error| MetaError::Activation(error.to_string()))?;
            plan.defer(
                "withdraw independent endpoint",
                Box::new(move || {
                    Box::pin(async move {
                        drop(registration);
                        Ok(())
                    })
                }),
            )
        }
    }
    struct Remote(Arc<dyn ApiClient>);
    #[async_trait]
    impl ReadValue for Remote {
        async fn read(&self) -> rsi_api_protocol::Result<usize> {
            let operation = operation();
            let input = self
                .0
                .input_budget(operation.class)
                .encode(&Value::Null, operation.maximum_request_bytes)?;
            let ApiOutput::Reply(message) = self.0.call(&operation, input).await? else {
                return Err(ApiError::Unavailable);
            };
            serde_json::from_slice(message.json.as_bytes())
                .map_err(|_| ApiError::Invalid("invalid counter reply".into()))
        }
    }
    #[derive(Debug)]
    struct Client;
    #[async_trait]
    impl PluginFactory for Client {
        fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()).requiring_local::<ApiClientContract>())
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            let api = plan.local::<ApiClientContract>()?;
            if !api.operations().contains(&operation()) {
                return Err(MetaError::Activation(
                    "addon endpoint is not enabled".into(),
                ));
            }
            plan.context()
                .provide_local::<ValueDomain>(Arc::new(Remote(api)))?;
            Ok(())
        }
    }

    fn domain_addon() -> StandardAddon {
        let mut faces = StandardAddonBuilder::new("fixture.domain");
        faces
            .register_linked(
                "fixture.endpoint",
                "one",
                UpdateMode::Replayable,
                Arc::new(Endpoint),
            )
            .unwrap();
        faces
            .register_factory(
                AddonScope::Client,
                "fixture.client",
                "one",
                UpdateMode::Replayable,
                Arc::new(Client),
            )
            .unwrap();
        faces.export_domain::<ValueDomain>().unwrap();
        faces
            .register_fragment(ProfileFragment::new(
                "fixture.endpoint",
                [ProfileEntry::new(
                    "fixture.endpoint",
                    "fixture.endpoint",
                    Value::Null,
                )],
            ))
            .unwrap();
        faces
            .register_fragment_at(
                AddonScope::Client,
                ProfileFragment::new(
                    "fixture.client",
                    [ProfileEntry::new(
                        "fixture.client",
                        "fixture.client",
                        Value::Null,
                    )],
                ),
            )
            .unwrap();
        faces.build().unwrap()
    }

    #[tokio::test]
    async fn declared_domain_crosses_embedded_and_real_uds_application_connections() {
        for remote in [false, true] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("config")).unwrap();
            std::fs::write(
                root.path().join("config/settings.json"),
                br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
            )
            .unwrap();
            let factory = Arc::new(CounterFactory::default());
            let composition = composition(root.path()).with_addons(
                StandardAddonSet::new([
                    addon(factory.clone(), AddonScope::Service, true),
                    domain_addon(),
                ])
                .unwrap(),
            );
            let profile = rsi::ProfileCatalog::new(composition.paths().clone())
                .host(&rsi::HostProfileId::new("standard").unwrap())
                .unwrap();
            let stop = tokio_util::sync::CancellationToken::new();
            let daemon = if remote {
                let owner = rsi_service_host::HostOwnerLease::try_acquire(
                    rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths())
                        .unwrap(),
                )
                .unwrap();
                let daemon =
                    rsi::StandardServiceDaemon::start(composition.clone(), &profile, owner)
                        .await
                        .unwrap();
                Some(tokio::spawn(daemon.run(stop.clone())))
            } else {
                None
            };
            let (host, diagnostic) =
                rsi::standard_application_host(composition.clone(), vec![]).unwrap();
            let running = host
                .start(Profile::new([ProfileEntry::new(
                    "connection",
                    "rsi.application.connection",
                    json!({"host_profile":"standard"}),
                )]))
                .await
                .unwrap_or_else(|error| {
                    panic!("remote={remote}: {error}; {:?}", diagnostic.take())
                });
            let expected = if remote {
                rsi_client::ConnectionLifetime::Remote
            } else {
                rsi_client::ConnectionLifetime::Embedded
            };
            assert_eq!(
                *running
                    .lookup_local::<rsi_client::ConnectionLifetimeContract>()
                    .unwrap(),
                expected
            );
            let value = running.lookup_local::<ValueDomain>().unwrap();
            assert_eq!(value.read().await.unwrap(), 41);
            assert!(running.shutdown().await.is_clean());
            if remote {
                assert!(value.read().await.is_err());
            }
            if let Some(daemon) = daemon {
                stop.cancel();
                daemon.await.unwrap().unwrap();
            }
            assert_eq!(factory.live.load(Ordering::SeqCst), 0);
            rsi_service_host::HostOwnerLease::try_acquire(
                rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths()).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn descriptor_limits_reject_depth_width_and_platform_overflow_without_prepare() {
    let factory = Arc::new(CounterFactory::default());
    let mut addon = StandardAddonBuilder::new("bounds");
    addon
        .register_linked(
            "bounds.factory",
            "one",
            UpdateMode::Replayable,
            factory.clone(),
        )
        .unwrap();
    let mut nested = Value::Null;
    for _ in 0..=rsi::MAXIMUM_ADDON_SCHEMA_DEPTH {
        nested = json!([nested]);
    }
    assert!(
        addon
            .describe_factory("bounds.factory", "too deep", Some(nested))
            .is_err()
    );
    let wide = Value::Array(vec![
        Value::Null;
        rsi::MAXIMUM_FACTORY_DESCRIPTION_BYTES + 1
    ]);
    assert!(
        addon
            .describe_factory("bounds.factory", "too wide", Some(wide))
            .is_err()
    );
    assert!(
        addon
            .platforms((0..=rsi::MAXIMUM_ADDON_PLATFORMS).map(|index| format!("target-{index}")))
            .is_err()
    );
    assert_eq!(factory.prepared.load(Ordering::SeqCst), 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn missing_export_rolls_back_the_embedded_connection_and_releases_its_owner() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/settings.json"),
        br#"{"rsi.agent":{"default_model":{"deployment":"fixture","model":"unused"}}}"#,
    )
    .unwrap();
    let mut addon = StandardAddonBuilder::new("missing");
    addon.export_domain::<Counter>().unwrap();
    let composition = composition(root.path())
        .with_addons(StandardAddonSet::new([addon.build().unwrap()]).unwrap());
    let (host, diagnostics) = rsi::standard_application_host(composition.clone(), vec![]).unwrap();
    assert!(
        host.start(Profile::new([ProfileEntry::new(
            "connection",
            "rsi.application.connection",
            json!({"host_profile":"standard"})
        )]))
        .await
        .is_err()
    );
    assert!(
        diagnostics
            .take()
            .unwrap()
            .to_string()
            .contains(Counter::KEY)
    );
    rsi_service_host::HostOwnerLease::try_acquire(
        rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths()).unwrap(),
    )
    .unwrap();
}
