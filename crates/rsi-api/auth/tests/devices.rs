use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_api_auth::DeviceRegistry;
use rsi_api_protocol::{ApiError, DeviceAdministration, DeviceAuthentication, EndpointId};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_storage::{Address, StorageFactory};
use rsi_storage_domain::{DomainFacility, DomainFacilityContract, DomainFactory};
use rsi_storage_testkit::{BackendFactory, MemoryBackend};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

#[derive(Debug)]
struct TestDomain {
    backend: Arc<MemoryBackend>,
    facility: Arc<dyn DomainFacility>,
    _runtime: Runtime,
}
impl TestDomain {
    async fn new() -> Arc<Self> {
        let runtime = Runtime::default();
        let backend = Arc::new(MemoryBackend::default());
        let factories: [(&str, Arc<dyn rsi_meta::PluginFactory>); 3] = [
            ("hub", Arc::new(StorageFactory)),
            (
                "memory",
                Arc::new(BackendFactory::new("memory", backend.clone())),
            ),
            ("domain", Arc::new(DomainFactory)),
        ];
        for (name, factory) in factories {
            let plugin = runtime
                .root()
                .apply(
                    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
                    Value::Null,
                )
                .await
                .unwrap();
            assert_eq!(plugin.snapshot().state, rsi_meta::FiberState::Active);
        }
        let facility = runtime
            .root()
            .lookup_local::<DomainFacilityContract>()
            .unwrap();
        Arc::new(Self {
            backend,
            facility,
            _runtime: runtime,
        })
    }
}
#[derive(Clone, Debug)]
struct TestRegistry {
    registry: DeviceRegistry,
    _fixture: Arc<TestDomain>,
}
impl std::ops::Deref for TestRegistry {
    type Target = DeviceRegistry;
    fn deref(&self) -> &DeviceRegistry {
        &self.registry
    }
}
async fn open(fixture: Arc<TestDomain>) -> TestRegistry {
    let registry = DeviceRegistry::open(
        Execution::native(tokio::runtime::Handle::current()),
        fixture.facility.as_ref(),
        "memory",
        EndpointId::from_bytes([1; 16]),
    )
    .await
    .unwrap();
    TestRegistry {
        registry,
        _fixture: fixture,
    }
}

#[tokio::test]
async fn stored_verifiers_survive_restart_without_plaintext_and_revoke_live_leases() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let device = registry.register("browser").await.unwrap();
    assert!(!format!("{device:?}").contains(device.token.expose_secret()));
    let stored = serde_json::to_string(&domain.backend.values("rsi.api.devices")).unwrap();
    assert!(!stored.contains(device.token.expose_secret()));
    let authenticated = registry.authenticate(&device.token).unwrap();
    assert_eq!(authenticated.id, device.record.id);
    registry.close().await;
    assert!(authenticated.revoked.is_cancelled());
    let registry = open(domain.clone()).await;
    let lease = registry.authenticate(&device.token).unwrap();
    assert_eq!(lease.id, device.record.id);
    assert_eq!(registry.list().unwrap(), vec![device.record.clone()]);
    assert!(registry.revoke(&device.record.id).await.unwrap());
    assert!(lease.revoked.is_cancelled());
    assert!(matches!(
        registry.authenticate(&device.token),
        Err(ApiError::Unauthorized)
    ));
    assert!(!registry.revoke(&device.record.id).await.unwrap());
    registry.close().await;
    assert!(open(domain).await.list().unwrap().is_empty());
}

#[tokio::test]
async fn failed_publication_preserves_valid_authentication_and_rejects_new_registration() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let device = registry.register("retained").await.unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    let snapshot = domain.backend.values("rsi.api.devices");
    domain
        .backend
        .state
        .reject_write
        .store(true, Ordering::SeqCst);
    assert!(registry.revoke(&device.record.id).await.is_err());
    assert!(registry.register("lost").await.is_err());
    assert!(!lease.revoked.is_cancelled());
    assert!(registry.authenticate(&device.token).is_ok());
    assert_eq!(domain.backend.values("rsi.api.devices"), snapshot);
    assert_eq!(registry.list().unwrap(), vec![device.record]);
    registry.close().await;
}

