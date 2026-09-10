use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, Message, MetaError, PluginFactory,
    PreparedActivation, ProviderChannel, ResolvedFactory, Runtime, ServiceEndpoint, UpdateMode,
};
use rsi_ui::*;
use rsi_ui_protocol::portable::{self, Request};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug)]
enum Behavior {
    Valid,
    Extra,
    Missing,
    Oversized,
    FailedTerminal,
}
#[derive(Debug)]
struct Endpoint {
    behavior: Behavior,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl ServiceEndpoint for Endpoint {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let message = channel.recv().await.ok_or(MetaError::Cancelled)?;
        let request: Request = serde_json::from_slice(message.as_bytes()).unwrap();
        assert!(message.capabilities().is_empty());
        assert!(
            channel.recv().await.is_none(),
            "unary request ends before reply"
        );
        let bytes = match request {
            Request::Describe {} => {
                let description = portable::Description {
                    name: "fixture".into(),
                    surfaces: vec![portable::Surface {
                        name: "panel".into(),
                        title: "Portable panel".into(),
                        target: TargetKind::Application,
                    }],
                    actions: vec![portable::Action {
                        name: "run".into(),
                        target: TargetKind::Application,
                    }],
                };
                channel
                    .send(Message::new(serde_json::to_vec(&description).unwrap()))
                    .await?;
                return Ok(());
            }
            Request::Snapshot {
                presentation,
                scope,
            } => {
                assert!(scope.is_none());
                assert_eq!(presentation.reference.name, "panel");
                assert!(presentation.epoch.starts_with("presentation"));
                serde_json::to_vec(&model()).unwrap()
            }
            Request::Invoke {
                presentation,
                action,
                input,
                scope,
            } => {
                assert!(scope.is_none());
                assert_eq!(presentation.reference.name, "panel");
                assert_eq!(action, "run");
                assert!(input.value.is_null());
                self.calls.fetch_add(1, Ordering::SeqCst);
                serde_json::to_vec(&model()).unwrap()
            }
            Request::Source {
                name,
                offset,
                maximum,
                ..
            } => {
                assert_eq!(name, "raw");
                b"Portable source"
                    .get(usize::try_from(offset).unwrap_or(usize::MAX)..)
                    .unwrap_or_default()
                    .iter()
                    .take(maximum)
                    .copied()
                    .collect()
            }
        };
        if matches!(self.behavior, Behavior::Missing) {
            return Ok(());
        }
        if matches!(self.behavior, Behavior::Oversized) {
            channel
                .send(Message::new(vec![b' '; portable::MAXIMUM_PACKET_BYTES + 1]))
                .await?;
            return Ok(());
        }
        channel.send(Message::new(bytes.clone())).await?;
        if matches!(self.behavior, Behavior::Extra) {
            channel.send(Message::new(bytes)).await?;
        }
        if matches!(self.behavior, Behavior::FailedTerminal) {
            return Err(MetaError::Activation("terminal fixture".into()));
        }
        Ok(())
    }
}
fn model() -> UiModel {
    let mut model = UiModel::standard(UiView {
        title: "Portable model".into(),
        elements: vec![UiElement::Button {
            action: "run".into(),
            label: "Run".into(),
            value: ConfigValue::Null,
        }],
    })
    .unwrap();
    model.sources.push(ModelSource {
        name: "raw".into(),
        title: "Raw".into(),
        media_type: "text/plain".into(),
    });
    model
}
#[derive(Debug)]
struct Provider(Arc<Endpoint>);
#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context().provide(
            "fixture.ui",
            portable::CONTRACT,
            ContractVersion(portable::VERSION),
            self.0.clone(),
        )?;
        Ok(())
    }
}
async fn apply(
    runtime: &Runtime,
    id: &str,
    factory: impl PluginFactory,
    config: ConfigValue,
) -> rsi_meta::FiberHandle {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(id, "1", UpdateMode::Replayable, Arc::new(factory)),
            config,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn actual_portable_source_feeds_local_snapshots_actions_and_binary_windows() {
    let runtime = Runtime::default();
    apply(&runtime, "ui", UiFactory, ConfigValue::Null).await;
    apply(
        &runtime,
        "target",
        UiTargetFactory,
        serde_json::json!("application"),
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = apply(
        &runtime,
        "provider",
        Provider(Arc::new(Endpoint {
            behavior: Behavior::Valid,
            calls: calls.clone(),
        })),
        ConfigValue::Null,
    )
    .await;
    let adapter = apply(
        &runtime,
        "adapter",
        rsi_ui_portable::PortableUiFactory,
        serde_json::json!({"service":"fixture.ui"}),
    )
    .await;
    assert_eq!(adapter.snapshot().state, rsi_meta::FiberState::Active);
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
    let lease = ui
        .present(&ui.surfaces(&target).unwrap()[0].reference)
        .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(3), lease.ready())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first.model().model.standard_view.unwrap().title,
        "Portable model"
    );
    assert_eq!(
        lease
            .source(first.revision(), "raw", 9, 6)
            .await
            .unwrap()
            .as_bytes(),
        b"source"
    );
    let next = lease
        .invoke(&first.action("run").unwrap(), ActionInput::default())
        .await
        .unwrap();
    assert_eq!(next.revision(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        lease
            .invoke(&first.action("run").unwrap(), ActionInput::default())
            .await
            .is_err()
    );
    assert!(provider.dispose().await.is_clean());
    assert!(lease.snapshot().is_err());
    lease.close().await.unwrap();
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn incomplete_oversized_duplicate_and_failed_terminal_models_never_publish() {
    for behavior in [
        Behavior::Extra,
        Behavior::Missing,
        Behavior::Oversized,
        Behavior::FailedTerminal,
    ] {
        let runtime = Runtime::default();
        apply(&runtime, "ui", UiFactory, ConfigValue::Null).await;
        apply(
            &runtime,
            "target",
            UiTargetFactory,
            serde_json::json!("application"),
        )
        .await;
        apply(
            &runtime,
            "provider",
            Provider(Arc::new(Endpoint {
                behavior,
                calls: Arc::new(AtomicUsize::new(0)),
            })),
            ConfigValue::Null,
        )
        .await;
        apply(
            &runtime,
            "adapter",
            rsi_ui_portable::PortableUiFactory,
            serde_json::json!({"service":"fixture.ui"}),
        )
        .await;
        let ui = runtime.root().lookup_local::<UiContract>().unwrap();
        let target = runtime.root().lookup_local::<UiTargetContract>().unwrap();
        let lease = ui
            .present(&ui.surfaces(&target).unwrap()[0].reference)
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), lease.ready())
                .await
                .unwrap()
                .is_err(),
            "{behavior:?}"
        );
        assert!(lease.snapshot().unwrap().is_none());
        lease.close().await.unwrap();
        assert_eq!(ui.presentation_usage(), (0, 0, 0));
        assert!(runtime.shutdown().await.is_clean());
    }
}
