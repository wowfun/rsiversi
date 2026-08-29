use rsi_media_protocol::{MediaBackendContract, MediaError, MediaId, MediaRef, StoredMedia};
use rsi_media_testkit::MemoryMediaBackendFactory;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use std::sync::Arc;

#[tokio::test]
async fn memory_backend_rejects_bytes_that_do_not_match_their_reference() {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.media.memory",
                "test",
                UpdateMode::Replayable,
                Arc::new(MemoryMediaBackendFactory),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let backend = runtime
        .root()
        .lookup_local::<MediaBackendContract>()
        .unwrap();
    let result = backend
        .put(StoredMedia {
            reference: MediaRef {
                id: MediaId::new("a".repeat(64)).unwrap(),
                mime: "image/png".into(),
                bytes: 4,
                width: 1,
                height: 1,
            },
            bytes: Arc::from(*b"nope"),
        })
        .await;

    assert!(matches!(result, Err(MediaError::Corrupt(_))));
    drop(backend);
    assert!(fiber.dispose().await.is_clean());
}
