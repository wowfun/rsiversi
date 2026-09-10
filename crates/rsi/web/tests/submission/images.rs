use super::*;
use bytes::Bytes;
use rsi_media_protocol::{Media, MediaId, MediaRef, StoredMedia};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sources::view;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
pub(super) struct Reader {
    objects: Mutex<BTreeMap<MediaId, StoredMedia>>,
    imports: AtomicUsize,
    reads: AtomicUsize,
    active: AtomicUsize,
    block_import: AtomicBool,
    block_read: AtomicBool,
    release: tokio::sync::Notify,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Media for Reader {
    async fn import_image(&self, source: Bytes) -> rsi_media_protocol::Result<MediaRef> {
        self.imports.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&self.active);
        if self.block_import.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        let decoded = image::load_from_memory(&source)
            .map_err(|error| rsi_media_protocol::MediaError::Codec(error.to_string()))?;
        // Fixture inputs are already PNG; real codec canonicalization is covered by product HTTP tests.
        let reference = MediaRef {
            id: MediaId::new(format!("{:x}", Sha256::digest(&source))).unwrap(),
            mime: "image/png".into(),
            bytes: source.len() as u64,
            width: decoded.width(),
            height: decoded.height(),
        };
        self.objects.lock().unwrap().insert(
            reference.id.clone(),
            StoredMedia {
                reference: reference.clone(),
                bytes: source,
            },
        );
        Ok(reference)
    }
    async fn read(&self, reference: &MediaRef) -> rsi_media_protocol::Result<StoredMedia> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&self.active);
        if self.block_read.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.objects
            .lock()
            .unwrap()
            .get(&reference.id)
            .filter(|object| object.reference == *reference)
            .cloned()
            .ok_or_else(|| rsi_media_protocol::MediaError::NotFound(reference.id.clone()))
    }
}
fn png(width: u32) -> Bytes {
    let mut data = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(width, 2, image::Rgba([20, 100, 150, 255]))
        .write_to(&mut data, image::ImageFormat::Png)
        .unwrap();
    data.into_inner().into()
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn cmd(app: &Arc<rsi_web::WebApplication>, value: Value) {
    app.command(&value.to_string()).await.unwrap();
}
async fn edit(app: &Arc<rsi_web::WebApplication>, from: u8, to: Option<u8>) {
    let pane = view(app)["panes"][0].clone();
    cmd(app, json!({"action":"image_edit","pane":0,"generation":pane["generation"],"revision":pane["images_revision"],"from":from,"to":to})).await;
}

#[tokio::test]
async fn ordered_images_and_image_only_retry_freeze_content_and_preserve_edited_drafts() {
    for resolution in [1, 2] {
        let (runtime, backend, app) = sources::fixture().await;
        let pane = view(&app)["panes"][0].clone();
        let generation = pane["generation"].as_str().unwrap();
        assert!(app.import_image(0, generation, Bytes::new()).await.is_err());
        assert!(
            app.import_image(0, generation, vec![0; 16 * 1024 * 1024 + 1].into())
                .await
                .is_err()
        );
        assert_eq!(backend.media.imports.load(Ordering::SeqCst), 0);
        let first = app.import_image(0, generation, png(2)).await.unwrap();
        let second = app.import_image(0, generation, png(3)).await.unwrap();
        let old_revision = view(&app)["panes"][0]["images_revision"].clone();
        edit(&app, 1, Some(0)).await;
        assert!(app.command(&json!({"action":"image_edit","pane":0,"generation":generation,"revision":old_revision,"from":0,"to":null}).to_string()).await.is_err());
        let submit =
            json!({"action":"submit","pane":0,"generation":generation,"text":"","steer":false});
        assert!(app.command(&submit.to_string()).await.is_err());
        let original = backend.requests.lock().unwrap()[0].clone();
        assert_eq!(
            original.content,
            vec![
                SessionInput::Image {
                    media: second.clone()
                },
                SessionInput::Image {
                    media: first.clone()
                }
            ]
        );
        assert_eq!(view(&app)["panes"][0]["unresolved_text"], "");
        edit(&app, 0, None).await;
        let third = app.import_image(0, generation, png(4)).await.unwrap();
        cmd(
            &app,
            json!({"action":"open","pane":0,"session":pane["session"]}),
        )
        .await;
        let generation = view(&app)["panes"][0]["generation"]
            .as_str()
            .unwrap()
            .to_owned();
        backend.resolution.store(resolution, Ordering::SeqCst);
        cmd(&app, json!({"action":"submit","pane":0,"generation":generation,"text":"edited","steer":true})).await;
        let requests = backend.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), if resolution == 1 { 2 } else { 1 });
        assert!(
            requests
                .iter()
                .all(|request| json!(request) == json!(original))
        );
        let current = view(&app)["panes"][0].clone();
        assert_eq!(current["draft"], "edited");
        assert_eq!(current["images"], json!([first, third]));
        cmd(&app, json!({"action":"submit","pane":0,"generation":generation,"text":"/plan on","steer":false})).await;
        assert!(
            backend.commands.lock().unwrap().is_empty(),
            "images prevent text-only slash dispatch"
        );
        {
            let requests = backend.requests.lock().unwrap();
            assert_ne!(requests.last().unwrap().message_id, original.message_id);
            assert_eq!(requests.last().unwrap().content.len(), 3);
        }
        assert_eq!(view(&app)["panes"][0]["images"], json!([]));
        assert_eq!(
            backend.media.objects.lock().unwrap().len(),
            3,
            "submission never deletes independent imports"
        );
        for _ in 0..8 {
            app.import_image(0, &generation, png(2)).await.unwrap();
        }
        let imports = backend.media.imports.load(Ordering::SeqCst);
        assert!(app.import_image(0, &generation, png(2)).await.is_err());
        assert_eq!(backend.media.imports.load(Ordering::SeqCst), imports);
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn a_lost_upload_waiter_keeps_its_captured_draft_and_admission_through_pane_replacement() {
    let (runtime, backend, app) = sources::fixture().await;
    let pane = view(&app)["panes"][0].clone();
    let generation = pane["generation"].as_str().unwrap();
    let header = backend.header.lock().unwrap().clone();
    backend.media.block_import.store(true, Ordering::SeqCst);
    drop(app.import_image(0, generation, png(2)));
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    assert!(app.import_image(0, generation, png(3)).await.is_err());
    assert!(app.command(&json!({"action":"submit","pane":0,"generation":generation,"text":"blocked","steer":false}).to_string()).await.is_err());
    cmd(&app, json!({"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false})).await;
    backend.media.release.notify_one();
    until(|| backend.media.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(view(&app)["panes"][0]["images"], json!([]));
    // This minimal fixture has one active header; restore the original service record before attachment.
    *backend.header.lock().unwrap() = header;
    cmd(
        &app,
        json!({"action":"open","pane":0,"session":pane["session"]}),
    )
    .await;
    assert_eq!(
        view(&app)["panes"][0]["images"].as_array().unwrap().len(),
        1
    );
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn clearing_the_edited_draft_cannot_prevent_retrying_the_frozen_image_request() {
    let (runtime, backend, app) = sources::fixture().await;
    let generation = view(&app)["panes"][0]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    app.import_image(0, &generation, png(2)).await.unwrap();
    let submit =
        json!({"action":"submit","pane":0,"generation":generation,"text":"","steer":false})
            .to_string();
    assert!(app.command(&submit).await.is_err());
    let original = json!(backend.requests.lock().unwrap()[0]);
    edit(&app, 0, None).await;
    backend.resolution.store(1, Ordering::SeqCst);
    app.command(&submit).await.unwrap();
    assert_eq!(json!(backend.requests.lock().unwrap()[1]), original);
    assert!(
        app.command(&submit).await.is_err(),
        "a new empty draft remains invalid after retry resolves"
    );
    assert_eq!(backend.requests.lock().unwrap().len(), 2);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn preview_uses_exact_media_and_cancels_with_its_detail_or_application() {
    let (runtime, backend, app) = sources::fixture().await;
    let generation = view(&app)["panes"][0]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let media = app.import_image(0, &generation, png(2)).await.unwrap();
    let direct = json!({"kind":"draft","pane":0,"generation":generation,"index":0,"media":media});
    assert_eq!(
        app.read_image(&direct.to_string()).await.unwrap().bytes,
        png(2)
    );
    let mut stale = direct;
    stale["index"] = json!(1);
    let reads = backend.media.reads.load(Ordering::SeqCst);
    assert!(app.read_image(&stale.to_string()).await.is_err());
    assert_eq!(backend.media.reads.load(Ordering::SeqCst), reads);
    let inspect =
        json!({"action":"inspect_image","pane":0,"generation":generation,"index":0,"media":media});
    cmd(&app, inspect.clone()).await;
    let ticket = view(&app)["image_detail"]["ticket"].clone();
    let selection = json!({"kind":"source","ticket":ticket}).to_string();
    assert_eq!(app.read_image(&selection).await.unwrap().bytes, png(2));
    backend.media.block_read.store(true, Ordering::SeqCst);
    let reading = app.read_image(&selection);
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    assert!(app.import_image(0, &generation, png(3)).await.is_err());
    cmd(&app, json!({"action":"close_detail"})).await;
    assert!(reading.await.is_err());
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 0);
    assert!(app.read_image(&selection).await.is_err());
    assert!(view(&app)["image_detail"].is_null());
    backend.media.block_read.store(false, Ordering::SeqCst);
    backend.facts.lock().unwrap().push(
        SessionFact::new(
            7,
            1,
            SessionFactBody::ImageOutput {
                turn_id: TurnId::new("image-turn").unwrap(),
                effect_id: EffectId::new("image-effect").unwrap(),
                index: 0,
                media: media.clone(),
            },
        )
        .unwrap(),
    );
    cmd(&app, json!({"action":"inspect_source","pane":0,"generation":generation,"source":{"seq":"7","field":{"kind":"image_output"}}})).await;
    assert_eq!(view(&app)["source_media"], json!(media));
    let selection =
        json!({"kind":"source","ticket":view(&app)["source_detail"]["ticket"]}).to_string();
    assert_eq!(app.read_image(&selection).await.unwrap().bytes, png(2));
    backend.media.block_read.store(true, Ordering::SeqCst);
    cmd(&app, inspect).await;
    let selection =
        json!({"kind":"source","ticket":view(&app)["image_detail"]["ticket"]}).to_string();
    let reading = app.read_image(&selection);
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    assert!(runtime.shutdown().await.is_clean());
    assert!(reading.await.is_err());
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 0);
    assert!(backend.cancel.lock().unwrap().is_empty());
}
