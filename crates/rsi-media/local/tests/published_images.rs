use rsi_media_local::{LocalAttachmentReaderFactory, LocalMediaBackendFactory};
use rsi_media_protocol::{
    MediaContract, MediaDescriptor, MediaError, MediaId, MediaKind, MediaReadContract, MediaRef,
};
use rsi_meta::{PluginFactory, ResolvedFactory, Runtime, UpdateMode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Cursor, path::Path, sync::Arc};

async fn mount(runtime: &Runtime, id: &str, factory: impl PluginFactory + 'static, config: Value) {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(id, "test", UpdateMode::RestartRequired, Arc::new(factory)),
            config,
        )
        .await
        .unwrap();
}

async fn media_runtime(native: &Path, published: &Path) -> Runtime {
    let runtime = Runtime::default();
    mount(
        &runtime,
        "backend",
        LocalMediaBackendFactory,
        json!({"root":native}),
    )
    .await;
    mount(&runtime, "media", rsi_media::MediaFactory, Value::Null).await;
    mount(
        &runtime,
        "publisher",
        LocalAttachmentReaderFactory,
        json!({"root":published}),
    )
    .await;
    runtime
}

async fn reads_exact_object(runtime: &Runtime, reference: &MediaRef, bytes: &[u8]) {
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    let stored = media.read(reference).await.unwrap();
    assert_eq!(&stored.reference, reference);
    assert_eq!(stored.bytes.as_ref(), bytes);
    let descriptor = MediaDescriptor::new(
        MediaKind::Image,
        &reference.mime,
        reference.bytes,
        reference.id.as_str(),
    )
    .unwrap()
    .with_image_dimensions(reference.width, reference.height)
    .unwrap();
    let reader = runtime.root().lookup_local::<MediaReadContract>().unwrap();
    assert_eq!(
        reader
            .read_descriptor(&descriptor)
            .await
            .unwrap()
            .bytes
            .as_ref(),
        bytes
    );
}

#[tokio::test]
async fn media_reads_original_publisher_bytes_and_reports_corruption_at_its_source() {
    let temporary = tempfile::tempdir().unwrap();
    let native = temporary.path().join("native");
    let published = temporary.path().join("published");
    let runtime = media_runtime(&native, &published).await;
    let mut jpeg = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(4, 3)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let bytes = jpeg.into_inner();
    let digest = hex::encode(Sha256::digest(&bytes));
    let reference = MediaRef {
        id: MediaId::new(&digest).unwrap(),
        mime: "image/jpeg".into(),
        bytes: bytes.len() as u64,
        width: 4,
        height: 3,
    };
    let path = published.join("objects").join(&digest[..2]).join(&digest);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();
    reads_exact_object(&runtime, &reference, &bytes).await;
    let native_shard = native.join("objects").join(&digest[..2]);
    assert!(!native_shard.exists());
    let media = runtime.root().lookup_local::<MediaContract>().unwrap();
    let wrong = MediaRef {
        width: 2,
        ..reference.clone()
    };
    assert!(matches!(
        media.read(&wrong).await,
        Err(MediaError::Corrupt(_))
    ));
    fs::create_dir(&native_shard).unwrap();
    let primary = native_shard.join(format!("{digest}.rsi-media"));
    fs::write(&primary, b"broken").unwrap();
    assert!(matches!(
        media.read(&reference).await,
        Err(MediaError::Corrupt(_))
    ));
    fs::remove_file(primary).unwrap();
    fs::write(&path, b"broken").unwrap();
    assert!(matches!(
        media.read(&reference).await,
        Err(MediaError::Corrupt(_))
    ));
    fs::remove_file(path).unwrap();
    assert!(matches!(
        media.read(&reference).await,
        Err(MediaError::NotFound(_))
    ));
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[ignore = "requires RSI_MEDIA_IMAGE_FIXTURE from an external image publisher"]
async fn published_images_reach_both_native_read_contracts() {
    let fixture: Value = serde_json::from_slice(
        &fs::read(std::env::var_os("RSI_MEDIA_IMAGE_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let runtime = media_runtime(
        &temporary.path().join("native"),
        Path::new(fixture["root"].as_str().unwrap()),
    )
    .await;
    let samples = fixture["samples"].as_array().unwrap();
    assert!(!samples.is_empty());
    for sample in samples {
        let image = &sample["image"];
        let reference = serde_json::from_value(json!({
            "id": image["attachmentId"].as_str().unwrap().strip_prefix("sha256:").unwrap(),
            "mime": image["mediaType"], "bytes": image["bytes"], "width": image["width"], "height": image["height"]
        })).unwrap();
        reads_exact_object(
            &runtime,
            &reference,
            &fs::read(sample["file"].as_str().unwrap()).unwrap(),
        )
        .await;
    }
    assert!(runtime.shutdown().await.is_clean());
}
