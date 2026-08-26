use super::*;

#[tokio::test]
async fn loader_preflights_every_config_before_it_applies_any_child() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({
                "entries": [
                    {
                        "id": "valid",
                        "artifact": native_fixture(),
                        "config": { "prefix": "ok:" }
                    },
                    {
                        "id": "invalid",
                        "artifact": native_fixture(),
                        "config": { "prefix": 42 }
                    }
                ]
            }),
        )
        .await
        .unwrap();
    assert!(matches!(loader.snapshot().state, FiberState::Failed(_)));
    assert_eq!(
        runtime.snapshot().fibers.len(),
        1,
        "preflight leaked a child Fiber"
    );
}

#[tokio::test]
async fn initial_loader_batch_respects_a_single_catalog_load_slot() {
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_concurrent_callbacks = 1;
    let catalog = NativeCatalog::new(options).unwrap();
    let observed_catalog = catalog.clone();
    let runtime = Runtime::default();

    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({
                "entries": [
                    {
                        "id": "first",
                        "artifact": native_fixture(),
                        "config": { "prefix": "first:" }
                    },
                    {
                        "id": "second",
                        "artifact": native_fixture(),
                        "config": { "prefix": "second:" }
                    }
                ]
            }),
        )
        .await
        .unwrap();

    wait_active(&loader).await;
    let snapshot = observed_catalog.snapshot();
    assert_eq!(snapshot.peak_loads, 1);
    assert_eq!(snapshot.rejected_loads, 0);
    assert_eq!(runtime.snapshot().fibers.len(), 3);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn loader_serializes_preflight_for_entries_sharing_one_native_module() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let markers = tempfile::tempdir().unwrap();
    let first_entered = markers.path().join("first-entered");
    let first_release = markers.path().join("first-release");
    let second_entered = markers.path().join("second-entered");
    let second_release = markers.path().join("second-release");
    let application = tokio::spawn({
        let root = runtime.root();
        let catalog = catalog.clone();
        let artifact = native_fixture().clone();
        let first_entered = first_entered.clone();
        let first_release = first_release.clone();
        let second_entered = second_entered.clone();
        let second_release = second_release.clone();
        async move {
            root.apply(
                Arc::new(LoaderFactory::new(catalog)),
                json!({
                    "entries": [
                        {
                            "id": "first",
                            "artifact": artifact,
                            "config": {
                                "prefix": "first:",
                                "validate_entered_path": first_entered,
                                "validate_release_path": first_release
                            }
                        },
                        {
                            "id": "second",
                            "artifact": native_fixture(),
                            "config": {
                                "prefix": "second:",
                                "validate_entered_path": second_entered,
                                "validate_release_path": second_release
                            }
                        }
                    ]
                }),
            )
            .await
        }
    });

    let first_started = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if first_entered.exists() {
                break true;
            }
            if second_entered.exists() {
                break false;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one same-module validation starts");
    let (started_release, waiting_entered, waiting_release) = if first_started {
        (&first_release, &second_entered, &second_release)
    } else {
        (&second_release, &first_entered, &first_release)
    };
    std::fs::write(started_release, b"release").unwrap();
    let waiting_started =
        tokio::time::timeout(Duration::from_secs(1), wait_for_file(waiting_entered))
            .await
            .is_ok();
    std::fs::write(waiting_release, b"release").unwrap();
    let loader = application.await.unwrap().unwrap();

    assert!(
        waiting_started,
        "same-module sibling failed fast instead of entering after its predecessor"
    );
    wait_active(&loader).await;
    assert_eq!(runtime.snapshot().fibers.len(), 3);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn loader_capacity_preflight_publishes_no_child() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 3,
            maximum_fiber_depth: 3,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
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
            json!({
                "entries": [
                    {
                        "id": "applied-before-failure",
                        "artifact": native_fixture(),
                        "config": { "prefix": "first:" }
                    },
                    {
                        "id": "capacity-failure",
                        "artifact": native_fixture(),
                        "config": { "prefix": "second:" }
                    }
                ]
            }),
        )
        .await
        .unwrap();

    assert!(matches!(loader.snapshot().state, FiberState::Failed(_)));
    assert_eq!(
        runtime.snapshot().fibers.len(),
        2,
        "capacity preflight published a child Fiber"
    );
    assert_clean_shutdown(&runtime).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancelled_initial_preflight_cannot_accumulate_native_workers() {
    let markers = tempfile::tempdir().unwrap();
    let entry_log = markers.path().join("entry-log");
    let entry_release = markers.path().join("entry-release");
    let (_fixture, artifact) = blocking_entry_fixture(&entry_log, &entry_release);
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(10);
    options.limits.maximum_concurrent_callbacks = 1;
    let catalog = NativeCatalog::new(options).unwrap();
    let runtime = Runtime::default();

    let first = tokio::spawn({
        let root = runtime.root();
        let factory = Arc::new(LoaderFactory::new(catalog.clone()));
        let artifact = artifact.clone();
        async move {
            root.apply(
                factory,
                json!({
                    "entries": [{
                        "id": "cancelled-owner",
                        "artifact": artifact,
                        "config": {}
                    }]
                }),
            )
            .await
        }
    });
    wait_for_file(&entry_log).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let staging_before_rejections = catalog.snapshot().staging_bytes;

    let mut rejected = Vec::new();
    for index in 0..8 {
        let loader = runtime
            .root()
            .apply(
                Arc::new(LoaderFactory::new(catalog.clone())),
                json!({
                    "entries": [{
                        "id": format!("rejected-{index}"),
                        "artifact": artifact,
                        "config": {}
                    }]
                }),
            )
            .await
            .unwrap();
        assert!(matches!(loader.snapshot().state, FiberState::Failed(_)));
        rejected.push(loader);
    }

    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.active_loads, 1);
    assert_eq!(snapshot.peak_loads, 1);
    assert_eq!(snapshot.rejected_loads, 8);
    assert_eq!(snapshot.staging_bytes, staging_before_rejections);
    assert_eq!(std::fs::read(&entry_log).unwrap(), b"x");

    std::fs::write(&entry_release, b"release").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while catalog.snapshot().active_loads != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled initial preflight retained load admission after worker exit");
    drop(rejected);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn concurrent_loader_commands_cannot_claim_the_same_id_twice() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({ "entries": [] }),
        )
        .await
        .unwrap();
    wait_active(&loader).await;
    let slot = Arc::new(Mutex::new(None));
    let client = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory::new(
                "concurrent-loader-client",
                Requirement::new(
                    LOADER_SERVICE_KEY,
                    LOADER_CONTRACT_ID,
                    LOADER_CONTRACT_VERSION,
                ),
                Arc::clone(&slot),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&client).await;
    let service = slot.lock().unwrap().take().unwrap();
    let command = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "same-id",
        "artifact": native_fixture(),
        "config": { "prefix": "native:" }
    }))
    .unwrap();

    let first = service.invoke(Message::new(command.clone()));
    let second = service.invoke(Message::new(command));
    let (first, second) = tokio::join!(first, second);
    let responses = [first.unwrap(), second.unwrap()]
        .map(|frame| serde_json::from_slice::<Value>(frame.as_bytes()).unwrap());

    assert_eq!(
        responses
            .iter()
            .filter(|response| response["ok"] == true)
            .count(),
        1
    );
    assert_eq!(
        responses
            .iter()
            .filter(|response| response["error"] == "loader entry id already exists")
            .count(),
        1
    );
    drop(service);
    assert_clean_shutdown(&runtime).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers pre-task admission, cancellation, and ID reuse together.
