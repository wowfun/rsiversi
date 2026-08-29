use image::{ImageBuffer, ImageFormat, Rgba};
use rsi_media::MediaFactory;
use rsi_media_local::LocalMediaBackendFactory;
use rsi_media_protocol::{MediaContract, MediaError};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::{Value, json};
use std::fs;
use std::io::Cursor;
use std::sync::Arc;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[tokio::test]
async fn local_object_survives_reactivation_and_tampering_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("media");
    let image = ImageBuffer::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
    let mut source = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut source), ImageFormat::Png)
        .unwrap();
    let source: Arc<[u8]> = Arc::from(source);
    let runtime = Runtime::default();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.local", Arc::new(LocalMediaBackendFactory)),
            json!({"root":root}),
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(linked("rsi.media", Arc::new(MediaFactory)), Value::Null)
        .await
        .unwrap();
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    let reference = media.import_image(Arc::clone(&source)).await.unwrap();
    let mut concurrent = Vec::new();
    for _ in 0..16 {
        let media = Arc::clone(&media);
        let source = Arc::clone(&source);
        concurrent.push(tokio::spawn(
            async move { media.import_image(source).await },
        ));
    }
    for imported in concurrent {
        assert_eq!(imported.await.unwrap().unwrap(), reference);
    }
    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());

    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.local", Arc::new(LocalMediaBackendFactory)),
            json!({"root":root}),
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(linked("rsi.media", Arc::new(MediaFactory)), Value::Null)
        .await
        .unwrap();
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    assert_eq!(media.read(&reference).await.unwrap().reference, reference);
    let path = root
        .join("objects")
        .join(&reference.id.as_str()[..2])
        .join(format!("{}.rsi-media", reference.id));
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 0xff;
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        media.read(&reference).await,
        Err(MediaError::Corrupt(message)) if message.contains("SHA-256")
    ));

    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
}

#[cfg(unix)]
#[tokio::test]
async fn object_symlink_is_rejected_without_reading_its_target() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("media");
    let image = ImageBuffer::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
    let mut source = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut source), ImageFormat::Png)
        .unwrap();
    let runtime = Runtime::default();
    let backend = runtime
        .root()
        .apply(
            linked("rsi.media.local", Arc::new(LocalMediaBackendFactory)),
            json!({"root":root}),
        )
        .await
        .unwrap();
    let service = runtime
        .root()
        .apply(linked("rsi.media", Arc::new(MediaFactory)), Value::Null)
        .await
        .unwrap();
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    let reference = media.import_image(Arc::from(source)).await.unwrap();
    let path = root
        .join("objects")
        .join(&reference.id.as_str()[..2])
        .join(format!("{}.rsi-media", reference.id));
    let victim = temporary.path().join("outside-object");
    fs::copy(&path, &victim).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&victim, &path).unwrap();

    assert!(matches!(
        media.read(&reference).await,
        Err(MediaError::Corrupt(_))
    ));

    drop(media);
    assert!(service.dispose().await.is_clean());
    assert!(backend.dispose().await.is_clean());
}
