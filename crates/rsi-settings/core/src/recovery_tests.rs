use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct Provider {
    raw: Mutex<SettingsDocument>,
    fail_reads: AtomicBool,
    publish: AtomicBool,
    writes: AtomicUsize,
}
#[async_trait]
impl SettingsProvider for Provider {
    fn writable(&self) -> bool {
        true
    }
    async fn load(&self) -> Result<SettingsDocument> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(SettingsError::Io("read failed".into()));
        }
        Ok(self.raw.lock().unwrap().clone())
    }
    async fn compare_and_set(
        &self,
        namespace: &str,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> Result<Option<Value>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut raw = self.raw.lock().unwrap();
        assert_eq!(raw.get(namespace), expected);
        if self.publish.load(Ordering::SeqCst) {
            if let Some(value) = replacement {
                raw.insert(namespace.into(), value.clone());
            } else {
                raw.remove(namespace);
            }
        }
        Err(SettingsError::OutcomeUnknown(
            "postpublication failure".into(),
        ))
    }
}
fn service() -> (Service, Arc<Provider>) {
    let provider = Arc::new(Provider::default());
    let service = Service {
        provider: provider.clone(),
        state: Arc::new(ServiceState {
            inner: Mutex::new(ServiceInner {
                raw: SettingsDocument::new(),
                pending: HashMap::new(),
                migrations: HashSet::new(),
                next_registration: 0,
                namespaces: HashMap::new(),
            }),
            write_lock: Arc::new(AsyncMutex::new(())),
        }),
    };
    (service, provider)
}
fn spec(name: &str) -> SettingsSpec {
    SettingsSpec {
        namespace: name.into(),
        defaults: json!({"value":0}),
        base: json!({}),
        metadata: SettingsMetadata {
            schema: json!({"type":"object"}),
            applies: rsi_settings_protocol::SettingsApply::Live,
            description: "Recovery test".into(),
            sensitive_fields: vec![],
        },
        validator: Arc::new(rsi_settings_protocol::ValidateWith(|value: &Value| {
            if value["value"].is_u64() {
                Ok(())
            } else {
                Err(SettingsError::InvalidInput("integer required".into()))
            }
        })),
    }
}
#[tokio::test]
async fn uncertain_publication_reconciles_visible_state_without_certifying_the_write() {
    for publish in [false, true] {
        let (service, provider) = service();
        provider.publish.store(publish, Ordering::SeqCst);
        let target = service.register(spec("target")).unwrap();
        let sibling = service.register(spec("sibling")).unwrap();
        assert!(matches!(
            target.scope.replace(0, json!({"value":1})).await,
            Err(SettingsError::OutcomeUnknown(_))
        ));
        let current = target.scope.get().unwrap();
        assert_eq!(current.revision, 1);
        assert_eq!(current.value["value"], usize::from(publish));
        assert_eq!(sibling.scope.get().unwrap().revision, 0);
        assert!(matches!(
            target.scope.replace(0, json!({"value":2})).await,
            Err(SettingsError::Conflict { actual: 1, .. })
        ));
        assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn failed_recovery_fences_only_target_and_survives_lease_retirement() {
    for retire in [false, true] {
        let (service, provider) = service();
        provider.publish.store(true, Ordering::SeqCst);
        let target = service.register(spec("target")).unwrap();
        let old_id = target.scope.get().unwrap().scope_id;
        let sibling = service.register(spec("sibling")).unwrap();
        provider.fail_reads.store(true, Ordering::SeqCst);
        assert!(matches!(
            target.scope.replace(0, json!({"value":1})).await,
            Err(SettingsError::OutcomeUnknown(_))
        ));
        assert!(matches!(
            target.scope.get(),
            Err(SettingsError::RecoveryRequired(_))
        ));
        assert!(matches!(
            service.describe("target").await,
            Err(SettingsError::RecoveryRequired(_))
        ));
        assert!(matches!(
            target.scope.clear(0).await,
            Err(SettingsError::RecoveryRequired(_))
        ));
        assert_eq!(service.read("sibling").await.unwrap().revision, 0);
        if retire {
            drop(target.lease);
            assert!(matches!(
                service.register(spec("target")),
                Err(SettingsError::RecoveryRequired(_))
            ));
        }
        provider.fail_reads.store(false, Ordering::SeqCst);
        if retire {
            assert!(matches!(
                service.read("target").await,
                Err(SettingsError::UnknownNamespace(_))
            ));
            let new = service.register(spec("target")).unwrap();
            let current = new.scope.get().unwrap();
            assert_ne!(current.scope_id, old_id);
            assert_eq!(current.value["value"], 1);
        } else {
            assert_eq!(service.read("target").await.unwrap().revision, 1);
        }
        assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
        drop(sibling);
    }
}
#[tokio::test]
async fn invalid_observation_stays_pending_and_migration_never_fabricates_registration() {
    let (service, provider) = service();
    provider.publish.store(true, Ordering::SeqCst);
    provider.fail_reads.store(true, Ordering::SeqCst);
    let migration = Arc::new(rsi_settings_protocol::MigrateWith(|_: Option<&Value>| {
        Ok(Some(json!({"value":1})))
    }));
    assert!(matches!(
        service.register_migrating(spec("target"), migration).await,
        Err(SettingsError::OutcomeUnknown(_))
    ));
    assert!(service.list(None, 10).await.unwrap().namespaces.is_empty());
    provider.fail_reads.store(false, Ordering::SeqCst);
    provider
        .raw
        .lock()
        .unwrap()
        .insert("target".into(), json!({"value":"invalid"}));
    assert!(matches!(
        service.read("target").await,
        Err(SettingsError::RecoveryRequired(_))
    ));
    provider
        .raw
        .lock()
        .unwrap()
        .insert("target".into(), json!({"value":2}));
    assert!(matches!(
        service.read("target").await,
        Err(SettingsError::UnknownNamespace(_))
    ));
    let registration = service.register(spec("target")).unwrap();
    assert_eq!(registration.scope.get().unwrap().value["value"], 2);
    assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
}
