use super::*;

#[derive(Debug)]
struct CaptureOnlyFactory {
    descriptor: PluginDescriptor,
    handle: Arc<Mutex<Option<rsi_meta::ServiceHandle>>>,
}

#[async_trait]
impl PluginFactory for CaptureOnlyFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        *self.handle.lock().expect("capture poisoned") = Some(context.service("echo")?);
        Ok(())
    }
}

#[tokio::test]
async fn bounded_frames_are_rejected_at_the_calling_seam() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_frame_bytes: 4,
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("capture-only", "1"))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
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
        call.send(ServiceFrame::new(vec![0; 5])).await.unwrap_err(),
        MetaError::PayloadTooLarge { maximum: 4 }
    );
}
