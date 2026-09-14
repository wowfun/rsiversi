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

#[tokio::test]
async fn redacted_status_preserves_source_precedence_and_admin_editability() {
    use rsi_credentials_protocol::{CredentialAvailability, CredentialsStatusContract};
    let reference = CredentialRef::new("rsi.ai.deepseek", "key").unwrap();
    for (stored, environment) in [(false, false), (false, true), (true, false), (true, true)] {
        let store = Arc::new(MemorySecretStore::default());
        if stored {
            store
                .set(
                    &reference,
                    &SecretValue::new("stored-secret-marker").unwrap(),
                )
                .unwrap();
        }
        let runtime = Runtime::default();
        runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "credentials",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(CredentialsLocalFactory::with_store(
                        store,
                        if environment {
                            BTreeMap::from([(
                                "FIXTURE_KEY".into(),
                                SecretValue::new("environment-secret-marker").unwrap(),
                            )])
                        } else {
                            BTreeMap::new()
                        },
                    )),
                ),
                json!({ "environment":[{"reference":reference,"variable":"FIXTURE_KEY"}]}),
            )
            .await
            .unwrap();
        let status = runtime
            .root()
            .lookup_local::<CredentialsStatusContract>()
            .unwrap()
            .status(&reference)
            .await
            .unwrap();
        let expected = if stored {
            CredentialAvailability::Configured {
                source: CredentialSource::File,
            }
        } else if environment {
            CredentialAvailability::Configured {
                source: CredentialSource::Environment {
                    variable: "FIXTURE_KEY".into(),
                },
            }
        } else {
            CredentialAvailability::Missing
        };
        assert_eq!(status.availability, expected);
        assert!(status.editable);
        let wire = serde_json::to_string(&status).unwrap();
        assert!(!wire.contains("secret-marker"));
        assert_eq!(
            serde_json::from_str::<rsi_credentials_protocol::CredentialStatus>(&wire).unwrap(),
            status
        );
        assert!(runtime.shutdown().await.is_clean());
    }
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "credentials",
                "test",
                UpdateMode::Replayable,
                Arc::new(CredentialsLocalFactory::with_store(
                    Arc::new(FailingStore),
                    BTreeMap::new(),
                )),
            ),
            json!({}),
        )
        .await
        .unwrap();
    let status = runtime
        .root()
        .lookup_local::<CredentialsStatusContract>()
        .unwrap()
        .status(&reference)
        .await
        .unwrap();
    assert_eq!(
        status.availability,
        CredentialAvailability::Unavailable {
            reason: rsi_credentials_protocol::CredentialStoreFailure::Io
        }
    );
    assert!(!status.editable);
    assert!(!serde_json::to_string(&status).unwrap().contains("backend"));
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct FailingStore;

impl SecretStore for FailingStore {
    fn get(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        Err(CredentialsError::Store(
            rsi_credentials_protocol::CredentialStoreFailure::Io,
        ))
    }

    fn set(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
        _secret: &SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        Err(CredentialsError::Store(
            rsi_credentials_protocol::CredentialStoreFailure::Io,
        ))
    }

    fn unset(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<bool> {
        Err(CredentialsError::Store(
            rsi_credentials_protocol::CredentialStoreFailure::Io,
        ))
    }
}

#[derive(Debug)]
struct OldReadStore {
    value: Mutex<SecretValue>,
    first: std::sync::atomic::AtomicBool,
    gate: BlockingStore,
}
impl SecretStore for OldReadStore {
    fn get(&self, _: &CredentialRef) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        let value = self.value.lock().unwrap().clone();
        if self.first.swap(false, Ordering::SeqCst) {
            self.gate.block();
        }
        Ok(Some(value))
    }
    fn set(&self, _: &CredentialRef, secret: &SecretValue) -> rsi_credentials_protocol::Result<()> {
        *self.value.lock().unwrap() = secret.clone();
        Ok(())
    }
    fn unset(&self, _: &CredentialRef) -> rsi_credentials_protocol::Result<bool> {
        unreachable!()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_replacement_cannot_join_a_lookup_started_before_the_write() {
    let store = Arc::new(OldReadStore {
        value: Mutex::new(SecretValue::new("old").unwrap()),
        first: std::sync::atomic::AtomicBool::new(true),
        gate: BlockingStore::default(),
    });
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "credentials",
                "test",
                UpdateMode::Replayable,
                Arc::new(CredentialsLocalFactory::with_store(
                    store.clone(),
                    BTreeMap::new(),
                )),
            ),
            json!({"resolution_timeout_ms":200}),
        )
        .await
        .unwrap();
    let reference = CredentialRef::new("fixture", "primary").unwrap();
    let resolver = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();
    let admin = runtime
        .root()
        .lookup_local::<CredentialsAdminContract>()
        .unwrap();
    let first = {
        let resolver = resolver.clone();
        let reference = reference.clone();
        tokio::spawn(async move { resolver.resolve(&reference).await })
    };
    store.gate.entered.notified().await;
    admin
        .set(&reference, SecretValue::new("new").unwrap())
        .await
        .unwrap();
    let next = resolver.resolve(&reference).await;
    store.gate.release();
    let _ = first.await.unwrap();
    assert_eq!(next.unwrap().secret.expose_secret(), "new");
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct PanickingStore {
    calls: AtomicUsize,
}

impl SecretStore for PanickingStore {
    fn get(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("fixture keyring panic");
    }

