use super::*;
use base64::Engine;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription,
    EndpointId, HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct Evidence {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    budget: ByteBudget,
    png: String,
    calls: Mutex<Vec<Value>>,
}
#[async_trait]
impl ApiClient for Evidence {
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
        request: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        assert_eq!(spec.id.name(), "artifact");
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.as_bytes()).unwrap());
        Ok(ApiOutput::Reply(ApiMessage {
            json: self.budget.encode(&json!({"png":self.png}), 1024 * 1024)?,
            binary: None,
        }))
    }
}
#[derive(Debug)]
struct Connection(Arc<Evidence>);
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
async fn artifact_bytes_never_enter_the_incremental_frame_baseline() {
    let mut bytes = std::io::Cursor::new(vec![]);
    image::DynamicImage::new_rgba8(8, 8)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let remote = Arc::new(Evidence {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: rsi_automation_api::operations(),
        budget: ByteBudget::default(),
        png: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        calls: Mutex::new(vec![]),
    });
    let runtime = Runtime::default();
    let root = runtime.root();
    for (id, factory) in [
        (
            "domains",
            Arc::new(Providers(Arc::new(Backend::default()))) as Arc<dyn PluginFactory>,
        ),
        ("connection", Arc::new(Connection(remote.clone()))),
        ("ui", Arc::new(rsi_ui::UiFactory)),
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
    let command = app
        .command(
            r#"{"action":"automation","request":{"operation":"artifact","id":"1","ordinal":0}}"#,
        )
        .await;
    let frame = app.view().unwrap();
    let rejected = command.is_err();
    let retained = std::str::from_utf8(frame.as_bytes())
        .unwrap()
        .contains(&remote.png);
    drop(frame);
    let evidence: Value = serde_json::from_str(
        &app.automation_artifact(r#"{"operation":"artifact","id":"1","ordinal":0}"#)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(evidence["png"], remote.png);
    assert!(
        app.automation_artifact(r#"{"operation":"cancel","id":"1","request_id":"forbidden"}"#)
            .await
            .is_err()
    );
    assert!(
        app.automation_artifact(r#"{"operation":"artifact","id":"01","ordinal":0}"#)
            .await
            .is_err()
    );
    assert_eq!(remote.calls.lock().unwrap().len(), 1);
    assert!(
        !std::str::from_utf8(app.next_frame(None).unwrap().as_bytes())
            .unwrap()
            .contains(&remote.png)
    );
    assert!(runtime.shutdown().await.is_clean());
    assert!(rejected, "artifact reads must use the finite response lane");
    assert!(!retained, "PNG bytes must never be projected into frames");
}
