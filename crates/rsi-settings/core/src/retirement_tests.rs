use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug)]
struct Provider;
#[async_trait]
impl SettingsProvider for Provider {
    fn writable(&self) -> bool {
        true
    }
    async fn load(&self) -> Result<SettingsDocument> {
        unreachable!()
    }
    async fn compare_and_set(
        &self,
        _: &str,
        _: Option<&Value>,
        _: Option<&Value>,
    ) -> Result<Option<Value>> {
        unreachable!()
    }
}
fn service() -> Service {
    Service {
        provider: Arc::new(Provider),
        state: Arc::new(ServiceState {
            inner: Mutex::new(ServiceInner {
                raw: SettingsDocument::new(),
                migrations: HashSet::new(),
                next_registration: 0,
                namespaces: HashMap::new(),
            }),
            write_lock: Arc::new(AsyncMutex::new(())),
        }),
    }
}
fn fixture_spec(
    name: &str,
    validator: Arc<dyn rsi_settings_protocol::SettingsValidator>,
) -> SettingsSpec {
    SettingsSpec {
        namespace: name.into(),
        defaults: serde_json::json!({}),
        base: serde_json::json!({}),
        metadata: SettingsMetadata {
            schema: serde_json::json!({"type":"object"}),
            applies: rsi_settings_protocol::SettingsApply::Live,
            description: "Registration fixture".into(),
            sensitive_fields: vec![],
        },
        validator,
    }
}
#[derive(Debug)]
struct Validator {
    state: Weak<ServiceState>,
    child: Mutex<Option<SettingsLease>>,
    unlocked: Arc<AtomicBool>,
}
impl rsi_settings_protocol::SettingsValidator for Validator {
    fn validate(&self, _: &Value) -> Result<()> {
        let state = self.state.upgrade().unwrap();
        assert!(
            state.inner.try_lock().is_ok(),
            "registration invokes an external validator under the registry lock"
        );
        Ok(())
    }
}
impl Drop for Validator {
    fn drop(&mut self) {
        let state = self.state.upgrade().unwrap();
        let unlocked = state.inner.try_lock().is_ok();
        self.unlocked.store(unlocked, Ordering::SeqCst);
        if unlocked {
            drop(self.child.get_mut().unwrap().take());
        } else {
            std::mem::forget(self.child.get_mut().unwrap().take());
        }
    }
}
#[test]
fn validator_destruction_can_withdraw_a_child_after_immediate_or_deferred_retirement() {
    for retained_commit in [false, true] {
        let service = service();
        let unlocked = Arc::new(AtomicBool::new(false));
        let spec = |name: &str, child| {
            fixture_spec(
                name,
                Arc::new(Validator {
                    state: Arc::downgrade(&service.state),
                    child: Mutex::new(child),
                    unlocked: unlocked.clone(),
                }),
            )
        };
        let child = service.register_inner(spec("child", None), false).unwrap();
        let parent = service
            .register_inner(spec("parent", Some(child.lease)), false)
            .unwrap();
        let registration = service.state.inner.lock().unwrap().namespaces["parent"].registration;
        if retained_commit {
            service
                .state
                .inner
                .lock()
                .unwrap()
                .namespaces
                .get_mut("parent")
                .unwrap()
                .in_flight = 1;
        }
        drop(parent.lease);
        if retained_commit {
            assert!(service.state.inner.lock().unwrap().namespaces["parent"].retiring);
            drop(InFlightCommit {
                state: service.state.clone(),
                namespace: "parent".into(),
                registration,
            });
        }
        assert!(unlocked.load(Ordering::SeqCst));
        assert!(service.state.inner.lock().unwrap().namespaces.is_empty());
        assert!(matches!(
            parent.scope.get(),
            Err(SettingsError::StaleRegistration(_))
        ));
    }
}

#[derive(Debug)]
struct CallbackValidator {
    state: Weak<ServiceState>,
    child: Mutex<Option<SettingsLease>>,
    replace_raw: bool,
}
impl rsi_settings_protocol::SettingsValidator for CallbackValidator {
    fn validate(&self, _: &Value) -> Result<()> {
        let state = self.state.upgrade().unwrap();
        assert!(state.inner.try_lock().is_ok());
        drop(self.child.lock().unwrap().take());
        if self.replace_raw {
            state
                .inner
                .lock()
                .unwrap()
                .raw
                .insert("parent".into(), serde_json::json!({"concurrent":true}));
        }
        Ok(())
    }
}
fn callback(
    service: &Service,
    child: Option<SettingsLease>,
    replace_raw: bool,
) -> Arc<CallbackValidator> {
    Arc::new(CallbackValidator {
        state: Arc::downgrade(&service.state),
        child: Mutex::new(child),
        replace_raw,
    })
}
#[test]
fn registration_validator_can_withdraw_a_dependency_and_cannot_publish_a_stale_raw_snapshot() {
    for replace_raw in [false, true] {
        let service = service();
        let child = service
            .register_inner(
                fixture_spec("child", callback(&service, None, false)),
                false,
            )
            .unwrap();
        let result = service.register_inner(
            fixture_spec("parent", callback(&service, Some(child.lease), replace_raw)),
            false,
        );
        assert!(
            !service
                .state
                .inner
                .lock()
                .unwrap()
                .namespaces
                .contains_key("child")
        );
        if replace_raw {
            assert!(matches!(result, Err(SettingsError::StaleRegistration(_))));
            assert_eq!(
                service.state.inner.lock().unwrap().raw["parent"],
                serde_json::json!({"concurrent":true})
            );
        } else {
            assert_eq!(
                result.unwrap().scope.get().unwrap().value,
                serde_json::json!({})
            );
        }
    }
}
#[tokio::test]
async fn an_exhausted_revision_is_rejected_before_the_durable_provider_or_raw_cache_changes() {
    let service = service();
    let registered = service
        .register_inner(
            fixture_spec("parent", callback(&service, None, false)),
            false,
        )
        .unwrap();
    service
        .state
        .inner
        .lock()
        .unwrap()
        .namespaces
        .get_mut("parent")
        .unwrap()
        .revision = u64::MAX;
    assert!(
        matches!(registered.scope.replace(u64::MAX, serde_json::json!({"new":true})).await,
        Err(SettingsError::InvalidInput(message)) if message.contains("revision exhausted"))
    );
    assert_eq!(registered.scope.get().unwrap().revision, u64::MAX);
    assert!(service.state.inner.lock().unwrap().raw.is_empty());
}
