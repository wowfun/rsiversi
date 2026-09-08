use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_api_auth::DeviceRegistry;
use rsi_api_protocol::{ApiError, DeviceAdministration, DeviceAuthentication, EndpointId};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use rsi_storage::StorageError;
use rsi_storage_domain::{Domain, DomainSpec};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Semaphore;

#[derive(Debug)]
struct TestDomain {
    spec: DomainSpec,
    records: Mutex<BTreeMap<String, Value>>,
    fail: AtomicBool,
    block: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
impl TestDomain {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            spec: DomainSpec {
                id: "rsi.api.devices".into(),
                backend: "memory".into(),
                version: 1,
                maximum_records: 1,
                maximum_bytes: 64 * 1024,
            },
            records: Mutex::default(),
            fail: AtomicBool::new(false),
            block: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
}
#[async_trait]
impl Domain for TestDomain {
    fn spec(&self) -> &DomainSpec {
        &self.spec
    }
    async fn snapshot(&self) -> BTreeMap<String, Value> {
        self.records.lock().unwrap().clone()
    }
    async fn put(&self, key: &str, value: Value) -> std::result::Result<(), StorageError> {
        if self.block.load(Ordering::SeqCst) {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(StorageError::Corrupt("injected publication failure".into()));
        }
        self.records.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn delete(&self, _: &str) -> std::result::Result<bool, StorageError> {
        panic!("device state publishes one atomic record")
    }
}

async fn open(domain: Arc<TestDomain>) -> DeviceRegistry {
    DeviceRegistry::open(
        Execution::native(tokio::runtime::Handle::current()),
        domain,
        EndpointId::from_bytes([1; 16]),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn stored_verifiers_survive_restart_without_plaintext_and_revoke_live_leases() {
    let domain = TestDomain::new();
    let registry = open(domain.clone()).await;
    let device = registry.register("browser").await.unwrap();
    assert!(!format!("{device:?}").contains(device.token.expose_secret()));
    let stored = serde_json::to_string(&domain.snapshot().await).unwrap();
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
    let domain = TestDomain::new();
    let registry = open(domain.clone()).await;
    let device = registry.register("retained").await.unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    let snapshot = domain.snapshot().await;
    domain.fail.store(true, Ordering::SeqCst);
    assert!(registry.revoke(&device.record.id).await.is_err());
    assert!(registry.register("lost").await.is_err());
    assert!(!lease.revoked.is_cancelled());
    assert!(registry.authenticate(&device.token).is_ok());
    assert_eq!(domain.snapshot().await, snapshot);
    assert_eq!(registry.list().unwrap(), vec![device.record]);
    registry.close().await;
}

#[tokio::test]
async fn dropped_revoke_waiter_does_not_cancel_publication_or_release_commit_ownership() {
    let domain = TestDomain::new();
    let registry = open(domain.clone()).await;
    let device = registry.register("remote").await.unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    domain.block.store(true, Ordering::SeqCst);
    let mut revoke = Box::pin(registry.revoke(&device.record.id));
    assert!(revoke.as_mut().now_or_never().is_none());
    domain.entered.acquire().await.unwrap().forget();
    drop(revoke);
    assert!(
        !lease.revoked.is_cancelled(),
        "publication is still pending"
    );
    let mut queued = Box::pin(registry.register("queued"));
    assert!(queued.as_mut().now_or_never().is_none());
    assert_eq!(
        domain.entered.available_permits(),
        0,
        "dropped waiter retains serialization ownership"
    );
    drop(queued);
    domain.release.add_permits(1);
    lease.revoked.cancelled().await;
    registry.close().await;
    assert!(open(domain).await.list().unwrap().is_empty());
}

#[tokio::test]
async fn retirement_fences_escaped_access_before_waiting_for_an_owned_commit() {
    let domain = TestDomain::new();
    let registry = open(domain.clone()).await;
    let device = registry.register("existing").await.unwrap();
    let lease = registry.authenticate(&device.token).unwrap();
    domain.block.store(true, Ordering::SeqCst);
    let mut register = Box::pin(registry.register("admitted"));
    assert!(register.as_mut().now_or_never().is_none());
    domain.entered.acquire().await.unwrap().forget();
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
    domain.release.add_permits(1);
    let admitted = register.await.unwrap();
    closing.await;
    assert!(matches!(
        registry.authenticate(&admitted.token),
        Err(ApiError::ShuttingDown)
    ));
}

#[tokio::test]
async fn capacity_and_durable_validation_fail_closed_without_accepting_unknown_devices() {
    let domain = TestDomain::new();
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
    let correct = domain.snapshot().await;
    let original = &correct["deployment"];
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
            .records
            .lock()
            .unwrap()
            .insert("deployment".into(), malformed);
        assert!(
            DeviceRegistry::open(
                Execution::native(tokio::runtime::Handle::current()),
                domain.clone(),
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
impl rsi_storage_domain::DomainFacility for AuthDependencies {
    async fn open(&self, spec: DomainSpec) -> std::result::Result<Arc<dyn Domain>, StorageError> {
        assert_eq!(&spec, self.0.spec());
        Ok(self.0.clone())
    }
}
#[async_trait]
impl rsi_meta::PluginFactory for AuthDependencies {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let domain = plan
            .context()
            .provide_local::<rsi_storage_domain::DomainFacilityContract>(Arc::new(Self(
                self.0.clone(),
            )))?;
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
        Arc::new(AuthDependencies(TestDomain::new())),
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
