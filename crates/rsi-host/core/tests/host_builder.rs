use async_trait::async_trait;
use rsi_host::{
    HostBuilder, HostError, HostLimits, HostPaths, ProfileControlContract, ProfileEntry,
    ProfileFragment,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Emit, LocalContract, LocalEvent, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;

enum LocalA {}

impl LocalContract for LocalA {
    const KEY: &'static str = "test.local.a";
    type Service = usize;
}

enum LocalB {}

impl LocalContract for LocalB {
    const KEY: &'static str = "test.local.b";
    type Service = usize;
}

enum EventA {}

impl LocalEvent for EventA {
    const KEY: &'static str = "test.event.a";
    type Value = ();
    type Error = Infallible;
    type Mode = Emit;
}

enum EventWithDuplicateKey {}

impl LocalEvent for EventWithDuplicateKey {
    const KEY: &'static str = EventA::KEY;
    type Value = ();
    type Error = Infallible;
    type Mode = Emit;
}

#[derive(Debug)]
struct Noop;

#[async_trait]
impl PluginFactory for Noop {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, _plan: ActivationPlan) -> rsi_meta::Result<()> {
        Ok(())
    }
}

fn paths() -> HostPaths {
    HostPaths::new("/config", "/state", "/cache").unwrap()
}

#[test]
fn catalog_markers_fragments_defines_and_profile_identity_freeze_before_build() {
    let mut builder = HostBuilder::new(paths());
    builder.platform("test-platform").unwrap();
    builder.define("answer", Value::from(42)).unwrap();
    assert!(matches!(
        builder.define("answer", Value::Null),
        Err(HostError::DuplicateDefine { .. })
    ));
    assert!(format!("{builder:?}").contains("answer"));
    builder
        .register_linked("test.noop", "1", UpdateMode::Replayable, Arc::new(Noop))
        .unwrap();
    assert!(matches!(
        builder.register_linked("test.noop", "2", UpdateMode::Replayable, Arc::new(Noop)),
        Err(HostError::DuplicatePlugin { .. })
    ));
    assert!(matches!(
        builder.register_linked(
            "rsi.meta.profile",
            "1",
            UpdateMode::RestartRequired,
            Arc::new(Noop)
        ),
        Err(HostError::DuplicatePlugin { .. })
    ));
    builder.register_local_contract::<LocalA>().unwrap();
    assert!(builder.register_local_contract::<LocalA>().is_err());
    builder.register_local_event::<EventA>().unwrap();
    assert!(builder.register_local_event::<EventA>().is_err());
    assert!(matches!(
        builder.register_local_event::<EventWithDuplicateKey>(),
        Err(HostError::DuplicateLocalEventKey { key }) if key.as_str() == EventA::KEY
    ));
    builder
        .register_fragment(ProfileFragment::new(
            "base",
            [ProfileEntry::new("base", "test.noop", Value::Null)],
        ))
        .unwrap();
    assert!(
        builder
            .register_fragment(ProfileFragment::new("base", []))
            .is_err()
    );

    let host = builder.build().unwrap();
    assert!(host.has_local_contract::<LocalA>());
    assert!(!host.has_local_contract::<LocalB>());
    assert!(host.has_local_contract::<ProfileControlContract>());
    assert!(host.has_local_event::<EventA>());
}

#[test]
fn path_and_collection_bounds_fail_before_runtime_construction() {
    assert!(matches!(
        HostPaths::new("relative", "/state", "/cache"),
        Err(HostError::PathNotAbsolute { kind: "config", .. })
    ));
    let limits = HostLimits {
        maximum_linked_plugins: 0,
        ..HostLimits::default()
    };
    assert!(matches!(
        HostBuilder::new(paths()).limits(limits).build(),
        Err(HostError::CapacityExceeded {
            resource: "linked plugins",
            maximum: 0
        })
    ));
}

#[test]
fn build_revalidates_registered_inputs_after_limits_are_lowered() {
    let mut builder = HostBuilder::new(paths());
    builder.define("answer", Value::from(42)).unwrap();
    let limits = HostLimits {
        maximum_identifier_bytes: 1,
        ..HostLimits::default()
    };
    assert!(matches!(
        builder.limits(limits).build(),
        Err(HostError::InvalidIdentifier {
            kind: "platform" | "define",
            maximum: 1
        })
    ));

    let mut builder = HostBuilder::new(paths());
    builder
        .register_launch_patch(rsi_host::ProfilePatch::SetEnabled {
            target: "first".into(),
            enabled: false,
        })
        .unwrap();
    builder
        .register_launch_patch(rsi_host::ProfilePatch::SetEnabled {
            target: "second".into(),
            enabled: false,
        })
        .unwrap();
    let mut limits = HostLimits::default();
    limits.profile.maximum_steps = 1;
    assert!(matches!(
        builder.limits(limits).build(),
        Err(HostError::CapacityExceeded {
            resource: "launch patches",
            maximum: 1
        })
    ));
}

#[test]
fn profile_control_marker_is_reserved_for_the_bootstrap() {
    let mut builder = HostBuilder::new(paths());
    assert!(matches!(
        builder.register_local_contract::<ProfileControlContract>(),
        Err(HostError::DuplicateLocalContractType { .. })
    ));
    let _ = MetaError::Cancelled;
}
