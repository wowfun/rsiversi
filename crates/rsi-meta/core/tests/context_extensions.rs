use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContextExtension, DispatchMode, EventHandler, EventOptions,
    EventOutcome, FactoryIdentity, InvocationContext, IsolationId, MetaError, PayloadLimits,
    PluginFactory, PreparedActivation, Result, Runtime, RuntimeLimits, TopologyLimits,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

mod support;

use support::FactorySpec;

struct ScopeName;

impl ContextExtension for ScopeName {
    type Value = String;
}

struct Missing;

impl ContextExtension for Missing {
    type Value = u64;
}

struct SameValueType;

impl ContextExtension for SameValueType {
    type Value = String;
}

struct OtherValue;

impl ContextExtension for OtherValue {
    type Value = u64;
}

#[derive(Debug)]
struct ExtensionCaptureFactory {
    spec: FactorySpec,
    seen: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl PluginFactory for ExtensionCaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.seen.lock().expect("extension capture poisoned") =
            plan.context().extension::<ScopeName>().as_deref().cloned();
        Ok(())
    }
}

#[derive(Debug)]
struct InvocationCaptureHandler {
    seen: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl EventHandler for InvocationCaptureHandler {
    async fn handle(
        &self,
        invocation: InvocationContext,
        value: Arc<Value>,
    ) -> Result<EventOutcome> {
        let provider = invocation
            .provider_context()
            .extension::<ScopeName>()
            .expect("listener provider Context retains its extension");
        *self.seen.lock().expect("invocation capture poisoned") = Some(provider.as_ref().clone());
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}

#[derive(Debug)]
struct ExtensionListenerFactory {
    spec: FactorySpec,
    handler: Arc<dyn EventHandler>,
}

#[async_trait]
impl PluginFactory for ExtensionListenerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.spec.identity()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        self.spec.prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .clone()
            .with_extension::<ScopeName>("provider".to_owned())?
            .on(
                "extension",
                Arc::clone(&self.handler),
                EventOptions::default(),
            )?;
        Ok(())
    }
}

#[test]
fn typed_extensions_derive_copy_on_write_context_branches() {
    let runtime = Runtime::default();
    let root = runtime.root();

    let first = root
        .clone()
        .with_extension::<ScopeName>("first".to_owned())
        .unwrap();
    let second = first
        .clone()
        .with_extension::<ScopeName>("second".to_owned())
        .unwrap();
    let independently_keyed = first
        .clone()
        .with_extension::<SameValueType>("other marker".to_owned())
        .unwrap();
    let derived = first
        .clone()
        .isolate("service", IsolationId(9))
        .unwrap()
        .intercept("service", Value::Null)
        .unwrap();

    assert!(root.extension::<ScopeName>().is_none());
    assert_eq!(
        first
            .extension::<ScopeName>()
            .as_deref()
            .map(String::as_str),
        Some("first")
    );
    assert_eq!(
        second
            .extension::<ScopeName>()
            .as_deref()
            .map(String::as_str),
        Some("second")
    );
    assert_eq!(
        independently_keyed
            .extension::<ScopeName>()
            .as_deref()
            .map(String::as_str),
        Some("first")
    );
    assert_eq!(
        independently_keyed
            .extension::<SameValueType>()
            .as_deref()
            .map(String::as_str),
        Some("other marker")
    );
    assert_eq!(
        derived
            .extension::<ScopeName>()
            .as_deref()
            .map(String::as_str),
        Some("first")
    );
}

#[test]
fn extension_reads_and_shadowing_obey_the_shared_context_entry_bound() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_context_entries: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let root = runtime.root();
    let resources = runtime.resource_snapshot();

    assert!(root.extension::<Missing>().is_none());
    assert_eq!(runtime.resource_snapshot(), resources);

    let first = root
        .with_extension::<ScopeName>("first".to_owned())
        .unwrap();
    let shadowed = first
        .clone()
        .with_extension::<ScopeName>("shadowed".to_owned())
        .unwrap();
    assert_eq!(
        shadowed
            .extension::<ScopeName>()
            .as_deref()
            .map(String::as_str),
        Some("shadowed")
    );
    assert!(matches!(
        first.clone().with_extension::<OtherValue>(1),
        Err(MetaError::CapacityExhausted {
            resource: "context entries"
        })
    ));
    assert!(matches!(
        first.isolate("service", IsolationId(1)),
        Err(MetaError::CapacityExhausted {
            resource: "context entries"
        })
    ));
}

#[test]
fn opaque_extension_values_are_independent_of_the_context_byte_budget() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: PayloadLimits {
            maximum_context_bytes: 1,
            ..PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();

    let context = runtime
        .root()
        .with_extension::<ScopeName>("x".repeat(1024))
        .unwrap();
    assert_eq!(context.extension::<ScopeName>().unwrap().len(), 1024);
}

#[tokio::test]
async fn child_activation_inherits_extensions_from_its_parent_context() {
    let runtime = Runtime::default();
    let seen = Arc::new(Mutex::new(None));
    let parent = runtime
        .root()
        .with_extension::<ScopeName>("inherited".to_owned())
        .unwrap();

    let child = parent
        .apply(
            Arc::new(ExtensionCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("extension-child", "1")),
                seen: Arc::clone(&seen),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    child.wait_active(&CancellationToken::new()).await.unwrap();

    assert_eq!(
        seen.lock().expect("extension capture poisoned").as_deref(),
        Some("inherited")
    );
}

#[tokio::test]
async fn listener_invocation_preserves_its_provider_extension() {
    let runtime = Runtime::default();
    let seen = Arc::new(Mutex::new(None));
    let listeners = runtime
        .root()
        .apply(
            Arc::new(ExtensionListenerFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("extension-listener", "1")),
                handler: Arc::new(InvocationCaptureHandler {
                    seen: Arc::clone(&seen),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    listeners
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();

    let receipt = runtime
        .root()
        .dispatch("extension", DispatchMode::Emit, Value::Null)
        .await
        .unwrap();

    assert_eq!(receipt.invoked, 1);
    assert_eq!(
        seen.lock().expect("invocation capture poisoned").as_deref(),
        Some("provider")
    );
}

#[test]
fn context_debug_does_not_reveal_extension_values() {
    let context = Runtime::default()
        .root()
        .with_extension::<ScopeName>("private extension value".to_owned())
        .unwrap();

    assert!(!format!("{context:?}").contains("private extension value"));
}

#[tokio::test]
async fn shutdown_preserves_extension_reads_but_rejects_new_derivations() {
    let runtime = Runtime::default();
    let context = runtime
        .root()
        .with_extension::<ScopeName>("retained".to_owned())
        .unwrap();
    assert!(runtime.shutdown().await.is_complete());

    assert_eq!(
        context
            .extension::<ScopeName>()
            .as_deref()
            .map(String::as_str),
        Some("retained")
    );
    assert!(matches!(
        context.with_extension::<ScopeName>("rejected".to_owned()),
        Err(MetaError::RuntimeShuttingDown)
    ));
}
