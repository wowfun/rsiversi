use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiDispatch, ApiDispatchContract, ApiError, ApiOutput,
    ByteBudget, CallOrigin, ConnectionDescription, EndpointId, HostEpoch, OperationClass,
    OperationSpec, RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_settings_api::{SettingsApiFactory, SettingsClientFactory};
use rsi_settings_protocol::*;
use serde_json::{Value, json};
use std::sync::Arc;

fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}
#[derive(Debug)]
struct Connection {
    dispatch: Arc<dyn ApiDispatch>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    corrupt: Option<Corruption>,
}
#[derive(Clone, Copy, Debug)]
enum Corruption {
    Revision,
    Scope,
    Section,
}
#[async_trait]
impl ApiClient for Connection {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let output = self
            .dispatch
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await?;
        if let Some(corrupt) = self.corrupt {
            let ApiOutput::Reply(mut message) = output else {
                panic!("finite Settings reply")
            };
            let mut snapshot: SettingsSnapshot =
                serde_json::from_slice(message.json.as_bytes()).unwrap();
            match corrupt {
                Corruption::Revision => snapshot.revision += 2,
                Corruption::Scope => {
                    let mut id = snapshot.scope_id.as_str().to_owned();
                    id.replace_range(..1, if id.starts_with('0') { "1" } else { "0" });
                    snapshot.scope_id = SettingsScopeId::parse(id).unwrap();
                }
                Corruption::Section => {
                    snapshot.value = json!("x".repeat(MAXIMUM_SETTINGS_SECTION_BYTES + 1));
                }
            }
            message.json = ByteBudget::default()
                .encode(&snapshot, operation.maximum_response_bytes)
                .unwrap();
            return Ok(ApiOutput::Reply(message));
        }
        Ok(output)
    }
}
#[derive(Debug)]
struct ConnectionFactory(Arc<Connection>);
#[async_trait]
impl PluginFactory for ConnectionFactory {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ApiClientContract>(self.0.clone())?;
        plan.defer(
            "withdraw fixture connection",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
fn spec() -> SettingsSpec {
    SettingsSpec {
        namespace: "ui".into(),
        defaults: json!({"enabled":true}),
        base: json!({}),
        validator: Arc::new(ValidateWith(|value: &Value| {
            if value.get("enabled").is_some_and(Value::is_boolean) {
                Ok(())
            } else {
                Err(SettingsError::InvalidInput(
                    "enabled must be boolean".into(),
                ))
            }
        })),
    }
}
async fn server(path: &std::path::Path) -> Runtime {
    let runtime = Runtime::default();
    for (name, factory, config) in [
        (
            "local",
            Arc::new(rsi_settings_local::LocalSettingsFactory) as Arc<dyn PluginFactory>,
            json!({"path":path}),
        ),
        (
            "settings",
            Arc::new(rsi_settings::SettingsFactory),
            Value::Null,
        ),
        ("api", Arc::new(rsi_api::ApiFactory), Value::Null),
        ("settings-api", Arc::new(SettingsApiFactory), Value::Null),
    ] {
        runtime
            .root()
            .apply(linked(name, factory), config)
            .await
            .unwrap();
    }
    runtime
}

async fn client_composition(dispatch: Arc<dyn ApiDispatch>) -> (Runtime, rsi_meta::FiberHandle) {
    let connection = Arc::new(Connection {
        operations: dispatch.operations(),
        dispatch: dispatch.clone(),
        corrupt: None,
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
    });
    let client = Runtime::default();
    client
        .root()
        .apply(
            linked("connection", Arc::new(ConnectionFactory(connection))),
            Value::Null,
        )
        .await
        .unwrap();
    let plugin = client
        .root()
        .apply(
            linked("settings-client", Arc::new(SettingsClientFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    (client, plugin)
}

#[tokio::test]
async fn native_file_and_namespace_api_preserve_cas_last_good_state_and_scope_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("settings.json");
    std::fs::write(&path, br#"{"hidden":{"private":"retained"}}"#).unwrap();
    let server = server(&path).await;
    let settings = server.root().lookup_local::<SettingsContract>().unwrap();
    let registration = settings.register(spec()).unwrap();
    let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
    let (client, plugin) = client_composition(dispatch.clone()).await;
    assert!(client.root().lookup_local::<SettingsContract>().is_none());
    assert!(
        client
            .root()
            .lookup_local::<SettingsProviderContract>()
            .is_none()
    );
    let access = client
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    assert!(
        matches!(access.read("hidden").await, Err(SettingsError::UnknownNamespace(namespace)) if namespace == "hidden")
    );
    let initial = access.read("ui").await.unwrap();
    assert_eq!(initial.revision, 0);
    let updated = access
        .replace(
            "ui",
            &initial.version(),
            json!({"enabled":false,"large":u64::MAX}),
        )
        .await
        .unwrap();
    assert_eq!(updated.scope_id, initial.scope_id);
    assert_eq!(updated.revision, 1);
    assert_eq!(updated.value["large"].as_u64(), Some(u64::MAX));
    assert!(matches!(
        access.clear("ui", &initial.version()).await,
        Err(SettingsError::Conflict {
            expected: 0,
            actual: 1
        })
    ));
    assert!(matches!(
        access
            .replace("ui", &updated.version(), json!({"enabled":"wrong"}))
            .await,
        Err(SettingsError::InvalidInput(_))
    ));
    assert_eq!(access.read("ui").await.unwrap(), updated);
    let cleared = access.clear("ui", &updated.version()).await.unwrap();
    assert_eq!(cleared.value, json!({"enabled":true}));
    assert_eq!(cleared.revision, 2);
    drop(registration.lease);
    assert!(matches!(
        access.read("ui").await,
        Err(SettingsError::UnknownNamespace(_))
    ));
    let replacement = settings.register(spec()).unwrap();
    let fresh = access.read("ui").await.unwrap();
    assert_ne!(fresh.scope_id, initial.scope_id);
    assert_eq!(fresh.revision, 0);
    assert!(matches!(
        access.clear("ui", &initial.version()).await,
        Err(SettingsError::StaleRegistration(_))
    ));
    access
        .replace("ui", &fresh.version(), json!({"enabled":false}))
        .await
        .unwrap();
    let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(document["hidden"], json!({"private":"retained"}));
    let operation = dispatch
        .operations()
        .into_iter()
        .find(|operation| operation.id.name() == "read")
        .unwrap();
    let invocation = dispatch.admit(&operation.id, CallOrigin::Local).unwrap();
    let input = invocation
        .input_budget()
        .copy(br#"{"namespace":"ui","raw":true}"#)
        .unwrap();
    assert!(matches!(
        invocation.invoke(input).await,
        Err(ApiError::Invalid(_))
    ));
    drop(replacement.lease);
    plugin.dispose().await;
    assert!(
        client
            .root()
            .lookup_local::<SettingsAccessContract>()
            .is_none()
    );
    assert!(client.shutdown().await.is_clean());
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test]
async fn malformed_success_after_real_file_commit_is_unknown_and_the_write_remains_visible() {
    for corrupt in [Corruption::Revision, Corruption::Scope, Corruption::Section] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("settings.json");
        let server = server(&path).await;
        let settings = server.root().lookup_local::<SettingsContract>().unwrap();
        let registration = settings.register(spec()).unwrap();
        let initial = registration.scope.get().unwrap();
        let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
        let connection = Arc::new(Connection {
            operations: dispatch.operations(),
            dispatch,
            corrupt: Some(corrupt),
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
        });
        let client = rsi_settings_api::SettingsClient::new(connection).unwrap();
        assert!(matches!(
            client
                .replace("ui", &initial.version(), json!({"enabled":false}))
                .await,
            Err(SettingsError::Api(ApiError::OutcomeUnknown))
        ));
        let current = registration.scope.get().unwrap();
        assert_eq!(current.revision, 1);
        assert_eq!(current.value, json!({"enabled":false}));
        let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["ui"], current.value);
        drop(registration.lease);
        assert!(server.shutdown().await.is_clean());
    }
}
