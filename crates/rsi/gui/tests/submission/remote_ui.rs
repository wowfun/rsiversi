use super::*;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiError, ApiMessage, ApiOutput, ByteBudget,
    ConnectionDescription, EndpointId, HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

// Scripted API transport exercises the public Worker application. Real HTTP,
// authenticated binding and device revocation are covered by the product fixture.
#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    budget: ByteBudget,
    sender: Mutex<Option<tokio::sync::mpsc::Sender<rsi_api_protocol::Result<ApiMessage>>>>,
    active: Arc<AtomicUsize>,
    calls: Mutex<Vec<(String, Value)>>,
}
impl Remote {
    fn message(&self, value: &impl serde::Serialize) -> ApiMessage {
        ApiMessage {
            json: self
                .budget
                .encode(value, rsi_ui_api::MAXIMUM_ITEM_BYTES)
                .unwrap(),
            binary: None,
        }
    }
    fn item(revision: u64) -> Value {
        json!({"selection":0,"ticket":format!("ticket-{revision}"),"snapshot":{
            "presentation":{"reference":{"application":"server","target":"bound-session","contribution":"remote","name":"counter"},"epoch":"server-epoch"},
            "revision":revision,"model":{"renderer":"fixture.rust","schema":{"name":"fixture.counter","version":1},
                "data":{"label":"Remote counter","count":revision},"actions":[{"name":"apply","title":"Apply"}],
                "sources":[{"name":"raw","title":"Bytes","media_type":"application/octet-stream"}],"standard_view":null}
        }})
    }
    async fn push(&self, revision: u64) {
        let sender = self.sender.lock().unwrap().as_ref().unwrap().clone();
        sender
            .send(Ok(self.message(&Self::item(revision))))
            .await
            .unwrap();
    }
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ApiClient for Remote {
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
        let request: Value = serde_json::from_slice(request.as_bytes()).unwrap();
        let name = spec.id.name();
        self.calls
            .lock()
            .unwrap()
            .push((name.into(), request.clone()));
        match name {
            "catalog" => Ok(ApiOutput::Reply(self.message(&json!({"entries":[{"bundle":"remote","surface":"counter","title":"Remote counter"}],"next":null})))),
            "observe" => {
                assert_eq!(self.active.fetch_add(1, Ordering::SeqCst), 0);
                let (sender, receiver) = tokio::sync::mpsc::channel(2);
                sender.try_send(Ok(self.message(&Self::item(1)))).unwrap();
                *self.sender.lock().unwrap() = Some(sender);
                let guard = Active(self.active.clone());
                let stream = futures_util::stream::unfold((receiver, guard), |(mut receiver, guard)| async move {
                    receiver.recv().await.map(|item| (item, (receiver, guard)))
                });
                Ok(ApiOutput::Stream(Box::pin(stream)))
            }
            "source" => {
                assert_eq!(request["presentation"]["reference"]["target"], "bound-session");
                assert_eq!(request["name"], "raw");
                assert_eq!(request["revision"], 1);
                let start = usize::try_from(request["offset"].as_u64().unwrap()).unwrap();
                let maximum = usize::try_from(request["maximum"].as_u64().unwrap()).unwrap();
                let bytes: Vec<_> = b"\0\xffABC"[start..].iter().copied().take(maximum).collect();
                let mut message = self.message(&json!({"bytes":bytes.len()}));
                message.binary = Some(self.budget.reserve(bytes.len())?.retain_vec(bytes)?);
                Ok(ApiOutput::Reply(message))
            }
            "invoke" => Err(ApiError::OutcomeUnknown),
            _ => unreachable!(),
        }
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
async fn ready(app: &rsi_gui::GuiApplication, revision: u64) -> Value {
    let mut changed = app.changes();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let detail = sources::view(app)["ui_detail"].clone();
            if detail["model"]["data"]["count"] == revision && detail["busy"] == false {
                return detail;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn remote_arbitrary_models_derive_scope_from_the_pane_and_never_replay_consumed_tickets() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: rsi_ui_api::operations().to_vec(),
        budget: ByteBudget::default(),
        sender: Mutex::new(None),
        active: Arc::new(AtomicUsize::new(0)),
        calls: Mutex::new(vec![]),
    });
    for (id, factory) in [
        (
            "domains",
            Arc::new(Providers(Arc::new(Backend::default()))) as Arc<dyn PluginFactory>,
        ),
        ("connection", Arc::new(Connection(remote.clone()))),
        ("ui", Arc::new(rsi_ui::UiFactory)),
        ("web", Arc::new(rsi_gui::GuiApplicationFactory)),
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
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    let view = sources::view(&app);
    let pane = &view["surfaces"]["main"];
    assert_eq!(view["has_remote_ui"], true);
    let list = json!({"action":"remote_ui_list","pane":"main","generation":pane["generation"]})
        .to_string();
    for _ in 0..24 {
        app.command(&list).await.unwrap();
        let catalog = sources::view(&app)["remote_ui_catalog"].clone();
        let mut open = json!({"action":"remote_ui_surface","ticket":catalog["ticket"],"bundle":"foreign","surface":"counter"});
        assert!(app.command(&open.to_string()).await.is_err());
        open["bundle"] = "remote".into();
        app.command(&open.to_string()).await.unwrap();
        let detail = ready(&app, 1).await;
        assert!(detail["model"]["standard_view"].is_null());
        assert_eq!(detail["model"]["renderer"], "fixture.rust");
        let ticket = detail["ticket"].as_str().unwrap();
        assert_eq!(
            app.read_ui_source(ticket, "raw", 1, 3)
                .await
                .unwrap()
                .as_bytes(),
            b"\xffAB"
        );
        assert!(app.read_ui_source(ticket, "foreign", 0, 1).await.is_err());
        assert!(app.read_ui_source(ticket, "raw", 0, 65537).await.is_err());
        assert!(app.read_ui_source("foreign", "raw", 0, 1).await.is_err());
        let invoke = json!({"action":"ui_invoke","ticket":ticket,"name":"apply","input":{"value":null,"fields":{}}}).to_string();
        app.command(&invoke).await.unwrap();
        assert!(app.command(&invoke).await.is_err());
        assert_eq!(sources::view(&app)["ui_detail"]["busy"], true);
        remote.push(2).await;
        ready(&app, 2).await;
        assert!(app.command(&invoke).await.is_err());
        app.command(r#"{"action":"close_detail"}"#).await.unwrap();
        assert!(app.read_ui_source(ticket, "raw", 0, 1).await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while remote.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(remote.budget.used(), 0);
    }
    {
        let calls = remote.calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|(name, _)| name == "invoke").count(),
            24
        );
        for (_, request) in calls.iter().filter(|(name, _)| name == "catalog") {
            assert_eq!(request["scope"]["key"], pane["session"]);
        }
        for (_, request) in calls.iter().filter(|(name, _)| name == "observe") {
            assert_eq!(request["selections"][0]["scope"]["key"], pane["session"]);
        }
    }
    assert!(runtime.shutdown().await.is_clean());
}
