use super::*;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{StorageFactory, StoredDomain};
use std::{
    future::Future,
    pin::pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Backend {
    records: Mutex<BTreeMap<String, Value>>,
    loads: AtomicUsize,
    pause_load: AtomicBool,
    reject_load: AtomicBool,
    pause_write: AtomicBool,
    entered: Notify,
    release: Notify,
    unhealthy: AtomicBool,
    panic_after_write: AtomicBool,
    pause_after_write: AtomicBool,
}
#[async_trait]
impl KvBackend for Backend {
    fn ensure_available(&self) -> Result<(), StorageError> {
        if self.unhealthy.load(Ordering::Acquire) {
            Err(StorageError::RecoveryRequired)
        } else {
            Ok(())
        }
    }
    async fn load(&self, _: &str) -> Result<Option<StoredDomain>, StorageError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        if self.reject_load.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Io("injected load failure".into()));
        }
        let records = self.records.lock().unwrap().clone();
        if self.pause_load.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(Some(StoredDomain {
            version: 1,
            records,
        }))
    }
    async fn put(&self, _: &str, _: u32, key: &str, value: &Value) -> Result<(), StorageError> {
        if self.pause_write.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.records
            .lock()
            .unwrap()
            .insert(key.into(), value.clone());
        if self.pause_after_write.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        assert!(
            !self.panic_after_write.swap(false, Ordering::SeqCst),
            "after put"
        );
        Ok(())
    }
    async fn delete(&self, _: &str, _: u32, key: &str) -> Result<(), StorageError> {
        if self.pause_write.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.records.lock().unwrap().remove(key);
        if self.pause_after_write.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        assert!(
            !self.panic_after_write.swap(false, Ordering::SeqCst),
            "after delete"
        );
        Ok(())
    }
}

