use super::*;

#[tokio::test]
async fn real_dynamic_library_runs_through_loader_plugin_and_outbound_host_bridge() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;

    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({ "entries": [] }),
        )
        .await
        .unwrap();
    wait_active(&loader).await;

    let loader_slot = Arc::new(Mutex::new(None));
    let loader_client = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("loader-client", "1"))
                    .requiring(Requirement::new(
                        LOADER_SERVICE_KEY,
                        LOADER_CONTRACT_ID,
                        LOADER_CONTRACT_VERSION,
                    )),
                slot: Arc::clone(&loader_slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&loader_client).await;
    let command = json!({
        "operation": "load",
        "id": "native-echo",
        "artifact": native_fixture(),
        "config": { "prefix": "native:" }
    });
    let loader_service = loader_slot
        .lock()
        .expect("loader capture poisoned")
        .clone()
        .unwrap();
    let response = loader_service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(serde_json::to_vec(&command).unwrap()))
        .await
        .unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["fiber"]["state"], "active");

    let echo_slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("echo-client", "1"))
                    .requiring(Requirement::new("echo", "fixture.echo", V1)),
                slot: Arc::clone(&echo_slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let echo_service = echo_slot
        .lock()
        .expect("echo capture poisoned")
        .clone()
        .unwrap();
    let response = echo_service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(b"hello".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"native:upstream:hello");
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(flavor = "current_thread")]
async fn outbound_native_host_bridge_supports_a_current_thread_runtime() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let (_native, service) =
        apply_delayed_native(&runtime, &catalog, json!({ "prefix": "current-thread:" })).await;

    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(b"bridge".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"current-thread:upstream:bridge");
    assert!(runtime.shutdown().await.is_clean());
}

#[cfg(target_os = "linux")]
#[test]
fn catalog_preserves_a_plugin_entry_failure_status() {
    let (_fixture, artifact) = failed_entry_fixture(rsi_meta_plugin::STATUS_PANICKED);
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("a panicked plugin entry must fail loading");

    assert!(
        matches!(
            error,
            LoaderError::PluginEntry {
                status: rsi_meta_plugin::STATUS_PANICKED,
            }
        ),
        "{error}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn malformed_plugin_buffer_is_released_before_rejection() {
    let markers = tempfile::tempdir().unwrap();
    let released = markers.path().join("released");
    let (_fixture, artifact) = malformed_buffer_fixture(&released);
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("a nonempty null buffer must be rejected");

    assert!(
        matches!(
            error,
            rsi_meta_loader::LoaderError::Callback {
                operation: "descriptor",
                ref message,
            } if message.contains("null pointer")
        ),
        "{error}"
    );
    assert!(released.exists(), "rejection skipped release_buffer");
}
