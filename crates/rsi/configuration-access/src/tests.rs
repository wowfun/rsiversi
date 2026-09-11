use super::*;
use rsi_api_protocol::{
    ApiDispatchContract, ApiOutput, ApiRegistrarContract, ByteBudget, DeviceAuthentication,
    EndpointId, OperationId,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_settings_protocol::{SettingsContract, SettingsMetadata, SettingsSpec, ValidateWith};
use rsi_storage::StorageError;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
struct TestDomain {
    spec: DomainSpec,
    values: Mutex<BTreeMap<String, Value>>,
    fail: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
    pause: AtomicBool,
}
impl TestDomain {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            spec: DomainSpec {
                id: id.into(),
                backend: "fixture".into(),
                version: 1,
                maximum_records: 1,
                maximum_bytes: 64 * 1024,
            },
            values: Mutex::default(),
            fail: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            pause: AtomicBool::new(false),
        })
    }
}
#[async_trait]
impl Domain for TestDomain {
    fn spec(&self) -> &DomainSpec {
        &self.spec
    }
    async fn snapshot(&self) -> BTreeMap<String, Value> {
        self.values.lock().unwrap().clone()
    }
    async fn put(&self, key: &str, value: Value) -> std::result::Result<(), StorageError> {
        if self.pause.load(Ordering::SeqCst) {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(StorageError::Io("fixture durable failure".into()));
        }
        self.values.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn delete(&self, _: &str) -> std::result::Result<bool, StorageError> {
        unreachable!("single-document owner")
    }
}
async fn settings() -> Runtime {
    let runtime = Runtime::default();
    for (name, factory) in [
        (
            "provider",
            Arc::new(rsi_settings_testkit::MemorySettingsProviderFactory::new(
                json!({}),
            )) as Arc<dyn PluginFactory>,
        ),
        ("settings", Arc::new(rsi_settings::SettingsFactory)),
        ("api", Arc::new(rsi_api::ApiFactory)),
    ] {
        runtime
            .root()
            .apply(
                ResolvedFactory::linked(name, "1", UpdateMode::Replayable, factory),
                Value::Null,
            )
            .await
            .unwrap();
    }
    runtime
}
async fn owner(
    domain: Arc<TestDomain>,
    devices: Arc<rsi_api_auth::DeviceRegistry>,
    runtime: &Runtime,
) -> Arc<ConfigurationAccess> {
    ConfigurationAccess::open(
        Execution::native(tokio::runtime::Handle::current()),
        domain,
        devices,
        runtime
            .root()
            .lookup_local::<SettingsAccessContract>()
            .unwrap(),
    )
    .await
    .unwrap()
}
async fn devices() -> Arc<rsi_api_auth::DeviceRegistry> {
    Arc::new(
        rsi_api_auth::DeviceRegistry::open(
            Execution::native(tokio::runtime::Handle::current()),
            TestDomain::new("rsi.api.devices"),
            EndpointId::from_bytes([1; 16]),
        )
        .await
        .unwrap(),
    )
}
#[tokio::test]
async fn revocation_fences_admission_drains_existing_leases_and_survives_restart() {
    let runtime = settings().await;
    let devices = devices().await;
    let registered = devices.register("remote").await.unwrap();
    let origin = CallOrigin::Device(devices.authenticate(&registered.token).unwrap());
    let domain = TestDomain::new("rsi.configuration.grants");
    let owner = owner(domain.clone(), devices.clone(), &runtime).await;
    assert!(matches!(owner.admit(&origin), Err(ApiError::Unauthorized)));
    assert!(matches!(
        owner.set_grant(&origin, registered.record.id.clone(), "0", true),
        Err(ApiError::Unauthorized)
    ));
    let first = owner
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(first.revision, "1");
    let lease = owner.admit(&origin).unwrap();
    let waiter = owner
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "1", false)
        .unwrap();
    // Yield through the actual owner task until its gate has closed, without timing assumptions.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while owner.allowed(&origin) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(owner.admit(&origin), Err(ApiError::Unauthorized)));
    assert_eq!(
        owner.snapshot().await.unwrap().revision,
        "1",
        "revocation must await retained mutations before commit"
    );
    drop(lease);
    assert_eq!(waiter.await.unwrap().revision, "2");
    owner.close().await;
    let reopened = ConfigurationAccess::open(
        Execution::native(tokio::runtime::Handle::current()),
        domain,
        devices.clone(),
        runtime
            .root()
            .lookup_local::<SettingsAccessContract>()
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(!reopened.allowed(&origin));
    assert_eq!(reopened.snapshot().await.unwrap().revision, "2");
    reopened.close().await;
    devices.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn uncertain_grant_write_keeps_its_writer_and_failed_revoke_stays_closed() {
    let runtime = settings().await;
    let devices = devices().await;
    let registered = devices.register("remote").await.unwrap();
    let origin = CallOrigin::Device(devices.authenticate(&registered.token).unwrap());
    let domain = TestDomain::new("rsi.configuration.grants");
    let owner = owner(domain.clone(), devices.clone(), &runtime).await;
    domain.pause.store(true, Ordering::SeqCst);
    let waiter = owner
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
        .unwrap();
    domain.entered.acquire().await.unwrap().forget();
    drop(waiter);
    assert!(!owner.allowed(&origin));
    assert!(matches!(
        owner.set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true),
        Err(ApiError::Capacity)
    ));
    domain.pause.store(false, Ordering::SeqCst);
    domain.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !owner.allowed(&origin) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    domain.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        owner
            .set_grant(&CallOrigin::Local, registered.record.id.clone(), "1", false)
            .unwrap()
            .await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert!(!owner.allowed(&origin));
    assert_eq!(
        owner.snapshot().await.unwrap().devices,
        vec![registered.record.id.clone()]
    );
    domain.fail.store(false, Ordering::SeqCst);
    owner
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "1", true)
        .unwrap()
        .await
        .unwrap();
    assert!(owner.allowed(&origin));
    let leases = (0..8)
        .map(|_| owner.admit(&origin).unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(owner.admit(&origin), Err(ApiError::Capacity)));
    drop(leases);
    owner.close().await;
    devices.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One observable lifecycle with shared setup and assertions"
)]
async fn existing_settings_wire_cannot_bypass_grants_or_change_preset_roots() {
    let runtime = settings().await;
    let devices = devices().await;
    let registered = devices.register("remote").await.unwrap();
    let origin = CallOrigin::Device(devices.authenticate(&registered.token).unwrap());
    let owner = owner(
        TestDomain::new("rsi.configuration.grants"),
        devices.clone(),
        &runtime,
    )
    .await;
    let settings = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    let registry = runtime.root().lookup_local::<SettingsContract>().unwrap();
    let mut scopes = Vec::new();
    for namespace in ["rsi.agent", "rsi.agent-presets", "rsi.other"] {
        scopes.push(
            registry
                .register(SettingsSpec {
                    namespace: namespace.into(),
                    defaults: json!({"default":"standard","roots":[]}),
                    base: json!({}),
                    metadata: SettingsMetadata {
                        schema: json!({"type":"object"}),
                        applies: rsi_settings_protocol::SettingsApply::Live,
                        description: "fixture".into(),
                        sensitive_fields: vec![],
                    },
                    validator: Arc::new(ValidateWith(|_: &Value| Ok(()))),
                })
                .unwrap(),
        );
    }
    let api = rsi_settings_api::SettingsApi::register(
        runtime
            .root()
            .lookup_local::<ApiRegistrarContract>()
            .unwrap()
            .as_ref(),
        settings.clone(),
        owner.clone(),
    )
    .unwrap();
    let dispatch = runtime
        .root()
        .lookup_local::<ApiDispatchContract>()
        .unwrap();
    for granted in [false, true] {
        if granted {
            owner
                .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
                .unwrap()
                .await
                .unwrap();
        }
        for (namespace, name, value, expected_allowed) in [
            ("rsi.agent", "replace", json!({"enabled":true}), granted),
            ("rsi.agent", "clear", Value::Null, granted),
            (
                "rsi.agent-presets",
                "replace",
                json!({"default":"another","roots":[]}),
                granted,
            ),
            (
                "rsi.agent-presets",
                "replace",
                json!({"default":"another","roots":[{"path":"/other"}]}),
                false,
            ),
            ("rsi.agent-presets", "clear", Value::Null, false),
            ("rsi.other", "replace", json!({}), false),
        ] {
            let before = settings.read(namespace).await.unwrap();
            let mut input = json!({"namespace":namespace,"expected":before.version()});
            if name == "replace" {
                input["value"] = value;
            }
            let input = ByteBudget::default()
                .encode(&input, 8 * 1024 * 1024)
                .unwrap();
            let result = dispatch
                .admit(
                    &OperationId::new("settings", name, 1).unwrap(),
                    origin.clone(),
                )
                .unwrap()
                .invoke(input)
                .await;
            if expected_allowed {
                assert!(
                    matches!(result, Ok(ApiOutput::Reply(_))),
                    "{namespace} {name}: {result:?}"
                );
            } else {
                assert!(
                    matches!(result, Err(ApiError::Unauthorized)),
                    "{namespace} {name}: {result:?}"
                );
                assert_eq!(before, settings.read(namespace).await.unwrap());
            }
        }
    }
    api.close().await;
    owner.close().await;
    drop(scopes);
    devices.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn device_authentication_revocation_fences_old_origins_but_retains_admitted_work() {
    let runtime = settings().await;
    let devices = devices().await;
    let registered = devices.register("remote").await.unwrap();
    let origin = CallOrigin::Device(devices.authenticate(&registered.token).unwrap());
    let owner = owner(
        TestDomain::new("rsi.configuration.grants"),
        devices.clone(),
        &runtime,
    )
    .await;
    owner
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
        .unwrap()
        .await
        .unwrap();
    let admitted = owner.admit(&origin).unwrap();
    devices.revoke(&registered.record.id).await.unwrap();
    assert!(!owner.allowed(&origin));
    assert!(matches!(owner.admit(&origin), Err(ApiError::Unauthorized)));
    assert!(devices.authenticate(&registered.token).is_err());
    let closing = owner.close();
    tokio::pin!(closing);
    assert!(futures_util::poll!(&mut closing).is_pending());
    drop(admitted);
    closing.await;
    devices.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn grant_capacity_rejects_the_sixty_fifth_device_without_changing_durable_state() {
    let runtime = settings().await;
    let devices = devices().await;
    let domain = TestDomain::new("rsi.configuration.grants");
    let owner = owner(domain.clone(), devices.clone(), &runtime).await;
    for index in 0..64 {
        let registered = devices.register(&format!("device-{index}")).await.unwrap();
        owner
            .set_grant(
                &CallOrigin::Local,
                registered.record.id,
                &index.to_string(),
                true,
            )
            .unwrap()
            .await
            .unwrap();
    }
    let before = domain.snapshot().await;
    let retired = devices.list().unwrap()[0].id.clone();
    devices.revoke(&retired).await.unwrap();
    let registered = devices.register("overflow").await.unwrap();
    assert!(matches!(
        owner
            .set_grant(&CallOrigin::Local, registered.record.id.clone(), "64", true)
            .unwrap()
            .await,
        Err(ApiError::Capacity)
    ));
    assert_eq!(domain.snapshot().await, before);
    assert_eq!(owner.snapshot().await.unwrap().devices.len(), 64);
    owner.close().await;
    // Reject the same oversized set at the durable-input boundary on restart.
    domain.values.lock().unwrap().get_mut("grants").unwrap()["devices"]
        .as_array_mut()
        .unwrap()
        .push(json!(registered.record.id));
    let reopened = ConfigurationAccess::open(
        Execution::native(tokio::runtime::Handle::current()),
        domain,
        devices.clone(),
        runtime
            .root()
            .lookup_local::<SettingsAccessContract>()
            .unwrap(),
    )
    .await;
    assert!(matches!(reopened, Err(ApiError::Backend(_))));
    devices.close().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct BlockingCredentials {
    entered: Semaphore,
    release: Semaphore,
    writes: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl rsi_credentials_protocol::CredentialsStatus for BlockingCredentials {
    async fn status(
        &self,
        _: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<rsi_credentials_protocol::CredentialStatus> {
        unreachable!("mutation fixture")
    }
}
#[async_trait]
impl rsi_credentials_protocol::CredentialsAdmin for BlockingCredentials {
    async fn set(
        &self,
        _: &rsi_credentials_protocol::CredentialRef,
        _: rsi_credentials_protocol::SecretValue,
    ) -> rsi_credentials_protocol::Result<()> {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn unset(
        &self,
        _: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<bool> {
        unreachable!("set fixture")
    }
}

#[tokio::test]
async fn credential_write_survives_reply_loss_and_holds_the_revoke_fence() {
    let runtime = settings().await;
    let devices = devices().await;
    let registered = devices.register("remote").await.unwrap();
    let origin = CallOrigin::Device(devices.authenticate(&registered.token).unwrap());
    let owner = owner(
        TestDomain::new("rsi.configuration.grants"),
        devices.clone(),
        &runtime,
    )
    .await;
    owner
        .set_grant(&CallOrigin::Local, registered.record.id.clone(), "0", true)
        .unwrap()
        .await
        .unwrap();
    let credentials = Arc::new(BlockingCredentials {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        writes: std::sync::atomic::AtomicUsize::new(0),
    });
    let registrations = super::credentials::register(
        runtime
            .root()
            .lookup_local::<ApiRegistrarContract>()
            .unwrap()
            .as_ref(),
        owner.clone(),
        credentials.clone(),
        credentials.clone(),
    )
    .unwrap();
    let dispatch = runtime
        .root()
        .lookup_local::<ApiDispatchContract>()
        .unwrap();
    let spec = rsi_configuration_api::CredentialOperation::Set.spec();
    let input = ByteBudget::default()
        .encode(
            &json!({"provider":"deepseek","slot":"fixture","secret":"isolated-value"}),
            spec.maximum_request_bytes,
        )
        .unwrap();
    let call = dispatch.admit(&spec.id, origin.clone()).unwrap();
    let waiter = tokio::spawn(async move { call.invoke(input).await });
    credentials.entered.acquire().await.unwrap().forget();
    waiter.abort();
    let _ = waiter.await;
    let revoke = owner
        .set_grant(&CallOrigin::Local, registered.record.id, "1", false)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while owner.allowed(&origin) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(owner.snapshot().await.unwrap().revision, "1");
    assert_eq!(credentials.writes.load(Ordering::SeqCst), 0);
    credentials.release.add_permits(1);
    assert_eq!(revoke.await.unwrap().revision, "2");
    assert_eq!(credentials.writes.load(Ordering::SeqCst), 1);
    for registration in registrations {
        registration.close().await;
    }
    owner.close().await;
    devices.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
