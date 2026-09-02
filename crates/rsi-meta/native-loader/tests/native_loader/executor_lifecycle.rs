use super::*;

fn release_gate_watchdog(
    release: PathBuf,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (cancel, cancelled) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if cancelled.recv_timeout(Duration::from_secs(5)).is_err() {
            let _ = std::fs::write(release, b"watchdog-release");
        }
    });
    (cancel, watchdog)
}

#[cfg(target_os = "linux")]
fn assert_destroy_worker(path: &Path) {
    let name = std::fs::read_to_string(path).expect("fixture recorded destroy thread name");
    assert!(
        name.starts_with("rsi-native-d-"),
        "foreign destruction ran on unexpected thread `{name}`"
    );
}

#[cfg(not(target_os = "linux"))]
fn assert_destroy_worker(_path: &Path) {}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn callback_admission_fails_before_spawning_an_extra_native_thread() {
    let markers = tempfile::tempdir().unwrap();
    let entry_log = markers.path().join("entry-log");
    let entry_release = markers.path().join("entry-release");
    let (_fixture, artifact) = blocking_entry_fixture(&entry_log, &entry_release);
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_millis(30);
    options.limits.maximum_concurrent_callbacks = 1;
    let catalog = NativeCatalog::new(options).unwrap();

    let first = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        move || catalog.load(artifact)
    });
    wait_for_file(&entry_log).await;
    assert!(matches!(
        first.await.unwrap(),
        Err(LoaderError::Timeout("native module initialization"))
    ));
    assert_eq!(catalog.snapshot().active_callbacks, 1);

    assert!(matches!(
        catalog.load(native_fixture()),
        Err(LoaderError::Busy { operation: "load" })
    ));
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.peak_callbacks, 1);
    assert_eq!(snapshot.rejected_callbacks, 1);

    std::fs::write(&entry_release, b"release").unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while catalog.snapshot().active_callbacks != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released native worker retained callback admission");
}

#[tokio::test]
async fn prepared_creates_fail_fast_at_the_shared_factory_gate() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(5));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let factory = catalog.load(native_fixture()).unwrap();
    wait_for_callback_quiescence_async(&catalog).await;
    let markers = tempfile::tempdir().unwrap();
    let first_entered = markers.path().join("first-create-entered");
    let first_release = markers.path().join("first-create-release");
    let second_entered = markers.path().join("second-create-entered");
    let first = runtime
        .prepare(
            factory.clone(),
            json!({
                "prefix": "first:",
                "create_entered_path": first_entered,
                "create_release_path": first_release,
            }),
        )
        .unwrap();
    let second = runtime
        .prepare(
            factory,
            json!({
                "prefix": "second:",
                "create_entered_path": second_entered,
            }),
        )
        .unwrap();

    let first_application = tokio::spawn({
        let root = runtime.root();
        async move { root.apply_prepared(first).await }
    });
    wait_for_file(&first_entered).await;
    let rejected = tokio::time::timeout(
        Duration::from_millis(100),
        runtime.root().apply_prepared(second),
    )
    .await
    .expect("a busy factory accumulated a hidden create waiter")
    .unwrap();
    let rejected_state = rejected.snapshot().state;
    let second_entered_native = second_entered.exists();
    let active_callbacks = catalog.snapshot().active_callbacks;
    std::fs::write(&first_release, b"release").unwrap();
    assert!(matches!(
        rejected_state,
        FiberState::Failed(ref message) if message.contains("busy")
    ));
    assert!(!second_entered_native, "busy create entered native code");
    assert_eq!(active_callbacks, 1);

    let first = first_application.await.unwrap().unwrap();
    wait_active(&first).await;
    assert!(rejected.dispose().await.is_clean());
    assert!(first.dispose().await.is_clean());
    assert!(upstream.dispose().await.is_clean());
    assert_clean_shutdown(&runtime).await;
}