#[tokio::test]
async fn dropped_revoke_waiter_does_not_cancel_publication_or_release_commit_ownership() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let device = registry.register("remote").await.unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    domain
        .backend
        .state
        .pause_write
        .store(true, Ordering::SeqCst);
    let mut revoke = Box::pin(registry.revoke(&device.record.id));
    assert!(revoke.as_mut().now_or_never().is_none());
    domain.backend.state.entered.notified().await;
    drop(revoke);
    assert!(
        !lease.revoked.is_cancelled(),
        "publication is still pending"
    );
    let mut queued = Box::pin(registry.register("queued"));
    assert!(queued.as_mut().now_or_never().is_none());
    assert_eq!(
        domain.backend.state.writes.load(Ordering::SeqCst),
        2,
        "queued registration cannot publish while the accepted revoke is paused"
    );
    drop(queued);
    domain.backend.state.release.notify_one();
    lease.revoked.cancelled().await;
    registry.close().await;
    assert!(open(domain).await.list().unwrap().is_empty());
}

#[tokio::test]
async fn retirement_fences_escaped_access_before_waiting_for_an_owned_commit() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let device = registry.register("existing").await.unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    domain
        .backend
        .state
        .pause_write
        .store(true, Ordering::SeqCst);
    let mut register = Box::pin(registry.register("admitted"));
    assert!(register.as_mut().now_or_never().is_none());
    domain.backend.state.entered.notified().await;
    let mut closing = Box::pin(registry.close());
    assert!(closing.as_mut().now_or_never().is_none());
    assert!(
        lease.revoked.is_cancelled(),
        "retirement must fence old leases before the write finishes"
    );
    assert!(matches!(
        registry.authenticate(&device.token),
        Err(ApiError::ShuttingDown)
    ));
    domain.backend.state.release.notify_one();
    let admitted = register.await.unwrap();
    closing.await;
    assert!(matches!(
        registry.authenticate(&admitted.token),
        Err(ApiError::ShuttingDown)
    ));
}

#[tokio::test]
async fn capacity_and_durable_validation_fail_closed_without_accepting_unknown_devices() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    for label in ["", "\n", &"a".repeat(129)] {
        assert!(registry.register(label).await.is_err());
    }
    for index in 0..64 {
        registry.register(&format!("device {index}")).await.unwrap();
    }
    assert_eq!(
        registry.register("overflow").await.unwrap_err(),
        ApiError::Capacity
    );
    assert!(matches!(
        registry.authenticate(&SecretValue::new("a".repeat(64)).unwrap()),
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        registry.authenticate(&SecretValue::new("invalid").unwrap()),
        Err(ApiError::Unauthorized)
    ));
    registry.close().await;
    let correct = domain.backend.values("rsi.api.devices");
    let original = &correct[""];
    let mut duplicate = original.clone();
    duplicate["devices"][1] = duplicate["devices"][0].clone();
    let mut extra = original.clone();
    extra["unknown"] = json!(true);
    for malformed in [
        json!({}),
        duplicate,
        extra,
        json!({"endpoint":"ff".repeat(16),"devices":[]}),
    ] {
        domain
            .backend
            .seed("rsi.api.devices", 2, Address::global(), &malformed);
        assert!(
            DeviceRegistry::open(
                Execution::native(tokio::runtime::Handle::current()),
                domain.facility.as_ref(),
                "memory",
                EndpointId::from_bytes([1; 16])
            )
            .await
            .is_err()
        );
    }
}

