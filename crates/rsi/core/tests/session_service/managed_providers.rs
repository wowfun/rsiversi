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
        workspace_trust: WorkspaceTrust::Untrusted,
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
struct MemorySecrets(Mutex<BTreeMap<String, SecretValue>>);
impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MemorySecrets(<redacted>)")
    }
}
impl SecretStore for MemorySecrets {
    fn get(&self, _: &str, account: &str) -> CredentialResult<Option<SecretValue>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, _: &str, account: &str, secret: &SecretValue) -> CredentialResult<()> {
        self.0
            .lock()
            .unwrap()
            .insert(account.into(), secret.clone());
        Ok(())
    }
    fn unset(&self, _: &str, account: &str) -> CredentialResult<bool> {
        Ok(self.0.lock().unwrap().remove(account).is_some())
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
async fn credential_configuration_uses_real_store_and_grants_with_separate_redacted_receipts() {
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
        json!({"availability":{"kind":"missing"},"editable":true})
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
            .get("rsi.ai.provider.deepseek/managed")
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
        json!({"availability":{"kind":"configured","source":{"kind":"keyring"}},"editable":true})
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
    assert_eq!(status["editable"], false);
    shadowed["secret"] = json!("must-not-write");
    assert!(matches!(
        wire(
            &running,
            origin.clone(),
            CredentialOperation::Set.spec(),
            shadowed
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
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