    fn set(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
        _secret: &SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        unreachable!("panic cleanup test does not mutate credentials")
    }

    fn unset(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<bool> {
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

    fn block(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.release_changed.wait(released).unwrap();
        }
        self.completed.notify_one();
    }
}

impl SecretStore for BlockingStore {
    fn get(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        self.block();
        Ok(Some(SecretValue::new("secret").unwrap()))
    }

    fn set(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
        _secret: &SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        self.block();
        Ok(())
    }

    fn unset(
        &self,
        _reference: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<bool> {
        self.block();
        Ok(true)
    }
}

#[tokio::test]
async fn admin_and_resolve_share_admission_and_dropped_admin_waiter_keeps_its_permit() {
    let admin_reference = CredentialRef::new("rsi.ai.openai", "admin").unwrap();
    let resolve_reference = CredentialRef::new("rsi.ai.openai", "resolve").unwrap();
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

                "maximum_concurrent_store_operations":1
            }),
        )
        .await
        .unwrap();
    let admin = runtime
        .root()
        .lookup_local::<CredentialsAdminContract>()
        .unwrap();
    let resolve = runtime
        .root()
        .lookup_local::<CredentialsResolveContract>()
        .unwrap();

    let admin_task = tokio::spawn({
        let admin = Arc::clone(&admin);
        async move {
            admin
                .set(&admin_reference, SecretValue::new("secret").unwrap())
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified())
        .await
        .unwrap();
    let resolve_task = tokio::spawn({
        let resolve = Arc::clone(&resolve);
        async move { resolve.resolve(&resolve_reference).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), store.entered.notified())
            .await
            .is_err()
    );
    admin_task.abort();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), store.entered.notified())
            .await
            .is_err(),
        "dropping the admin waiter released a still-running Store operation"
    );

    store.release();
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        resolve_task.await.unwrap().unwrap().secret.expose_secret(),
        "secret"
    );
    assert_eq!(store.calls.load(Ordering::SeqCst), 2);
    drop(admin);
    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
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
            json!({}),
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
            json!({}),
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
            json!({"resolution_timeout_ms":1000}),
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
            Err(CredentialsError::Store(
                rsi_credentials_protocol::CredentialStoreFailure::Io
            ))
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

                "maximum_concurrent_store_operations":1
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

                "maximum_concurrent_store_operations":1,
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
async fn file_precedence_replacement_and_redaction_are_explicit() {
    let reference = CredentialRef::new("rsi.ai.openai", "primary").unwrap();
    let store = Arc::new(MemorySecretStore::default());
    store
        .set(&reference, &SecretValue::new("file-secret").unwrap())
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
    assert_eq!(resolved.source, CredentialSource::File);
    assert_eq!(resolved.secret.expose_secret(), "file-secret");
    let diagnostic = format!("{resolved:?}");
    assert!(!diagnostic.contains("file-secret"));
    assert!(!diagnostic.contains("environment-secret"));

    admin
        .set(&reference, SecretValue::new("replacement-secret").unwrap())
        .await
        .unwrap();
    assert_eq!(
        resolve
            .resolve(&reference)
            .await
            .unwrap()
            .secret
            .expose_secret(),
        "replacement-secret"
    );
    assert!(admin.unset(&reference).await.unwrap());
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
async fn file_failure_never_selects_captured_environment() {
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
    assert!(matches!(
        resolve.resolve(&reference).await,
        Err(CredentialsError::Store(
            rsi_credentials_protocol::CredentialStoreFailure::Io
        ))
    ));

    let unbound = CredentialRef::new("rsi.ai.openai", "unbound").unwrap();
    assert!(matches!(
        resolve.resolve(&unbound).await,
        Err(CredentialsError::Store(
            rsi_credentials_protocol::CredentialStoreFailure::Io
        ))
    ));
    drop(resolve);
    assert!(fiber.dispose().await.is_clean());
}
