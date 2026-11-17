use super::*;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{Address, StorageFactory, StorageHubContract};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll, Waker},
};

#[path = "../tests/support/mod.rs"]
mod support;
use support::Backend;

fn spec() -> DomainSpec {
    DomainSpec {
        id: "domain".into(),
        backend: "memory".into(),
        maximum_records: 4,
        maximum_bytes: 1024,
        ..DomainSpec::default()
    }
}
fn schema() -> Schema {
    Schema::default().table::<Value>("rows").unwrap()
}
fn address(key: &str) -> Address {
    Address::record("rows", key).unwrap()
}
async fn fixture(backend: Arc<Backend>) -> (Runtime, rsi_storage::BackendLease, Arc<Facility>) {
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
        registry: Arc::default(),
    });
    (runtime, lease, facility)
}
fn rows(domain: &Domain) -> BTreeMap<String, Value> {
    domain
        .table::<Value>("rows")
        .unwrap()
        .snapshot()
        .unwrap()
        .entries()
        .map(|(key, value)| (key.to_owned(), value.as_ref().clone()))
        .collect()
}

#[tokio::test]
async fn oversized_batch_rejects_before_waiting_for_queue_capacity() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = fixture(backend).await;
    let domain = facility.open(spec(), schema()).await.unwrap();
    let first = domain.authority.admit().await.unwrap();
    let second = domain.authority.admit().await.unwrap();
    let changes = (0..=2 * domain.spec().maximum_records)
        .map(|key| Change::delete(address(&key.to_string())))
        .collect();
    let mut batch = pin!(domain.batch(changes));
    assert!(matches!(
        batch.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Err(StorageError::InvalidInput(message))) if message.contains("batch capacity")
    ));
    drop((first, second));
    domain.close().await;
}

#[tokio::test]
async fn schema_mismatch_is_distinct_from_storage_corruption() {
    let backend = Arc::new(Backend::default());
    backend.seed("domain", 1, address("one"), &Value::Null);
    let (_runtime, _lease, facility) = fixture(backend).await;
    let mut changed = spec();
    changed.version = 2;
    assert!(matches!(
        facility.open(changed, schema()).await,
        Err(StorageError::DomainVersionMismatch {
            stored: 1,
            requested: 2,
            ..
        })
    ));
    let domain = facility.open(spec(), schema()).await.unwrap();
    assert_eq!(rows(&domain)["one"], Value::Null);
    domain.close().await;
}

#[tokio::test]
async fn lost_commit_completion_is_unknown_and_fences_the_domain_snapshot() {
    for delete in [false, true] {
        let backend = Arc::new(Backend::default());
        let (_runtime, _lease, facility) = fixture(backend.clone()).await;
        let domain = facility.open(spec(), schema()).await.unwrap();
        let table = domain.table::<Value>("rows").unwrap();
        table.put("one", Value::Null).await.unwrap();
        backend
            .state
            .panic_after_write
            .store(true, Ordering::SeqCst);
        let result = if delete {
            table.delete("one").await.map(|_| ())
        } else {
            table.put("one", Value::Bool(true)).await
        };
        assert!(matches!(result, Err(StorageError::OutcomeUnknown(_))));
        assert!(matches!(
            domain.snapshot(),
            Err(StorageError::RecoveryRequired)
        ));
        assert_eq!(
            table.delete("absent").await,
            Err(StorageError::RecoveryRequired)
        );
        assert!(matches!(
            facility.open(spec(), schema()).await,
            Err(StorageError::RecoveryRequired)
        ));
        assert_eq!(backend.values("domain").contains_key("rows/one"), !delete);
        domain.close().await;
        let replacement = facility.open(spec(), schema()).await.unwrap();
        assert_eq!(rows(&replacement).contains_key("one"), !delete);
        replacement.close().await;
    }
}

