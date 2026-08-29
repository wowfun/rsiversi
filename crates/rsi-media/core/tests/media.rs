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
    let png_ref = media.import_image(Arc::from(png)).await.unwrap();
    let bmp_ref = media.import_image(Arc::from(bmp)).await.unwrap();
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
        media.import_image(Arc::from([])).await,
        Err(MediaError::InvalidInput(
            "source image length must be within 1..=1048576 bytes".into()
        ))
    );
    drop(read);
    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
}
