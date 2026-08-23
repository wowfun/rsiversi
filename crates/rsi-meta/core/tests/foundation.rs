use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_meta::{
    Context, ContractVersion, DispatchMode, EventHandler, EventOptions, EventOutcome,
    FactoryIdentity, FiberState, InvocationContext, IsolationId, MetaError, PluginDescriptor,
    PluginFactory, ProviderChannel, Provision, Requirement, Result, Runtime, RuntimeLimits,
    ServiceEndpoint, ServiceFrame,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct EchoEndpoint {
    overlays: Arc<Mutex<Vec<Vec<Value>>>>,
}

#[async_trait]
impl ServiceEndpoint for EchoEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        self.overlays
            .lock()
            .expect("overlay log poisoned")
            .push(invocation.edge_overlay().to_vec());
        while let Some(frame) = channel.recv().await {
            channel.send(frame).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ProviderFactory {
    descriptor: PluginDescriptor,
    overlays: Arc<Mutex<Vec<Vec<Value>>>>,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

impl ProviderFactory {
    fn new(cleanup: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("provider", "1"))
                .providing(Provision::new("echo", "test.echo", V1)),
            overlays: Arc::new(Mutex::new(Vec::new())),
            cleanup,
        }
    }
}

#[async_trait]
impl PluginFactory for ProviderFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.provide(
            "echo",
            "test.echo",
            V1,
            Arc::new(EchoEndpoint {
                overlays: Arc::clone(&self.overlays),
            }),
        )?;
        let cleanup = Arc::clone(&self.cleanup);
        context.defer(
            "provider",
            Box::new(move || {
                async move {
                    cleanup
                        .lock()
                        .expect("cleanup log poisoned")
                        .push("provider");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct ConsumerFactory {
    descriptor: PluginDescriptor,
    observed: Arc<Mutex<Vec<Vec<u8>>>>,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

impl ConsumerFactory {
    fn new(cleanup: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("consumer", "1"))
                .requiring(Requirement::new("echo", "test.echo", V1)),
            observed: Arc::new(Mutex::new(Vec::new())),
            cleanup,
        }
    }
}

#[async_trait]
impl PluginFactory for ConsumerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        let service = context.service("echo")?;
        let response = service
            .open()?
            .unary(ServiceFrame::new(b"active".to_vec()))
            .await?;
        self.observed
            .lock()
            .expect("observation log poisoned")
            .push(response.into_bytes());
        let cleanup = Arc::clone(&self.cleanup);
        context.defer(
            "consumer",
            Box::new(move || {
                async move {
                    let response = service
                        .open()
                        .map_err(|error| error.to_string())?
                        .unary(ServiceFrame::new(b"cleanup".to_vec()))
                        .await
                        .map_err(|error| error.to_string())?;
                    if response.as_bytes() != b"cleanup" {
                        return Err("cleanup call returned wrong bytes".to_owned());
                    }
                    cleanup
                        .lock()
                        .expect("cleanup log poisoned")
                        .push("consumer");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

async fn wait_active(handle: &rsi_meta::FiberHandle) {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handle.wait_active(&CancellationToken::new()),
    )
    .await
    .expect("fiber activation timed out")
    .expect("fiber should activate");
}

#[tokio::test]
async fn missing_dependency_converges_and_provider_retires_after_consumer_cleanup() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let consumer_factory = Arc::new(ConsumerFactory::new(Arc::clone(&cleanup)));
    let consumer = runtime
        .root()
        .intercept("echo", json!({ "source": "direct-edge" }))
        .unwrap()
        .apply(consumer_factory.clone(), Value::Null)
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));

    let provider_factory = Arc::new(ProviderFactory::new(Arc::clone(&cleanup)));
    let provider = runtime
        .root()
        .apply(provider_factory.clone(), Value::Null)
        .await
        .unwrap();
    wait_active(&provider).await;
    wait_active(&consumer).await;
    assert_eq!(
        consumer_factory
            .observed
            .lock()
            .expect("observation log poisoned")
            .as_slice(),
        &[b"active".to_vec()]
    );
    assert_eq!(
        provider_factory
            .overlays
            .lock()
            .expect("overlay log poisoned")[0],
        vec![json!({ "source": "direct-edge" })]
    );

    let report = provider.dispose().await;
    assert!(report.is_clean(), "{report:?}");
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    assert_eq!(
        cleanup.lock().expect("cleanup log poisoned").as_slice(),
        &["consumer", "provider"]
    );
}

#[tokio::test]
async fn captured_service_handle_is_fenced_after_its_provider_retires() {
    #[derive(Debug)]
    struct CaptureFactory {
        descriptor: PluginDescriptor,
        handle: Arc<Mutex<Option<rsi_meta::ServiceHandle>>>,
    }

    #[async_trait]
    impl PluginFactory for CaptureFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        async fn activate(&self, context: Context, _: Value) -> Result<()> {
            *self.handle.lock().expect("handle poisoned") = Some(context.service("echo")?);
            Ok(())
        }
    }

    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(Arc::new(ProviderFactory::new(cleanup)), Value::Null)
        .await
        .unwrap();
    wait_active(&provider).await;
    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("capture", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                handle: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let old = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("service captured");
    let consumer_generation = consumer.snapshot().generation;
    provider.dispose().await;
    assert_eq!(
        old.open().unwrap_err(),
        MetaError::StaleContext {
            fiber: consumer.id(),
            generation: consumer_generation,
        }
    );
}

#[tokio::test]
async fn captured_service_handle_is_generation_fenced_after_consumer_reloads() {
    #[derive(Debug)]
    struct CaptureFactory {
        descriptor: PluginDescriptor,
        handle: Arc<Mutex<Option<rsi_meta::ServiceHandle>>>,
    }

    #[async_trait]
    impl PluginFactory for CaptureFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        async fn activate(&self, context: Context, _: Value) -> Result<()> {
            *self.handle.lock().expect("handle poisoned") = Some(context.service("echo")?);
            Ok(())
        }
    }

    let runtime = Runtime::default();
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory::new(Arc::new(Mutex::new(Vec::new())))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&provider).await;
    let captured = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("capture-reload", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                handle: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let old = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("service captured");
    let old_generation = consumer.snapshot().generation;

    let reloaded = consumer.reconfigure(Value::Null).await.unwrap();
    assert_ne!(reloaded.generation, old_generation);
    assert_eq!(
        old.open().unwrap_err(),
        MetaError::StaleContext {
            fiber: consumer.id(),
            generation: old_generation,
        }
    );
    let current = captured
        .lock()
        .expect("handle poisoned")
        .clone()
        .expect("replacement service captured");
    assert_eq!(
        current
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"current".to_vec()))
            .await
            .unwrap()
            .as_bytes(),
        b"current"
    );
}

#[derive(Debug)]
struct EffectFactory {
    descriptor: PluginDescriptor,
    log: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

#[async_trait]
impl PluginFactory for EffectFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, config: Value) -> Result<()> {
        let generation = context.owner().expect("plugin has owner").1.0;
        self.log
            .lock()
            .expect("effect log poisoned")
            .push(format!("activate:{generation}:{config}"));
        for label in ["a", "b"] {
            let log = Arc::clone(&self.log);
            context.defer(
                label,
                Box::new(move || {
                    async move {
                        log.lock()
                            .expect("effect log poisoned")
                            .push(format!("cleanup:{label}"));
                        Ok(())
                    }
                    .boxed()
                }),
            )?;
        }
        if self.fail {
            return Err(MetaError::Activation("requested failure".to_owned()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn failed_setup_and_reconfigure_cleanup_in_strict_reverse_order() {
    let runtime = Runtime::default();
    let failed_log = Arc::new(Mutex::new(Vec::new()));
    let failed = runtime
        .root()
        .apply(
            Arc::new(EffectFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("failed", "1")),
                log: Arc::clone(&failed_log),
                fail: true,
            }),
            json!(1),
        )
        .await
        .unwrap();
    assert!(matches!(failed.snapshot().state, FiberState::Failed(_)));
    assert_eq!(
        failed_log.lock().expect("effect log poisoned").as_slice(),
        &["activate:1:1", "cleanup:b", "cleanup:a"]
    );

    let log = Arc::new(Mutex::new(Vec::new()));
    let active = runtime
        .root()
        .apply(
            Arc::new(EffectFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("effect", "1")),
                log: Arc::clone(&log),
                fail: false,
            }),
            json!(1),
        )
        .await
        .unwrap();
    let first_generation = active.snapshot().generation;
    active.reconfigure(json!(2)).await.unwrap();
    assert!(active.snapshot().generation > first_generation);
    assert_eq!(
        log.lock().expect("effect log poisoned").as_slice(),
        &["activate:2:1", "cleanup:b", "cleanup:a", "activate:3:2"]
    );
    active.dispose().await;
    assert_eq!(
        &log.lock().expect("effect log poisoned")[3..],
        &["activate:3:2", "cleanup:b", "cleanup:a"]
    );
}

#[tokio::test]
async fn service_isolation_allows_private_provider_slots() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let first = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&first).await;
    let duplicate = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(duplicate.snapshot().state, FiberState::Failed(_)));

    let (private, _) = runtime.root().isolate_fresh("echo");
    let isolated = private
        .apply(Arc::new(ProviderFactory::new(cleanup)), Value::Null)
        .await
        .unwrap();
    wait_active(&isolated).await;
}