#[tokio::test]
async fn cancelled_last_handle_write_retains_authority_through_publication() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = fixture(backend.clone()).await;
    let domain = facility.open(spec(), schema()).await.unwrap();
    backend.state.pause_write.store(true, Ordering::SeqCst);
    let table = domain.table::<Value>("rows").unwrap();
    let writing = tokio::spawn(async move { table.put("one", Value::Null).await });
    backend.state.entered.notified().await;
    drop(domain);
    writing.abort();
    assert!(writing.await.unwrap_err().is_cancelled());
    let reopened = facility.open(spec(), schema()).await.unwrap();
    assert_eq!(backend.state.loads.load(Ordering::SeqCst), 1);
    let mut changed = spec();
    changed.maximum_records += 1;
    assert!(matches!(
        facility.open(changed, schema()).await,
        Err(StorageError::InvalidInput(_))
    ));
    backend.state.release.notify_one();
    // A queued operation is the barrier; synchronous reads may still observe the
    // previous committed state while the first write is in progress.
    reopened
        .table::<Value>("rows")
        .unwrap()
        .put("two", json!(2))
        .await
        .unwrap();
    assert_eq!(
        rows(&reopened),
        BTreeMap::from([("one".into(), Value::Null), ("two".into(), json!(2))])
    );
    reopened.close().await;
    assert!(facility.registry.domains.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opening_reserves_one_authority_and_cancelled_initializer_can_retry() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = fixture(backend.clone()).await;
    backend.state.pause_load.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let facility = facility.clone();
        async move { facility.open(spec(), schema()).await }
    });
    backend.state.entered.notified().await;
    let mut second = pin!(facility.open(spec(), schema()));
    assert!(matches!(
        second
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    let mut conflict = spec();
    conflict.maximum_bytes += 1;
    assert!(facility.open(conflict, schema()).await.is_err());
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let domain = second.await.unwrap();
    assert_eq!(backend.state.loads.load(Ordering::SeqCst), 2);
    domain.close().await;
}

#[tokio::test]
async fn completed_load_is_shared_and_historical_domain_names_are_removed() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = fixture(backend.clone()).await;
    let first = facility.open(spec(), schema()).await.unwrap();
    let second = facility.open(spec(), schema()).await.unwrap();
    assert_eq!(backend.state.loads.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&first.authority, &second.authority));
    assert!(
        facility
            .open(spec(), Schema::default().table::<String>("rows").unwrap())
            .await
            .is_err()
    );
    drop(first);
    drop(second);
    assert!(facility.registry.domains.lock().unwrap().is_empty());
    for n in 0..20 {
        let mut spec = spec();
        spec.id = format!("historical-{n}");
        let domain = facility.open(spec, schema()).await.unwrap();
        domain.close().await;
    }
    assert!(facility.registry.domains.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_initialization_is_retryable_and_stale_cleanup_preserves_replacement() {
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = fixture(backend.clone()).await;
    backend.state.reject_load.store(true, Ordering::SeqCst);
    assert!(facility.open(spec(), schema()).await.is_err());
    assert!(facility.registry.domains.lock().unwrap().is_empty());
    let old = facility.open(spec(), schema()).await.unwrap();
    old.close().await;
    let replacement = facility.open(spec(), schema()).await.unwrap();
    drop(old);
    assert!(
        facility
            .registry
            .domains
            .lock()
            .unwrap()
            .get("domain")
            .unwrap()
            .ptr_eq(&Arc::downgrade(&replacement.authority))
    );
    replacement.close().await;
    assert!(facility.registry.domains.lock().unwrap().is_empty());
}

#[test]
fn runtime_shutdown_drops_an_unfinished_commit_and_fences_its_retained_handle() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let backend = Arc::new(Backend::default());
    let (_meta, _lease, _facility, domain) = runtime.block_on(async {
        let (meta, lease, facility) = fixture(backend.clone()).await;
        let domain = facility.open(spec(), schema()).await.unwrap();
        backend
            .state
            .pause_after_write
            .store(true, Ordering::SeqCst);
        let table = domain.table::<Value>("rows").unwrap();
        let waiter = tokio::spawn(async move { table.put("one", Value::Null).await });
        backend.state.entered.notified().await;
        assert!(backend.values("domain").contains_key("rows/one"));
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        (meta, lease, facility, domain)
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
        assert!(matches!(
            domain.snapshot(),
            Err(StorageError::RecoveryRequired)
        ));
        domain.close().await;
    });
}

