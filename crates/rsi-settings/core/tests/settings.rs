use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_settings::SettingsFactory;
use rsi_settings_protocol::{
    Result as SettingsResult, SettingsContract, SettingsDocument, SettingsError, SettingsProvider,
    SettingsProviderContract, SettingsSpec, ValidateWith,
};
use rsi_settings_testkit::MemorySettingsProviderFactory;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct PausingProvider {
    read_only: bool,
    document: Mutex<SettingsDocument>,
    committed: Notify,
    release: Notify,
}

#[async_trait]
impl SettingsProvider for PausingProvider {
    fn writable(&self) -> bool {
        !self.read_only
    }

    async fn load(&self) -> SettingsResult<SettingsDocument> {
        Ok(self.document.lock().unwrap().clone())
    }

    async fn compare_and_set(
        &self,
        namespace: &str,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> SettingsResult<Option<Value>> {
        {
            let mut document = self.document.lock().unwrap();
            if document.get(namespace) != expected {
                return Err(SettingsError::ConcurrentDocumentChange);
            }
            match replacement {
                Some(value) => {
                    document.insert(namespace.to_owned(), value.clone());
                }
                None => {
                    document.remove(namespace);
                }
            }
        }
        self.committed.notify_one();
        self.release.notified().await;
        Ok(self.document.lock().unwrap().get(namespace).cloned())
    }
}

#[derive(Debug)]
struct PausingProviderFactory(Arc<PausingProvider>);

#[async_trait]
impl PluginFactory for PausingProviderFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let provider: Arc<dyn SettingsProvider> = self.0.clone();
        let supply = plan
            .context()
            .provide_local::<SettingsProviderContract>(provider)?;
        plan.defer(
            "withdraw pausing Settings provider",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug, Default)]
struct PanickingProvider;

#[async_trait]
impl SettingsProvider for PanickingProvider {
    fn writable(&self) -> bool {
        true
    }

    async fn load(&self) -> SettingsResult<SettingsDocument> {
        Ok(SettingsDocument::new())
    }

    async fn compare_and_set(
        &self,
        _namespace: &str,
        _expected: Option<&Value>,
        _replacement: Option<&Value>,
    ) -> SettingsResult<Option<Value>> {
        panic!("fixture provider panic")
    }
}

#[derive(Debug)]
struct PanickingProviderFactory;