#[tokio::test]
async fn cloned_context_isolation_branches_remain_independent() {
    let runtime = Runtime::default();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let base = runtime.root();
    let left = base.clone().isolate("echo", IsolationId(41));
    let right = base.isolate("echo", IsolationId(42));

    let left_provider = left
        .apply(
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    let right_provider = right
        .apply(Arc::new(ProviderFactory::new(cleanup)), Value::Null)
        .await
        .unwrap();

    wait_active(&left_provider).await;
    wait_active(&right_provider).await;
}

#[derive(Debug)]
struct RecordingHandler {
    name: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
    outcome: EventOutcome,
    fail: bool,
}

#[async_trait]
impl EventHandler for RecordingHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        self.log.lock().expect("event log poisoned").push(self.name);
        if self.fail {
            Err(MetaError::Event(self.name.to_owned()))
        } else {
            Ok(self.outcome.clone())
        }
    }
}

#[derive(Debug)]
struct ListenerFactory {
    descriptor: PluginDescriptor,
    handlers: Vec<(Arc<dyn EventHandler>, EventOptions)>,
}

#[async_trait]
impl PluginFactory for ListenerFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        for (handler, options) in &self.handlers {
            context.on("test", Arc::clone(handler), *options)?;
        }
        Ok(())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One scenario proves the interactions among all dispatch modes.