async fn cancelled_dynamic_loads_cannot_accumulate_tasks_or_id_claims_before_admission() {
    let markers = tempfile::tempdir().unwrap();
    let entry_log = markers.path().join("entry-log");
    let entry_release = markers.path().join("entry-release");
    let (_fixture, artifact) = blocking_entry_fixture(&entry_log, &entry_release);
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(10);
    options.limits.maximum_concurrent_callbacks = 1;
    let catalog = NativeCatalog::new(options).unwrap();
    let runtime = Runtime::default();
    let service = apply_loader_service(&runtime, catalog.clone(), "bounded-loader-client").await;

    let first = tokio::spawn({
        let service = service.clone();
        let artifact = artifact.clone();
        async move {
            let command = serde_json::to_vec(&json!({
                "operation": "load",
                "id": "cancelled-owner",
                "artifact": artifact,
                "config": {}
            }))
            .unwrap();
            service.invoke(Message::new(command)).await
        }
    });
    wait_for_file(&entry_log).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let staging_before_rejections = catalog.snapshot().staging_bytes;

    for index in 0..16 {
        let command = serde_json::to_vec(&json!({
            "operation": "load",
            "id": format!("rejected-{index}"),
            "artifact": artifact,
            "config": {}
        }))
        .unwrap();
        let response = service.invoke(Message::new(command)).await.unwrap();
        let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
        assert_eq!(response["error"], "native operation is busy: load");
    }
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.active_loads, 1);
    assert_eq!(snapshot.peak_loads, 1);
    assert_eq!(snapshot.rejected_loads, 16);
    assert_eq!(snapshot.staging_bytes, staging_before_rejections);

    std::fs::write(&entry_release, b"release").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while catalog.snapshot().active_loads != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled runtime-owned load retained its admission after completion");

    let retry = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "rejected-0",
        "artifact": native_fixture(),
        "config": { "prefix": "retry:" }
    }))
    .unwrap();
    let response = service.invoke(Message::new(retry)).await.unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    drop(service);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One cancellation scenario proves claim, rollback, and reuse ordering.