async fn assert_same_lineage_reentry(
    catalog: &NativeCatalog,
    service: &Capability,
    entered: &Path,
    release: &Path,
) {
    let first_same_lineage = service.clone();
    let second_same_lineage = service.clone();
    let first = tokio::spawn(async move {
        first_same_lineage
            .invoke(Message::new(b"same-lineage-first".as_slice()))
            .await
    });
    wait_for_catalog_marker(entered, catalog).await;
    assert_eq!(catalog.snapshot().active_callbacks, 1);

    let second = tokio::spawn(async move {
        second_same_lineage
            .invoke(Message::new(b"same-lineage-second".as_slice()))
            .await
    });
    let second = tokio::time::timeout(Duration::from_secs(1), second).await;
    let active_callbacks = catalog.snapshot().active_callbacks;
    std::fs::write(release, b"release").unwrap();
    let second = second
        .expect("same-lineage reentry waited instead of failing fast")
        .unwrap();
    assert_eq!(
        active_callbacks, 1,
        "same-lineage reentry spawned a native callback thread"
    );
    assert!(
        matches!(second, Err(MetaError::Service(ref message)) if message.contains("reentrant")),
        "same-lineage instance reentry did not surface REENTRANT: {second:?}"
    );
    assert_eq!(
        first.await.unwrap().unwrap().as_bytes(),
        b"serial:upstream:same-lineage-first"
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while catalog.snapshot().active_callbacks != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same-lineage callback did not quiesce before the unrelated-lineage phase");
}

async fn assert_unrelated_lineage_contention(
    runtime: &Runtime,
    catalog: &NativeCatalog,
    service: Capability,
    entered: &Path,
    release: &Path,
) {
    for marker in [entered, release] {
        match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not reset native callback marker: {error}"),
        }
    }
    let unrelated_slot = Arc::new(Mutex::new(None));
    let unrelated_client = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureFactory::new(
                "unrelated-lineage-client",
                Requirement::new("echo", "fixture.echo", V1),
                Arc::clone(&unrelated_slot),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&unrelated_client).await;
    let unrelated_service = unrelated_slot.lock().unwrap().take().unwrap();

    let first = tokio::spawn(async move {
        service
            .invoke(Message::new(b"unrelated-first".as_slice()))
            .await
    });
    wait_for_catalog_marker(entered, catalog).await;
    assert_eq!(catalog.snapshot().active_callbacks, 1);
    let second = tokio::spawn(async move {
        unrelated_service
            .invoke(Message::new(b"unrelated-second".as_slice()))
            .await
    });
    let second = tokio::time::timeout(Duration::from_secs(1), second).await;
    let active_callbacks = catalog.snapshot().active_callbacks;
    std::fs::write(release, b"release").unwrap();
    let second = second
        .expect("unrelated instance contention waited instead of failing fast")
        .unwrap();
    assert_eq!(
        active_callbacks, 1,
        "unrelated contention spawned a native callback thread"
    );
    assert!(
        matches!(second, Err(MetaError::Service(ref message)) if message.contains("busy")),
        "unrelated lineage contention did not surface BUSY: {second:?}"
    );
    assert_eq!(
        first.await.unwrap().unwrap().as_bytes(),
        b"serial:upstream:unrelated-first"
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while catalog.snapshot().active_callbacks != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unrelated-lineage callback did not quiesce");
}

#[tokio::test]
async fn instance_gate_distinguishes_reentry_from_unrelated_lineage_contention() {
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(2);
    options.limits.maximum_concurrent_callbacks = 2;
    let catalog = NativeCatalog::new(options).unwrap();
    let runtime = Runtime::default();
    let markers = tempfile::tempdir().unwrap();
    let call_entered = markers.path().join("call-entered");
    let call_release = markers.path().join("call-release");
    let (_native, service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({
            "prefix": "serial:",
            "call_entered_path": call_entered,
            "call_release_path": call_release,
        }),
    )
    .await;

    assert_same_lineage_reentry(&catalog, &service, &call_entered, &call_release).await;
    assert_unrelated_lineage_contention(&runtime, &catalog, service, &call_entered, &call_release)
        .await;
    assert_clean_shutdown(&runtime).await;
}

#[tokio::test]
async fn native_watchdog_terminalizes_even_after_core_drops_the_adapter_future() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::new(RuntimeLimits {
        deadlines: DeadlineLimits {
            service_call: Duration::from_millis(20),
            ..DeadlineLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_native, service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({ "prefix": "native:", "delay_ms": 200 }),
    )
    .await;
    assert_eq!(
        service
            .invoke(Message::new(b"slow".as_slice()))
            .await
            .unwrap_err(),
        MetaError::Timeout("service call")
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime.snapshot().terminal.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("adapter watchdog did not terminalize the runtime");
    assert!(matches!(service.open(), Err(MetaError::RuntimeTerminal(_))));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timed_out_native_call_defers_destruction_until_the_callback_returns() {
    let markers = tempfile::tempdir().unwrap();
    let call_entered = markers.path().join("call-entered");
    let call_release = markers.path().join("call-release");
    let destroy_entered = markers.path().join("destroy-entered");
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(500));
    let runtime = Runtime::default();
    let (native, service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({
            "prefix": "native:",
            "call_entered_path": call_entered,
            "call_release_path": call_release,
            "destroy_entered_path": destroy_entered,
        }),
    )
    .await;
    let call =
        tokio::spawn(async move { service.invoke(Message::new(b"blocked".as_slice())).await });
    wait_for_file(&call_entered).await;
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(
        error,
        MetaError::RuntimeTerminal("trusted native plugin service callback timed out".to_owned()),
        "native endpoint timeout did not publish the Runtime terminal reason"
    );
    let retained = catalog.snapshot();
    assert_eq!(retained.active_callbacks, 1, "{retained:?}");
    assert_eq!(retained.active_instances, 1, "{retained:?}");
    assert_eq!(retained.host_capabilities, 1, "{retained:?}");
    assert_eq!(retained.pending_instance_destructions, 0, "{retained:?}");

    let (disposal_started, started) = tokio::sync::oneshot::channel();
    let disposal = tokio::spawn(async move {
        let _ = disposal_started.send(());
        native.dispose().await
    });
    started.await.expect("disposal task entered");
    tokio::task::yield_now().await;
    assert!(
        !disposal.is_finished(),
        "cleanup completed while its native callback was still running"
    );
    assert!(
        !destroy_entered.exists(),
        "destruction entered while the timed-out callback still held the instance"
    );
    let retained_during_cleanup = catalog.snapshot();
    assert_eq!(
        retained_during_cleanup.active_callbacks, 1,
        "{retained_during_cleanup:?}"
    );
    assert_eq!(
        retained_during_cleanup.active_instances, 1,
        "{retained_during_cleanup:?}"
    );

    std::fs::write(&call_release, b"release").unwrap();
    let report = tokio::time::timeout(Duration::from_secs(1), disposal)
        .await
        .expect("cleanup did not join released native destruction")
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    wait_for_catalog_marker(&destroy_entered, &catalog).await;
    wait_for_callback_quiescence_async(&catalog).await;
    let released = catalog.snapshot();
    assert_eq!(released.active_callbacks, 0, "{released:?}");
    assert_eq!(released.active_instances, 0, "{released:?}");
    assert_eq!(released.host_capabilities, 0, "{released:?}");
}

#[tokio::test]
async fn timed_out_factory_callback_poison_prevents_later_overlap() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(500));
    let factory = catalog.load(native_fixture()).unwrap();
    let runtime = Runtime::default();
    let timed_out = runtime
        .root()
        .apply(
            factory.clone(),
            json!({ "prefix": "slow:", "validate_delay_ms": 2_000 }),
        )
        .await;
    assert!(
        matches!(
            timed_out,
            Err(MetaError::Timeout("native prepare callback"))
        ),
        "unexpected native prepare timeout result: {timed_out:?}"
    );
    let error = runtime
        .root()
        .apply(factory, json!({ "prefix": "later:" }))
        .await
        .expect_err("a timed-out factory must remain poisoned");
    assert!(error.to_string().contains("poisoned"), "{error}");
}

#[tokio::test]
async fn native_create_timeout_terminalizes_without_publishing_the_fiber() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(500));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let factory = catalog.load(native_fixture()).unwrap();
    let native = runtime
        .root()
        .apply(
            factory,
            json!({ "prefix": "slow:", "create_delay_ms": 2_000 }),
        )
        .await
        .unwrap();

    assert!(runtime.snapshot().terminal.is_some());
    assert!(matches!(native.snapshot().state, FiberState::Failed(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn publication_failure_never_runs_native_destruction_on_the_executor() {
    let markers = tempfile::tempdir().unwrap();
    let destroy_entered = markers.path().join("destroy-entered");
    let destroy_release = markers.path().join("destroy-release");
    let destroy_thread = markers.path().join("destroy-thread");
    let (cancel_watchdog, watchdog) = release_gate_watchdog(destroy_release.clone());
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let collision = runtime
        .root()
        .apply(crate::resolved(Arc::new(EchoCollisionFactory)), Value::Null)
        .await
        .unwrap();
    wait_active(&collision).await;

    let factory = catalog.load(native_fixture()).unwrap();
    let config = json!({
        "prefix": "native:",
        "destroy_entered_path": destroy_entered,
        "destroy_release_path": destroy_release,
        "destroy_thread_path": destroy_thread
    });
    let application = tokio::spawn({
        let root = runtime.root();
        async move { root.apply(factory, config).await }
    });
    wait_for_file(&destroy_entered).await;
    wait_for_file(&destroy_thread).await;
    assert_destroy_worker(&destroy_thread);
    assert!(
        !application.is_finished(),
        "publication failure did not join the gated foreign destructor"
    );
    std::fs::write(&destroy_release, b"release").unwrap();
    let native = tokio::time::timeout(Duration::from_secs(2), application)
        .await
        .expect("publication failure did not join released destruction")
        .unwrap()
        .unwrap();
    let _ = cancel_watchdog.send(());
    watchdog.join().unwrap();
    assert!(matches!(native.snapshot().state, FiberState::Failed(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn native_instance_cleanup_joins_destruction_beyond_the_callback_deadline() {
    let markers = tempfile::tempdir().unwrap();
    let destroy_entered = markers.path().join("destroy-entered");
    let destroy_release = markers.path().join("destroy-release");
    let destroy_thread = markers.path().join("destroy-thread");
    let (cancel_watchdog, watchdog) = release_gate_watchdog(destroy_release.clone());
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::default();
    let (native, _service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({
            "prefix": "native:",
            "destroy_entered_path": destroy_entered,
            "destroy_release_path": destroy_release,
            "destroy_thread_path": destroy_thread
        }),
    )
    .await;
    let disposal = tokio::spawn(async move { native.dispose().await });
    wait_for_file(&destroy_entered).await;
    wait_for_file(&destroy_thread).await;
    assert_destroy_worker(&destroy_thread);
    assert!(
        !disposal.is_finished(),
        "cleanup completed before the foreign destructor returned"
    );
    std::fs::write(&destroy_release, b"release").unwrap();
    let report = tokio::time::timeout(Duration::from_secs(1), disposal)
        .await
        .expect("cleanup did not join its foreign destructor")
        .unwrap();
    let _ = cancel_watchdog.send(());
    watchdog.join().unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_all_runtime_owners_uses_the_reserved_native_finalizer() {
    let markers = tempfile::tempdir().unwrap();
    let destroy_entered = markers.path().join("destroy-entered");
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
            json!({
                "prefix": "drop:",
                "destroy_entered_path": destroy_entered
            }),
        )
        .await
        .unwrap();
    wait_active(&native).await;

    drop(runtime);
    drop(native);
    drop(upstream);

    wait_for_catalog_marker(&destroy_entered, &catalog).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = catalog.snapshot();
            if snapshot.active_destructions == 0
                && snapshot.queued_destructions == 0
                && snapshot.staging_bytes == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "reserved native instance and factory finalizers did not drain: {:?}",
            catalog.snapshot()
        )
    });
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_native_finalization_enforces_the_live_instance_boundary() {
    let markers = tempfile::tempdir().unwrap();
    let destroy_entered = markers.path().join("destroy-entered");
    let destroy_release = markers.path().join("destroy-release");
    let rejected_create = markers.path().join("rejected-create");
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.limits.maximum_live_instances = 1;
    options.limits.maximum_concurrent_destructions = 1;
    let catalog = NativeCatalog::new(options).unwrap();
    let factory = catalog.load(native_fixture()).unwrap();

    let first_runtime = Runtime::default();
    let first_upstream = first_runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&first_upstream).await;
    let first = first_runtime
        .root()
        .apply(
            factory.clone(),
            json!({
                "prefix": "first:",
                "destroy_entered_path": destroy_entered,
                "destroy_release_path": destroy_release
            }),
        )
        .await
        .unwrap();
    wait_active(&first).await;
    drop(first_runtime);
    drop(first);
    drop(first_upstream);
    wait_for_catalog_marker(&destroy_entered, &catalog).await;

    let blocked = catalog.snapshot();
    assert_eq!(blocked.active_instances, 1);
    assert_eq!(blocked.peak_instances, 1);
    assert_eq!(blocked.pending_instance_destructions, 1);

    let second_runtime = Runtime::default();
    let second_upstream = second_runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&second_upstream).await;
    let rejected = second_runtime
        .root()
        .apply(
            factory.clone(),
            json!({
                "prefix": "rejected:",
                "create_entered_path": rejected_create
            }),
        )
        .await
        .unwrap();
    assert!(matches!(rejected.snapshot().state, FiberState::Failed(_)));
    assert!(
        !rejected_create.exists(),
        "live-instance Busy was reported after spawning create"
    );
    assert_eq!(catalog.snapshot().rejected_instances, 1);

    std::fs::write(&destroy_release, b"release").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = catalog.snapshot();
            if snapshot.active_instances == 0 && snapshot.pending_instance_destructions == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "released destructor retained live-instance admission: {:?}",
            catalog.snapshot()
        )
    });

    let retry = second_runtime
        .root()
        .apply(factory, json!({ "prefix": "retry:" }))
        .await
        .unwrap();
    wait_active(&retry).await;
    assert_eq!(catalog.snapshot().active_instances, 1);
    assert!(retry.dispose().await.is_clean());
    tokio::time::timeout(Duration::from_secs(2), async {
        while catalog.snapshot().active_instances != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "explicit cleanup did not release live-instance admission: {:?}",
            catalog.snapshot()
        )
    });
    assert_clean_shutdown(&second_runtime).await;
}