async fn events_snapshot_order_once_waterfall_and_aggregate_errors() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let handler = |name, outcome, fail| {
        Arc::new(RecordingHandler {
            name,
            log: Arc::clone(&log),
            outcome,
            fail,
        }) as Arc<dyn EventHandler>
    };
    let listeners = runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("listeners", "1")),
                handlers: vec![
                    (
                        handler("prepended", EventOutcome::Continue(json!(0)), false),
                        EventOptions {
                            prepend: true,
                            ..EventOptions::default()
                        },
                    ),
                    (
                        handler("first", EventOutcome::Continue(json!(2)), false),
                        EventOptions::default(),
                    ),
                    (
                        handler("once", EventOutcome::Complete(json!(3)), false),
                        EventOptions {
                            once: true,
                            ..EventOptions::default()
                        },
                    ),
                    (
                        handler("last", EventOutcome::Continue(json!(4)), false),
                        EventOptions {
                            once: true,
                            ..EventOptions::default()
                        },
                    ),
                ],
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&listeners).await;

    let first = runtime
        .root()
        .dispatch("test", DispatchMode::Serial, json!(1))
        .await
        .unwrap();
    assert_eq!(first.completed, Some(json!(3)));
    assert_eq!(
        log.lock().expect("event log poisoned").as_slice(),
        &["prepended", "first", "once"]
    );
    log.lock().expect("event log poisoned").clear();
    let second = runtime
        .root()
        .dispatch("test", DispatchMode::Waterfall, json!(1))
        .await
        .unwrap();
    assert_eq!(second.completed, Some(json!(4)));
    assert_eq!(
        log.lock().expect("event log poisoned").as_slice(),
        &["prepended", "first", "last"]
    );
    log.lock().expect("event log poisoned").clear();
    let third = runtime
        .root()
        .dispatch("test", DispatchMode::Serial, json!(1))
        .await
        .unwrap();
    assert_eq!(third.invoked, 2);
    assert_eq!(
        log.lock().expect("event log poisoned").as_slice(),
        &["prepended", "first"]
    );

    let failing = runtime
        .root()
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("failing", "1")),
                handlers: vec![
                    (
                        handler("bad-a", EventOutcome::Continue(Value::Null), true),
                        EventOptions::default(),
                    ),
                    (
                        handler("bad-b", EventOutcome::Continue(Value::Null), true),
                        EventOptions::default(),
                    ),
                ],
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&failing).await;
    let error = runtime
        .root()
        .dispatch("test", DispatchMode::Parallel, Value::Null)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("bad-a"));
    assert!(error.contains("bad-b"));
}

