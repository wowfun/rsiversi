use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, Emit, EmitEventHandler, FactoryIdentity, FiberState,
    LocalEvent, LocalEventOptions, MetaError, PayloadLimits, PluginFactory, PreparedActivation,
    Result, Runtime, RuntimeLimits, ServiceKey,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[path = "support/resolver.rs"]
mod resolver;
mod support;
use resolver::resolved;

use support::FactorySpec;

#[derive(Debug)]
struct ContextCaptureFactory {
    spec: FactorySpec,
    captured: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl PluginFactory for ContextCaptureFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.captured.lock().expect("context capture poisoned") = Some(plan.context().clone());
        Ok(())
    }
}

async fn captured_context(runtime: &Runtime) -> Context {
    let captured = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ContextCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::linked("ownr", "1")),
                captured: Arc::clone(&captured),
            })),
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

struct LongEvent;

impl LocalEvent for LongEvent {
    const KEY: &'static str = "event";
    type Value = ();
    type Error = std::convert::Infallible;
    type Mode = Emit;
}

#[derive(Debug)]
struct Noop;

impl EmitEventHandler<LongEvent> for Noop {
    fn handle(&self, (): &()) {}
}

#[tokio::test]
async fn local_event_identifiers_are_checked_at_context_boundaries() {
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
            .on_emit::<LongEvent, _>(Arc::new(Noop), LocalEventOptions::default())
            .unwrap_err(),
        MetaError::InvalidInput(
            "Local event identifier exceeds the configured byte limit".to_owned(),
        )
    );
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);
}

#[derive(Debug)]
struct PanickingPreparationFactory;

#[async_trait]
impl PluginFactory for PanickingPreparationFactory {
    fn prepare(&self, _: &ConfigValue) -> Result<PreparedActivation> {
        panic!("plugin preparation panicked")
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        unreachable!("panicking preparation cannot reach activation")
    }
}

#[derive(Debug)]
struct StructuredLongActivationFactory(FactorySpec);

#[async_trait]
impl PluginFactory for StructuredLongActivationFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.0.prepare(desired)
    }

    async fn activate(&self, _: ActivationPlan) -> Result<()> {
        Err(MetaError::DuplicateProvider {
            service: ServiceKey::from("界".repeat(16)),
        })
    }
}

#[tokio::test]
async fn retained_effect_and_activation_diagnostics_obey_the_byte_bound() {
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

    let effects_before_rejection = runtime.resource_snapshot().effects;
    assert_eq!(
        context
            .defer("界".repeat(6), Box::new(|| async { Ok(()) }.boxed()))
            .unwrap_err(),
        MetaError::InvalidInput(
            "effect label exceeds the configured diagnostic byte limit".to_owned(),
        )
    );
    let effects = runtime.resource_snapshot().effects;
    assert_eq!(effects.current, effects_before_rejection.current);
    assert_eq!(
        effects.high_watermark,
        effects_before_rejection.high_watermark
    );

    let error = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(PanickingPreparationFactory)),
            Value::Null,
        )
        .await
        .unwrap_err();
    let MetaError::InvalidConfig(message) = error else {
        panic!("preparation task error changed category: {error:?}");
    };
    assert_eq!(message, "plug [truncated]");

    let failed = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(StructuredLongActivationFactory(FactorySpec::new(
                FactoryIdentity::linked("fail", "1"),
            )))),
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