#[async_trait]
impl PluginFactory for PanickingProviderFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let provider: Arc<dyn SettingsProvider> = Arc::new(PanickingProvider);
        let supply = plan
            .context()
            .provide_local::<SettingsProviderContract>(provider)?;
        plan.defer(
            "withdraw panicking Settings provider",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn spec() -> SettingsSpec {
    SettingsSpec {
        namespace: "agent".into(),
        defaults: json!({"model":"default"}),
        base: json!({}),
        metadata: rsi_settings_protocol::SettingsMetadata {
            schema: serde_json::json!({"type":"object"}),
            applies: rsi_settings_protocol::SettingsApply::Live,
            description: "Fixture values apply live".into(),
            sensitive_fields: vec![],
        },
        validator: Arc::new(ValidateWith(|value: &Value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(|_| ())
                .ok_or_else(|| SettingsError::InvalidInput("model is required".into()))
        })),
    }
}

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn merge_revision_cas_and_lease_staleness_are_one_contract() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            linked(
                "rsi.settings.memory",
                Arc::new(MemorySettingsProviderFactory::new(json!({
                    "agent": {"model":"stored", "nested":{"user":true}}
                }))),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let settings = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let registration = settings
        .register(SettingsSpec {
            namespace: "agent".into(),
            defaults: json!({"model":"default", "nested":{"default":true}, "list":[1]}),
            base: json!({"nested":{"base":true}, "list":[2]}),
            metadata: rsi_settings_protocol::SettingsMetadata {
                schema: serde_json::json!({"type":"object"}),
                applies: rsi_settings_protocol::SettingsApply::Live,
                description: "Fixture values apply live".into(),
                sensitive_fields: vec![],
            },
            validator: Arc::new(ValidateWith(|value: &Value| {
                value
                    .get("model")
                    .and_then(Value::as_str)
                    .map(|_| ())
                    .ok_or_else(|| SettingsError::InvalidInput("model is required".into()))
            })),
        })
        .unwrap();
    assert_eq!(
        registration.scope.get().unwrap().value,
        json!({
            "model":"stored",
            "nested":{"default":true,"base":true,"user":true},
            "list":[2]
        })
    );
    let updated = registration
        .scope
        .replace(0, json!({"model":"next"}))
        .await
        .unwrap();
    let projected = settings.scope("agent").unwrap();
    assert_eq!(projected.get().unwrap(), updated);
    assert!(settings.scope("unregistered").is_err());
    assert!(settings.scope("bad namespace").is_err());
    assert_eq!(updated.revision, 1);
    assert!(matches!(
        registration.scope.clear(0).await,
        Err(SettingsError::Conflict {
            expected: 0,
            actual: 1
        })
    ));
    drop(registration.lease);
    assert!(projected.get().is_err());
    assert!(settings.scope("agent").is_err());
    assert!(matches!(
        registration.scope.get(),
        Err(SettingsError::StaleRegistration(namespace)) if namespace == "agent"
    ));

    assert!(service.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(
        runtime
            .root()
            .lookup_local::<SettingsProviderContract>()
            .is_none()
    );
}

#[tokio::test]
async fn cancelling_a_caller_after_durable_commit_cannot_split_live_and_raw_state() {
    let runtime = Runtime::default();
    let pausing = Arc::new(PausingProvider::default());
    let provider = runtime
        .root()
        .apply(
            linked(
                "rsi.settings.pausing",
                Arc::new(PausingProviderFactory(pausing.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let settings = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let registration = settings.register(spec()).unwrap();
    let scope = registration.scope.clone();
    let write = tokio::spawn(async move { scope.replace(0, json!({"model":"committed"})).await });
    pausing.committed.notified().await;
    write.abort();
    pausing.release.notify_one();

    tokio::time::timeout(std::time::Duration::from_millis(250), async {
        loop {
            if registration.scope.get().unwrap().revision == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("service-owned commit must publish live state");
    assert_eq!(
        registration.scope.get().unwrap().value,
        json!({"model":"committed"})
    );

    drop(registration.lease);
    let replacement = settings.register(spec()).unwrap();
    assert_eq!(
        replacement.scope.get().unwrap().value,
        json!({"model":"committed"})
    );
    drop(replacement);
    assert!(service.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
}

#[tokio::test]
async fn panicking_provider_does_not_strand_retiring_namespace_ownership() {
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            linked("rsi.settings.panicking", Arc::new(PanickingProviderFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let settings = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let registration = settings.register(spec()).unwrap();

    assert!(matches!(
        registration
            .scope
            .replace(0, json!({"model":"never-committed"}))
            .await,
        Err(SettingsError::Io(_))
    ));
    drop(registration);
    let replacement = settings
        .register(spec())
        .expect("provider panic cleanup must release retiring ownership");

    drop(replacement);
    drop(settings);
    assert!(service.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
}

#[tokio::test]
async fn client_projection_fences_recreated_namespaces_even_when_the_revision_repeats() {
    use rsi_settings_protocol::SettingsAccessContract;
    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            linked(
                "rsi.settings.memory",
                Arc::new(MemorySettingsProviderFactory::new(json!({}))),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.settings", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let registry = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let access = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    assert!(matches!(
        access.read("client").await,
        Err(SettingsError::UnknownNamespace(_))
    ));
    let first = registry.register(client_spec()).unwrap();
    let before = access.read("client").await.unwrap();
    let updated = access
        .replace("client", &before.version(), json!({"count":2}))
        .await
        .unwrap();
    assert_eq!(updated.scope_id, before.scope_id);
    assert_eq!(updated.revision, 1);
    assert!(matches!(
        access.clear("client", &before.version()).await,
        Err(SettingsError::Conflict {
            expected: 0,
            actual: 1
        })
    ));
    drop(first.lease);
    let second = registry.register(client_spec()).unwrap();
    let replacement = access.read("client").await.unwrap();
    assert_eq!(replacement.revision, 0);
    assert_ne!(replacement.scope_id, before.scope_id);
    assert!(matches!(
        access
            .replace("client", &before.version(), json!({"count":9}))
            .await,
        Err(SettingsError::StaleRegistration(_))
    ));
    assert_eq!(
        access.read("client").await.unwrap().value,
        json!({"count":2})
    );
    assert!(
        access
            .replace("client", &replacement.version(), json!({"count":"bad"}))
            .await
            .is_err()
    );
    assert_eq!(access.read("client").await.unwrap(), replacement);
    drop(second.lease);
    assert!(service.dispose().await.is_clean());
    let restarted = runtime
        .root()
        .apply(
            linked("rsi.settings.restarted", Arc::new(SettingsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let registration = runtime
        .root()
        .lookup_local::<SettingsContract>()
        .unwrap()
        .register(client_spec())
        .unwrap();
    let access = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    assert_ne!(
        access.read("client").await.unwrap().scope_id,
        replacement.scope_id
    );
    assert!(matches!(
        access.clear("client", &replacement.version()).await,
        Err(SettingsError::StaleRegistration(_))
    ));
    drop(registration.lease);
    assert!(restarted.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
}

fn client_spec() -> SettingsSpec {
    SettingsSpec {
        namespace: "client".into(),
        defaults: json!({"count":1}),
        base: json!({}),
        metadata: rsi_settings_protocol::SettingsMetadata {
            schema: serde_json::json!({"type":"object"}),
            applies: rsi_settings_protocol::SettingsApply::Live,
            description: "Fixture values apply live".into(),
            sensitive_fields: vec![],
        },
        validator: Arc::new(ValidateWith(|value: &Value| {
            if value["count"].is_u64() {
                Ok(())
            } else {
                Err(SettingsError::InvalidInput("count must be unsigned".into()))
            }
        })),
    }
}

#[tokio::test]
async fn discovery_is_bounded_to_active_names_and_registration_metadata() {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            linked(
                "memory",
                Arc::new(MemorySettingsProviderFactory::new(
                    json!({"unregistered":{"not_a_namespace":true}}),
                )),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(linked("settings", Arc::new(SettingsFactory)), Value::Null)
        .await
        .unwrap();
    let registry = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let access = runtime
        .root()
        .lookup_local::<rsi_settings_protocol::SettingsAccessContract>()
        .unwrap();
    assert!(access.list(None, 64).await.unwrap().namespaces.is_empty());
    let mut leases = Vec::new();
    for index in (0..70).rev() {
        let mut entry = spec();
        entry.namespace = format!("test-{index:02}");
        leases.push(registry.register(entry).unwrap());
    }
    let first = access.list(None, 64).await.unwrap();
    assert_eq!(first.namespaces.len(), 64);
    assert_eq!(first.namespaces[0], "test-00");
    assert_eq!(first.next.as_deref(), Some("test-63"));
    let last = access.list(first.next.as_deref(), 64).await.unwrap();
    assert_eq!(
        last.namespaces,
        (64..70)
            .map(|index| format!("test-{index:02}"))
            .collect::<Vec<_>>()
    );
    assert!(last.next.is_none());
    let description = access.describe("test-00").await.unwrap();
    assert_eq!(description.defaults, spec().defaults);
    assert_eq!(description.metadata, spec().metadata);
    assert!(description.writable);
    assert_eq!(description.version.revision, 0);
    drop(leases.pop().unwrap());
    assert!(matches!(
        access.describe("test-00").await,
        Err(SettingsError::UnknownNamespace(_))
    ));
    assert_eq!(access.list(None, 1).await.unwrap().namespaces, ["test-01"]);
    let mut entry = spec();
    entry.namespace = "test-00".into();
    let replacement = registry.register(entry).unwrap();
    assert_ne!(
        access.describe("test-00").await.unwrap().version.scope_id,
        description.version.scope_id
    );
    let mut bad = spec();
    bad.metadata.schema = json!("invalid schema");
    assert!(registry.register(bad).is_err());
    assert!(access.describe("agent").await.is_err());
    assert!(access.describe("unregistered").await.is_err());
    drop(replacement);
    drop(leases);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn descriptions_report_read_only_and_hide_retiring_registrations() {
    for read_only in [true, false] {
        let runtime = Runtime::default();
        let provider = Arc::new(PausingProvider {
            read_only,
            ..Default::default()
        });
        runtime
            .root()
            .apply(
                linked(
                    "provider",
                    Arc::new(PausingProviderFactory(provider.clone())),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        runtime
            .root()
            .apply(linked("settings", Arc::new(SettingsFactory)), Value::Null)
            .await
            .unwrap();
        let registry = runtime.root().lookup_local::<SettingsContract>().unwrap();
        let access = runtime
            .root()
            .lookup_local::<rsi_settings_protocol::SettingsAccessContract>()
            .unwrap();
        let registration = registry.register(spec()).unwrap();
        let description = access.describe("agent").await.unwrap();
        assert_eq!(description.writable, !read_only);
        let scope = registration.scope.clone();
        if read_only {
            assert!(matches!(
                scope.replace(0, json!({"model":"changed"})).await,
                Err(SettingsError::ReadOnly)
            ));
            assert!(provider.document.lock().unwrap().is_empty());
            drop(registration);
        } else {
            let write =
                tokio::spawn(async move { scope.replace(0, json!({"model":"changed"})).await });
            provider.committed.notified().await;
            drop(registration);
            assert!(access.list(None, 64).await.unwrap().namespaces.is_empty());
            assert!(matches!(
                access.describe("agent").await,
                Err(SettingsError::UnknownNamespace(_))
            ));
            assert!(matches!(
                registry.register(spec()),
                Err(SettingsError::DuplicateNamespace(_))
            ));
            provider.release.notify_one();
            write.await.unwrap().unwrap();
            let replacement = registry.register(spec()).unwrap();
            assert_ne!(
                access.describe("agent").await.unwrap().version.scope_id,
                description.version.scope_id
            );
            drop(replacement);
        }
        assert!(runtime.shutdown().await.is_clean());
    }
}