async fn cancelling_a_loader_command_releases_its_id_reservation() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(10));
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
    let slot = Arc::new(Mutex::new(None));
    let client = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory::new(
                "cancelled-loader-client",
                Requirement::new(
                    LOADER_SERVICE_KEY,
                    LOADER_CONTRACT_ID,
                    LOADER_CONTRACT_VERSION,
                ),
                Arc::clone(&slot),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&client).await;
    let service = slot.lock().unwrap().take().unwrap();
    let markers = tempfile::tempdir().unwrap();
    let entered = markers.path().join("create-entered");
    let release = markers.path().join("create-release");
    let completed = markers.path().join("create-completed");
    let task_entered = entered.clone();
    let task_release = release.clone();
    let task_completed = completed.clone();
    let first = tokio::spawn({
        let service = service.clone();
        let command = serde_json::to_vec(&json!({
            "operation": "load",
            "id": "retryable",
            "artifact": native_fixture(),
            "config": {
                "prefix": "cancelled:",
                "create_entered_path": task_entered,
                "create_release_path": task_release,
                "create_completed_path": task_completed
            }
        }))
        .unwrap();
        async move { service.invoke(Message::new(command)).await }
    });
    wait_for_file(&entered).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let overlapping = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "retryable",
        "artifact": markers.path().join("does-not-exist.so"),
        "config": {}
    }))
    .unwrap();
    let overlapping = service.invoke(Message::new(overlapping)).await.unwrap();
    let overlapping: Value = serde_json::from_slice(overlapping.as_bytes()).unwrap();
    assert_eq!(
        overlapping["error"], "loader entry id already exists",
        "a cancelled load released its ID before the runtime-owned apply settled: {overlapping}"
    );

    std::fs::write(&release, b"release").unwrap();
    wait_for_file(&completed).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.snapshot().fibers.len() != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled Loader command did not roll back its child");

    let retry = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "retryable",
        "artifact": native_fixture(),
        "config": { "prefix": "retry:" }
    }))
    .unwrap();
    let response = service.invoke(Message::new(retry)).await.unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    drop(service);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One real-native scenario spans load, blocked unload, conflict, and reuse.