#[tokio::test]
async fn scoped_dispatch_keeps_isolated_listeners_private_and_includes_global_listeners() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let handler = |name| {
        Arc::new(RecordingHandler {
            name,
            log: Arc::clone(&log),
            outcome: EventOutcome::Continue(Value::Null),
            fail: false,
        }) as Arc<dyn EventHandler>
    };
    let (first_scope, _) = runtime.root().isolate_fresh("scoped-service");
    let (second_scope, _) = runtime.root().isolate_fresh("scoped-service");
    let listeners = first_scope
        .apply(
            Arc::new(ListenerFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "scoped-listeners",
                    "1",
                )),
                handlers: vec![
                    (handler("isolated"), EventOptions::default()),
                    (
                        handler("global"),
                        EventOptions {
                            global: true,
                            ..EventOptions::default()
                        },
                    ),
                ],
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&listeners).await;

    let foreign = second_scope
        .dispatch_scoped("scoped-service", "test", DispatchMode::Emit, Value::Null)
        .await
        .unwrap();
    assert_eq!(foreign.invoked, 1);
    assert_eq!(
        log.lock().expect("event log poisoned").as_slice(),
        &["global"]
    );

    log.lock().expect("event log poisoned").clear();
    let local = first_scope
        .dispatch_scoped("scoped-service", "test", DispatchMode::Emit, Value::Null)
        .await
        .unwrap();
    assert_eq!(local.invoked, 2);
    assert_eq!(
        log.lock().expect("event log poisoned").as_slice(),
        &["isolated", "global"]
    );
}

