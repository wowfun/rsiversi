use super::*;

#[derive(Debug)]
struct DualCaptureFactory {
    slot: Arc<Mutex<Option<(Capability, Capability)>>>,
}

#[async_trait]
impl PluginFactory for DualCaptureFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone())
            .requiring(Requirement::new("echo", "fixture.echo", V1))
            .requiring(Requirement::new("upstream", "fixture.upstream", V1)))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let echo = plan
            .inject("echo")
            .ok_or_else(|| MetaError::ServiceUnavailable {
                service: "echo".into(),
            })?
            .clone();
        let upstream = plan
            .inject("upstream")
            .ok_or_else(|| MetaError::ServiceUnavailable {
                service: "upstream".into(),
            })?
            .clone();
        *self.slot.lock().expect("dual capture poisoned") = Some((echo, upstream));
        Ok(())
    }
}

#[tokio::test]
async fn explicit_artifact_path_dynamically_provides_echo_without_a_loader_service() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;

    let native = runtime
        .root()
        .apply(
            catalog.load(native_fixture()).unwrap(),
            json!({ "prefix": "native:" }),
        )
        .await
        .unwrap();
    wait_active(&native).await;

    let echo_slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureFactory::new(
                "echo-client",
                Requirement::new("echo", "fixture.echo", V1),
                Arc::clone(&echo_slot),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let echo = echo_slot.lock().unwrap().take().unwrap();
    let response = echo
        .invoke(Message::new(b"hello".as_slice()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"native:upstream:hello");
    drop(response);
    drop(echo);
    let report = tokio::time::timeout(Duration::from_secs(5), runtime.shutdown())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "native shutdown stalled: runtime={:?}, resources={:?}, catalog={:?}",
                runtime.snapshot(),
                runtime.resource_snapshot(),
                catalog.snapshot(),
            )
        });
    assert!(
        report.is_clean(),
        "native shutdown was not clean: outcome={report:?}, resources={:?}, catalog={:?}",
        runtime.resource_snapshot(),
        catalog.snapshot()
    );
}

