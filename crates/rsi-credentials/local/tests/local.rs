use rsi_credentials_local::{CredentialsLocalFactory, MemorySecretStore, SecretStore};
use rsi_credentials_protocol::{
    CredentialRef, CredentialSource, CredentialsAdminContract, CredentialsError,
    CredentialsResolveContract, SecretValue,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug)]
struct FailingStore;

impl SecretStore for FailingStore {
    fn get(
        &self,
        _service: &str,
        _account: &str,
    ) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        Err(CredentialsError::Store("backend unavailable".into()))
    }

    fn set(
        &self,
        _service: &str,
        _account: &str,
        _secret: &SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        Err(CredentialsError::Store("backend unavailable".into()))
    }

    fn unset(&self, _service: &str, _account: &str) -> rsi_credentials_protocol::Result<bool> {
        Err(CredentialsError::Store("backend unavailable".into()))
    }
}

#[derive(Debug, Default)]
struct PanickingStore {
    calls: AtomicUsize,
}

impl SecretStore for PanickingStore {
    fn get(
        &self,
        _service: &str,
        _account: &str,
    ) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("fixture keyring panic");
    }

    fn set(
        &self,
        _service: &str,
        _account: &str,
        _secret: &SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        unreachable!("panic cleanup test does not mutate credentials")
    }

    fn unset(&self, _service: &str, _account: &str) -> rsi_credentials_protocol::Result<bool> {
        unreachable!("panic cleanup test does not mutate credentials")
    }
}

#[derive(Debug, Default)]
struct BlockingStore {
    calls: AtomicUsize,
    entered: Notify,
    completed: Notify,
    released: Mutex<bool>,
    release_changed: Condvar,
}

impl BlockingStore {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release_changed.notify_all();
    }
}

