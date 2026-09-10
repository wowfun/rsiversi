use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId, HostEpoch,
    OperationAccess, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
    RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, Context, ContractVersion, Message, PluginFactory,
    PreparedActivation, ProviderChannel, ResolvedFactory, Runtime, ServiceEndpoint, UpdateMode,
};
use rsi_ui::*;
use rsi_ui_protocol::portable::{self, Request};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn operation() -> OperationSpec {
    OperationSpec {
        id: OperationId::new("fixture", "read-target", 1).unwrap(),
        access: OperationAccess::Authenticated,
        class: OperationClass::Data,
        effect: OperationEffect::Read,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 16,
        maximum_response_bytes: 1024,
    }
}
#[derive(Debug)]
struct TargetApi {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    budget: ByteBudget,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl ApiClient for TargetApi {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.budget.clone()
    }
    async fn call(
        &self,
        spec: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        assert_eq!(spec, &operation());
        assert_eq!(input.as_bytes(), b"{}");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ApiOutput::Reply(ApiMessage {
            json: self
                .budget
                .encode(&serde_json::json!({"actual":"domain-owned-value"}), 1024)?,
            binary: None,
        }))
    }
}
#[derive(Debug)]
struct Grant(Arc<TargetApi>);
#[async_trait]
impl PluginFactory for Grant {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<UiBusinessApiContract>(Arc::new(UiBusinessApi {
                scope: ExportScope {
                    kind: "fixture".into(),
                    key: "semantic-key".into(),
                },
                client: self.0.clone(),
            }))?;
        plan.defer(
            "business facet",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Endpoint {
    retained: Arc<Mutex<Option<Capability>>>,
}
#[async_trait]
impl ServiceEndpoint for Endpoint {
    async fn serve(
        &self,
        invocation: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let message = channel.recv().await.unwrap();
        let request: Request = serde_json::from_slice(message.as_bytes()).unwrap();
        assert!(channel.recv().await.is_none());
        if matches!(request, Request::Describe {}) {
            assert!(message.capabilities().is_empty());
            return channel
                .send(Message::new(
                    serde_json::to_vec(&portable::Description {
                        name: "business".into(),
                        surfaces: vec![portable::Surface {
                            name: "panel".into(),
                            title: "Business".into(),
                            target: TargetKind::Application,
                        }],
                        actions: vec![],
                    })
                    .unwrap(),
                ))
                .await;
        }
        let Request::Snapshot {
            presentation: _,
            scope: Some(scope),
        } = request
        else {
            panic!("scoped snapshot")
        };
        assert_eq!(scope.key, "semantic-key");
        assert_eq!(message.capabilities().len(), 1);
        let grant = message.capabilities()[0].clone();
        *self.retained.lock().unwrap() = Some(grant.clone());
        let api = rsi_api_portable::PortableApiClient::connect(
            invocation.provider_context().runtime().execution().clone(),
            grant,
        )
        .await
        .unwrap();
        let input = api.input_budget(OperationClass::Data).copy(b"{}").unwrap();
        let ApiOutput::Reply(reply) = api.call(&operation(), input).await.unwrap() else {
            panic!("business reply")
        };
        let value: serde_json::Value = serde_json::from_slice(reply.json.as_bytes()).unwrap();
        api.close().await;
        let model = UiModel::standard(UiView {
            title: value["actual"].as_str().unwrap().into(),
            elements: vec![],
        })
        .unwrap();
        channel
            .send(Message::new(serde_json::to_vec(&model).unwrap()))
            .await
    }
}
#[derive(Debug)]
struct Provider(Arc<Endpoint>);
#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan.context().provide(
            "fixture.ui",
            portable::CONTRACT,
            ContractVersion(1),
            self.0.clone(),
        )?;
        plan.defer(
            "UI provider",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
async fn apply(context: &Context, id: &str, factory: impl PluginFactory, config: ConfigValue) {
    let handle = context
        .apply(
            ResolvedFactory::linked(id, "test", UpdateMode::Replayable, Arc::new(factory)),
            config,
        )
        .await
        .unwrap();
    assert_eq!(handle.snapshot().state, rsi_meta::FiberState::Active);
}

#[tokio::test]
async fn presentation_child_transfers_only_its_explicit_grant_and_retires_escaped_possession() {
    let runtime = Runtime::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let retained = Arc::new(Mutex::new(None));
    apply(&runtime.root(), "ui", UiFactory, ConfigValue::Null).await;
    apply(
        &runtime.root(),
        "target",
        UiTargetFactory,
        serde_json::json!("application"),
    )
    .await;
    apply(
        &runtime.root(),
        "grant",
        Grant(Arc::new(TargetApi {
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: vec![operation()],
            budget: ByteBudget::default(),
            calls: calls.clone(),
        })),
        ConfigValue::Null,
    )
    .await;
    apply(
        &runtime.root(),
        "source",
        Provider(Arc::new(Endpoint {
            retained: retained.clone(),
        })),
        ConfigValue::Null,
    )
    .await;
    apply(
        &runtime.root(),
        "import",
        rsi_ui_portable::PortableUiFactory,
        serde_json::json!({"service":"fixture.ui","business_api":true}),
    )
    .await;
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
    let baseline = runtime
        .inspect(rsi_meta::InspectionRequest::default())
        .unwrap()
        .total_fibers;
    let lease = ui
        .present(&ui.surfaces(&target).unwrap().remove(0).reference)
        .unwrap();
    let model = lease.ready().await.unwrap().model();
    assert_eq!(
        model.model.standard_view.unwrap().title,
        "domain-owned-value"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        runtime
            .inspect(rsi_meta::InspectionRequest::default())
            .unwrap()
            .total_fibers
            > baseline
    );
    lease.close().await.unwrap();
    assert_eq!(
        runtime
            .inspect(rsi_meta::InspectionRequest::default())
            .unwrap()
            .total_fibers,
        baseline
    );
    let grant = retained.lock().unwrap().take().unwrap();
    assert!(grant.open().is_err());
    drop(grant);
    assert!(runtime.shutdown().await.is_clean());
}
