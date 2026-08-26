use super::*;

#[derive(Debug)]
struct CaptureOnlyFactory {
    identity: FactoryIdentity,
    handle: Arc<Mutex<Option<rsi_meta::Capability>>>,
}

#[async_trait]
impl PluginFactory for CaptureOnlyFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "echo",
                "test.echo",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.handle.lock().expect("capture poisoned") = Some(
            plan.inject("echo")
                .expect("prepared echo requirement must be injected")
                .clone(),
        );
        Ok(())
    }
}

#[tokio::test]
async fn bounded_frames_are_rejected_at_the_calling_seam() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_message_bytes: 4,
            ..rsi_meta::PayloadLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(
            Arc::new(foundation_service::ProviderFactory::new(Arc::clone(
                &cleanup,
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&provider).await;
    let capture = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureOnlyFactory {
                identity: FactoryIdentity::builtin("capture-only", "1"),
                handle: Arc::clone(&capture),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&consumer).await;
    let handle = capture.lock().expect("capture poisoned").clone().unwrap();
    let call = handle.open().unwrap();
    assert_eq!(
        call.send(Message::new(vec![0; 5])).await.unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 4 }
    );
}
