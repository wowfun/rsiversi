use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_meta::{
    Context, DispatchMode, EventHandler, EventOptions, EventOutcome, FactoryIdentity, FiberState,
    InvocationContext, MetaError, PayloadLimits, PluginDescriptor, PluginFactory, Result, Runtime,
    RuntimeLimits, ServiceKey,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

mod support;

use support::NoopHandler;

#[derive(Debug)]
struct ContextCaptureFactory {
    descriptor: PluginDescriptor,
    captured: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        *self.captured.lock().expect("context capture poisoned") = Some(context);
        Ok(())
    }
}

async fn captured_context(runtime: &Runtime) -> Context {
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("ownr", "1")),
                captured: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captured its Context")
}

#[tokio::test]
async fn event_and_scoped_service_identifiers_are_checked_at_context_boundaries() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_identifier_bytes: 4,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let context = captured_context(&runtime).await;

    assert_eq!(
        context
            .on("event", Arc::new(NoopHandler), EventOptions::default(),)
            .unwrap_err(),
        MetaError::InvalidInput("event identifier exceeds the configured byte limit".to_owned(),)
    );
    assert_eq!(
        runtime
            .root()
            .dispatch("event", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::InvalidInput("event identifier exceeds the configured byte limit".to_owned(),)
    );
    assert_eq!(
        runtime
            .root()
            .dispatch_scoped("scope", "okay", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::InvalidInput(
            "context service identifier exceeds the configured byte limit".to_owned(),
        )
    );
    assert_eq!(
        runtime
            .root()
            .dispatch_scoped("okay", "event", DispatchMode::Emit, Value::Null)
            .await
            .unwrap_err(),
        MetaError::InvalidInput("event identifier exceeds the configured byte limit".to_owned(),)
    );

    let dispatches = runtime.resource_snapshot().event_dispatches;
    assert_eq!(dispatches.current, 0);
    assert_eq!(dispatches.high_watermark, 0);
    assert_eq!(dispatches.rejected, 0);
    assert_eq!(
        runtime
            .root()
            .dispatch("okay", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0,
    );
}

#[derive(Debug)]
struct LongDiagnosticHandler;

#[async_trait]
impl EventHandler for LongDiagnosticHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Err(MetaError::Service("界".repeat(16)))
    }
}

#[derive(Debug)]
struct PanickingDescriptorFactory;

#[async_trait]
impl PluginFactory for PanickingDescriptorFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        panic!("descriptor panic evidence")
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        unreachable!("descriptor validation must finish before activation")
    }
}

#[derive(Debug)]
struct StructuredLongDiagnosticHandler;

#[async_trait]
impl EventHandler for StructuredLongDiagnosticHandler {
    async fn handle(&self, _: InvocationContext, _: Arc<Value>) -> Result<EventOutcome> {
        Err(MetaError::DuplicateProvider {
            service: ServiceKey::from("界".repeat(16)),
        })
    }
}

#[derive(Debug)]
struct StructuredLongActivationFactory(PluginDescriptor);

#[async_trait]
impl PluginFactory for StructuredLongActivationFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
        Err(MetaError::DuplicateProvider {
            service: ServiceKey::from("界".repeat(16)),
        })
    }
}

#[tokio::test]
async fn retained_effect_labels_and_listener_errors_obey_the_diagnostic_byte_bound() {
    const MAXIMUM_DIAGNOSTIC_BYTES: usize = 16;
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_diagnostic_bytes: MAXIMUM_DIAGNOSTIC_BYTES,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let context = captured_context(&runtime).await;
    context
        .on(
            "diagnostic",
            Arc::new(LongDiagnosticHandler),
            EventOptions::default(),
        )
        .unwrap();

    let error = runtime
        .root()
        .dispatch("diagnostic", DispatchMode::Emit, Value::Null)
        .await
        .unwrap_err();
    let MetaError::Event(message) = error else {
        panic!("listener error was not normalized at the event boundary: {error:?}");
    };
    assert!(message.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());

    context
        .on(
            "structured",
            Arc::new(StructuredLongDiagnosticHandler),
            EventOptions::default(),
        )
        .unwrap();
    let error = runtime
        .root()
        .dispatch("structured", DispatchMode::Emit, Value::Null)
        .await
        .unwrap_err();
    let MetaError::Event(message) = error else {
        panic!("structured listener error was not normalized at the event boundary: {error:?}");
    };
    assert!(message.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());

    assert_eq!(
        context
            .defer("界".repeat(6), Box::new(|| async { Ok(()) }.boxed()),)
            .unwrap_err(),
        MetaError::InvalidInput(
            "effect label exceeds the configured diagnostic byte limit".to_owned(),
        )
    );
    let effects = runtime.resource_snapshot().effects;
    assert_eq!(effects.current, 0);
    assert_eq!(effects.high_watermark, 0);

    let error = runtime
        .root()
        .apply(Arc::new(PanickingDescriptorFactory), Value::Null)
        .await
        .unwrap_err();
    let MetaError::Activation(message) = error else {
        panic!("preparation task error changed category: {error:?}");
    };
    assert_eq!(message, "plug [truncated]");

    let failed = runtime
        .root()
        .apply(
            Arc::new(StructuredLongActivationFactory(PluginDescriptor::new(
                FactoryIdentity::builtin("fail", "1"),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    let FiberState::Failed(message) = failed.snapshot().state else {
        panic!("activation error did not leave bounded failure evidence");
    };
    assert!(message.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
}
