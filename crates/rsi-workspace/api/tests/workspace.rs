use async_trait::async_trait;
use rsi_api_http::{HttpConfig, HttpServer, HttpServices};
use rsi_api_http_client::{HttpClient, HttpClientConfig};
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiDispatchContract, ApiError, ApiRegistrarContract,
    AuthenticatedDevice, DeviceAuthentication, DeviceId, EndpointId, HostEpoch,
};
use rsi_credentials_protocol::{CredentialRef, SecretValue};
use rsi_meta::{
    ActivationPlan, PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_workspace_api::{WorkspaceApiFactory, WorkspaceClientFactory};
use rsi_workspace_protocol::{WorkspaceError, WorkspaceRegistryContract, WorkspaceStatus};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[derive(Debug)]
struct Auth;
impl DeviceAuthentication for Auth {
    fn authenticate(&self, token: &SecretValue) -> rsi_api_protocol::Result<AuthenticatedDevice> {
        if token.expose_secret() != TOKEN {
            return Err(ApiError::Unauthorized);
        }
        Ok(AuthenticatedDevice {
            id: DeviceId::from_bytes([1; 16]),
            revoked: CancellationToken::new(),
        })
    }
}
fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}
#[derive(Debug)]
struct ClientProvider(Arc<HttpClient>);
#[async_trait]
impl PluginFactory for ClientProvider {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ApiClientContract>(self.0.clone())?;
        let client = self.0.clone();
        plan.defer(
            "close fixture client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    client.close().await;
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn actual_workspace_storage_and_domain_plugins_work_over_http_without_session() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("one");
    let second = temporary.path().join("two");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    std::fs::write(first.join("keep.txt"), b"keep").unwrap();
    let server_runtime = Runtime::default();
    let (server, endpoint, config, connection) =
        server_setup(&server_runtime, temporary.path()).await;
    let stop = CancellationToken::new();
    let task = tokio::spawn(server.serve(stop.clone()));
    let client_runtime = Runtime::default();
    let api = Arc::new(
        HttpClient::connect(
            client_runtime.execution().clone(),
            config,
            SecretValue::new(TOKEN).unwrap(),
        )
        .await
        .unwrap(),
    );
    client_runtime
        .root()
        .apply(
            linked("connection", Arc::new(ClientProvider(api.clone()))),
            Value::Null,
        )
        .await
        .unwrap();
    let client_plugin = client_runtime
        .root()
        .apply(
            linked("workspace-client", Arc::new(WorkspaceClientFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let workspace = client_runtime
        .root()
        .lookup_local::<WorkspaceRegistryContract>()
        .unwrap();
    exercise_workspace(workspace.as_ref(), api.as_ref(), &first, &second).await;
    client_plugin.dispose().await;
    assert!(
        client_runtime
            .root()
            .lookup_local::<WorkspaceRegistryContract>()
            .is_none()
    );
    assert!(client_runtime.shutdown().await.is_clean());
    endpoint.dispose().await;
    assert!(
        !server_runtime
            .root()
            .lookup_local::<ApiDispatchContract>()
            .unwrap()
            .operations()
            .iter()
            .any(|operation| operation.id.domain() == "workspace")
    );
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    connection.close().await;
    assert!(server_runtime.shutdown().await.is_clean());
}

async fn server_setup(
    server_runtime: &Runtime,
    directory: &std::path::Path,
) -> (
    HttpServer,
    rsi_meta::FiberHandle,
    HttpClientConfig,
    rsi_api::ConnectionApi,
) {
    for (name, factory, config) in [
        (
            "storage",
            Arc::new(rsi_storage::StorageFactory) as Arc<dyn PluginFactory>,
            Value::Null,
        ),
        (
            "json",
            Arc::new(rsi_storage_json::JsonStorageFactory),
            json!({"name":"json","path":directory.join("domains.json")}),
        ),
        (
            "domains",
            Arc::new(rsi_storage_domain::DomainFactory),
            Value::Null,
        ),
        (
            "workspace",
            Arc::new(rsi_workspace::WorkspaceFactory),
            json!({"backend":"json"}),
        ),
        ("api", Arc::new(rsi_api::ApiFactory), Value::Null),
    ] {
        server_runtime
            .root()
            .apply(linked(name, factory), config)
            .await
            .unwrap();
    }
    let endpoint = server_runtime
        .root()
        .apply(
            linked("workspace-api", Arc::new(WorkspaceApiFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let endpoint_id = EndpointId::from_bytes([2; 16]);
    let connection = rsi_api::ConnectionApi::register(
        server_runtime
            .root()
            .lookup_local::<ApiDispatchContract>()
            .unwrap(),
        server_runtime
            .root()
            .lookup_local::<ApiRegistrarContract>()
            .unwrap()
            .as_ref(),
        endpoint_id.clone(),
        HostEpoch::from_bytes([3; 16]),
    )
    .unwrap();
    let server = HttpServer::from_listener(
        server_runtime.execution().clone(),
        listener,
        HttpConfig {
            bind: address,
            public_origin: origin.clone(),
            tls: None,
            allow_loopback_http: true,
        },
        HttpServices {
            dispatch: server_runtime
                .root()
                .lookup_local::<ApiDispatchContract>()
                .unwrap(),
            authentication: Arc::new(Auth),
            endpoint: endpoint_id.clone(),
            epoch: HostEpoch::from_bytes([3; 16]),
        },
    )
    .await
    .unwrap();
    let config = HttpClientConfig {
        origin,
        endpoint_id,
        credential: CredentialRef::new("test", "device").unwrap(),
        tls_ca: None,
        allow_loopback_http: true,
    };
    (server, endpoint, config, connection)
}

async fn exercise_workspace(
    workspace: &dyn rsi_workspace_protocol::WorkspaceRegistry,
    api: &dyn ApiClient,
    first: &std::path::Path,
    second: &std::path::Path,
) {
    let one = workspace.get_or_create(first).await.unwrap();
    assert_eq!(
        workspace.get_or_create(&first.join(".")).await.unwrap(),
        one
    );
    let two = workspace.get_or_create(second).await.unwrap();
    assert_eq!(workspace.get(&one.id).await.unwrap(), one);
    assert_eq!(
        workspace.status(&one.id).await.unwrap(),
        WorkspaceStatus::Ok
    );
    let page = workspace.list(None, 1).await.unwrap();
    assert_eq!(page.records, vec![one.clone()]);
    assert!(workspace.list(None, 257).await.is_err());
    assert!(workspace.delete_registration(&one.id).await.unwrap());
    assert_eq!(
        workspace.list(page.next, 1).await.unwrap().records,
        vec![two.clone()]
    );
    assert!(
        matches!(workspace.get(&one.id).await, Err(WorkspaceError::Unknown(id)) if id == one.id)
    );
    assert_eq!(std::fs::read(first.join("keep.txt")).unwrap(), b"keep");
    std::fs::remove_dir(second).unwrap();
    assert_eq!(
        workspace.status(&two.id).await.unwrap(),
        WorkspaceStatus::MissingDirectory
    );
    assert_eq!(workspace.get(&two.id).await.unwrap(), two);

    // The same advertised operation rejects a foreign field through its owning DTO.
    let operation = api
        .operations()
        .iter()
        .find(|operation| operation.id.name() == "register")
        .unwrap();
    let input = api
        .input_budget(operation.class)
        .encode(
            &json!({"path":first,"extra":true}),
            operation.maximum_request_bytes,
        )
        .unwrap();
    assert!(matches!(
        api.call(operation, input).await,
        Err(ApiError::Invalid(_))
    ));
    assert!(matches!(
        workspace
            .get_or_create(std::path::Path::new("relative"))
            .await,
        Err(WorkspaceError::InvalidInput(_))
    ));
}
