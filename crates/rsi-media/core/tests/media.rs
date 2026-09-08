use image::{ImageBuffer, ImageFormat, Rgba};
use rsi_media::MediaFactory;
use rsi_media_protocol::{
    MediaContract, MediaDescriptor, MediaError, MediaKind, MediaReadContract,
};
use rsi_media_testkit::MemoryMediaBackendFactory;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::{Value, json};
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn different_source_encodings_normalize_to_one_durable_identity() {
    let image = ImageBuffer::from_fn(2, 2, |x, y| {
        Rgba([
            u8::try_from(x * 80).unwrap(),
            u8::try_from(y * 80).unwrap(),
            5,
            255,
        ])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let mut bmp = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bmp), ImageFormat::Bmp)
        .unwrap();

    let runtime = Runtime::default();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.memory", Arc::new(MemoryMediaBackendFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.media", Arc::new(MediaFactory)),
            json!({"maximum_input_bytes":1_048_576,"maximum_pixels":100}),
        )
        .await
        .unwrap();
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    let png_ref = media.import_image(bytes::Bytes::from(png)).await.unwrap();
    let bmp_ref = media.import_image(bytes::Bytes::from(bmp)).await.unwrap();
    assert_eq!(png_ref, bmp_ref);
    let stored = media.read(&png_ref).await.unwrap();
    assert_eq!(stored.bytes.len(), usize::try_from(png_ref.bytes).unwrap());
    assert!(!format!("{stored:?}").contains("137, 80, 78, 71"));
    let descriptor = MediaDescriptor::new(
        MediaKind::Image,
        png_ref.mime.clone(),
        png_ref.bytes,
        png_ref.id.as_str(),
    )
    .unwrap()
    .with_image_dimensions(png_ref.width, png_ref.height)
    .unwrap();
    let read = runtime.root().lookup_local::<MediaReadContract>().unwrap();
    assert_eq!(
        read.read_descriptor(&descriptor).await.unwrap().bytes,
        stored.bytes
    );

    assert_eq!(
        media.import_image(bytes::Bytes::new()).await,
        Err(MediaError::InvalidInput(
            "source image length must be within 1..=1048576 bytes".into()
        ))
    );
    drop(read);
    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
}

#[tokio::test]
async fn one_image_larger_than_the_decode_gate_is_rejected_without_waiting() {
    let image = ImageBuffer::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();

    let runtime = Runtime::default();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.memory", Arc::new(MemoryMediaBackendFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.media", Arc::new(MediaFactory)),
            json!({
                "maximum_input_bytes": 1_048_576,
                "maximum_pixels": 1,
                "maximum_inflight_decode_bytes": 1
            }),
        )
        .await
        .unwrap();
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();

    let error = tokio::time::timeout(
        Duration::from_millis(100),
        media.import_image(bytes::Bytes::from(png)),
    )
    .await
    .expect("an impossible semaphore weight must not wait forever")
    .expect_err("one RGBA pixel cannot fit in one decode byte");
    assert!(matches!(error, MediaError::InvalidInput(_)));

    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
}

#[tokio::test]
async fn one_valid_input_must_fit_the_generation_source_gate() {
    let runtime = Runtime::default();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.memory", Arc::new(MemoryMediaBackendFactory)),
            Value::Null,
        )
        .await
        .unwrap();

    let error = runtime
        .root()
        .apply(
            linked("rsi.media", Arc::new(MediaFactory)),
            json!({
                "maximum_input_bytes": 2,
                "maximum_inflight_source_bytes": 1
            }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("maximum_input_bytes"));

    assert!(backend.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[test]
fn concurrent_valid_sources_report_transient_admission_pressure() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(source_admission_pressure());
}

async fn source_admission_pressure() {
    use std::future::poll_fn;
    use std::task::Poll;
    let image = ImageBuffer::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let source_bytes = png.len();

    let runtime = Runtime::default();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.memory", Arc::new(MemoryMediaBackendFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(
            linked("rsi.media", Arc::new(MediaFactory)),
            json!({
                "maximum_input_bytes": source_bytes,
                "maximum_pixels": 1,
                "maximum_concurrent_imports": 2,
                "maximum_inflight_source_bytes": source_bytes
            }),
        )
        .await
        .unwrap();
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    let source: bytes::Bytes = bytes::Bytes::from(png);

    // Hold the sole codec worker until both calls have reached source admission.
    // Dropping release also unblocks it if an assertion unwinds.
    let (release, waiting) = std::sync::mpsc::channel::<()>();
    let (started, ready) = tokio::sync::oneshot::channel();
    let blocker = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        let _ = waiting.recv();
    });
    ready.await.unwrap();

    let mut first = media.import_image(source.clone());
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(matches!(
        media.import_image(source.clone()).await,
        Err(MediaError::AdmissionFull(message)) if message.contains("source-byte")
    ));
    drop(release);
    blocker.await.unwrap();
    first.await.unwrap();
    // Pressure is temporary; the same valid source is admitted after release.
    media.import_image(source).await.unwrap();

    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
}