#[tokio::test]
async fn lost_commit_completion_is_unknown_and_fences_the_domain_snapshot() {
    for delete in [false, true] {
        let backend = Arc::new(Backend::default());
        let (_runtime, _lease, facility) = facility(backend.clone()).await;
        let domain = facility.open(spec()).await.unwrap();
        domain.put("one", Value::Null).await.unwrap();
        backend.panic_after_write.store(true, Ordering::SeqCst);
        let result = if delete {
            domain.delete("one").await.map(|_| ())
        } else {
            domain.put("one", Value::Bool(true)).await
        };
        assert!(
            matches!(result, Err(StorageError::OutcomeUnknown(_))),
            "{result:?}"
        );
        assert_eq!(domain.snapshot().await, Err(StorageError::RecoveryRequired));
        assert_eq!(
            domain.delete("absent").await,
            Err(StorageError::RecoveryRequired)
        );
        assert!(matches!(
            facility.open(spec()).await,
            Err(StorageError::RecoveryRequired)
        ));
        assert_eq!(backend.records.lock().unwrap().contains_key("one"), !delete);
    }
}
fn spec() -> DomainSpec {
    DomainSpec {
        id: "domain".into(),
        backend: "memory".into(),
        version: 1,
        maximum_records: 1,
        maximum_bytes: 16,
    }
}
async fn facility(backend: Arc<Backend>) -> (Runtime, rsi_storage::BackendLease, Arc<Facility>) {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "storage",
                "test",
                UpdateMode::Replayable,
                Arc::new(StorageFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let hub = runtime.root().lookup_local::<StorageHubContract>().unwrap();
    let lease = hub.register("memory", backend).unwrap();
    let facility = Arc::new(Facility {
        hub,
        registry: Arc::new(Registry::default()),
    });
    (runtime, lease, facility)
}

#[tokio::test]
async fn cancelled_last_handle_write_retains_authority_through_publication() {
    for delete in [false, true] {
        let backend = Arc::new(Backend::default());
        if delete {
            backend
                .records
                .lock()
                .unwrap()
                .insert("one".into(), Value::Null);
        }
        let (_runtime, _lease, facility) = facility(backend.clone()).await;
        let domain = facility.open(spec()).await.unwrap();
        backend.pause_write.store(true, Ordering::SeqCst);
        let writing = tokio::spawn(async move {
            if delete {
                domain.delete("one").await.map(|_| ())
            } else {
                domain.put("one", Value::Null).await
            }
        });
        backend.entered.notified().await;
        writing.abort();
        assert!(writing.await.unwrap_err().is_cancelled());
        let reopened = facility.open(spec()).await.unwrap();
        assert_eq!(backend.loads.load(Ordering::SeqCst), 1);
        let mut changed = spec();
        changed.maximum_records = 2;
        assert!(matches!(
            facility.open(changed).await,
            Err(StorageError::InvalidInput(_))
        ));
        backend.release.notify_one();
        let records = reopened.snapshot().await.unwrap();
        assert_eq!(records.contains_key("one"), !delete);
        if !delete {
            assert!(reopened.put("two", Value::Null).await.is_err());
        }
        assert_eq!(records, *backend.records.lock().unwrap());
        drop(reopened);
        assert!(facility.registry.domains.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn opening_reserves_one_authority_and_cancelled_initializer_can_retry() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = facility(backend.clone()).await;
    backend.pause_load.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let facility = facility.clone();
        async move { facility.open(spec()).await }
    });
    backend.entered.notified().await;
    let mut second = pin!(facility.open(spec()));
    assert!(matches!(
        second
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert_eq!(backend.loads.load(Ordering::SeqCst), 1);
    let mut conflict = spec();
    conflict.maximum_bytes = 32;
    assert!(facility.open(conflict).await.is_err());
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let domain = second.await.unwrap();
    assert_eq!(backend.loads.load(Ordering::SeqCst), 2);
    domain.put("one", Value::Null).await.unwrap();
    assert!(domain.put("two", Value::Null).await.is_err());
    backend.unhealthy.store(true, Ordering::Release);
    assert_eq!(domain.snapshot().await, Err(StorageError::RecoveryRequired));
    assert_eq!(
        domain.delete("missing").await,
        Err(StorageError::RecoveryRequired)
    );
    assert!(matches!(
        facility.open(spec()).await,
        Err(StorageError::RecoveryRequired)
    ));
}

#[tokio::test]
async fn completed_load_is_shared_and_historical_domain_names_are_removed() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = facility(backend.clone()).await;
    backend.pause_load.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let facility = facility.clone();
        async move { facility.open(spec()).await }
    });
    backend.entered.notified().await;
    let mut second = pin!(facility.open(spec()));
    assert!(matches!(
        second
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    backend.release.notify_one();
    let first = first.await.unwrap().unwrap();
    first.put("one", Value::Null).await.unwrap();
    drop(first);
    let second = second.await.unwrap();
    assert_eq!(backend.loads.load(Ordering::SeqCst), 1);
    assert!(second.put("two", Value::Null).await.is_err());
    drop(second);
    for index in 0..100 {
        let mut spec = spec();
        spec.id = format!("domain-{index}");
        drop(facility.open(spec).await.unwrap());
        assert!(facility.registry.domains.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn failed_initialization_is_retryable_and_stale_cleanup_preserves_replacement() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = facility(backend.clone()).await;
    backend.reject_load.store(true, Ordering::SeqCst);
    assert!(matches!(
        facility.open(spec()).await,
        Err(StorageError::Io(_))
    ));
    assert!(facility.registry.domains.lock().unwrap().is_empty());
    let domain = facility.open(spec()).await.unwrap();
    assert_eq!(backend.loads.load(Ordering::SeqCst), 2);
    drop(domain);
    let old = Arc::new(Authority {
        spec: spec(),
        loaded: OnceCell::new(),
        registry: Arc::downgrade(&facility.registry),
        recovery_required: AtomicBool::new(false),
    });
    let replacement = Arc::new(Authority {
        spec: spec(),
        loaded: OnceCell::new(),
        registry: Arc::downgrade(&facility.registry),
        recovery_required: AtomicBool::new(false),
    });
    facility
        .registry
        .domains
        .lock()
        .unwrap()
        .insert(spec().id, Arc::downgrade(&replacement));
    drop(old);
    assert!(
        facility
            .registry
            .domains
            .lock()
            .unwrap()
            .get(&spec().id)
            .unwrap()
            .ptr_eq(&Arc::downgrade(&replacement))
    );
    drop(replacement);
    assert!(facility.registry.domains.lock().unwrap().is_empty());
}

#[test]
fn api_projection_preserves_commit_certainty_and_diagnostic_failures() {
    use rsi_api_protocol::ApiError;
    for error in [
        StorageError::Io("before commit".into()),
        StorageError::RecoveryRequired,
        StorageError::BackendUnavailable("retired".into()),
    ] {
        assert_eq!(storage_error(error), ApiError::Unavailable);
    }
    assert_eq!(
        storage_error(StorageError::OutcomeUnknown("lost reply".into())),
        ApiError::OutcomeUnknown
    );
    for error in [
        StorageError::InvalidInput("invalid".into()),
        StorageError::Corrupt("invalid".into()),
        StorageError::DuplicateBackend("duplicate".into()),
    ] {
        assert_eq!(
            storage_error(error.clone()),
            ApiError::Backend(error.to_string())
        );
    }
}

#[test]
fn runtime_shutdown_drops_an_unfinished_commit_and_fences_its_retained_handle() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let backend = Arc::new(Backend::default());
    let domain = Arc::new(DomainHandle {
        authority: Arc::new(Authority {
            spec: spec(),
            loaded: OnceCell::new_with(Some(LoadedDomain {
                backend: backend.clone(),
                records: Arc::new(AsyncMutex::new(DomainRecords {
                    values: BTreeMap::new(),
                    size: RecordObjectSize::default(),
                })),
            })),
            registry: Weak::new(),
            recovery_required: AtomicBool::new(false),
        }),
    });
    runtime.block_on(async {
        backend.pause_after_write.store(true, Ordering::SeqCst);
        let owner = domain.clone();
        let waiter = tokio::spawn(async move { owner.put("one", Value::Null).await });
        backend.entered.notified().await;
        assert!(backend.records.lock().unwrap().contains_key("one"));
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
    });
    drop(runtime);
    assert_eq!(
        domain.ensure_available(),
        Err(StorageError::RecoveryRequired)
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert_eq!(domain.snapshot().await, Err(StorageError::RecoveryRequired));
        assert_eq!(
            domain.put("invalid key", Value::Null).await,
            Err(StorageError::RecoveryRequired)
        );
    });
}

#[tokio::test]
async fn cached_entry_sizes_preserve_exact_bounds_across_load_replace_and_delete() {
    let backend = Arc::new(Backend::default());
    backend
        .records
        .lock()
        .unwrap()
        .insert("one".into(), serde_json::json!("界\"\n"));
    let (_runtime, _lease, facility) = facility(backend.clone()).await;
    let mut bounded = spec();
    bounded.maximum_records = 3;
    bounded.maximum_bytes = 48;
    let domain = facility.open(bounded.clone()).await.unwrap();
    domain.put("two", serde_json::json!([1, 2])).await.unwrap();
    domain.put("one", Value::Null).await.unwrap();
    domain.put("three", Value::Bool(true)).await.unwrap();
    let before = domain.snapshot().await.unwrap();
    assert!(matches!(
        domain.put("two", Value::String("x".repeat(48))).await,
        Err(StorageError::InvalidInput(_))
    ));
    assert_eq!(domain.snapshot().await.unwrap(), before);
    assert!(domain.delete("one").await.unwrap());
    assert!(domain.delete("three").await.unwrap());
    // {"two":"..."} is exactly 48 bytes, including an escaped value's compact form.
    let boundary = Value::String("x".repeat(38));
    domain.put("two", boundary.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_vec(&domain.snapshot().await.unwrap())
            .unwrap()
            .len(),
        48
    );
    assert!(matches!(
        domain.put("two", Value::String("x".repeat(39))).await,
        Err(StorageError::InvalidInput(_))
    ));
    drop(domain);
    let reopened = facility.open(bounded).await.unwrap();
    assert_eq!(reopened.snapshot().await.unwrap()["two"], boundary);
    assert!(reopened.delete("two").await.unwrap());
    assert!(reopened.snapshot().await.unwrap().is_empty());
    reopened.put("one", Value::Null).await.unwrap();
    backend.unhealthy.store(true, Ordering::Release);
    assert_eq!(
        reopened.delete("invalid key").await,
        Err(StorageError::RecoveryRequired)
    );
}