#[tokio::test]
async fn catalog_destruction_lane_bounds_concurrent_foreign_cleanup() {
    let markers = tempfile::tempdir().unwrap();
    let first_entered = markers.path().join("first-entered");
    let first_release = markers.path().join("first-release");
    let first_thread = markers.path().join("first-thread");
    let second_entered = markers.path().join("second-entered");
    let second_release = markers.path().join("second-release");
    let second_thread = markers.path().join("second-thread");
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(1);
    options.limits.maximum_concurrent_destructions = 1;
    let catalog = NativeCatalog::new(options).unwrap();
    let first_runtime = Runtime::default();
    let second_runtime = Runtime::default();
    let (first, first_service) = apply_delayed_native(
        &first_runtime,
        &catalog,
        json!({
            "prefix": "first:",
            "destroy_entered_path": first_entered,
            "destroy_release_path": first_release,
            "destroy_thread_path": first_thread
        }),
    )
    .await;
    let (second, second_service) = apply_delayed_native(
        &second_runtime,
        &catalog,
        json!({
            "prefix": "second:",
            "destroy_entered_path": second_entered,
            "destroy_release_path": second_release,
            "destroy_thread_path": second_thread
        }),
    )
    .await;

    let first_disposal = tokio::spawn(async move { first.dispose().await });
    wait_for_file(&first_entered).await;
    wait_for_file(&first_thread).await;
    assert_destroy_worker(&first_thread);
    let (second_started, second_entered_disposal) = tokio::sync::oneshot::channel();
    let second_disposal = tokio::spawn(async move {
        let _ = second_started.send(());
        second.dispose().await
    });
    second_entered_disposal
        .await
        .expect("second disposal task entered");
    tokio::task::yield_now().await;
    assert!(!second_disposal.is_finished());
    assert!(!second_entered.exists());
    std::fs::write(&first_release, b"release").unwrap();
    assert!(first_disposal.await.unwrap().is_clean());
    wait_for_file(&second_entered).await;
    wait_for_file(&second_thread).await;
    assert_destroy_worker(&second_thread);
    std::fs::write(&second_release, b"release").unwrap();
    assert!(second_disposal.await.unwrap().is_clean());

    drop(first_service);
    drop(second_service);
    assert_clean_shutdown(&first_runtime).await;
    assert_clean_shutdown(&second_runtime).await;

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = catalog.snapshot();
            if snapshot.active_destructions == 0
                && snapshot.pending_instance_destructions == 0
                && snapshot.queued_destructions == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed destruction retained its active admission");
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.peak_destructions, 1);
    assert_eq!(snapshot.active_destructions, 0);
    assert_eq!(snapshot.pending_instance_destructions, 0);
    assert_eq!(snapshot.queued_destructions, 0);
}

