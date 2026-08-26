use super::*;

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
    spec: FactorySpec,
    handlers: Vec<(Arc<dyn EventHandler>, EventOptions)>,
}

#[async_trait]
impl PluginFactory for ListenerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        for (handler, options) in &self.handlers {
            plan.context().on("test", Arc::clone(handler), *options)?;
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
                spec: FactorySpec::new(FactoryIdentity::builtin("listeners", "1")),
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
                spec: FactorySpec::new(FactoryIdentity::builtin("failing", "1")),
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
