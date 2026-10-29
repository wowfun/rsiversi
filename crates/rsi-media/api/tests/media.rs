#![cfg(unix)]

use async_trait::async_trait;
use image::{ImageBuffer, ImageFormat, Rgba};
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiDispatch, ApiDispatchContract, ApiError, ApiOutput,
    ApiRegistrarContract, ByteBudget, CallOrigin, ConnectionDescription, EndpointId, HostEpoch,
    OperationClass, OperationSpec, RetainedBytes,
};
use rsi_media_api::{MediaApi, MediaApiFactory, MediaClient, MediaClientFactory};
use rsi_media_protocol::{Media, MediaBackendContract, MediaContract, MediaError, MediaRef};
use rsi_meta::{
    ActivationPlan, PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy, Debug)]
enum Corruption {
    None,
    LostImportReply,
    Digest,
    Length,
    Metadata,
    ExtraField,
    MissingBinary,
}
#[derive(Debug)]
struct Connection {
    dispatch: Arc<dyn ApiDispatch>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    corruption: Corruption,
    received: ByteBudget,
    imports: AtomicUsize,
    published: std::sync::Mutex<Option<MediaRef>>,
}
impl Connection {
    fn new(dispatch: Arc<dyn ApiDispatch>, corruption: Corruption) -> Self {
        Self {
            operations: dispatch.operations().unwrap(),
            dispatch,
            corruption,
            description: ConnectionDescription {
                wire_version: rsi_api_protocol::API_WIRE_VERSION,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            received: ByteBudget::new(16 * 1024).unwrap(),
            imports: AtomicUsize::new(0),
            published: std::sync::Mutex::new(None),
        }
    }
}
#[async_trait]
impl ApiClient for Connection {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(self.operations.clone())
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let reply = self
            .dispatch
            .admit(&operation.call(), CallOrigin::Local)?
            .invoke(input)
            .await?;
        if operation.id.name() == "import" {
            self.imports.fetch_add(1, Ordering::SeqCst);
            let ApiOutput::Reply(message) = &reply else {
                panic!("finite import reply")
            };
            *self.published.lock().unwrap() =
                Some(serde_json::from_slice(message.json.as_bytes()).unwrap());
            return if matches!(self.corruption, Corruption::LostImportReply) {
                Err(ApiError::OutcomeUnknown)
            } else {
                Ok(reply)
            };
        }
        let ApiOutput::Reply(mut message) = reply else {
            panic!("finite Media reply")
        };
        let raw = message.binary.take().unwrap();
        let mut bytes = raw.as_bytes().to_vec();
        let mut metadata: Value = serde_json::from_slice(message.json.as_bytes()).unwrap();
        match self.corruption {
            Corruption::Digest => bytes[0] ^= 1,
            Corruption::Length => {
                bytes.pop();
            }
            Corruption::Metadata => metadata["width"] = json!(100),
            Corruption::ExtraField => metadata["foreign"] = json!(true),
            _ => {}
        }
        message.json = self.received.encode(&metadata, 1024)?;
        message.binary = if matches!(self.corruption, Corruption::MissingBinary) {
            None
        } else {
            Some(self.received.copy(&bytes)?)
        };
        Ok(ApiOutput::Reply(message))
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
fn linked(name: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory)
}
async fn server(root: &std::path::Path) -> Runtime {
    let runtime = Runtime::default();
    for (name, factory, config) in [
        (
            "local",
            Arc::new(rsi_media_local::LocalMediaBackendFactory) as Arc<dyn PluginFactory>,
            json!({"root":root}),
        ),
        ("media", Arc::new(rsi_media::MediaFactory), Value::Null),
        ("api", Arc::new(rsi_api::ApiFactory), Value::Null),
        ("media-api", Arc::new(MediaApiFactory), Value::Null),
    ] {
        runtime
            .root()
            .apply(linked(name, factory), config)
            .await
            .unwrap();
    }
    runtime
}
async fn client(api: Arc<Connection>) -> Runtime {
    let runtime = Runtime::default();
    for (name, factory) in [
        (
            "connection",
            Arc::new(ConnectionFactory(api)) as Arc<dyn PluginFactory>,
        ),
        ("media-client", Arc::new(MediaClientFactory)),
    ] {
        runtime
            .root()
            .apply(linked(name, factory), Value::Null)
            .await
            .unwrap();
    }
    runtime
}
fn source(format: ImageFormat) -> bytes::Bytes {
    let image = ImageBuffer::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap();
    bytes.into()
}
async fn import_pair(media: &dyn Media) -> MediaRef {
    let first = media.import_image(source(ImageFormat::Png)).await.unwrap();
    assert_eq!(
        media.import_image(source(ImageFormat::Bmp)).await.unwrap(),
        first
    );
    first
}

#[derive(Debug)]
struct GatedMedia {
    inner: Arc<dyn Media>,
    active: AtomicUsize,
    release: tokio::sync::Semaphore,
}
struct ActiveRead<'a>(&'a AtomicUsize);
impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Media for GatedMedia {
    async fn import_image_with_options(
        &self,
        bytes: bytes::Bytes,
        options: rsi_media_protocol::ImageImportOptions,
    ) -> rsi_media_protocol::Result<MediaRef> {
        self.inner.import_image_with_options(bytes, options).await
    }
    async fn read(
        &self,
        reference: &MediaRef,
    ) -> rsi_media_protocol::Result<rsi_media_protocol::StoredMedia> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveRead(&self.active);
        self.release.acquire().await.unwrap().forget();
        self.inner.read(reference).await
    }
}
async fn active_reads(media: &GatedMedia, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while media.active.load(Ordering::SeqCst) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn two_api_reads_reach_the_provider_before_release_and_cancellation_restores_capacity() {
    let temporary = tempfile::tempdir().unwrap();
    let server = server(temporary.path()).await;
    let inner = server.root().lookup_local::<MediaContract>().unwrap();
    let reference = import_pair(inner.as_ref()).await;
    let media = Arc::new(GatedMedia {
        inner,
        active: AtomicUsize::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let runtime = Runtime::default();
    runtime
        .root()
        .apply(linked("api", Arc::new(rsi_api::ApiFactory)), Value::Null)
        .await
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<ApiRegistrarContract>()
        .unwrap();
    let _api = MediaApi::register(registrar.as_ref(), media.clone()).unwrap();
    let dispatch = runtime
        .root()
        .lookup_local::<ApiDispatchContract>()
        .unwrap();
    let read = rsi_media_api::Operation::Read.spec();
    let invoke = |reference: &MediaRef| {
        let invocation = dispatch.admit(&read.call(), CallOrigin::Local).unwrap();
        let input = invocation.input_budget().encode(reference, 1024).unwrap();
        invocation.invoke(input)
    };
    let first = invoke(&reference);
    let second = invoke(&reference);
    active_reads(&media, 2).await;
    assert!(matches!(invoke(&reference).await, Err(ApiError::Capacity)));
    drop(first);
    active_reads(&media, 1).await;
    let replacement = invoke(&reference);
    active_reads(&media, 2).await;
    media.release.add_permits(2);
    let (ApiOutput::Reply(first), ApiOutput::Reply(second)) =
        (second.await.unwrap(), replacement.await.unwrap())
    else {
        panic!("finite read replies")
    };
    assert_eq!(first.binary.as_ref().unwrap().len() as u64, reference.bytes);
    // Replies retain wire allocations, but no longer consume provider scratch.
    let third = invoke(&reference);
    let fourth = invoke(&reference);
    active_reads(&media, 2).await;
    drop(third);
    drop(fourth);
    active_reads(&media, 0).await;
    let mut wrong = reference.clone();
    wrong.bytes = 1;
    let corrupt = invoke(&wrong);
    active_reads(&media, 1).await;
    media.release.add_permits(1);
    assert!(matches!(corrupt.await, Err(ApiError::Domain(_))));
    assert_eq!(
        second.binary.as_ref().unwrap().len() as u64,
        reference.bytes
    );
    drop(first);
    drop(second);
    assert!(runtime.shutdown().await.is_clean());
    assert!(server.shutdown().await.is_clean());
}

#[tokio::test]
async fn real_cas_and_independent_plugins_preserve_publication_retention_and_restart() {
    let temporary = tempfile::tempdir().unwrap();
    let server = server(temporary.path()).await;
    let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
    let connection = Arc::new(Connection::new(dispatch.clone(), Corruption::None));
    let client = client(connection.clone()).await;
    assert!(
        client
            .root()
            .lookup_local::<MediaBackendContract>()
            .is_none()
    );
    let media = client.root().lookup_local::<MediaContract>().unwrap();
    let reference = import_pair(media.as_ref()).await;
    let stored = media.read(&reference).await.unwrap();
    assert_eq!(stored.bytes.len() as u64, reference.bytes);
    let slice = stored.bytes.slice(0..1);
    assert_eq!(connection.received.used() as u64, reference.bytes);
    drop(stored);
    assert_eq!(connection.received.used() as u64, reference.bytes);
    drop(slice);
    assert_eq!(connection.received.used(), 0);
    assert!(matches!(
        media.import_image(bytes::Bytes::from_static(b"bad")).await,
        Err(MediaError::Codec(_))
    ));
    assert!(matches!(
        media.import_image(bytes::Bytes::new()).await,
        Err(MediaError::InvalidInput(_))
    ));
    let read = connection
        .operations
        .iter()
        .find(|spec| spec.id.name() == "read")
        .unwrap();
    let invocation = dispatch.admit(&read.call(), CallOrigin::Local).unwrap();
    let mut foreign = serde_json::to_value(&reference).unwrap();
    foreign["session"] = json!("wrong");
    let input = invocation.input_budget().encode(&foreign, 1024).unwrap();
    assert!(matches!(
        invocation.invoke(input).await,
        Err(ApiError::Invalid(_))
    ));
    assert!(client.shutdown().await.is_clean());
    assert!(server.shutdown().await.is_clean());
    assert!(matches!(dispatch.operations(), Err(ApiError::ShuttingDown)));
    let reopened = self::server(temporary.path()).await;
    let media = reopened.root().lookup_local::<MediaContract>().unwrap();
    assert_eq!(media.read(&reference).await.unwrap().reference, reference);
    assert!(reopened.shutdown().await.is_clean());
}

#[tokio::test]
async fn reply_loss_does_not_replay_and_bad_read_metadata_or_bytes_never_escape() {
    let temporary = tempfile::tempdir().unwrap();
    let server = server(temporary.path()).await;
    let dispatch = server.root().lookup_local::<ApiDispatchContract>().unwrap();
    let lost = Arc::new(Connection::new(
        dispatch.clone(),
        Corruption::LostImportReply,
    ));
    let media = MediaClient::new(lost.clone());
    assert_eq!(
        media
            .import_image(source(ImageFormat::Png))
            .await
            .unwrap_err(),
        MediaError::Api(ApiError::OutcomeUnknown)
    );
    assert_eq!(lost.imports.load(Ordering::SeqCst), 1);
    let native = server.root().lookup_local::<MediaContract>().unwrap();
    let reference = lost.published.lock().unwrap().clone().unwrap();
    // Read the first publication before any explicit retry could create an object.
    assert!(native.read(&reference).await.is_ok());
    assert_eq!(
        native.import_image(source(ImageFormat::Png)).await.unwrap(),
        reference
    );
    for corruption in [
        Corruption::Digest,
        Corruption::Length,
        Corruption::Metadata,
        Corruption::ExtraField,
        Corruption::MissingBinary,
    ] {
        let connection = Arc::new(Connection::new(dispatch.clone(), corruption));
        let media = MediaClient::new(connection.clone());
        assert!(matches!(
            media.read(&reference).await,
            Err(MediaError::Api(ApiError::Invalid(_)))
        ));
        assert_eq!(connection.received.used(), 0);
    }
    let media = MediaClient::new(Arc::new(Connection::new(dispatch, Corruption::None)));
    let mut unknown = reference.clone();
    unknown.id = rsi_media_protocol::MediaId::new("0".repeat(64)).unwrap();
    assert_eq!(
        media.read(&unknown).await.unwrap_err(),
        MediaError::NotFound(unknown.id)
    );
    assert!(server.shutdown().await.is_clean());
}