#[derive(Debug)]
struct AuthDependencies(Arc<TestDomain>);
#[async_trait]
impl rsi_meta::PluginFactory for AuthDependencies {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let domain = plan
            .context()
            .provide_local::<DomainFacilityContract>(self.0.facility.clone())?;
        let fixture = self.0.clone();
        let identity = plan
            .context()
            .provide_local::<rsi_api_protocol::EndpointIdentityContract>(Arc::new(
                EndpointId::from_bytes([1; 16]),
            ))?;
        plan.defer(
            "withdraw fixture dependencies",
            Box::new(move || {
                Box::pin(async move {
                    drop(identity);
                    drop(domain);
                    drop(fixture);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn plugin_uses_exact_dependencies_and_retires_both_escaped_authorities() {
    use rsi_api_auth::DeviceAuthFactory;
    use rsi_api_protocol::{DeviceAdministrationContract, DeviceAuthenticationContract};
    use rsi_meta::{PluginFactory, ResolvedFactory, Runtime, RuntimeLimits, UpdateMode};
    assert!(
        DeviceAuthFactory
            .prepare(&json!({"backend":"memory","token":"forbidden"}))
            .is_err()
    );
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let deps = ResolvedFactory::linked(
        "deps",
        "test",
        UpdateMode::Replayable,
        Arc::new(AuthDependencies(TestDomain::new().await)),
    );
    let deps = runtime.root().apply(deps, Value::Null).await.unwrap();
    let auth = ResolvedFactory::linked(
        "auth",
        "test",
        UpdateMode::Replayable,
        Arc::new(DeviceAuthFactory),
    );
    let auth = runtime
        .root()
        .apply(auth, json!({"backend":"memory"}))
        .await
        .unwrap();
    let admin = runtime
        .root()
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap();
    let verify = runtime
        .root()
        .lookup_local::<DeviceAuthenticationContract>()
        .unwrap();
    let device = admin.register("device").await.unwrap();
    let lease = verify.authenticate(&device.token).unwrap();
    let duplicate = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "duplicate-auth",
                "test",
                UpdateMode::Replayable,
                Arc::new(DeviceAuthFactory),
            ),
            json!({"backend":"memory"}),
        )
        .await
        .unwrap();
    assert_ne!(duplicate.snapshot().state, rsi_meta::FiberState::Active);
    assert_eq!(auth.snapshot().state, rsi_meta::FiberState::Active);
    assert!(
        verify.authenticate(&device.token).is_ok(),
        "failed duplicate activation must not close the active registry"
    );
    duplicate.dispose().await;
    auth.dispose().await;
    assert!(lease.revoked.is_cancelled());
    assert!(matches!(
        verify.authenticate(&device.token),
        Err(ApiError::ShuttingDown)
    ));
    assert!(matches!(
        admin.register("retired").await,
        Err(ApiError::ShuttingDown)
    ));
    assert!(
        runtime
            .root()
            .lookup_local::<DeviceAuthenticationContract>()
            .is_none()
    );
    deps.dispose().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn managed_rotation_preserves_identity_and_failed_publication_preserves_authority() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let first = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    let lease = registry.authenticate(&first.token).unwrap();
    domain
        .backend
        .state
        .reject_write
        .store(true, Ordering::SeqCst);
    assert!(
        registry
            .rotate_managed("local-web", "Local Web")
            .await
            .is_err()
    );
    assert!(!lease.revoked.is_cancelled());
    assert!(registry.authenticate(&first.token).is_ok());
    domain
        .backend
        .state
        .reject_write
        .store(false, Ordering::SeqCst);
    let second = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    assert_eq!(first.record.id, second.record.id);
    assert!(lease.revoked.is_cancelled());
    assert!(registry.authenticate(&first.token).is_err());
    assert!(registry.authenticate(&second.token).is_ok());
    assert_eq!(registry.list().unwrap().len(), 1);
    registry.close().await;
    let registry = open(domain.clone()).await;
    let third = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    assert_eq!(third.record.id, first.record.id);
    assert!(
        !serde_json::to_string(&domain.backend.values("rsi.api.devices"))
            .unwrap()
            .contains(third.token.expose_secret())
    );
    assert!(
        registry
            .rotate_managed("../bad", "Local Web")
            .await
            .is_err()
    );
    registry.revoke(&third.record.id).await.unwrap();
    let fourth = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    assert_ne!(fourth.record.id, third.record.id);
    registry.close().await;
}

#[tokio::test]
async fn exact_credential_cleanup_cannot_revoke_a_later_managed_rotation() {
    let registry = open(TestDomain::new().await).await;
    let old = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    let current = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    assert_eq!(old.record.id, current.record.id);
    assert!(!registry.revoke_credential(&old).await.unwrap());
    let lease = registry.authenticate(&current.token).unwrap();
    assert!(!lease.revoked.is_cancelled());
    assert!(registry.revoke_credential(&current).await.unwrap());
    assert!(lease.revoked.is_cancelled());
    assert_eq!(registry.list().unwrap(), vec![current.record.clone()]);
    assert!(registry.authenticate(&current.token).is_err());
    assert!(!registry.revoke_credential(&current).await.unwrap());
    let resumed = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    assert_eq!(resumed.record.id, current.record.id);
}

#[tokio::test]
async fn retired_managed_credential_preserves_principal_across_restart_and_failed_publication() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let device = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    domain
        .backend
        .state
        .reject_write
        .store(true, Ordering::SeqCst);
    assert!(registry.revoke_credential(&device).await.is_err());
    assert!(!lease.revoked.is_cancelled());
    assert!(registry.authenticate(&device.token).is_ok());
    domain
        .backend
        .state
        .reject_write
        .store(false, Ordering::SeqCst);
    assert!(registry.revoke_credential(&device).await.unwrap());
    assert!(lease.revoked.is_cancelled());
    registry.close().await;
    let registry = open(domain).await;
    assert!(registry.authenticate(&device.token).is_err());
    let resumed = registry
        .rotate_managed("local-web", "Local Web")
        .await
        .unwrap();
    assert_eq!(device.record.id, resumed.record.id);
    assert!(registry.authenticate(&resumed.token).is_ok());
    let unmanaged = registry.register("Manual").await.unwrap();
    assert!(registry.revoke_credential(&unmanaged).await.unwrap());
    assert!(
        !registry
            .list()
            .unwrap()
            .iter()
            .any(|row| row.id == unmanaged.record.id)
    );
    registry.close().await;
}

#[tokio::test]
async fn managed_rotation_checks_observed_identity_without_revoking_on_conflict() {
    let registry = open(TestDomain::new().await).await;
    assert!(registry.managed_device("local-web").unwrap().is_none());
    let first = registry
        .rotate_managed_if("local-web", "Local Web", None)
        .await
        .unwrap();
    let lease = registry.authenticate(&first.token).unwrap();
    let observed = registry.managed_device("local-web").unwrap().unwrap();
    assert_eq!(observed, first.record);
    assert!(
        registry
            .rotate_managed_if("local-web", "Local Web", None)
            .await
            .is_err()
    );
    assert!(!lease.revoked.is_cancelled());
    assert!(registry.authenticate(&first.token).is_ok());
    registry.revoke(&first.record.id).await.unwrap();
    let second = registry
        .rotate_managed_if("local-web", "Local Web", None)
        .await
        .unwrap();
    assert!(
        registry
            .rotate_managed_if("local-web", "Local Web", Some(&observed.id))
            .await
            .is_err()
    );
    assert!(registry.authenticate(&second.token).is_ok());
    let third = registry
        .rotate_managed_if("local-web", "Local Web", Some(&second.record.id))
        .await
        .unwrap();
    assert_eq!(third.record.id, second.record.id);
    assert!(registry.authenticate(&second.token).is_err());
    assert!(registry.authenticate(&third.token).is_ok());
    registry.close().await;
}

#[tokio::test]
async fn backend_failure_in_another_domain_blocks_cached_authentication_and_administration() {
    let domain = TestDomain::new().await;
    let registry = open(domain.clone()).await;
    let device = registry.register("fixture").await.unwrap();
    domain
        .backend
        .state
        .unhealthy
        .store(true, Ordering::Release);
    assert!(matches!(
        registry.authenticate(&device.token),
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(registry.list(), Err(ApiError::Unavailable)));
    assert!(matches!(
        registry.managed_device("slot"),
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(
        registry.register("another").await,
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(
        registry.revoke(&device.record.id).await,
        Err(ApiError::Unavailable)
    ));
    registry.close().await;
}