async fn loader_id_remains_reserved_until_unload_cleanup_finishes() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
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
    let slot = Arc::new(Mutex::new(None));
    let client = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory::new(
                "unloading-loader-client",
                Requirement::new(
                    LOADER_SERVICE_KEY,
                    LOADER_CONTRACT_ID,
                    LOADER_CONTRACT_VERSION,
                ),
                Arc::clone(&slot),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&client).await;
    let service = slot.lock().unwrap().take().unwrap();
    let markers = tempfile::tempdir().unwrap();
    let destroy_entered = markers.path().join("destroy-entered");
    let destroy_release = markers.path().join("destroy-release");
    let initial = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "stable",
        "artifact": native_fixture(),
        "config": {
            "prefix": "initial:",
            "destroy_entered_path": destroy_entered,
            "destroy_release_path": destroy_release
        }
    }))
    .unwrap();
    let load_response = service.invoke(Message::new(initial)).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(load_response.as_bytes()).unwrap()["ok"],
        true
    );

    let unloading = tokio::spawn({
        let service = service.clone();
        async move {
            let command = serde_json::to_vec(&json!({
                "operation": "unload",
                "id": "stable"
            }))
            .unwrap();
            service.invoke(Message::new(command)).await
        }
    });
    wait_for_file(&destroy_entered).await;
    let conflicting = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "stable",
        "artifact": native_fixture(),
        "config": { "prefix": "conflicting:" }
    }))
    .unwrap();
    let response = service.invoke(Message::new(conflicting)).await.unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["error"], "loader entry id already exists");

    std::fs::write(&destroy_release, b"release").unwrap();
    let unloaded = unloading.await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(unloaded.as_bytes()).unwrap()["ok"],
        true
    );
    let retry = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "stable",
        "artifact": native_fixture(),
        "config": { "prefix": "retry:" }
    }))
    .unwrap();
    let response = service.invoke(Message::new(retry)).await.unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    drop(service);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn loader_preflight_runs_distinct_native_artifacts_with_bounded_concurrency() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let observed_catalog = catalog.clone();
    let runtime = Runtime::default();
    let artifacts = tempfile::tempdir().unwrap();
    let first_artifact = artifacts.path().join("first-native");
    let second_artifact = artifacts.path().join("second-native");
    let bytes = std::fs::read(native_fixture()).unwrap();
    std::fs::write(&first_artifact, &bytes).unwrap();
    let mut distinct_bytes = bytes;
    distinct_bytes.extend_from_slice(b"distinct-digest");
    std::fs::write(&second_artifact, distinct_bytes).unwrap();

    let markers = tempfile::tempdir().unwrap();
    let first_entered = markers.path().join("first-entered");
    let first_release = markers.path().join("first-release");
    let second_entered = markers.path().join("second-entered");
    let second_release = markers.path().join("second-release");
    let task_first_entered = first_entered.clone();
    let task_first_release = first_release.clone();
    let task_second_entered = second_entered.clone();
    let task_second_release = second_release.clone();
    let application = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                Arc::new(LoaderFactory::new(catalog)),
                json!({
                    "entries": [
                        {
                            "id": "first",
                            "artifact": first_artifact,
                            "config": {
                                "prefix": "first:",
                                "validate_entered_path": task_first_entered,
                                "validate_release_path": task_first_release
                            }
                        },
                        {
                            "id": "second",
                            "artifact": second_artifact,
                            "config": {
                                "prefix": "second:",
                                "validate_entered_path": task_second_entered,
                                "validate_release_path": task_second_release
                            }
                        }
                    ]
                }),
            )
            .await
        }
    });
    let concurrent = tokio::time::timeout(Duration::from_secs(5), async {
        while !first_entered.exists() || !second_entered.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    std::fs::write(&first_release, b"release").unwrap();
    std::fs::write(&second_release, b"release").unwrap();
    let loader = application.await.unwrap().unwrap();
    wait_active(&loader).await;
    assert!(concurrent, "loader preflight serialized distinct artifacts");
    assert_eq!(observed_catalog.snapshot().peak_loads, 2);
    assert_eq!(observed_catalog.snapshot().rejected_loads, 0);
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn loader_distinct_module_preflight_respects_runtime_preparation_limit() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(10));
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            maximum_concurrent_preparations: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    let first_artifact = artifacts.path().join("first-native");
    let second_artifact = artifacts.path().join("second-native");
    let bytes = std::fs::read(native_fixture()).unwrap();
    std::fs::write(&first_artifact, &bytes).unwrap();
    let mut distinct_bytes = bytes;
    distinct_bytes.extend_from_slice(b"distinct-preparation-limit-digest");
    std::fs::write(&second_artifact, distinct_bytes).unwrap();

    let markers = tempfile::tempdir().unwrap();
    let first_entered = markers.path().join("first-entered");
    let first_release = markers.path().join("first-release");
    let second_entered = markers.path().join("second-entered");
    let second_release = markers.path().join("second-release");
    let application = tokio::spawn({
        let root = runtime.root();
        let first_entered = first_entered.clone();
        let first_release = first_release.clone();
        let second_entered = second_entered.clone();
        let second_release = second_release.clone();
        async move {
            root.apply(
                Arc::new(LoaderFactory::new(catalog)),
                json!({
                    "entries": [
                        {
                            "id": "first",
                            "artifact": first_artifact,
                            "config": {
                                "prefix": "first:",
                                "validate_entered_path": first_entered,
                                "validate_release_path": first_release
                            }
                        },
                        {
                            "id": "second",
                            "artifact": second_artifact,
                            "config": {
                                "prefix": "second:",
                                "validate_entered_path": second_entered,
                                "validate_release_path": second_release
                            }
                        }
                    ]
                }),
            )
            .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        while !first_entered.exists() && !second_entered.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one native normalizer did not enter");
    std::fs::write(first_release, b"release").unwrap();
    std::fs::write(second_release, b"release").unwrap();

    let loader = application.await.unwrap().unwrap();
    wait_active(&loader).await;
    let preparations = runtime.resource_snapshot().preparations;
    assert_eq!(preparations.high_watermark, 1);
    assert_eq!(preparations.rejected, 0);
    assert_eq!(runtime.snapshot().fibers.len(), 3);
    assert_clean_shutdown(&runtime).await;
}
