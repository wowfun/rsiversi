use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_api_protocol::{
    DeviceAdministrationContract, DeviceAuthenticationContract, EndpointIdentityContract,
    HostEpoch, HostGenerationContract,
};
use rsi_host::HostPaths;
use rsi_meta::{
    ActivationPlan, PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_service_host::{
    HostOwnerLease, ServiceHostError, ServiceHostPaths, ServiceIdentityFactory, ServiceOwnerFactory,
};
use rsi_storage_domain::{DomainFacility, DomainFacilityContract, DomainSpec};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

fn linked(name: &str, factory: impl PluginFactory) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, Arc::new(factory))
}
fn paths(root: &Path) -> ServiceHostPaths {
    let paths =
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap();
    ServiceHostPaths::from_host_paths_with_runtime(&paths, None).unwrap()
}
fn spec() -> DomainSpec {
    DomainSpec {
        id: "rsi.service.identity".into(),
        backend: "base".into(),
        version: 1,
        maximum_records: 1,
        maximum_bytes: 128,
    }
}

async fn storage(root: &Path) -> Runtime {
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("storage", rsi_storage::StorageFactory), Value::Null)
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("sqlite", rsi_storage_sqlite::SqliteStorageFactory),
            json!({"name":"base","path":root.join("state/base.sqlite3")}),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("domain", rsi_storage_domain::DomainFactory),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
}

#[tokio::test]
async fn durable_endpoint_and_device_verifier_survive_restart_with_a_new_epoch() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let owner = Arc::new(HostOwnerLease::try_acquire(paths.clone()).unwrap());
    let first = storage(directory.path()).await;
    let old_epoch = HostEpoch::generate().unwrap();
    publish_identity_and_devices(&first, owner.clone(), old_epoch.clone()).await;
    let endpoint = first
        .root()
        .lookup_local::<EndpointIdentityContract>()
        .unwrap();
    assert_eq!(
        *first
            .root()
            .lookup_local::<HostGenerationContract>()
            .unwrap(),
        old_epoch
    );
    let device = first
        .root()
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap()
        .register("isolated fixture")
        .await
        .unwrap();
    let auth = first
        .root()
        .lookup_local::<DeviceAuthenticationContract>()
        .unwrap();
    let old_authority = auth.authenticate(&device.token).unwrap();
    assert_eq!(old_authority.id, device.record.id);
    assert!(matches!(
        HostOwnerLease::try_acquire(paths.clone()),
        Err(ServiceHostError::OwnerActive)
    ));
    assert!(first.shutdown().await.is_clean());
    assert!(old_authority.revoked.is_cancelled());
    drop(first);
    drop(owner);

    let owner = Arc::new(HostOwnerLease::try_acquire(paths.clone()).unwrap());
    let second = storage(directory.path()).await;
    let new_epoch = HostEpoch::generate().unwrap();
    assert_ne!(old_epoch, new_epoch);
    publish_identity_and_devices(&second, owner.clone(), new_epoch.clone()).await;
    assert_eq!(
        second
            .root()
            .lookup_local::<EndpointIdentityContract>()
            .unwrap(),
        endpoint
    );
    assert_eq!(
        *second
            .root()
            .lookup_local::<HostGenerationContract>()
            .unwrap(),
        new_epoch
    );
    let auth = second
        .root()
        .lookup_local::<DeviceAuthenticationContract>()
        .unwrap();
    assert_eq!(
        auth.authenticate(&device.token).unwrap().id,
        device.record.id
    );
    let admin = second
        .root()
        .lookup_local::<DeviceAdministrationContract>()
        .unwrap();
    assert!(admin.revoke(&device.record.id).await.unwrap());
    assert!(auth.authenticate(&device.token).is_err());
    assert!(second.shutdown().await.is_clean());
    drop(second);
    drop(owner);
    HostOwnerLease::try_acquire(paths).unwrap();
}

