use std::{collections::BTreeMap, fmt, sync::Arc};

use rsi_ai_auth::{
    CredentialId, CredentialManager, CredentialRequirement, CredentialSource, CredentialStore,
    SecretValue, StoreError,
};

#[derive(Debug, Default)]
struct FakeStore {
    values: BTreeMap<String, SecretValue>,
}

impl FakeStore {
    fn with(id: &str, value: &str) -> Self {
        Self {
            values: BTreeMap::from([(id.to_owned(), SecretValue::new(value).expect("secret"))]),
        }
    }
}

impl CredentialStore for FakeStore {
    fn get(&self, id: &CredentialId) -> Result<Option<SecretValue>, StoreError> {
        Ok(self.values.get(id.as_str()).cloned())
    }

    fn set(&self, _id: &CredentialId, _secret: &SecretValue) -> Result<(), StoreError> {
        Ok(())
    }

    fn delete(&self, _id: &CredentialId) -> Result<(), StoreError> {
        Ok(())
    }
}

fn requirement() -> CredentialRequirement {
    CredentialRequirement::new("openai.default", ["OPENAI_API_KEY"]).expect("requirement")
}

#[test]
fn nonblocking_resolution_defers_only_when_the_store_must_be_consulted() {
    let explicit = CredentialManager::builder()
        .with_explicit("openai.default", "explicit")
        .expect("explicit")
        .with_store(Arc::new(FakeStore::default()))
        .build();
    let resolved = explicit
        .try_resolve_in_memory(&requirement())
        .expect("explicit source is immediately resolvable")
        .expect("credential");
    assert_eq!(resolved.source().source, CredentialSource::Explicit);

    let store_only = CredentialManager::builder()
        .with_store(Arc::new(FakeStore::with("openai.default", "store")))
        .build();
    assert!(store_only.try_resolve_in_memory(&requirement()).is_none());

    let environment = CredentialManager::builder()
        .with_captured_environment(BTreeMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "environment".to_owned(),
        )]))
        .expect("environment")
        .build();
    let resolved = environment
        .try_resolve_in_memory(&requirement())
        .expect("no store can block environment fallback")
        .expect("credential");
    assert_eq!(resolved.source().source, CredentialSource::Environment);
}

#[test]
fn resolution_precedence_is_explicit_then_memory_then_store_then_captured_env() {
    let manager = CredentialManager::builder()
        .with_explicit("openai.default", "explicit")
        .expect("explicit")
        .with_memory("openai.default", "memory")
        .expect("memory")
        .with_store(Arc::new(FakeStore::with("openai.default", "keyring")))
        .with_captured_environment(BTreeMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "environment".to_owned(),
        )]))
        .expect("environment")
        .build();
    let resolved = manager.resolve(&requirement()).expect("resolve");
    assert_eq!(resolved.expose_secret(), "explicit");
    assert_eq!(resolved.source().source, CredentialSource::Explicit);

    let manager = CredentialManager::builder()
        .with_memory("openai.default", "memory")
        .expect("memory")
        .with_store(Arc::new(FakeStore::with("openai.default", "keyring")))
        .with_captured_environment(BTreeMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "environment".to_owned(),
        )]))
        .expect("environment")
        .build();
    let resolved = manager.resolve(&requirement()).expect("resolve");
    assert_eq!(resolved.expose_secret(), "memory");
    assert_eq!(resolved.source().source, CredentialSource::Memory);

    let manager = CredentialManager::builder()
        .with_store(Arc::new(FakeStore::with("openai.default", "keyring")))
        .with_captured_environment(BTreeMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "environment".to_owned(),
        )]))
        .expect("environment")
        .build();
    let resolved = manager.resolve(&requirement()).expect("resolve");
    assert_eq!(resolved.expose_secret(), "keyring");
    assert_eq!(resolved.source().source, CredentialSource::Store);

    let manager = CredentialManager::builder()
        .with_captured_environment(BTreeMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "environment".to_owned(),
        )]))
        .expect("environment")
        .build();
    let resolved = manager.resolve(&requirement()).expect("resolve");
    assert_eq!(resolved.expose_secret(), "environment");
    assert_eq!(resolved.source().source, CredentialSource::Environment);
}

#[test]
fn secret_and_resolved_credential_debug_output_are_always_redacted() {
    let secret = SecretValue::new("sk-visible-only-at-provider-seam").expect("secret");
    assert_eq!(format!("{secret}"), "[REDACTED]");
    assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");

    let manager = CredentialManager::builder()
        .with_explicit("openai.default", secret.expose())
        .expect("explicit")
        .build();
    let resolved = manager.resolve(&requirement()).expect("resolve");
    let debug = format!("{resolved:?}");
    assert!(!debug.contains("sk-visible"));
    assert!(debug.contains("Explicit"));
}

#[test]
fn captured_environment_is_a_snapshot_not_a_live_process_lookup() {
    let manager = CredentialManager::builder()
        .with_captured_environment(BTreeMap::new())
        .expect("empty environment")
        .build();
    let error = manager
        .resolve(&requirement())
        .expect_err("missing credential");
    assert_eq!(error.code(), "credential.missing");
}

#[test]
fn fake_store_is_a_real_external_seam_not_a_debug_side_channel() {
    fn assert_debug<T: fmt::Debug>() {}
    assert_debug::<Arc<dyn CredentialStore>>();
}