impl SecretStore for BlockingStore {
    fn get(
        &self,
        _service: &str,
        _account: &str,
    ) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.release_changed.wait(released).unwrap();
        }
        self.completed.notify_one();
        Ok(Some(SecretValue::new("secret").unwrap()))
    }

    fn set(
        &self,
        _service: &str,
        _account: &str,
        _secret: &SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        unreachable!("singleflight test does not mutate credentials")
    }

    fn unset(&self, _service: &str, _account: &str) -> rsi_credentials_protocol::Result<bool> {
        unreachable!("singleflight test does not mutate credentials")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_backend_lookup_does_not_block_the_async_runtime() {
    let reference = CredentialRef::new("rsi.ai.openai", "heartbeat").unwrap();
    let store = Arc::new(BlockingStore::default());
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(CredentialsLocalFactory::with_store(
                    store.clone(),
                    BTreeMap::new(),
                )),
            ),
            json!({"service":"rsiversi"}),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();

    let (watchdog_done, watchdog_wait) = std::sync::mpsc::sync_channel(1);
    let watchdog_store = Arc::clone(&store);
    let watchdog = std::thread::spawn(move || {
        if watchdog_wait
            .recv_timeout(Duration::from_millis(500))
            .is_err()
        {
            watchdog_store.release();
        }
    });
    let lookup = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        async move { resolve.resolve(&reference).await }
    });
    let started = std::time::Instant::now();
    let entered = tokio::time::timeout(Duration::from_millis(100), store.entered.notified()).await;
    let elapsed = started.elapsed();

    store.release();
    let _ = watchdog_done.send(());
    let credential = lookup.await.unwrap().unwrap();
    watchdog.join().unwrap();
    assert!(
        entered.is_ok(),
        "backend entry did not leave the runtime schedulable"
    );
    assert!(
        elapsed < Duration::from_millis(250),
        "synchronous backend work blocked the runtime for {elapsed:?}"
    );
    assert_eq!(credential.secret.expose_secret(), "secret");

    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn concurrent_resolution_of_one_reference_uses_one_backend_call() {
    let reference = CredentialRef::new("rsi.ai.openai", "primary").unwrap();
    let store = Arc::new(BlockingStore::default());
    let factory = CredentialsLocalFactory::with_store(store.clone(), BTreeMap::new());
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(factory),
            ),
            json!({"service":"rsiversi"}),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    let first = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        let reference = reference.clone();
        async move { resolve.resolve(&reference).await }
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified())
        .await
        .unwrap();
    let second = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        let reference = reference.clone();
        async move { resolve.resolve(&reference).await }
    });
    let duplicate =
        tokio::time::timeout(Duration::from_millis(100), store.entered.notified()).await;
    store.release();
    assert_eq!(
        first.await.unwrap().unwrap().secret.expose_secret(),
        "secret"
    );
    assert_eq!(
        second.await.unwrap().unwrap().secret.expose_secret(),
        "secret"
    );
    assert!(duplicate.is_err(), "a duplicate backend call was admitted");
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);

    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn a_panicking_backend_does_not_leave_a_dead_singleflight_entry() {
    let reference = CredentialRef::new("rsi.ai.openai", "primary").unwrap();
    let store = Arc::new(PanickingStore::default());
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(CredentialsLocalFactory::with_store(
                    store.clone(),
                    BTreeMap::new(),
                )),
            ),
            json!({"service":"rsiversi","resolution_timeout_ms":1000}),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();

    for _ in 0..2 {
        assert!(matches!(
            resolve.resolve(&reference).await,
            Err(CredentialsError::Store(message)) if message.contains("keyring task failed")
        ));
    }
    assert_eq!(store.calls.load(Ordering::SeqCst), 2);

    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn different_references_obey_the_configured_backend_admission_limit() {
    let first_reference = CredentialRef::new("rsi.ai.openai", "first").unwrap();
    let second_reference = CredentialRef::new("rsi.ai.openai", "second").unwrap();
    let store = Arc::new(BlockingStore::default());
    let factory = CredentialsLocalFactory::with_store(store.clone(), BTreeMap::new());
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(factory),
            ),
            json!({
                "service":"rsiversi",
                "maximum_concurrent_resolutions":1
            }),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    let first = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        async move { resolve.resolve(&first_reference).await }
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified())
        .await
        .unwrap();
    let second = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        async move { resolve.resolve(&second_reference).await }
    });
    let admitted_while_full =
        tokio::time::timeout(Duration::from_millis(100), store.entered.notified()).await;
    store.release();
    assert_eq!(
        first.await.unwrap().unwrap().secret.expose_secret(),
        "secret"
    );
    assert_eq!(
        second.await.unwrap().unwrap().secret.expose_secret(),
        "secret"
    );
    assert!(
        admitted_while_full.is_err(),
        "a second backend call bypassed the configured admission limit"
    );
    assert_eq!(store.calls.load(Ordering::SeqCst), 2);

    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn timed_out_unadmitted_reference_does_not_leave_background_work() {
    let first_reference = CredentialRef::new("rsi.ai.openai", "first").unwrap();
    let second_reference = CredentialRef::new("rsi.ai.openai", "second").unwrap();
    let store = Arc::new(BlockingStore::default());
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(CredentialsLocalFactory::with_store(
                    store.clone(),
                    BTreeMap::new(),
                )),
            ),
            json!({
                "service":"rsiversi",
                "maximum_concurrent_resolutions":1,
                "resolution_timeout_ms":50
            }),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    let first = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        async move { resolve.resolve(&first_reference).await }
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified())
        .await
        .unwrap();

    assert!(matches!(
        resolve.resolve(&second_reference).await,
        Err(CredentialsError::Timeout(_))
    ));
    let _ = first.await.unwrap();
    store.release();
    tokio::time::timeout(Duration::from_secs(1), store.completed.notified())
        .await
        .unwrap();
    let queued_backend_call =
        tokio::time::timeout(Duration::from_millis(100), store.entered.notified()).await;
    assert!(
        queued_backend_call.is_err(),
        "a caller that timed out before admission must not leave background work"
    );
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);

    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn resolution_timeout_detaches_the_waiter_without_abandoning_backend_work() {
    let reference = CredentialRef::new("rsi.ai.openai", "primary").unwrap();
    let store = Arc::new(BlockingStore::default());
    let factory = CredentialsLocalFactory::with_store(store.clone(), BTreeMap::new());
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(factory),
            ),
            json!({
                "service":"rsiversi",
                "resolution_timeout_ms":50
            }),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    assert!(matches!(
        resolve.resolve(&reference).await,
        Err(CredentialsError::Timeout(account)) if account == reference.account()
    ));
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
    store.release();
    tokio::time::timeout(Duration::from_secs(1), store.completed.notified())
        .await
        .unwrap();

    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn keyring_precedence_environment_shadow_and_redaction_are_explicit() {
    let reference = CredentialRef::new("rsi.ai.openai", "primary").unwrap();
    let store = Arc::new(MemorySecretStore::default());
    store
        .set(
            "rsiversi",
            &reference.account(),
            &SecretValue::new("keyring-secret").unwrap(),
        )
        .unwrap();
    let factory = CredentialsLocalFactory::with_store(
        store.clone(),
        BTreeMap::from([(
            "OPENAI_API_KEY".into(),
            SecretValue::new("environment-secret").unwrap(),
        )]),
    );
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(factory),
            ),
            json!({
                "service":"rsiversi",
                "environment":[{
                    "reference":{"owner":"rsi.ai.openai","slot":"primary"},
                    "variable":"OPENAI_API_KEY"
                }]
            }),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    let admin = runtime
        .root()
        .lookup_local::<CredentialsAdminContract>()
        .unwrap();
    let resolved = resolve.resolve(&reference).await.unwrap();
    assert_eq!(resolved.source, CredentialSource::Keyring);
    assert_eq!(resolved.secret.expose_secret(), "keyring-secret");
    let diagnostic = format!("{resolved:?}");
    assert!(!diagnostic.contains("keyring-secret"));
    assert!(!diagnostic.contains("environment-secret"));

    assert_eq!(
        admin.unset(&reference).await,
        Err(CredentialsError::EnvironmentShadow("OPENAI_API_KEY".into()))
    );
    store.unset("rsiversi", &reference.account()).unwrap();
    let fallback = resolve.resolve(&reference).await.unwrap();
    assert_eq!(
        fallback.source,
        CredentialSource::Environment {
            variable: "OPENAI_API_KEY".into()
        }
    );
    assert_eq!(fallback.secret.expose_secret(), "environment-secret");

    drop(admin);
    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn explicitly_captured_environment_survives_an_unavailable_keyring_backend() {
    let reference = CredentialRef::new("rsi.ai.openai", "primary").unwrap();
    let factory = CredentialsLocalFactory::with_store(
        Arc::new(FailingStore),
        BTreeMap::from([(
            "OPENAI_API_KEY".into(),
            SecretValue::new("environment-secret").unwrap(),
        )]),
    );
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.credentials.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(factory),
            ),
            json!({
                "service":"rsiversi",
                "environment":[{
                    "reference":{"owner":"rsi.ai.openai","slot":"primary"},
                    "variable":"OPENAI_API_KEY"
                }]
            }),
        )
        .await
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    let fallback = resolve.resolve(&reference).await.unwrap();
    assert_eq!(
        fallback.source,
        CredentialSource::Environment {
            variable: "OPENAI_API_KEY".into()
        }
    );
    assert_eq!(fallback.secret.expose_secret(), "environment-secret");

    let unbound = CredentialRef::new("rsi.ai.openai", "unbound").unwrap();
    assert!(matches!(
        resolve.resolve(&unbound).await,
        Err(CredentialsError::Store(message)) if message == "backend unavailable"
    ));
    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}
