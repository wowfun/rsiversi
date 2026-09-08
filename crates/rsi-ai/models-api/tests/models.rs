use async_trait::async_trait;
use rsi_ai_models_api::{ModelsApiFactory, ModelsClient, ModelsClientFactory};
use rsi_ai_protocol::{
    LanguageCallContract, LanguageModelLimits, LanguageModelPage, LanguageModelProfiles,
    LanguageModels, LanguageModelsContract, LanguageProfile, LanguageRequest, ModelRef,
    ModelsError,
};
use rsi_ai_provider::{
    AdapterFuture, LanguageAdapter, LanguageAdapterStream, LanguageRegistrarContract,
    PrepareContext, Prepared, ProviderRegistration, RegistrationGate,
};
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiDispatch, ApiDispatchContract, ApiError, ApiMessage,
    ApiOutput, ByteBudget, CallOrigin, ConnectionDescription, EndpointId, HostEpoch,
    OperationClass, OperationSpec, RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use serde_json::Value;
use std::sync::Arc;

fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}
#[derive(Debug)]
struct Adapter(LanguageModelProfiles);
impl LanguageAdapter for Adapter {
    fn models(&self) -> &LanguageModelProfiles {
        &self.0
    }
    fn describe(&self, _: &str) -> Result<LanguageProfile, rsi_ai_protocol::AiError> {
        panic!("Models must not describe a provider")
    }
    fn validate_request(
        &self,
        _: &str,
        _: &LanguageRequest,
    ) -> Result<(), rsi_ai_protocol::AiError> {
        panic!("Models must not validate an invocation")
    }
    fn prepare(
        &self,
        _: PrepareContext,
        _: String,
        _: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, rsi_ai_protocol::AiError>> {
        panic!("Models must not prepare or perform provider I/O")
    }
}
#[derive(Debug)]
struct Connection {
    dispatch: Arc<dyn ApiDispatch>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    response: Option<Vec<u8>>,
}
impl Connection {
    fn new(dispatch: Arc<dyn ApiDispatch>, response: Option<Vec<u8>>) -> Self {
        Self {
            operations: dispatch.operations(),
            dispatch,
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            response,
        }
    }
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
        if let Some(response) = &self.response {
            return Ok(ApiOutput::Reply(ApiMessage {
                json: ByteBudget::default().copy(response)?,
                binary: None,
            }));
        }
        self.dispatch
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await
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
async fn server() -> Runtime {
    let runtime = Runtime::default();
    for (name, factory) in [
        (
            "credentials",
            Arc::new(rsi_credentials_testkit::MemoryCredentialsFactory) as Arc<dyn PluginFactory>,
        ),
        ("language", Arc::new(rsi_ai::LanguageRouterFactory)),
        ("api", Arc::new(rsi_api::ApiFactory)),
        ("models-api", Arc::new(ModelsApiFactory)),
    ] {
        runtime
            .root()
            .apply(linked(name, factory), Value::Null)
            .await
            .unwrap();
    }
    runtime
}

#[tokio::test]
async fn real_router_registration_gates_and_pages_cross_the_domain_api_without_invocation() {
    let server = server().await;
    let registrar = server
        .root()
        .lookup_local::<LanguageRegistrarContract>()
        .unwrap();
    let (leases, gates) = providers(registrar.as_ref());
    let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
    let connection = Arc::new(Connection::new(dispatch.clone(), None));
    let client = Runtime::default();
    client
        .root()
        .apply(
            linked("connection", Arc::new(ConnectionFactory(connection))),
            Value::Null,
        )
        .await
        .unwrap();
    let models_plugin = client
        .root()
        .apply(linked("models", Arc::new(ModelsClientFactory)), Value::Null)
        .await
        .unwrap();
    let models = client
        .root()
        .lookup_local::<LanguageModelsContract>()
        .unwrap();
    assert!(
        client
            .root()
            .lookup_local::<LanguageCallContract>()
            .is_none()
    );
    assert!(
        models
            .list_models(None, 256)
            .await
            .unwrap()
            .models
            .is_empty()
    );
    for gate in &gates {
        gate.commit();
    }
    let first = models.list_models(None, 256).await.unwrap();
    assert!(first.has_more);
    assert_eq!(first.models.len(), 256);
    assert!(first.models.iter().all(|model| model.deployment() == "a"));
    let second = models.list_models(first.models.last(), 256).await.unwrap();
    assert!(!second.has_more);
    assert_eq!(second.models.len(), 256);
    assert!(second.models.iter().all(|model| model.deployment() == "z"));
    for limit in [0, 257] {
        assert!(matches!(
            models.list_models(None, limit).await,
            Err(ModelsError::Invalid(_))
        ));
    }
    let operation = dispatch
        .operations()
        .into_iter()
        .find(|operation| operation.id.domain() == "models")
        .unwrap();
    let invocation = dispatch.admit(&operation.id, CallOrigin::Local).unwrap();
    let input = invocation
        .input_budget()
        .copy(br#"{"after":null,"limit":1,"extra":true}"#)
        .unwrap();
    assert!(matches!(
        invocation.invoke(input).await,
        Err(ApiError::Invalid(_))
    ));
    drop(leases);
    assert!(
        models
            .list_models(None, 256)
            .await
            .unwrap()
            .models
            .is_empty()
    );
    models_plugin.dispose().await;
    assert!(
        client
            .root()
            .lookup_local::<LanguageModelsContract>()
            .is_none()
    );
    assert!(client.shutdown().await.is_clean());
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test]
async fn peer_pages_reject_duplicates_count_and_cursor_regression() {
    let server = server().await;
    let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
    let model = ModelRef::new("a", "model").unwrap();
    for page in [
        LanguageModelPage {
            models: vec![model.clone(), model.clone()],
            has_more: false,
        },
        LanguageModelPage {
            models: vec![],
            has_more: true,
        },
        LanguageModelPage {
            models: vec![model.clone()],
            has_more: false,
        },
    ] {
        let connection = Arc::new(Connection::new(
            dispatch.clone(),
            Some(serde_json::to_vec(&page).unwrap()),
        ));
        let client = ModelsClient::new(connection).unwrap();
        assert!(client.list_models(Some(&model), 1).await.is_err());
    }
    assert!(server.shutdown().await.is_clean());
}

fn providers(
    registrar: &dyn rsi_ai_provider::LanguageRegistrar,
) -> (Vec<rsi_ai_provider::ProviderLease>, Vec<RegistrationGate>) {
    let mut leases = Vec::new();
    let mut gates = Vec::new();
    for deployment in ["z", "a"] {
        let mut models = LanguageModelProfiles::default();
        for index in 0..256 {
            models
                .insert(
                    format!("model-{index:03}"),
                    LanguageModelLimits::new(8192, 512, 1024).unwrap(),
                )
                .unwrap();
        }
        let provider = Arc::new(
            ProviderRegistration::builder(deployment, "test")
                .unwrap()
                .with_config_generation(1)
                .with_language(Adapter(models))
                .build()
                .unwrap(),
        );
        let gate = RegistrationGate::new();
        leases.push(registrar.register_language(provider, gate.clone()).unwrap());
        gates.push(gate);
    }
    (leases, gates)
}