#[tokio::test]
async fn corrupt_or_changed_durable_identity_is_not_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let owner = Arc::new(HostOwnerLease::try_acquire(paths(directory.path())).unwrap());
    let runtime = storage(directory.path()).await;
    let domain = runtime
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap()
        .open(spec())
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked(
                "owner",
                ServiceOwnerFactory::new(owner, HostEpoch::generate().unwrap()),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let factory = ServiceIdentityFactory;
    let identity = runtime
        .root()
        .apply(
            linked("identity", factory.clone()),
            json!({"backend":"base"}),
        )
        .await
        .unwrap();
    let original = domain.snapshot().await["deployment"].clone();
    identity.dispose().await;
    for value in [
        json!({"endpoint":"bad"}),
        json!({"endpoint":"a".repeat(32), "foreign":true}),
        json!({"endpoint":"b".repeat(32)}),
    ] {
        domain.put("deployment", value.clone()).await.unwrap();
        let failed = runtime
            .root()
            .apply(
                linked("identity", factory.clone()),
                json!({"backend":"base"}),
            )
            .await
            .unwrap();
        assert!(matches!(
            failed.snapshot().state,
            rsi_meta::FiberState::Failed(_)
        ));
        failed.dispose().await;
        assert_eq!(domain.snapshot().await["deployment"], value);
        assert!(
            runtime
                .root()
                .lookup_local::<EndpointIdentityContract>()
                .is_none()
        );
    }
    domain.put("deployment", original).await.unwrap();
    runtime
        .root()
        .apply(linked("identity", factory), json!({"backend":"base"}))
        .await
        .unwrap();
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct Gate {
    entered: Semaphore,
    release: Semaphore,
    facility: Arc<dyn DomainFacility>,
}
#[async_trait]
impl DomainFacility for Gate {
    async fn open(
        &self,
        spec: DomainSpec,
    ) -> Result<Arc<dyn rsi_storage_domain::Domain>, rsi_storage::StorageError> {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        self.facility.open(spec).await
    }
}
#[derive(Debug)]
struct Facility(Arc<Gate>);
#[async_trait]
impl PluginFactory for Facility {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<DomainFacilityContract>(self.0.clone())?;
        plan.defer(
            "withdraw fixture facility",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn shutdown_drains_owned_identity_initialization_before_releasing_the_lease() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let owner = Arc::new(HostOwnerLease::try_acquire(paths.clone()).unwrap());
    let storage = storage(directory.path()).await;
    let facility = storage
        .root()
        .lookup_local::<DomainFacilityContract>()
        .unwrap();
    let gate = Arc::new(Gate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        facility: facility.clone(),
    });
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("facility", Facility(gate.clone())), Value::Null)
        .await
        .unwrap();
    let activation = tokio::spawn({
        let root = runtime.root();
        let owner = owner.clone();
        async move {
            root.apply(
                linked(
                    "owner",
                    ServiceOwnerFactory::new(owner, HostEpoch::generate().unwrap()),
                ),
                Value::Null,
            )
            .await
            .unwrap();
            root.apply(
                linked("identity", ServiceIdentityFactory),
                json!({"backend":"base"}),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), gate.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let mut shutdown = Box::pin(runtime.shutdown());
    assert!(shutdown.as_mut().now_or_never().is_none());
    assert!(matches!(
        HostOwnerLease::try_acquire(paths.clone()),
        Err(ServiceHostError::OwnerActive)
    ));
    gate.release.add_permits(1);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .is_clean()
    );
    let _cancelled_activation = activation.await.unwrap();
    let records = facility.open(spec()).await.unwrap().snapshot().await;
    let endpoint = records["deployment"]["endpoint"].as_str().unwrap();
    rsi_api_protocol::EndpointId::parse(endpoint).unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<EndpointIdentityContract>()
            .is_none()
    );
    drop(runtime);
    drop(owner);
    HostOwnerLease::try_acquire(paths).unwrap();
    assert!(storage.shutdown().await.is_clean());
}

async fn publish_identity_and_devices(
    runtime: &Runtime,
    owner: Arc<HostOwnerLease>,
    epoch: HostEpoch,
) {
    runtime
        .root()
        .apply(
            linked("owner", ServiceOwnerFactory::new(owner, epoch)),
            Value::Null,
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("identity", ServiceIdentityFactory),
            json!({"backend":"base"}),
        )
        .await
        .unwrap();
    runtime
        .root()
        .apply(
            linked("devices", rsi_api_auth::DeviceAuthFactory),
            json!({"backend":"base"}),
        )
        .await
        .unwrap();
}
