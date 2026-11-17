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
use rsi_storage_domain::{Domain, DomainFacility, DomainFacilityContract, DomainSpec, Schema};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

fn linked(name: &str, factory: impl PluginFactory) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, Arc::new(factory))
}
fn paths(root: &Path) -> ServiceHostPaths {
    let paths = HostPaths::new(
        root.join("config"),
        root.join("data"),
        root.join("state"),
        root.join("cache"),
        root.join("tmp"),
    )
    .unwrap();
    ServiceHostPaths::from_host_paths_with_runtime(&paths, None).unwrap()
}
fn identity_unit() -> rsi_storage::UnitDescriptor {
    rsi_storage::UnitDescriptor::new(
        "rsi.service.identity".into(),
        2,
        BTreeSet::new(),
        BTreeMap::new(),
        true,
        rsi_storage::Layout::Single,
    )
    .unwrap()
}
async fn replace_identity(backend: &dyn rsi_storage::KvBackend, value: Value) {
    let opened = backend.open(identity_unit()).await.unwrap();
    opened
        .unit
        .write(vec![rsi_storage::Mutation {
            address: rsi_storage::Address::global(),
            value: Some(rsi_storage::EncodedValue::encode(&value).unwrap()),
        }])
        .await
        .unwrap();
}
async fn read_identity(backend: &dyn rsi_storage::KvBackend) -> Value {
    backend
        .open(identity_unit())
        .await
        .unwrap()
        .snapshot
        .records[&rsi_storage::Address::global()]
        .decode()
        .unwrap()
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
async fn older_identity_unit_is_refused_without_restamping_or_replacing_its_value() {
    let directory = tempfile::tempdir().unwrap();
    let owner = Arc::new(HostOwnerLease::try_acquire(paths(directory.path())).unwrap());
    let runtime = storage(directory.path()).await;
    let backend = runtime
        .root()
        .lookup_local::<rsi_storage::StorageHubContract>()
        .unwrap()
        .resolve("base")
        .unwrap();
    // Synthetic version admission evidence, not a historical writer corpus.
    let legacy = rsi_storage::UnitDescriptor::new(
        "rsi.service.identity".into(),
        1,
        BTreeSet::new(),
        BTreeMap::new(),
        true,
        rsi_storage::Layout::Single,
    )
    .unwrap();
    let original = json!({"endpoint":"a".repeat(32)});
    let opened = backend.open(legacy.clone()).await.unwrap();
    opened
        .unit
        .write(vec![rsi_storage::Mutation {
            address: rsi_storage::Address::global(),
            value: Some(rsi_storage::EncodedValue::encode(&original).unwrap()),
        }])
        .await
        .unwrap();
    drop(opened);
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
    let identity = runtime
        .root()
        .apply(
            linked("identity", ServiceIdentityFactory),
            json!({"backend":"base"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        identity.snapshot().state,
        rsi_meta::FiberState::Failed(error)
            if error.contains("domain `rsi.service.identity` has version 1, expected 2")
    ));
    assert!(
        runtime
            .root()
            .lookup_local::<EndpointIdentityContract>()
            .is_none()
    );
    let retained = backend.open(legacy).await.unwrap().snapshot;
    assert_eq!(retained.version, Some(1));
    assert_eq!(
        retained.records[&rsi_storage::Address::global()]
            .decode::<Value>()
            .unwrap(),
        original
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn corrupt_or_changed_durable_identity_is_not_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let owner = Arc::new(HostOwnerLease::try_acquire(paths(directory.path())).unwrap());
    let runtime = storage(directory.path()).await;
    let backend = runtime
        .root()
        .lookup_local::<rsi_storage::StorageHubContract>()
        .unwrap()
        .resolve("base")
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
    let original = read_identity(backend.as_ref()).await;
    identity.dispose().await;
    for value in [
        json!({"endpoint":"bad"}),
        json!({"endpoint":"a".repeat(32), "foreign":true}),
        json!({"endpoint":"b".repeat(32)}),
    ] {
        replace_identity(backend.as_ref(), value.clone()).await;
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
        assert_eq!(read_identity(backend.as_ref()).await, value);
        assert!(
            runtime
                .root()
                .lookup_local::<EndpointIdentityContract>()
                .is_none()
        );
    }
    replace_identity(backend.as_ref(), original).await;
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
    opened: std::sync::Mutex<Vec<Domain>>,
}
#[async_trait]
impl DomainFacility for Gate {
    async fn open(
        &self,
        spec: DomainSpec,
        schema: Schema,
    ) -> Result<Domain, rsi_storage::StorageError> {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        let domain = self.facility.open(spec, schema).await?;
        self.opened.lock().unwrap().push(domain.clone());
        Ok(domain)
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
        opened: std::sync::Mutex::new(vec![]),
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
    assert!(gate.opened.lock().unwrap()[0].ensure_available().is_err());
    let backend = storage
        .root()
        .lookup_local::<rsi_storage::StorageHubContract>()
        .unwrap()
        .resolve("base")
        .unwrap();
    let identity = read_identity(backend.as_ref()).await;
    rsi_api_protocol::EndpointId::parse(identity["endpoint"].as_str().unwrap()).unwrap();
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

#[test]
fn lifecycle_and_owner_admit_the_same_bounded_process_tokens() {
    use rsi_service_host::{HostOwnerMetadata, HostOwnerMode, LifecycleIdentity};
    let mut owner = HostOwnerMetadata::current(
        HostOwnerMode::Embedded,
        "a".repeat(64),
        HostEpoch::generate().unwrap(),
        rsi_api_protocol::EndpointId::from_bytes([1; 16]),
        None,
    )
    .unwrap();
    for (token, valid) in [
        ("0".to_owned(), true),
        ("18446744073709551615".into(), true),
        (format!("process-{}", owner.pid), true),
        ("process-0".into(), false),
        ("18446744073709551616".into(), false),
        (String::new(), false),
        ("1".repeat(129), false),
    ] {
        owner.process_start_token = token;
        assert_eq!(owner.validate().is_ok(), valid);
        assert_eq!(LifecycleIdentity::from(&owner).validate().is_ok(), valid);
    }
}
