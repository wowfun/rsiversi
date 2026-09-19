use super::*;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_configuration_api::{ManagedProvider, ProviderKind, ProvidersOperation, ProvidersSnapshot};
use serde_json::{Value, json};

fn deployment(name: &str) -> ManagedProvider {
    ManagedProvider {
        provider: ProviderKind::Deepseek,
        config: json!({
            "deployment": name, "credential":{"owner":"rsi.ai.provider.deepseek","slot":"default"},
            "endpoint":"http://127.0.0.1:1", "protocol":"chat-completions",
            "language_models":{"managed-model":{"context_window_tokens":128_000,"default_output_reserve_tokens":4096,"max_output_reserve_tokens":16384}}
        }),
    }
}
async fn call(
    running: &RunningRsi,
    origin: CallOrigin,
    operation: ProvidersOperation,
    input: Value,
) -> Result<ProvidersSnapshot, ApiError> {
    let spec = operation.spec();
    let input = ByteBudget::default().encode(&input, spec.maximum_request_bytes)?;
    let output = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.id, origin)?
        .invoke(input)
        .await?;
    let ApiOutput::Reply(output) = output else {
        panic!("providers returned a stream")
    };
    serde_json::from_slice(output.json.as_bytes())
        .map_err(|_| ApiError::Invalid("invalid provider snapshot".into()))
}
async fn replace(
    running: &RunningRsi,
    expected: &str,
    deployments: Vec<ManagedProvider>,
) -> Result<ProvidersSnapshot, ApiError> {
    call(
        running,
        CallOrigin::Local,
        ProvidersOperation::Replace,
        json!({"expected_revision":expected,"deployments":deployments}),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_preset_settings_select_future_headers_through_the_actual_api_scope() {
    use rsi_settings_protocol::SettingsAccess;
    let fixture = fixture("http://127.0.0.1:1");
    let alternate = fixture.paths.config().join("agent-presets/alternate");
    std::fs::create_dir_all(&alternate).unwrap();
    std::fs::write(
        alternate.join("agent.profile.toml"),
        include_bytes!("../../../../../plugins/rsi-agent-presets/standard/agent.profile.toml"),
    )
    .unwrap();
    let runtime = rsi_meta::Runtime::default();
    let connection = connect_or_embed_service_host(
        &runtime.root(),
        composition(fixture.paths.clone()),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    let settings = rsi_settings_api::SettingsClient::new(connection.api_client()).unwrap();
    let workspace = connection
        .workspace_registry()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let create = |name| CreateSession {
        session_id: SessionId::new(name).unwrap(),
        workspace_id: workspace.id.clone(),
        agent_preset_id: None,
    };
    let original = connection
        .session_service()
        .create(create("before-selection"))
        .await
        .unwrap();
    let header = original.header().await.unwrap();
    assert_eq!(header.agent_preset_id().as_str(), "standard");
    let snapshot = settings.read("rsi.agent-presets").await.unwrap();
    let mut replacement = snapshot.value.clone();
    replacement["default"] = json!("alternate");
    settings
        .replace("rsi.agent-presets", &snapshot.version(), replacement)
        .await
        .unwrap();
    let selected = connection
        .session_service()
        .create(create("after-selection"))
        .await
        .unwrap();
    assert_eq!(
        selected.header().await.unwrap().agent_preset_id().as_str(),
        "alternate"
    );
    assert_eq!(original.header().await.unwrap(), header);
    drop(original);
    drop(selected);
    connection.shutdown().await.unwrap();
    assert!(runtime.shutdown().await.is_clean());
    let daemon = DaemonFixture::new(&fixture).await;
    let selected = daemon
        .connection
        .session_service()
        .create(create("after-restart-selection"))
        .await
        .unwrap();
    assert_eq!(
        selected.header().await.unwrap().agent_preset_id().as_str(),
        "alternate"
    );
    drop(selected);
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_provider_storage_closes_writes_until_durable_truth_is_reloaded() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let database = rusqlite::Connection::open(fixture.paths.state().join("base.sqlite3")).unwrap();
    database.execute_batch("CREATE TRIGGER fixture_reject_provider BEFORE INSERT ON rsi_storage_records WHEN NEW.domain = 'rsi.managed-providers' BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
    assert!(matches!(
        replace(&running, "0", vec![deployment("blocked")]).await,
        Err(ApiError::OutcomeUnknown)
    ));
    database
        .execute_batch("DROP TRIGGER fixture_reject_provider;")
        .unwrap();
    assert!(matches!(
        replace(&running, "0", vec![deployment("cannot-replay")]).await,
        Err(ApiError::OutcomeUnknown)
    ));
    let status = call(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Read,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(status.desired_revision, "0");
    assert!(status.diagnostic.unwrap().contains("unknown"));
    drop(database);
    assert!(running.shutdown().await.is_clean());
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    assert_eq!(
        replace(&running, "0", vec![deployment("reconciled")])
            .await
            .unwrap()
            .applied_revision,
        "1"
    );
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "One observable lifecycle with shared setup and assertions"
)]
async fn managed_provider_public_api_preflights_converges_and_reconstructs_without_touching_source()
{
    let fixture = fixture("http://127.0.0.1:1");
    let source = std::fs::read(&fixture.profile).unwrap();
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let initial = call(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Read,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(initial.desired_revision, "0");
    let devices = running.device_administration().unwrap();
    let device = devices.register("ungranted").await.unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    assert!(matches!(
        call(
            &running,
            origin,
            ProvidersOperation::Replace,
            json!({"expected_revision":"0","deployments":[deployment("managed")]})
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    for malformed in [
        {
            let mut value = deployment("managed");
            value.config["api_key"] = json!("must-never-persist");
            value
        },
        {
            let mut value = deployment("managed");
            value.config["endpoint"] = json!("http://user:secret@localhost");
            value
        },
        {
            let mut value = deployment("managed");
            value.config["credential"]["owner"] = json!("another.owner");
            value
        },
    ] {
        assert!(matches!(
            replace(&running, "0", vec![malformed]).await,
            Err(ApiError::Invalid(_))
        ));
        assert_eq!(
            call(
                &running,
                CallOrigin::Local,
                ProvidersOperation::Read,
                json!({})
            )
            .await
            .unwrap()
            .desired_revision,
            "0"
        );
    }
    let applied = replace(&running, "0", vec![deployment("managed")])
        .await
        .unwrap();
    assert_eq!(
        (&*applied.desired_revision, &*applied.applied_revision),
        ("1", "1")
    );
    assert!(!applied.applying);
    assert!(applied.diagnostic.is_none());
    assert_eq!(
        running
            .language_models()
            .unwrap()
            .list_models(None, 16)
            .await
            .unwrap()
            .models
            .len(),
        2
    );
    assert!(matches!(
        replace(&running, "0", vec![]).await,
        Err(ApiError::Invalid(_))
    ));
    let conflict = replace(&running, "1", vec![deployment("fixture")])
        .await
        .unwrap();
    assert_eq!(
        (&*conflict.desired_revision, &*conflict.applied_revision),
        ("2", "1")
    );
    assert!(conflict.diagnostic.is_some());
    assert_eq!(
        running
            .language_models()
            .unwrap()
            .list_models(None, 16)
            .await
            .unwrap()
            .models
            .len(),
        2,
        "rollback preserves managed and source routes"
    );
    assert!(running.shutdown().await.is_clean());
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let repaired = replace(&running, "2", vec![deployment("repaired")])
        .await
        .unwrap();
    assert_eq!(
        (&*repaired.desired_revision, &*repaired.applied_revision),
        ("3", "3")
    );
    assert!(running.shutdown().await.is_clean());
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let restored = call(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Read,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        (&*restored.desired_revision, &*restored.applied_revision),
        ("3", "3")
    );
    assert_eq!(
        running
            .language_models()
            .unwrap()
            .list_models(None, 16)
            .await
            .unwrap()
            .models
            .len(),
        2
    );
    let removed = replace(&running, "3", vec![]).await.unwrap();
    assert_eq!(removed.applied_revision, "4");
    assert_eq!(
        running
            .language_models()
            .unwrap()
            .list_models(None, 16)
            .await
            .unwrap()
            .models
            .len(),
        1
    );
    assert_eq!(std::fs::read(&fixture.profile).unwrap(), source);
    assert!(running.shutdown().await.is_clean());
}

#[derive(Default)]
struct MemorySecrets(Mutex<BTreeMap<rsi_credentials_protocol::CredentialRef, SecretValue>>);

#[derive(Debug)]
struct GatedFileSecrets {
    file: rsi_credentials_local::FileSecretStore,
    entered: tokio::sync::Notify,
    completed: tokio::sync::Notify,
    released: Mutex<bool>,
    gate: std::sync::Condvar,
    unknown: bool,
    writes: std::sync::atomic::AtomicUsize,
}
impl SecretStore for GatedFileSecrets {
    fn get(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
    ) -> CredentialResult<Option<SecretValue>> {
        self.file.get(reference)
    }
    fn set(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
        secret: &SecretValue,
    ) -> CredentialResult<()> {
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.gate.wait(released).unwrap();
        }
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.file.set(reference, secret)?;
        self.completed.notify_one();
        if self.unknown {
            Err(CredentialsError::OutcomeUnknown)
        } else {
            Ok(())
        }
    }
    fn unset(&self, reference: &rsi_credentials_protocol::CredentialRef) -> CredentialResult<bool> {
        self.file.unset(reference)
    }
    fn location(&self) -> Option<String> {
        self.file.location()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_credential_write_survives_lost_waiter_and_unknown_is_never_replayed() {
    use rsi_configuration_api::CredentialOperation;
    for unknown in [false, true] {
        let fixture = fixture("http://127.0.0.1:1");
        let store = Arc::new(GatedFileSecrets {
            file: rsi_credentials_local::FileSecretStore::new(
                fixture.paths.config().join("credentials/credentials.json"),
            ),
            entered: tokio::sync::Notify::new(),
            completed: tokio::sync::Notify::new(),
            released: Mutex::new(false),
            gate: std::sync::Condvar::new(),
            unknown,
            writes: std::sync::atomic::AtomicUsize::new(0),
        });
        let running = RunningRsi::boot_host_profile(
            composition(fixture.paths.clone()).with_credential_store(store.clone()),
            &host_profile(&fixture),
        )
        .await
        .unwrap();
        let spec = CredentialOperation::Set.spec();
        let request = ByteBudget::default()
            .encode(
                &json!({"provider":"deepseek","slot":"saved","secret":"fixture-private-value"}),
                spec.maximum_request_bytes,
            )
            .unwrap();
        let admitted = running
            .api_dispatch()
            .unwrap()
            .admit(&spec.id, CallOrigin::Local)
            .unwrap();
        let waiter = tokio::spawn(async move { admitted.invoke(request).await });
        store.entered.notified().await;
        if !unknown {
            waiter.abort();
        }
        *store.released.lock().unwrap() = true;
        store.gate.notify_all();
        store.completed.notified().await;
        if unknown {
            assert!(matches!(
                waiter.await.unwrap(),
                Err(ApiError::OutcomeUnknown)
            ));
        } else {
            assert!(waiter.await.unwrap_err().is_cancelled());
        }
        let status = wire(
            &running,
            CallOrigin::Local,
            CredentialOperation::Status.spec(),
            json!({"provider":"deepseek","slot":"saved"}),
        )
        .await
        .unwrap();
        assert_eq!(status["availability"]["source"]["kind"], "file");
        assert!(!status.to_string().contains("fixture-private-value"));
        assert_eq!(store.writes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(running.shutdown().await.is_clean());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn credential_file_failure_is_redacted_and_does_not_use_environment_or_claim_unknown_write() {
    use rsi_configuration_api::CredentialOperation;
    let fixture = fixture("http://127.0.0.1:1");
    let path = fixture.paths.config().join("credentials/credentials.json");
    let store = Arc::new(rsi_credentials_local::FileSecretStore::new(&path));
    store
        .set(
            &rsi_credentials_protocol::CredentialRef::new("fixture", "unused").unwrap(),
            &SecretValue::new("fixture").unwrap(),
        )
        .unwrap();
    std::fs::write(&path, "{ corrupt-file-secret-marker").unwrap();
    let running = RunningRsi::boot_host_profile(
        composition(fixture.paths.clone()).with_credential_store(store),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    let status = wire(
        &running,
        CallOrigin::Local,
        CredentialOperation::Status.spec(),
        json!({"provider":"openai-compatible","slot":"default"}),
    )
    .await
    .unwrap();
    assert_eq!(
        status["availability"],
        json!({"kind":"unavailable","reason":"corrupt"})
    );
    assert_eq!(status["editable"], false);
    for (operation, input) in [
        (
            CredentialOperation::Set,
            json!({"provider":"openai-compatible","slot":"default","secret":"new-secret-marker"}),
        ),
        (
            CredentialOperation::Unset,
            json!({"provider":"openai-compatible","slot":"default"}),
        ),
    ] {
        let error = wire(&running, CallOrigin::Local, operation.spec(), input)
            .await
            .unwrap_err();
        let ApiError::Domain(ref bytes) = error else {
            panic!("expected a determinate credential domain failure, got {error}");
        };
        assert_eq!(bytes.as_bytes(), br#""corrupt""#);
        assert!(!format!("{status} {error}").contains("secret-marker"));
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ corrupt-file-secret-marker"
    );
    assert!(running.shutdown().await.is_clean());
    let runtime = rsi_meta::Runtime::default();
    let connection = connect_or_embed_service_host(
        &runtime.root(),
        composition(fixture.paths.clone())
            .with_credential_store(Arc::new(rsi_credentials_local::FileSecretStore::new(&path))),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    let client =
        rsi_configuration_api::ProviderCredentialsClient::new(connection.api_client()).unwrap();
    let provider = ProviderKind::OpenaiCompatible;
    for error in [
        client
            .set(
                provider,
                "default",
                SecretValue::new("new-secret-marker").unwrap(),
            )
            .await
            .unwrap_err(),
        client.unset(provider, "default").await.unwrap_err(),
    ] {
        assert_eq!(
            error,
            ApiError::Backend(
                rsi_credentials_protocol::CredentialStoreFailure::Corrupt.to_string()
            )
        );
        assert!(!error.to_string().contains("secret-marker"));
    }
    connection.shutdown().await.unwrap();
    assert!(runtime.shutdown().await.is_clean());
}
impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MemorySecrets(<redacted>)")
    }
}
impl SecretStore for MemorySecrets {
    fn get(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
    ) -> CredentialResult<Option<SecretValue>> {
        Ok(self.0.lock().unwrap().get(reference).cloned())
    }
    fn set(
        &self,
        reference: &rsi_credentials_protocol::CredentialRef,
        secret: &SecretValue,
    ) -> CredentialResult<()> {
        self.0
            .lock()
            .unwrap()
            .insert(reference.clone(), secret.clone());
        Ok(())
    }
    fn unset(&self, reference: &rsi_credentials_protocol::CredentialRef) -> CredentialResult<bool> {
        Ok(self.0.lock().unwrap().remove(reference).is_some())
    }
}
async fn wire(
    running: &RunningRsi,
    origin: CallOrigin,
    spec: rsi_api_protocol::OperationSpec,
    input: Value,
) -> Result<Value, ApiError> {
    let input = ByteBudget::default().encode(&input, spec.maximum_request_bytes)?;
    let ApiOutput::Reply(output) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.id, origin)?
        .invoke(input)
        .await?
    else {
        panic!("unexpected stream")
    };
    Ok(serde_json::from_slice(output.json.as_bytes()).unwrap())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "One observable lifecycle with shared setup and assertions"
)]
async fn credential_configuration_uses_injected_store_and_grants_with_separate_redacted_receipts() {
    use rsi_configuration_api::{ConfigurationOperation, CredentialOperation};
    let fixture = fixture("http://127.0.0.1:1");
    let store = Arc::new(MemorySecrets::default());
    let running = RunningRsi::boot_host_profile(
        composition(fixture.paths.clone()).with_credential_store(store.clone()),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("configuration")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let reference = json!({"provider":"deepseek","slot":"managed"});
    let mut request = reference.clone();
    request["secret"] = json!("fixture-private-secret");
    assert!(matches!(
        wire(
            &running,
            origin.clone(),
            CredentialOperation::Set.spec(),
            request.clone()
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert!(store.0.lock().unwrap().is_empty());
    wire(
        &running,
        CallOrigin::Local,
        ConfigurationOperation::SetGrant.spec(),
        json!({"device":device.record.id,"expected_revision":"0","granted":true}),
    )
    .await
    .unwrap();
    let status = wire(
        &running,
        origin.clone(),
        CredentialOperation::Status.spec(),
        reference.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        status,
        json!({"availability":{"kind":"missing"},"editable":true,"store_path":null})
    );
    let receipt = wire(
        &running,
        origin.clone(),
        CredentialOperation::Set.spec(),
        request,
    )
    .await
    .unwrap();
    assert_eq!(receipt, json!({"operation":"set","removed":null}));
    assert_eq!(
        store
            .0
            .lock()
            .unwrap()
            .get(
                &rsi_credentials_protocol::CredentialRef::new(
                    "rsi.ai.provider.deepseek",
                    "managed"
                )
                .unwrap()
            )
            .unwrap()
            .expose_secret(),
        "fixture-private-secret"
    );
    let status = wire(
        &running,
        origin.clone(),
        CredentialOperation::Status.spec(),
        reference.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        status,
        json!({"availability":{"kind":"configured","source":{"kind":"file"}},"editable":true,"store_path":null})
    );
    assert_eq!(
        call(
            &running,
            CallOrigin::Local,
            ProvidersOperation::Read,
            json!({})
        )
        .await
        .unwrap()
        .desired_revision,
        "0",
        "credential write cannot apply providers"
    );
    let mut shadowed = json!({"provider":"openai-compatible","slot":"default"});
    let status = wire(
        &running,
        origin.clone(),
        CredentialOperation::Status.spec(),
        shadowed.clone(),
    )
    .await
    .unwrap();
    assert_eq!(status["editable"], true);
    shadowed["secret"] = json!("replacement-environment-key");
    wire(
        &running,
        origin.clone(),
        CredentialOperation::Set.spec(),
        shadowed,
    )
    .await
    .unwrap();
    let receipt = wire(
        &running,
        origin.clone(),
        CredentialOperation::Unset.spec(),
        reference.clone(),
    )
    .await
    .unwrap();
    assert_eq!(receipt, json!({"operation":"unset","removed":true}));
    wire(
        &running,
        CallOrigin::Local,
        ConfigurationOperation::SetGrant.spec(),
        json!({"device":device.record.id,"expected_revision":"1","granted":false}),
    )
    .await
    .unwrap();
    assert!(matches!(
        wire(
            &running,
            origin,
            CredentialOperation::Unset.spec(),
            reference
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_local_api_client_follows_owned_and_borrowed_service_lifetimes() {
    use rsi_configuration_api::{ConfigurationClient, ManagedProvidersClient};
    let fixture = fixture("http://127.0.0.1:1");
    let runtime = rsi_meta::Runtime::default();
    let connection = connect_or_embed_service_host(
        &runtime.root(),
        composition(fixture.paths.clone()),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    assert_eq!(connection.mode(), ServiceHostConnectionMode::Embedded);
    let api = connection.api_client();
    assert!(
        ConfigurationClient::new(api.clone())
            .unwrap()
            .allowed()
            .await
            .unwrap()
    );
    let providers = ManagedProvidersClient::new(api).unwrap();
    assert_eq!(
        providers
            .replace("0", vec![deployment("native")])
            .await
            .unwrap()
            .applied_revision,
        "1"
    );
    connection.shutdown().await.unwrap();
    assert!(providers.read().await.is_err());
    assert!(runtime.shutdown().await.is_clean());
    let daemon = DaemonFixture::new(&fixture).await;
    let providers = ManagedProvidersClient::new(daemon.connection.api_client()).unwrap();
    assert_eq!(providers.read().await.unwrap().applied_revision, "1");
    daemon.connection.shutdown().await.unwrap();
    assert_eq!(
        daemon
            .running
            .language_models()
            .unwrap()
            .list_models(None, 16)
            .await
            .unwrap()
            .models
            .len(),
        2,
        "detach must preserve the daemon"
    );
    assert!(daemon.client_runtime.shutdown().await.is_clean());
    daemon.stop.cancel();
    daemon.task.await.unwrap().unwrap();
    assert!(daemon.running.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_durable_providers_fail_boot_and_release_the_host_owner() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    replace(&running, "0", vec![deployment("saved")])
        .await
        .unwrap();
    assert!(running.shutdown().await.is_clean());
    let database = rusqlite::Connection::open(fixture.paths.state().join("base.sqlite3")).unwrap();
    let original: Vec<u8> = database.query_row("SELECT value FROM rsi_storage_records WHERE domain = 'rsi.managed-providers' AND key = 'desired'", [], |row| row.get(0)).unwrap();
    for document in [
        json!({"revision":1,"deployments":[],"unexpected":true}),
        json!({"revision":1,"deployments":[{"provider":"deepseek","config":{"deployment":"invalid"}}]}),
        json!({"revision":1,"deployments":vec![deployment("duplicate");65]}),
    ] {
        database.execute("UPDATE rsi_storage_records SET value = ?1 WHERE domain = 'rsi.managed-providers' AND key = 'desired'", [document.to_string().into_bytes()]).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            RunningRsi::boot_host_profile(
                composition(fixture.paths.clone()),
                &host_profile(&fixture),
            ),
        )
        .await
        .expect("failed owner must reject promptly");
        assert!(
            result.is_err(),
            "malformed durable provider state became ready"
        );
        let paths = ServiceHostPaths::from_host_paths(&fixture.paths).unwrap();
        drop(HostOwnerLease::try_acquire(paths).expect("failed boot leaked the owner lease"));
    }
    database.execute("UPDATE rsi_storage_records SET value = ?1 WHERE domain = 'rsi.managed-providers' AND key = 'desired'", [original]).unwrap();
    let repaired =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    assert_eq!(
        call(
            &repaired,
            CallOrigin::Local,
            ProvidersOperation::Read,
            json!({})
        )
        .await
        .unwrap()
        .applied_revision,
        "1"
    );
    assert!(repaired.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "One lifecycle compares authority, read-only discovery and bounded HTTP failures"
)]
async fn discovery_is_authorized_bounded_and_does_not_publish_routes() {
    use rsi_configuration_api::CredentialOperation;
    let (endpoint, provider) = discovery_provider().await;
    let fixture = fixture(&endpoint);
    let store = Arc::new(MemorySecrets::default());
    let running = RunningRsi::boot_host_profile(
        composition(fixture.paths.clone()).with_credential_store(store),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    wire(
        &running,
        CallOrigin::Local,
        CredentialOperation::Set.spec(),
        json!({"provider":"deepseek","slot":"discovery","secret":"discovery-private-secret"}),
    )
    .await
    .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("discovery-without-grant")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let request = json!({"provider":"deepseek","endpoint":endpoint,"slot":"discovery"});
    assert!(matches!(
        wire(
            &running,
            origin.clone(),
            ProvidersOperation::Discover.spec(),
            request.clone()
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    let mut malformed = request.clone();
    malformed["endpoint"] = json!("invalid-url");
    assert!(matches!(
        wire(
            &running,
            origin,
            ProvidersOperation::Discover.spec(),
            malformed
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    let before = call(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Read,
        json!({}),
    )
    .await
    .unwrap();
    let result = wire(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Discover.spec(),
        request.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result["models"][0]["id"], "candidate-only");
    assert_eq!(result["models"][0]["context_window_tokens"], Value::Null);
    assert!(!result.to_string().contains("discovery-private-secret"));
    let equal = wire(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Discover.spec(),
        json!({"provider":"deepseek", "endpoint":format!("{endpoint}/equal"), "slot":"discovery"}),
    )
    .await
    .unwrap();
    assert_eq!(equal["models"][0]["context_window_tokens"], 10);
    assert_eq!(equal["models"][0]["max_output_tokens"], 10);
    for provider in ["openai", "deepseek", "openai-compatible"] {
        wire(
            &running,
            CallOrigin::Local,
            CredentialOperation::Set.spec(),
            json!({"provider":provider,"slot":"discovery","secret":"discovery-private-secret"}),
        )
        .await
        .unwrap();
        for suffix in ["", "/", "/v1", "/v1/"] {
            let discovered = wire(&running, CallOrigin::Local, ProvidersOperation::Discover.spec(), json!({"provider":provider,"endpoint":format!("{endpoint}{suffix}"),"slot":"discovery"})).await.unwrap();
            assert_eq!(
                discovered["models"][0]["id"], "candidate-only",
                "{provider} {suffix}"
            );
        }
    }
    let after = call(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Read,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(before, after);
    assert!(
        !running
            .language_models()
            .unwrap()
            .list_models(None, 128)
            .await
            .unwrap()
            .models
            .iter()
            .any(|model| model.model() == "candidate-only")
    );
    for path in [
        "unauthorized",
        "forbidden",
        "redirect",
        "invalid",
        "oversized",
        "paginated",
        "unavailable",
        "rate-limited",
    ] {
        let mut request = request.clone();
        request["endpoint"] = json!(format!("{endpoint}/{path}"));
        let error = wire(
            &running,
            CallOrigin::Local,
            ProvidersOperation::Discover.spec(),
            request,
        )
        .await
        .unwrap_err();
        match path {
            "rate-limited" => assert!(matches!(error, ApiError::Capacity), "{path}: {error}"),
            "unavailable" | "invalid" | "oversized" | "paginated" => {
                assert!(matches!(error, ApiError::Backend(_)), "{path}: {error}");
            }
            _ => assert!(matches!(error, ApiError::Invalid(_)), "{path}: {error}"),
        }
        assert!(!error.to_string().contains("private-upstream-diagnostic"));
    }
    let mut expanded = request;
    expanded["endpoint"] = json!(format!("{endpoint}/expanded"));
    assert_eq!(
        wire(
            &running,
            CallOrigin::Local,
            ProvidersOperation::Discover.spec(),
            expanded
        )
        .await
        .unwrap_err(),
        ApiError::Backend("domain result encoding failed".into())
    );
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_required_is_typed_for_embedded_and_daemon_clients() {
    for remote in [false, true] {
        let fixture = fixture("http://127.0.0.1:1");
        std::fs::remove_file(fixture.paths.config().join("settings.json")).unwrap();
        let daemon = if remote {
            Some(DaemonFixture::new(&fixture).await)
        } else {
            None
        };
        let runtime = rsi_meta::Runtime::default();
        let connection = connect_or_embed_service_host(
            &runtime.root(),
            composition(fixture.paths.clone()),
            &host_profile(&fixture),
        )
        .await
        .unwrap();
        let workspace = connection
            .workspace_registry()
            .get_or_create(&fixture.workspace)
            .await
            .unwrap();
        let error = connection
            .session_service()
            .create(CreateSession {
                session_id: SessionId::new("unconfigured").unwrap(),
                workspace_id: workspace.id,
                agent_preset_id: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, SessionError::SetupRequired), "{error}");
        connection.shutdown().await.unwrap();
        assert!(runtime.shutdown().await.is_clean());
        if let Some(daemon) = daemon {
            daemon.shutdown().await;
        }
    }
}

async fn discovery_provider() -> (String, tokio::task::JoinHandle<()>) {
    async fn candidates(headers: axum::http::HeaderMap) -> axum::Json<Value> {
        assert_eq!(headers["authorization"], "Bearer discovery-private-secret");
        axum::Json(json!({"data":[{"id":"candidate-only"}]}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/models", axum::routing::get(candidates))
        .route("/v1/models", axum::routing::get(candidates))
        .route("/equal/models", axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"equal-capacity", "context_window_tokens":10, "max_output_tokens":10}]})) }))
        .route("/expanded/models", axum::routing::get(|| async {
            let rows: Vec<_> = (0..4096).map(|i| json!({"id":format!("{}{i:04}", "a".repeat(251)), "name":format!("{}{}", "\0".repeat(93), "n".repeat(163))})).collect();
            let models: Vec<rsi_ai_protocol::DiscoveredModel> = rows.iter().map(|row| serde_json::from_value(row.clone()).unwrap()).collect();
            rsi_ai_protocol::validate_discovered_models(&models).unwrap();
            assert!(serde_json::to_vec(&models).unwrap().len() > rsi_ai_protocol::MAX_DISCOVERY_BYTES);
            let body = json!({"data":rows});
            assert!(serde_json::to_vec(&body).unwrap().len() < rsi_ai_protocol::MAX_DISCOVERY_BYTES);
            axum::Json(body)
        }))
        .route(
            "/paginated/models",
            axum::routing::get(|| async {
                axum::Json(json!({"data":[{"id":"private-upstream-diagnostic"}], "has_more":true}))
            }),
        )
        .route(
            "/unauthorized/models",
            axum::routing::get(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "private-upstream-diagnostic",
                )
            }),
        )
        .route("/unavailable/models", axum::routing::get(|| async { (axum::http::StatusCode::SERVICE_UNAVAILABLE, "private-upstream-diagnostic") }))
        .route("/rate-limited/models", axum::routing::get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }))
        .route(
            "/forbidden/models",
            axum::routing::get(|| async { axum::http::StatusCode::FORBIDDEN }),
        )
        .route(
            "/redirect/models",
            axum::routing::get(|| async { axum::response::Redirect::temporary("/models") }),
        )
        .route(
            "/invalid/models",
            axum::routing::get(|| async { "malformed-json" }),
        )
        .route(
            "/oversized/models",
            axum::routing::get(|| async { "x".repeat(4 * 1024 * 1024 + 1) }),
        );
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, provider)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_shutdown_cancels_discovery_before_draining_configuration_leases() {
    use rsi_configuration_api::CredentialOperation;
    let entered = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new().route(
        "/models",
        axum::routing::get({
            let entered = entered.clone();
            move || {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<axum::http::StatusCode>().await
                }
            }
        }),
    );
    let provider = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let fixture = fixture(&endpoint);
    let running = RunningRsi::boot_host_profile(
        composition(fixture.paths.clone())
            .with_credential_store(Arc::new(MemorySecrets::default())),
        &host_profile(&fixture),
    )
    .await
    .unwrap();
    wire(
        &running,
        CallOrigin::Local,
        CredentialOperation::Set.spec(),
        json!({"provider":"deepseek","slot":"discovery","secret":"shutdown-test-key"}),
    )
    .await
    .unwrap();
    let mut read = Box::pin(wire(
        &running,
        CallOrigin::Local,
        ProvidersOperation::Discover.spec(),
        json!({"provider":"deepseek","endpoint":endpoint,"slot":"discovery"}),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = &mut read => panic!("discovery finished before its HTTP gate: {result:?}"),
            () = entered.notified() => {},
        }
        let (report, result) = tokio::join!(running.shutdown(), read);
        assert!(report.is_clean(), "{report:?}");
        assert!(
            result.is_err(),
            "retirement must cancel the pending discovery"
        );
    })
    .await
    .expect("shutdown must cancel discovery without waiting for its 30-second deadline");
    provider.abort();
}