#[derive(Debug)]
struct ParentFactory {
    descriptor: PluginDescriptor,
    child: Arc<dyn PluginFactory>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for ParentFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.apply(Arc::clone(&self.child), Value::Null).await?;
        let log = Arc::clone(&self.log);
        context.defer(
            "parent",
            Box::new(move || {
                async move {
                    log.lock().expect("child log poisoned").push("parent");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct ChildFactory {
    descriptor: PluginDescriptor,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Debug)]
struct NamedChildFactory {
    descriptor: PluginDescriptor,
    label: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for NamedChildFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        let label = self.label;
        let log = Arc::clone(&self.log);
        context.defer(
            label,
            Box::new(move || {
                async move {
                    log.lock().expect("child log poisoned").push(label);
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[derive(Debug)]
struct MultiChildParentFactory {
    descriptor: PluginDescriptor,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl PluginFactory for MultiChildParentFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        for label in ["first-child", "second-child"] {
            context
                .apply(
                    Arc::new(NamedChildFactory {
                        descriptor: PluginDescriptor::new(FactoryIdentity::builtin(label, "1")),
                        label,
                        log: Arc::clone(&self.log),
                    }),
                    Value::Null,
                )
                .await?;
        }
        let log = Arc::clone(&self.log);
        context.defer(
            "parent",
            Box::new(move || {
                async move {
                    log.lock().expect("parent log poisoned").push("parent");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[async_trait]
impl PluginFactory for ChildFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        let log = Arc::clone(&self.log);
        context.defer(
            "child",
            Box::new(move || {
                async move {
                    log.lock().expect("child log poisoned").push("child");
                    Ok(())
                }
                .boxed()
            }),
        )
    }
}

#[tokio::test]
async fn parent_disposes_children_before_its_own_effects_and_dispose_is_joinable() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(
            Arc::new(ParentFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("parent", "1")),
                child: Arc::new(ChildFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin("child", "1")),
                    log: Arc::clone(&log),
                }),
                log: Arc::clone(&log),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&parent).await;
    let (left, right) = tokio::join!(parent.dispose(), parent.dispose());
    assert!(left.is_clean());
    assert!(right.is_clean());
    assert_eq!(
        log.lock().expect("child log poisoned").as_slice(),
        &["child", "parent"]
    );
}

#[tokio::test]
async fn parent_disposes_multiple_children_in_reverse_application_order() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(
            Arc::new(MultiChildParentFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "multi-child-parent",
                    "1",
                )),
                log: Arc::clone(&log),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&parent).await;

    assert!(parent.dispose().await.is_clean());
    assert_eq!(
        log.lock().expect("cleanup log poisoned").as_slice(),
        &["second-child", "first-child", "parent"]
    );
}

#[tokio::test]
async fn parent_reconfiguration_retires_the_old_child_before_publishing_a_new_generation() {
    let runtime = Runtime::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(
            Arc::new(ParentFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "reconfigured-parent",
                    "1",
                )),
                child: Arc::new(ChildFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "reconfigured-child",
                        "1",
                    )),
                    log: Arc::clone(&log),
                }),
                log: Arc::clone(&log),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&parent).await;
    let first_generation = parent.snapshot().generation;

    parent.reconfigure(Value::Null).await.unwrap();

    assert!(parent.snapshot().generation > first_generation);
    assert_eq!(
        log.lock().expect("cleanup log poisoned").as_slice(),
        &["child", "parent"]
    );
    assert_eq!(runtime.snapshot().fibers.len(), 2);
    assert!(parent.dispose().await.is_clean());
    assert_eq!(
        log.lock().expect("cleanup log poisoned").as_slice(),
        &["child", "parent", "child", "parent"]
    );
}

#[tokio::test]
async fn bounded_frames_are_rejected_at_the_calling_seam() {
    let runtime = Runtime::new(RuntimeLimits {
        maximum_frame_bytes: 4,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory::new(Arc::clone(&cleanup))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&provider).await;
    let capture = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureOnlyFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("capture-only", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                handle: Arc::clone(&capture),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let handle = capture.lock().expect("capture poisoned").clone().unwrap();
    let call = handle.open().unwrap();
    assert_eq!(
        call.send(ServiceFrame::new(vec![0; 5])).await.unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 4 }
    );
}

#[tokio::test]
async fn pending_dependency_cycles_are_reported_without_running_factories() {
    #[derive(Debug)]
    struct CycleFactory(PluginDescriptor);

    #[async_trait]
    impl PluginFactory for CycleFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.0
        }

        async fn activate(&self, _: Context, _: Value) -> Result<()> {
            panic!("a cyclic factory must remain pending")
        }
    }

    let runtime = Runtime::default();
    let left = runtime
        .root()
        .apply(
            Arc::new(CycleFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("left", "1"))
                    .requiring(Requirement::new("right", "test.right", V1))
                    .providing(Provision::new("left", "test.left", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let right = runtime
        .root()
        .apply(
            Arc::new(CycleFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("right", "1"))
                    .requiring(Requirement::new("left", "test.left", V1))
                    .providing(Provision::new("right", "test.right", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let reports_cycle = [left.snapshot(), right.snapshot()].iter().all(|snapshot| {
                matches!(
                    &snapshot.state,
                    FiberState::Pending(reasons)
                        if reasons.iter().any(|reason| matches!(
                            reason,
                            rsi_meta::PendingReason::DependencyCycle { .. }
                        ))
                )
            });
            if reports_cycle {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both cycle participants should report the cycle");
}

#[derive(Debug)]
struct CaptureOnlyFactory {
    descriptor: PluginDescriptor,
    handle: Arc<Mutex<Option<rsi_meta::ServiceHandle>>>,
}

#[async_trait]
impl PluginFactory for CaptureOnlyFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        *self.handle.lock().expect("capture poisoned") = Some(context.service("echo")?);
        Ok(())
    }
}
