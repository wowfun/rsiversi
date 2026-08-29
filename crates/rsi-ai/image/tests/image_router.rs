use async_trait::async_trait;
use futures_util::{StreamExt as _, stream};
use rsi_ai_image::ImageRouterFactory;
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageCallContract, ImageEvent, ImageRequest,
    MediaDescriptor, MediaKind, ModelRef,
};
use rsi_ai_provider::{
    AdapterFuture, ImageAdapter, ImageAdapterStream, ImageRegistrarContract, PrepareContext,
    Prepared, ProviderRegistration, RegistrationGate,
};
use rsi_credentials_protocol::CredentialRef;
use rsi_credentials_testkit::MemoryCredentialsFactory;
use rsi_media_protocol::{MediaBody, MediaError, MediaRead, MediaReadContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[derive(Clone)]
struct Adapter {
    prepares: Arc<AtomicUsize>,
    reject_request: bool,
}

#[derive(Debug)]
struct NoReadMedia;

#[async_trait]
impl MediaRead for NoReadMedia {
    async fn read_descriptor(
        &self,
        _descriptor: &MediaDescriptor,
    ) -> rsi_media_protocol::Result<MediaBody> {
        Err(MediaError::Io(
            "media admission must reject before durable reads".into(),
        ))
    }
}

#[derive(Debug)]
struct NoReadMediaFactory;

#[async_trait]
impl PluginFactory for NoReadMediaFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let media: Arc<dyn MediaRead> = Arc::new(NoReadMedia);
        let supply = plan.context().provide_local::<MediaReadContract>(media)?;
        plan.defer(
            "withdraw test Media reader",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

impl fmt::Debug for Adapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Adapter(..)")
    }
}

impl ImageAdapter for Adapter {
    fn validate_request(
        &self,
        _model: &str,
        _request: &ImageRequest,
    ) -> Result<(), rsi_ai_protocol::AiError> {
        if self.reject_request {
            return Err(AiError::new(
                ErrorKind::Unsupported,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "scripted image provider does not support this request",
            )
            .expect("static compatibility error"));
        }
        Ok(())
    }

    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, rsi_ai_protocol::AiError>> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        let snapshot = context.snapshot().clone();
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                Box::pin(async move {
                    let events = vec![
                        ImageEvent::OutputStarted {
                            index: 0,
                            mime_type: "image/png".into(),
                        },
                        ImageEvent::OutputChunk {
                            index: 0,
                            sequence: 1,
                            bytes: vec![1, 2, 3],
                        },
                        ImageEvent::OutputFinished { index: 0 },
                        ImageEvent::Finished,
                    ];
                    Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as ImageAdapterStream)
                })
            }))
        })
    }
}

#[tokio::test]
async fn image_route_is_gate_owned_and_media_is_resolved_only_for_edit_inputs() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.image", Arc::new(ImageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime.root().lookup_local::<ImageCallContract>().unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<ImageRegistrarContract>()
        .unwrap();
    let prepare_count = Arc::new(AtomicUsize::new(0));
    let registration = Arc::new(
        ProviderRegistration::builder("images", "scripted")
            .unwrap()
            .with_config_generation(4)
            .with_image(Adapter {
                prepares: Arc::clone(&prepare_count),
                reject_request: false,
            })
            .build()
            .unwrap(),
    );
    let gate = RegistrationGate::new();
    let lease = registrar
        .register_image(registration, gate.clone())
        .unwrap();
    let model = ModelRef::new("images", "image-model").unwrap();
    assert!(
        calls
            .prepare(model.clone(), ImageRequest::new("cat", 1).unwrap())
            .await
            .is_err()
    );
    gate.commit();

    let prepared = calls
        .prepare(model.clone(), ImageRequest::new("cat", 1).unwrap())
        .await
        .unwrap();
    assert_eq!(prepared.snapshot().config_generation, 4);
    assert_eq!(
        prepared.snapshot().request_sha256,
        hex::encode(Sha256::digest(
            br#"{"count":1,"inputs":[],"mask":null,"prompt":"cat"}"#
        ))
    );
    let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ImageEvent::OutputStarted { .. }
    ));
    assert_eq!(prepare_count.load(Ordering::SeqCst), 1);

    let descriptor =
        MediaDescriptor::new(MediaKind::Image, "image/png", 3, "0".repeat(64)).unwrap();
    let edit = ImageRequest::new("edit", 1)
        .unwrap()
        .with_inputs(vec![descriptor], None)
        .unwrap();
    assert!(calls.prepare(model.clone(), edit).await.is_err());
    assert_eq!(prepare_count.load(Ordering::SeqCst), 1);

    drop(lease);
    assert!(
        calls
            .prepare(model, ImageRequest::new("cat", 1).unwrap())
            .await
            .is_err()
    );
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}

#[tokio::test]
async fn image_compatibility_precedes_credential_resolution() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.image", Arc::new(ImageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime.root().lookup_local::<ImageCallContract>().unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<ImageRegistrarContract>()
        .unwrap();
    let registration = Arc::new(
        ProviderRegistration::builder("rejecting-images", "scripted")
            .unwrap()
            .with_config_generation(1)
            .with_credential(CredentialRef::new("provider", "missing").unwrap())
            .with_image(Adapter {
                prepares: Arc::new(AtomicUsize::new(0)),
                reject_request: true,
            })
            .build()
            .unwrap(),
    );
    let gate = RegistrationGate::new();
    let lease = registrar
        .register_image(registration, gate.clone())
        .unwrap();
    gate.commit();

    let error = calls
        .prepare(
            ModelRef::new("rejecting-images", "model").unwrap(),
            ImageRequest::new("cat", 1).unwrap(),
        )
        .await
        .expect_err("provider rejects before the missing credential is read");
    assert_eq!(error.kind(), ErrorKind::Unsupported);

    drop(lease);
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}

#[tokio::test]
async fn image_prepare_rejects_media_above_the_process_resident_budget_atomically() {
    let runtime = Runtime::default();
    let media = runtime
        .root()
        .apply(
            linked("rsi.media.test-read", Arc::new(NoReadMediaFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.image", Arc::new(ImageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime.root().lookup_local::<ImageCallContract>().unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<ImageRegistrarContract>()
        .unwrap();
    let prepare_count = Arc::new(AtomicUsize::new(0));
    let registration = Arc::new(
        ProviderRegistration::builder("resident-bound", "scripted")
            .unwrap()
            .with_config_generation(1)
            .with_image(Adapter {
                prepares: Arc::clone(&prepare_count),
                reject_request: false,
            })
            .build()
            .unwrap(),
    );
    let gate = RegistrationGate::new();
    let lease = registrar
        .register_image(registration, gate.clone())
        .unwrap();
    gate.commit();
    let inputs = (0_u8..9)
        .map(|index| {
            MediaDescriptor::new(
                MediaKind::Image,
                "image/png",
                32 * 1024 * 1024,
                format!("{index:064x}"),
            )
            .unwrap()
        })
        .collect();
    let request = ImageRequest::new("edit", 1)
        .unwrap()
        .with_inputs(inputs, None)
        .unwrap();
    let error = calls
        .prepare(ModelRef::new("resident-bound", "model").unwrap(), request)
        .await
        .expect_err("one request cannot reserve more than the process media budget");
    assert_eq!(error.kind(), ErrorKind::Artifact);
    assert_eq!(error.dispatch_status(), DispatchStatus::NotStarted);
    assert_eq!(prepare_count.load(Ordering::SeqCst), 0);

    drop(lease);
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
    assert!(media.dispose().await.is_clean());
    let _ = runtime.shutdown().await;
}