#[test]
fn native_create_and_call_do_not_occupy_tokios_shared_blocking_pool() {
    let asynchronous = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    asynchronous.block_on(async {
        let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
        let runtime = Runtime::default();
        let upstream = runtime
            .root()
            .apply(upstream_factory(), Value::Null)
            .await
            .unwrap();
        wait_active(&upstream).await;

        let markers = tempfile::tempdir().unwrap();
        let create_entered = markers.path().join("create-entered");
        let create_release = markers.path().join("create-release");
        let call_entered = markers.path().join("call-entered");
        let call_release = markers.path().join("call-release");
        let factory = catalog.load(native_fixture()).unwrap();
        let application = tokio::spawn({
            let root = runtime.root();
            let config = json!({
                "prefix": "native:",
                "create_entered_path": create_entered,
                "create_release_path": create_release,
                "call_entered_path": call_entered,
                "call_release_path": call_release
            });
            async move { root.apply(factory, config).await }
        });
        wait_for_file(&create_entered).await;
        let create_sentinel = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::task::spawn_blocking(|| 17_u8),
        )
        .await
        .is_ok();
        std::fs::write(&create_release, b"release").unwrap();
        let native = application.await.unwrap().unwrap();
        wait_active(&native).await;

        let slot = Arc::new(Mutex::new(None));
        let consumer = runtime
            .root()
            .apply(
                crate::resolved(Arc::new(CaptureFactory::new(
                    "blocking-pool-client",
                    Requirement::new("echo", "fixture.echo", V1),
                    Arc::clone(&slot),
                ))),
                Value::Null,
            )
            .await
            .unwrap();
        wait_active(&consumer).await;
        let service = slot.lock().unwrap().take().unwrap();
        let call =
            tokio::spawn(async move { service.invoke(Message::new(b"call".as_slice())).await });
        wait_for_file(&call_entered).await;
        let call_sentinel = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::task::spawn_blocking(|| 23_u8),
        )
        .await
        .is_ok();
        std::fs::write(&call_release, b"release").unwrap();
        assert_eq!(
            call.await.unwrap().unwrap().as_bytes(),
            b"native:upstream:call"
        );
        assert!(create_sentinel, "native create occupied the blocking pool");
        assert!(call_sentinel, "native call occupied the blocking pool");
        assert_clean_shutdown(&runtime).await;
    });
}
