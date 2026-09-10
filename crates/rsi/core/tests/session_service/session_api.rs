use super::{KEY, composition, fixture, provider};
use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{SessionId, WorkspaceTrust};
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
use rsi_session_protocol::{CreateSession, SessionContract, SessionIngressContract};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn linked(name: &str, factory: impl PluginFactory) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, Arc::new(factory))
}
#[derive(Debug)]
struct HostCapabilities {
    session: Arc<dyn rsi_session_protocol::SessionService>,
    ingress: Arc<dyn rsi_session_protocol::SessionIngress>,
}
#[async_trait]
impl PluginFactory for HostCapabilities {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let session = plan
            .context()
            .provide_local::<SessionContract>(self.session.clone())?;
        let ingress = plan
            .context()
            .provide_local::<SessionIngressContract>(self.ingress.clone())?;
        plan.defer(
            "withdraw fixture host capabilities",
            Box::new(move || {
                Box::pin(async move {
                    drop((session, ingress));
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct ClientCapability(Arc<HttpClient>);
#[async_trait]
impl PluginFactory for ClientCapability {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ApiClientContract>(self.0.clone())?;
        let client = self.0.clone();
        plan.defer(
            "close fixture connection",
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
#[derive(Debug)]
struct Auth;
const SECOND_KEY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
impl DeviceAuthentication for Auth {
    fn authenticate(&self, secret: &SecretValue) -> rsi_api_protocol::Result<AuthenticatedDevice> {
        let id = match secret.expose_secret() {
            KEY => 1,
            SECOND_KEY => 2,
            _ => return Err(ApiError::Unauthorized),
        };
        Ok(AuthenticatedDevice {
            id: DeviceId::from_bytes([id; 16]),
            revoked: CancellationToken::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)] // Keep server/client ownership and their ordered retirement in one scenario.
async fn http_session_plugins_pass_the_same_real_kernel_store_scenario() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let host = composition(fixture.paths.clone())
        .build()
        .unwrap()
        .start_file(&fixture.profile)
        .await
        .unwrap();
    let workspace = host
        .lookup_local::<rsi_workspace_protocol::WorkspaceRegistryContract>()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let server_runtime = Runtime::default();
    server_runtime
        .root()
        .apply(
            linked(
                "host-capabilities",
                HostCapabilities {
                    session: host.lookup_local::<SessionContract>().unwrap(),
                    ingress: host.lookup_local::<SessionIngressContract>().unwrap(),
                },
            ),
            Value::Null,
        )
        .await
        .unwrap();
    server_runtime
        .root()
        .apply(linked("api", rsi_api::ApiFactory), Value::Null)
        .await
        .unwrap();
    let endpoints = server_runtime
        .root()
        .apply(
            linked("session-api", rsi_session_api::SessionApiFactory),
            Value::Null,
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = listener.local_addr().unwrap();
    let origin = format!("http://{bind}");
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
            bind,
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
    let stop = CancellationToken::new();
    let task = tokio::spawn(server.serve(stop.clone()));
    let client_runtime = Runtime::default();
    let api = Arc::new(
        HttpClient::connect(
            client_runtime.execution().clone(),
            HttpClientConfig {
                origin: origin.clone(),
                endpoint_id: endpoint_id.clone(),
                credential: CredentialRef::new("test", "device").unwrap(),
                tls_ca: None,
                allow_loopback_http: true,
            },
            SecretValue::new(KEY).unwrap(),
        )
        .await
        .unwrap(),
    );
    client_runtime
        .root()
        .apply(
            linked("connection", ClientCapability(api.clone())),
            Value::Null,
        )
        .await
        .unwrap();
    let client_plugin = client_runtime
        .root()
        .apply(
            linked("session-client", rsi_session_api::SessionClientFactory),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        client_runtime
            .root()
            .lookup_local::<SessionIngressContract>()
            .is_none()
    );
    let session = client_runtime
        .root()
        .lookup_local::<SessionContract>()
        .unwrap();
    let create = CreateSession {
        workspace_id: workspace.id,
        session_id: SessionId::new("http-contract").unwrap(),
        agent_preset_id: None,
        workspace_trust: WorkspaceTrust::Untrusted,
    };
    let local = host
        .lookup_local::<SessionContract>()
        .unwrap()
        .create(create.clone())
        .await
        .unwrap();
    assert_eq!(
        session
            .create(create.clone())
            .await
            .unwrap()
            .header()
            .await
            .unwrap(),
        local.header().await.unwrap()
    );
    rsi_session_testkit::assert_session_contract(
        session.clone(),
        create.clone(),
        workspace.path.to_str().unwrap(),
        client_runtime.execution(),
    )
    .await;
    let handle = session.attach(&create.session_id).await.unwrap();
    let inspection = handle.inspect().await.unwrap();
    assert_eq!(inspection.header.session_id(), &create.session_id);
    target_grants(api.clone(), &create.session_id).await;
    assert!(handle.pending_questions().await.unwrap().is_empty());
    assert!(handle.pending_approvals().await.unwrap().is_empty());
    let mut interactions = handle.observe_interactions().await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), interactions.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(first.approvals().is_empty() && first.questions().is_empty());
    reject_untrusted_origin(api.as_ref(), &create).await;
    let second = Arc::new(
        HttpClient::connect(
            client_runtime.execution().clone(),
            HttpClientConfig {
                origin,
                endpoint_id,
                credential: CredentialRef::new("test", "second").unwrap(),
                tls_ca: None,
                allow_loopback_http: true,
            },
            SecretValue::new(SECOND_KEY).unwrap(),
        )
        .await
        .unwrap(),
    );
    let other = rsi_session_api::SessionClient::new(second.clone()).unwrap();
    device_drafts(session.as_ref(), &other, &create).await;
    second.close().await;
    lost_submission(api.clone(), &create.session_id).await;
    client_plugin.dispose().await;
    assert!(
        client_runtime
            .root()
            .lookup_local::<SessionContract>()
            .is_none()
    );
    assert!(client_runtime.shutdown().await.is_clean());
    assert!(matches!(
        handle.history_before(None, 8).await,
        Err(rsi_session_protocol::SessionError::Api(
            ApiError::ShuttingDown
        ))
    ));
    // Retirement completes even while the application retains an unpolled stream.
    endpoints.dispose().await;
    assert!(
        !server_runtime
            .root()
            .lookup_local::<ApiDispatchContract>()
            .unwrap()
            .operations()
            .iter()
            .any(|op| op.id.domain() == "session")
    );
    drop(interactions);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    connection.close().await;
    assert!(server_runtime.shutdown().await.is_clean());
    assert!(host.shutdown().await.is_clean());
    provider.abort();
}

async fn target_grants(api: Arc<HttpClient>, session_id: &SessionId) {
    let grant =
        Arc::new(rsi_session_api::SessionTargetClient::new(api, session_id.clone()).unwrap());
    assert_eq!(grant.operations().len(), 20);
    let handle = rsi_session_api::SessionClient::attach_target(grant.clone(), session_id)
        .await
        .unwrap();
    assert_eq!(handle.header().await.unwrap().session_id(), session_id);
    assert_eq!(
        handle.inspect().await.unwrap().header.session_id(),
        session_id
    );
    handle.commands().await.unwrap();
    assert!(matches!(
        handle.answer_approval(
            &SessionId::new("unrelated-approval-owner").unwrap(),
            "approval",
            rsi_approval_protocol::ApprovalDecision::Deny,
        ).await,
        Err(rsi_session_protocol::SessionError::Invalid(message))
            if message.contains("outside the Agent tree")
    ));
    assert!(matches!(
        rsi_session_api::SessionClient::attach_target(
            grant,
            &SessionId::new("different-session").unwrap()
        )
        .await,
        Err(rsi_session_protocol::SessionError::Api(
            ApiError::Unauthorized
        ))
    ));
}

async fn device_drafts(
    first: &dyn rsi_session_protocol::SessionService,
    second: &dyn rsi_session_protocol::SessionService,
    template: &CreateSession,
) {
    let mut request = template.clone();
    for index in 0..64 {
        request.session_id = SessionId::new(format!("device-draft-{index}")).unwrap();
        first.create(request.clone()).await.unwrap();
    }
    let expected = first
        .create(request.clone())
        .await
        .unwrap()
        .header()
        .await
        .unwrap();
    assert_eq!(
        second
            .create(request.clone())
            .await
            .unwrap()
            .header()
            .await
            .unwrap(),
        expected
    );
    request.session_id = SessionId::new("device-draft-overflow").unwrap();
    assert!(matches!(
        first.create(request.clone()).await,
        Err(rsi_session_protocol::SessionError::Capacity)
    ));
    // Authenticated device 2 can still acquire its own entry in the same global table.
    second.create(request).await.unwrap();
}

#[derive(Debug)]
struct LoseSubmitReply {
    api: Arc<HttpClient>,
    submissions: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl ApiClient for LoseSubmitReply {
    fn description(&self) -> &rsi_api_protocol::ConnectionDescription {
        self.api.description()
    }
    fn operations(&self) -> &[rsi_api_protocol::OperationSpec] {
        self.api.operations()
    }
    fn input_budget(
        &self,
        class: rsi_api_protocol::OperationClass,
    ) -> rsi_api_protocol::ByteBudget {
        self.api.input_budget(class)
    }
    async fn call(
        &self,
        operation: &rsi_api_protocol::OperationSpec,
        input: rsi_api_protocol::RetainedBytes,
    ) -> rsi_api_protocol::Result<rsi_api_protocol::ApiOutput> {
        let output = self.api.call(operation, input).await?;
        if operation.id.domain() == "session" && operation.id.name() == "submit" {
            self.submissions
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            drop(output);
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(output)
    }
}
async fn lost_submission(api: Arc<HttpClient>, session: &SessionId) {
    use rsi_session_protocol::SessionService as _;
    let connection = Arc::new(LoseSubmitReply {
        api,
        submissions: std::sync::atomic::AtomicUsize::new(0),
    });
    let client = rsi_session_api::SessionClient::new(connection.clone()).unwrap();
    let handle = client.attach(session).await.unwrap();
    let request = rsi_session_protocol::SubmitInput {
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        message_id: rsi_agent_session_protocol::MessageId::new("lost-http-reply").unwrap(),
        content: vec![rsi_session_protocol::SessionInput::Text {
            text: "preserve accepted identity".into(),
        }],
        model: None,
        sandbox: None,
    };
    assert!(
        matches!(handle.submit(request.clone()).await, Err(rsi_session_protocol::SessionError::MessageOutcomeUnknown { session: id, message }) if id == session.as_str() && message == request.message_id.as_str())
    );
    let receipt = handle.message_status(&request.message_id).await.unwrap();
    let message = handle
        .read_message(&request.message_id, receipt.accepted_control_seq)
        .await
        .unwrap();
    assert_eq!(message.message_id, request.message_id);
    assert_eq!(
        connection
            .submissions
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
}
async fn reject_untrusted_origin(api: &dyn ApiClient, create: &CreateSession) {
    let operation = api
        .operations()
        .iter()
        .find(|op| op.id.domain() == "session" && op.id.name() == "create")
        .unwrap();
    let mut input = serde_json::to_value(create).unwrap();
    input
        .as_object_mut()
        .unwrap()
        .insert("origin".into(), json!({"device":"forged"}));
    let input = api
        .input_budget(operation.class)
        .encode(&input, operation.maximum_request_bytes)
        .unwrap();
    assert!(matches!(
        api.call(operation, input).await,
        Err(ApiError::Invalid(_))
    ));
}