#[tokio::test]
async fn nested_native_bridge_preserves_transferred_capability_and_bidi_terminal() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let native = runtime
        .root()
        .apply(
            catalog.load(native_fixture()).unwrap(),
            json!({ "prefix": "native:" }),
        )
        .await
        .unwrap();
    wait_active(&native).await;

    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(DualCaptureFactory {
                slot: Arc::clone(&slot),
            })),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let (echo, transferable) = slot.lock().unwrap().take().unwrap();

    let mut call = echo.open().unwrap();
    call.send(Message::from_parts(b"first".as_slice(), vec![transferable]))
        .await
        .unwrap();
    call.finish();
    let response = call.recv().await.unwrap().unwrap();
    assert_eq!(response.as_bytes(), b"native:upstream:first");
    assert_eq!(response.capabilities().len(), 1);
    assert!(call.recv().await.unwrap().is_none());

    let nested = response.capabilities()[0]
        .invoke(Message::new(b"second".as_slice()))
        .await
        .unwrap();
    assert_eq!(nested.as_bytes(), b"upstream:second");
    drop(nested);
    drop(response);
    drop(call);
    drop(echo);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn outbound_native_host_calls_support_a_current_thread_runtime() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let (_native, service) =
        apply_delayed_native(&runtime, &catalog, json!({ "prefix": "current-thread:" })).await;

    let response = service
        .invoke(Message::new(b"bridge".as_slice()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"current-thread:upstream:bridge");
    drop(response);
    drop(service);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn native_provide_is_atomic_at_the_host_capability_limit() {
    let rejected_cache = tempfile::tempdir().unwrap();
    let mut rejected_options = CatalogOptions::new(rejected_cache.path());
    rejected_options.limits.maximum_host_capabilities = 3;
    let rejected_catalog = NativeCatalog::new(rejected_options).unwrap();
    let rejected_runtime = Runtime::default();
    let rejected_upstream = rejected_runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&rejected_upstream).await;
    let rejected_baseline = rejected_runtime.resource_snapshot();

    let rejected_native = rejected_runtime
        .root()
        .apply(
            rejected_catalog.load(native_fixture()).unwrap(),
            json!({ "prefix": "rejected:" }),
        )
        .await
        .unwrap();
    assert!(matches!(
        rejected_native.snapshot().state,
        FiberState::Failed(_)
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = rejected_catalog.snapshot();
            let resources = rejected_runtime.resource_snapshot();
            if snapshot.host_capabilities == 0
                && snapshot.active_instances == 0
                && resources.services.current == rejected_baseline.services.current
                && resources.effects.current == rejected_baseline.effects.current
                && resources.effect_transactions.current
                    == rejected_baseline.effect_transactions.current
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "failed provide did not roll back native ownership: catalog={:?}; resources={:?}",
            rejected_catalog.snapshot(),
            rejected_runtime.resource_snapshot()
        )
    });
    let rejected = rejected_catalog.snapshot();
    assert_eq!(rejected.peak_host_capabilities, 3);
    assert_eq!(rejected.rejected_host_capabilities, 1);
    assert_eq!(rejected.host_capabilities, 0);
    assert_eq!(rejected.active_instances, 0);
    let rejected_resources = rejected_runtime.resource_snapshot();
    assert_eq!(
        rejected_resources.services.current,
        rejected_baseline.services.current
    );
    assert_eq!(
        rejected_resources.effects.current,
        rejected_baseline.effects.current
    );
    assert_eq!(
        rejected_resources.effect_transactions.current,
        rejected_baseline.effect_transactions.current
    );
    assert_clean_shutdown(&rejected_runtime).await;
    assert_eq!(rejected_catalog.snapshot().retained_failed_finalizations, 0);

    let committed_cache = tempfile::tempdir().unwrap();
    let mut committed_options = CatalogOptions::new(committed_cache.path());
    committed_options.limits.maximum_host_capabilities = 4;
    let committed_catalog = NativeCatalog::new(committed_options).unwrap();
    let committed_runtime = Runtime::default();
    let committed_upstream = committed_runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&committed_upstream).await;
    let committed_native = committed_runtime
        .root()
        .apply(
            committed_catalog.load(native_fixture()).unwrap(),
            json!({ "prefix": "committed:" }),
        )
        .await
        .unwrap();
    wait_active(&committed_native).await;

    let committed = committed_catalog.snapshot();
    assert_eq!(committed.peak_host_capabilities, 4);
    assert_eq!(committed.rejected_host_capabilities, 0);
    assert_eq!(committed.host_capabilities, 1);
    assert_eq!(committed_runtime.resource_snapshot().services.current, 2);
    assert_clean_shutdown(&committed_runtime).await;
    let released = committed_catalog.snapshot();
    assert_eq!(released.host_capabilities, 0);
    assert_eq!(released.retained_failed_finalizations, 0);
}

#[cfg(target_os = "linux")]
#[test]
fn catalog_rejects_an_artifact_that_exports_only_v2() {
    let (_fixture, artifact) = v2_only_fixture();
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("ABI v3 must not probe or accept the removed v2 entry");
    assert!(
        error.to_string().contains("rsi_meta_plugin_entry_v3"),
        "{error}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn catalog_preserves_a_plugin_entry_failure_status() {
    let markers = tempfile::tempdir().unwrap();
    let exchange_log = markers.path().join("entry-failure-exchanges");
    let (_fixture, artifact) =
        failed_entry_fixture(rsi_meta_native::STATUS_PANICKED, &exchange_log);
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("a panicked plugin entry must fail loading");

    assert!(
        matches!(
            error,
            LoaderError::PluginEntry {
                status: rsi_meta_native::STATUS_PANICKED,
            }
        ),
        "{error}"
    );
    wait_for_staging_release(&catalog);
    assert_eq!(
        std::fs::read(exchange_log).unwrap(),
        b"df",
        "compatible non-OK entry must skip IDENTITY and run exactly DESTROY_FACTORY then FINALIZE"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn malformed_plugin_output_is_released_exactly_once_before_rejection() {
    let markers = tempfile::tempdir().unwrap();
    let released = markers.path().join("released");
    let finalized = markers.path().join("finalized");
    let (_fixture, artifact) = malformed_output_fixture(&released, &finalized);
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("a nonempty null byte range must be rejected");

    assert!(
        matches!(
            error,
            LoaderError::Protocol {
                operation: "identity",
                ref message,
            } if message.contains("null pointer")
        ),
        "{error}"
    );
    wait_for_file(&finalized).await;
    assert_eq!(
        std::fs::read(&released).unwrap(),
        b"x",
        "the compatible v3 table's transferred output lease was not released exactly once"
    );
}
