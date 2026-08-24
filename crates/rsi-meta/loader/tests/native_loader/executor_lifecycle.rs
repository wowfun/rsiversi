use super::*;

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
        Err(LoaderError::Timeout("library load, entry, or descriptor"))
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
    assert!(matches!(
        rejected.snapshot().state,
        FiberState::Failed(ref message) if message.contains("busy")
    ));
    assert!(!second_entered.exists(), "busy create entered native code");
    assert_eq!(catalog.snapshot().active_callbacks, 1);

    std::fs::write(&first_release, b"release").unwrap();
    let first = first_application.await.unwrap().unwrap();
    wait_active(&first).await;
    assert!(rejected.dispose().await.is_clean());
    assert!(first.dispose().await.is_clean());
    assert!(upstream.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn instance_gate_is_acquired_before_native_callback_thread_admission() {
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(2);
    options.limits.maximum_concurrent_callbacks = 2;
    let catalog = NativeCatalog::new(options).unwrap();
    let runtime = Runtime::default();
    let (_native, service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({ "prefix": "serial:", "delay_ms": 100 }),
    )
    .await;

    let first_service = service.clone();
    let first = tokio::spawn(async move {
        first_service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"first".to_vec()))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while catalog.snapshot().active_callbacks != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first native callback did not acquire admission");

    let second = tokio::spawn(async move {
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"second".to_vec()))
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        catalog.snapshot().active_callbacks,
        1,
        "the serialized waiter spawned a native callback thread"
    );

    assert_eq!(
        first.await.unwrap().unwrap().as_bytes(),
        b"serial:upstream:first"
    );
    assert_eq!(
        second.await.unwrap().unwrap().as_bytes(),
        b"serial:upstream:second"
    );
    assert!(runtime.shutdown().await.is_clean());
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
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"slow".to_vec()))
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
    let (_fixture, artifact) =
        destruction_order_fixture(&call_entered, &call_release, &destroy_entered);
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(500));
    let runtime = Runtime::default();
    let native = runtime
        .root()
        .apply(catalog.load(artifact).unwrap(), Value::Null)
        .await
        .unwrap();
    wait_active(&native).await;
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "destruction-order-client",
                    "1",
                ))
                .requiring(Requirement::new("echo", "fixture.echo", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let service = slot.lock().unwrap().clone().unwrap();
    let call = tokio::spawn(async move {
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"blocked".to_vec()))
            .await
    });
    wait_for_file(&call_entered).await;
    let error = call.await.unwrap().unwrap_err();
    assert!(
        matches!(error, MetaError::Service(ref message) if message.contains("native service callback")),
        "native endpoint timeout escaped the service boundary: {error:?}"
    );

    let disposal = tokio::spawn(async move { native.dispose().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !disposal.is_finished(),
        "cleanup completed while its native callback was still running"
    );
    assert!(
        !destroy_entered.exists(),
        "destruction entered while the timed-out callback still held the instance"
    );

    std::fs::write(&call_release, b"release").unwrap();
    let report = tokio::time::timeout(Duration::from_secs(1), disposal)
        .await
        .expect("cleanup did not join released native destruction")
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    wait_for_file(&destroy_entered).await;
}

#[tokio::test]
async fn timed_out_factory_callback_poison_prevents_later_overlap() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(500));
    let factory = catalog.load(native_fixture()).unwrap();
    let runtime = Runtime::default();
    assert!(matches!(
        runtime
            .root()
            .apply(
                factory.clone(),
                json!({ "prefix": "slow:", "validate_delay_ms": 2_000 }),
            )
            .await,
        Err(MetaError::Timeout("native config validation"))
    ));
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
        .apply(
            Arc::new(EchoCollisionFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("echo-collision", "1"))
                    .providing(Provision::new("echo", "fixture.echo", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&collision).await;

    let factory = catalog.load(native_fixture()).unwrap();
    let application = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                factory,
                json!({
                    "prefix": "native:",
                    "create_delay_ms": 50,
                    "destroy_delay_ms": 200
                }),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .snapshot()
                .fibers
                .iter()
                .any(|fiber| matches!(fiber.state, FiberState::Loading))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native activation did not begin");
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        started.elapsed() < Duration::from_millis(180),
        "publication failure ran the foreign destructor on the current-thread executor"
    );
    let native = application.await.unwrap().unwrap();
    assert!(matches!(native.snapshot().state, FiberState::Failed(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn native_instance_cleanup_joins_destruction_beyond_the_callback_deadline() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::default();
    let (native, _service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({ "prefix": "native:", "destroy_delay_ms": 200 }),
    )
    .await;
    let started = std::time::Instant::now();
    let disposal = tokio::spawn(async move { native.dispose().await });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "foreign destructor blocked the current-thread executor"
    );
    assert!(
        !disposal.is_finished(),
        "cleanup completed before the foreign destructor returned"
    );
    let report = tokio::time::timeout(Duration::from_secs(1), disposal)
        .await
        .expect("cleanup did not join its foreign destructor")
        .unwrap();
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

    wait_for_file(&destroy_entered).await;
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
    .expect("reserved native instance and factory finalizers did not drain");
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
    wait_for_file(&destroy_entered).await;

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
    .expect("released destructor retained live-instance admission");

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
    .expect("explicit cleanup did not release live-instance admission");
    assert!(second_runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn catalog_destruction_lane_bounds_concurrent_foreign_cleanup() {
    let cache = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(cache.path());
    options.callback_timeout = Duration::from_secs(1);
    options.limits.maximum_concurrent_destructions = 1;
    let catalog = NativeCatalog::new(options).unwrap();
    let first_runtime = Runtime::default();
    let second_runtime = Runtime::default();
    let (first, _first_service) = apply_delayed_native(
        &first_runtime,
        &catalog,
        json!({ "prefix": "first:", "destroy_delay_ms": 100 }),
    )
    .await;
    let (second, _second_service) = apply_delayed_native(
        &second_runtime,
        &catalog,
        json!({ "prefix": "second:", "destroy_delay_ms": 100 }),
    )
    .await;

    let first_disposal = tokio::spawn(async move { first.dispose().await });
    let second_disposal = tokio::spawn(async move { second.dispose().await });
    assert!(first_disposal.await.unwrap().is_clean());
    assert!(second_disposal.await.unwrap().is_clean());

    tokio::time::timeout(Duration::from_secs(1), async {
        while catalog.snapshot().active_destructions != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed destruction retained its active admission");
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.peak_destructions, 1);
    assert_eq!(snapshot.active_destructions, 0);
    assert!(first_runtime.shutdown().await.is_clean());
    assert!(second_runtime.shutdown().await.is_clean());
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
                Arc::new(CaptureFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "blocking-pool-client",
                        "1",
                    ))
                    .requiring(Requirement::new("echo", "fixture.echo", V1)),
                    slot: Arc::clone(&slot),
                }),
                Value::Null,
            )
            .await
            .unwrap();
        wait_active(&consumer).await;
        let service = slot.lock().unwrap().clone().unwrap();
        let call = tokio::spawn(async move {
            service
                .open()
                .unwrap()
                .unary(ServiceFrame::new(b"call".to_vec()))
                .await
        });
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
        assert!(runtime.shutdown().await.is_clean());
    });
}
