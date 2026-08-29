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
    document: Mutex<SettingsDocument>,
    committed: Notify,
    release: Notify,
}

#[async_trait]
impl SettingsProvider for PausingProvider {
    fn writable(&self) -> bool {
        true
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
    assert_eq!(updated.revision, 1);
    assert!(matches!(
        registration.scope.clear(0).await,
        Err(SettingsError::Conflict {
            expected: 0,
            actual: 1
        })
    ));
    drop(registration.lease);
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
