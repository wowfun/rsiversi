use super::*;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription,
    EndpointId, HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_configuration_api::CredentialOperation;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    reads: AtomicUsize,
}
#[async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(vec![
            CredentialOperation::Status.spec(),
            rsi_workspace_api::Operation::List.spec(),
            // Service::list_recent always fails. Navigation owns session lists,
            // so advertising this unrelated API must not break GUI refresh.
            rsi_session_api::Operation::Recent.spec(),
        ])
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        spec: &OperationSpec,
        _: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        assert_eq!(spec, &CredentialOperation::Status.spec());
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default().encode(&serde_json::json!({"availability":{"kind":"missing"},"editable":true,"store_path":null}), spec.maximum_response_bytes)?, binary: None,
        }))
    }
}
#[derive(Debug)]
struct Connection(Arc<Remote>);
#[async_trait]
impl PluginFactory for Connection {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<ApiClientContract>(self.0.clone())?;
        Ok(())
    }
}
#[tokio::test]
async fn refresh_ignores_recent_session_outages_and_credential_reads_preserve_catalog() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let backend = Arc::new(Backend::default());
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: rsi_api_protocol::API_WIRE_VERSION,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        reads: 0.into(),
    });
    for (id, factory) in [
        (
            "domains",
            Arc::new(Providers(backend.clone())) as Arc<dyn PluginFactory>,
        ),
        ("connection", Arc::new(Connection(remote.clone()))),
        ("ui", Arc::new(rsi_ui::UiFactory)),
        ("setup", Arc::new(rsi_workbench_ui::SetupFeatureFactory)),
        ("gui", Arc::new(rsi_gui::GuiApplicationFactory)),
    ] {
        let fiber = root
            .apply(
                ResolvedFactory::linked(id, "fixture", UpdateMode::RestartRequired, factory),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
    }
    let app = root
        .lookup_local::<rsi_gui::GuiApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"refresh"}"#).await.unwrap();
    let frame: serde_json::Value =
        serde_json::from_slice(app.next_frame(None).unwrap().as_bytes()).unwrap();
    assert!(frame["view"]["catalog"].get("sessions").is_none());
    assert!(frame["view"]["catalog"].get("sessions_more").is_none());
    app.acknowledge_frame(frame["frame_id"].as_str().unwrap())
        .unwrap();
    let before = backend.workspace_lists.load(Ordering::SeqCst);
    app.command(r#"{"action":"setup","command":{"kind":"credential_status","provider":"deepseek","slot":"edited-slot"}}"#).await.unwrap();
    assert_eq!(remote.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        sources::view(&app)["setup"]["credential"]["slot"],
        "edited-slot"
    );
    assert_eq!(backend.workspace_lists.load(Ordering::SeqCst), before);
    app.command(r#"{"action":"refresh"}"#).await.unwrap();
    assert!(backend.workspace_lists.load(Ordering::SeqCst) > before);
    assert!(runtime.shutdown().await.is_clean());
}