#[tokio::test]
async fn exact_bounds_survive_load_replace_delete_and_reopen() {
    let backend = Arc::new(Backend::default());
    backend.seed("domain", 1, address("one"), &json!("界\"\n"));
    let (_runtime, _lease, facility) = fixture(backend.clone()).await;
    let mut bounded = spec();
    bounded.maximum_bytes = 64;
    let domain = facility.open(bounded.clone(), schema()).await.unwrap();
    let table = domain.table::<Value>("rows").unwrap();
    table.put("two", json!([1, 2])).await.unwrap();
    table.put("one", Value::Null).await.unwrap();
    table.put("three", json!(true)).await.unwrap();
    let before = rows(&domain);
    assert!(matches!(
        table.put("two", json!("x".repeat(64))).await,
        Err(StorageError::InvalidInput(_))
    ));
    assert_eq!(rows(&domain), before);
    assert_eq!(
        table
            .delete_many(["one", "three", "one", "absent"])
            .await
            .unwrap(),
        2
    );
    let framing = serde_json::to_vec(&BTreeMap::from([(
        "rows/two",
        Value::String(String::new()),
    )]))
    .unwrap()
    .len();
    let boundary = json!("x".repeat(64 - framing));
    table.put("two", boundary.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_vec(&backend.values("domain")).unwrap().len(),
        64
    );
    assert!(matches!(
        table.put("two", json!("x".repeat(65 - framing))).await,
        Err(StorageError::InvalidInput(_))
    ));
    domain.close().await;
    let reopened = facility.open(bounded, schema()).await.unwrap();
    let table = reopened.table::<Value>("rows").unwrap();
    assert_eq!(table.get("two").unwrap().unwrap().as_ref(), &boundary);
    assert!(table.delete("two").await.unwrap());
    assert!(reopened.snapshot().unwrap().is_empty());
    backend.state.unhealthy.store(true, Ordering::Release);
    assert_eq!(
        table.delete(&"x".repeat(257)).await,
        Err(StorageError::RecoveryRequired)
    );
    reopened.close().await;
}

#[test]
fn api_projection_preserves_commit_certainty_and_diagnostic_failures() {
    use rsi_api_protocol::ApiError;
    assert_eq!(storage_error(StorageError::Closed), ApiError::ShuttingDown);
    for error in [
        StorageError::Io("before commit".into()),
        StorageError::RecoveryRequired,
        StorageError::BackendUnavailable("retired".into()),
        StorageError::Unsupported("native protection".into()),
    ] {
        assert_eq!(storage_error(error), ApiError::Unavailable);
    }
    assert_eq!(
        storage_error(StorageError::OutcomeUnknown("lost reply".into())),
        ApiError::OutcomeUnknown
    );
    for error in [
        StorageError::Corrupt("invalid".into()),
        StorageError::DuplicateBackend("duplicate".into()),
    ] {
        assert_eq!(
            storage_error(error.clone()),
            ApiError::Backend(error.to_string())
        );
    }
    assert_eq!(
        storage_error(StorageError::InvalidInput("invalid".into())),
        ApiError::Invalid("invalid".into())
    );
}

#[tokio::test]
async fn api_mutation_preserves_an_over_bound_domain_rejection_without_writing() {
    use rsi_api_protocol::{
        ApiContext, ApiDispatch, ApiError, ApiHandler, ApiOutput, ApiRegistrar, CallOrigin,
        OperationAccess, OperationClass, OperationEffect, OperationId, OperationSpec,
        RequestEncoding, RetainedBytes,
    };
    #[derive(Debug)]
    struct OverBoundWrite(Table<Value>);
    #[async_trait]
    impl ApiHandler for OverBoundWrite {
        async fn invoke(
            &self,
            _: ApiContext,
            _: RetainedBytes,
            _: rsi_api_protocol::ApiResponseCapacity,
        ) -> rsi_api_protocol::Result<ApiOutput> {
            self.0
                .put("two", Value::Null)
                .await
                .map_err(storage_error)?;
            unreachable!("over-bound write must reject")
        }
    }
    let backend = Arc::new(Backend::default());
    let (_runtime, _lease, facility) = fixture(backend.clone()).await;
    let mut specification = spec();
    specification.maximum_records = 1;
    let domain = facility.open(specification, schema()).await.unwrap();
    domain
        .table::<Value>("rows")
        .unwrap()
        .put("one", Value::Null)
        .await
        .unwrap();
    let registry = rsi_api::ApiRegistry::new(rsi_meta::Execution::native(
        tokio::runtime::Handle::current(),
    ));
    let operation = OperationSpec {
        protocol: rsi_api_protocol::connection_protocol(),
        id: OperationId::new("storage", "test-write", 1).unwrap(),
        class: OperationClass::Data,
        effect: OperationEffect::Mutation,
        access: OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128,
    };
    let registration = registry
        .register(
            operation.clone(),
            Arc::new(OverBoundWrite(domain.table("rows").unwrap())),
        )
        .unwrap();
    let call = registry
        .admit(&operation.call(), CallOrigin::Local)
        .unwrap();
    let input = call.input_budget().copy(b"null").unwrap();
    assert!(matches!(
        call.invoke(input).await,
        Err(ApiError::Invalid(_))
    ));
    assert_eq!(backend.values("domain").len(), 1);
    assert!(!backend.values("domain").contains_key("rows/two"));
    assert_eq!(domain.snapshot().unwrap().len(), 1);
    registration.close().await;
    registry.close().await;
}
