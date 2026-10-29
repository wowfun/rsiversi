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
    async fn import_image_with_options(
        &self,
        source: Bytes,
        _options: rsi_media_protocol::ImageImportOptions,
    ) -> rsi_media_protocol::Result<MediaRef> {
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
            self.release.notified().await;
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
async fn cmd(app: &Arc<rsi_gui::GuiApplication>, value: Value) {
    app.command(&value.to_string()).await.unwrap();
}
#[tokio::test]
async fn ordered_images_and_image_only_retry_freeze_complete_content() {
    for resolution in [1, 2] {
        let (runtime, backend, app) = sources::fixture().await;
        let pane = view(&app)["surfaces"]["main"].clone();
        let generation = pane["generation"].as_str().unwrap();
        assert!(
            app.import_image(rsi_gui::SurfaceId::MAIN, generation, Bytes::new())
                .await
                .is_err()
        );
        assert!(
            app.import_image(
                rsi_gui::SurfaceId::MAIN,
                generation,
                vec![0; 16 * 1024 * 1024 + 1].into()
            )
            .await
            .is_err()
        );
        assert_eq!(backend.media.imports.load(Ordering::SeqCst), 0);
        let first = app
            .import_image(rsi_gui::SurfaceId::MAIN, generation, png(2))
            .await
            .unwrap();
        let second = app
            .import_image(rsi_gui::SurfaceId::MAIN, generation, png(3))
            .await
            .unwrap();
        let prepared = prepare(
            &app,
            generation,
            "",
            vec![second.clone(), first.clone()],
            false,
        )
        .await;
        assert_eq!(
            dispatch(&app, generation, &prepared, "dispatch").await["status"],
            "unknown"
        );
        let original = backend.requests.lock().unwrap()[0].clone();
        assert_eq!(
            &*original.content,
            &[
                SessionInput::Image { media: second },
                SessionInput::Image {
                    media: first.clone()
                }
            ]
        );
        let third = app
            .import_image(rsi_gui::SurfaceId::MAIN, generation, png(4))
            .await
            .unwrap();
        cmd(
            &app,
            json!({"action":"open","pane":"main","session":pane["session"]}),
        )
        .await;
        let next = view(&app);
        let generation = next["surfaces"]["main"]["generation"].as_str().unwrap();
        backend.resolution.store(resolution, Ordering::SeqCst);
        assert_eq!(
            dispatch(&app, generation, &prepared, "retry_message").await["status"],
            "complete"
        );
        let requests = backend.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), if resolution == 1 { 2 } else { 1 });
        assert!(
            requests
                .iter()
                .all(|request| json!(request) == json!(original))
        );
        let next = prepare(
            &app,
            generation,
            "/plan on",
            vec![first.clone(), third],
            false,
        )
        .await;
        assert_eq!(
            dispatch(&app, generation, &next, "dispatch").await["status"],
            "complete"
        );
        assert!(
            backend.commands.lock().unwrap().is_empty(),
            "images prevent text-only slash dispatch"
        );
        assert_eq!(backend.media.objects.lock().unwrap().len(), 3);
        let nine = json!({"pane":"main","generation":generation,"text":"","images":vec![first;9],"steer":false});
        assert!(app.prepare_submission(&nine.to_string()).await.is_err());
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn a_lost_upload_waiter_retains_import_ownership_through_pane_replacement() {
    let (runtime, backend, app) = sources::fixture().await;
    let pane = view(&app)["surfaces"]["main"].clone();
    let generation = pane["generation"].as_str().unwrap();
    backend.media.block_import.store(true, Ordering::SeqCst);
    drop(app.import_image(rsi_gui::SurfaceId::MAIN, generation, png(2)));
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    assert!(
        app.import_image(rsi_gui::SurfaceId::MAIN, generation, png(3))
            .await
            .is_err()
    );
    cmd(&app, json!({"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})).await;
    backend.media.release.notify_one();
    until(|| backend.media.active.load(Ordering::SeqCst) == 0).await;
    assert!(view(&app)["surfaces"]["main"].get("images").is_none());
    assert_eq!(
        backend.media.objects.lock().unwrap().len(),
        1,
        "lost reply does not roll back durable Media"
    );
    assert_eq!(
        backend.media.imports.load(Ordering::SeqCst),
        1,
        "lost reply never replays import"
    );
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn retry_requires_only_the_frozen_request_and_new_empty_input_is_invalid() {
    let (runtime, backend, app) = sources::fixture().await;
    let pane = view(&app)["surfaces"]["main"].clone();
    let generation = pane["generation"].as_str().unwrap();
    let image = app
        .import_image(rsi_gui::SurfaceId::MAIN, generation, png(2))
        .await
        .unwrap();
    let prepared = prepare(&app, generation, "", vec![image], false).await;
    assert_eq!(
        dispatch(&app, generation, &prepared, "dispatch").await["status"],
        "unknown"
    );
    let original = json!(backend.requests.lock().unwrap()[0]);
    backend.resolution.store(1, Ordering::SeqCst);
    assert_eq!(
        dispatch(&app, generation, &prepared, "retry_message").await["status"],
        "complete"
    );
    assert_eq!(json!(backend.requests.lock().unwrap()[1]), original);
    assert!(
        app.prepare_submission(
            &json!({"pane":"main","generation":generation,"text":"","images":[],"steer":false})
                .to_string()
        )
        .await
        .is_err()
    );
    assert_eq!(backend.requests.lock().unwrap().len(), 2);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn preview_uses_exact_media_and_cancels_with_its_detail_or_application() {
    let (runtime, backend, app) = sources::fixture().await;
    let generation = view(&app)["surfaces"]["main"]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let media = app
        .import_image(rsi_gui::SurfaceId::MAIN, &generation, png(2))
        .await
        .unwrap();
    let direct = json!({"kind":"draft","pane":"main","generation":generation,"media":media});
    let reads = backend.media.reads.load(Ordering::SeqCst);
    assert!(app.read_image(&direct.to_string()).await.is_err());
    assert_eq!(backend.media.reads.load(Ordering::SeqCst), reads);
    let stale =
        json!({"action":"inspect_image","pane":"main","generation":"retired","media":media});
    assert!(app.command(&stale.to_string()).await.is_err());
    assert_eq!(backend.media.reads.load(Ordering::SeqCst), reads);
    let inspect =
        json!({"action":"inspect_image","pane":"main","generation":generation,"media":media});
    cmd(&app, inspect.clone()).await;
    let ticket = super::sources::panel(&app)["image_detail"]["ticket"].clone();
    let selection = json!({"kind":"source","ticket":ticket}).to_string();
    assert_eq!(app.read_image(&selection).await.unwrap().bytes, png(2));
    backend.media.block_read.store(true, Ordering::SeqCst);
    let reading = app.read_image(&selection);
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    app.import_image(rsi_gui::SurfaceId::MAIN, &generation, png(3))
        .await
        .unwrap();
    super::sources::close_panel(&app).await;
    assert!(reading.await.is_err());
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 0);
    assert!(app.read_image(&selection).await.is_err());
    assert!(super::sources::panel(&app)["image_detail"].is_null());
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
    cmd(&app, json!({"action":"inspect_source","pane":"main","generation":generation,"source":{"seq":"7","field":{"kind":"image_output"}}})).await;
    assert_eq!(super::sources::panel(&app)["source_media"], json!(media));
    let selection =
        json!({"kind":"source","ticket":super::sources::panel(&app)["source_detail"]["ticket"]})
            .to_string();
    assert_eq!(app.read_image(&selection).await.unwrap().bytes, png(2));
    backend.media.block_read.store(true, Ordering::SeqCst);
    cmd(&app, inspect).await;
    let selection =
        json!({"kind":"source","ticket":super::sources::panel(&app)["image_detail"]["ticket"]})
            .to_string();
    let reading = app.read_image(&selection);
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    assert!(runtime.shutdown().await.is_clean());
    assert!(reading.await.is_err());
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 0);
    assert!(backend.cancel.lock().unwrap().is_empty());
}

#[tokio::test]
async fn two_preview_reads_enter_before_release_and_upload_keeps_its_own_slot() {
    let (runtime, backend, app) = sources::fixture().await;
    let generation = view(&app)["surfaces"]["main"]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut panels = Vec::new();
    for width in [2, 3] {
        let media = app
            .import_image(rsi_gui::SurfaceId::MAIN, &generation, png(width))
            .await
            .unwrap();
        cmd(
            &app,
            json!({"action":"inspect_image","pane":"main","generation":generation,"media":media}),
        )
        .await;
        panels.push(sources::panel(&app));
    }
    let selections = panels
        .iter()
        .map(|panel| json!({"kind":"source","ticket":panel["image_detail"]["ticket"]}).to_string())
        .collect::<Vec<_>>();
    backend.media.block_read.store(true, Ordering::SeqCst);
    let first = app.read_image(&selections[0]);
    let second = app.read_image(&selections[1]);
    // No read has been released: reaching two proves genuine backend overlap.
    until(|| backend.media.active.load(Ordering::SeqCst) == 2).await;
    assert!(
        app.read_image(&selections[1])
            .await
            .unwrap_err()
            .contains("reads are busy")
    );
    let reads = backend.media.reads.load(Ordering::SeqCst);
    app.import_image(rsi_gui::SurfaceId::MAIN, &generation, png(4))
        .await
        .unwrap();
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 2);
    assert_eq!(backend.media.reads.load(Ordering::SeqCst), reads);
    cmd(&app, json!({"action":"close_detail","view":panels[0]["view"],"expected_open_revision":panels[0]["open_revision"]})).await;
    assert!(first.await.is_err());
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 1);
    backend.media.release.notify_one();
    assert_eq!(second.await.unwrap().bytes, png(3));
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn preview_admission_follows_the_last_returned_clone_or_slice() {
    let (runtime, _, app) = sources::fixture().await;
    let generation = view(&app)["surfaces"]["main"]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let media = app
        .import_image(rsi_gui::SurfaceId::MAIN, &generation, png(2))
        .await
        .unwrap();
    cmd(
        &app,
        json!({"action":"inspect_image","pane":"main","generation":generation,"media":media}),
    )
    .await;
    let selection =
        json!({"kind":"source","ticket":sources::panel(&app)["image_detail"]["ticket"]})
            .to_string();
    let first = app.read_image(&selection).await.unwrap();
    let copy = first.bytes.clone();
    let slice = copy.slice(1..2);
    let second = app.read_image(&selection).await.unwrap();
    drop(first);
    drop(copy);
    assert!(
        app.read_image(&selection)
            .await
            .unwrap_err()
            .contains("reads are busy")
    );
    drop(slice);
    assert!(app.read_image(&selection).await.is_ok());
    drop(second);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn preview_byte_budget_rejects_before_backend_work_and_recovers_after_body_drop() {
    let (runtime, backend, app) = sources::fixture().await;
    let generation = view(&app)["surfaces"]["main"]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    // The mock Media provider owns these immutable canonical-body fixtures.
    let bytes = Bytes::from(vec![0; 17 * 1024 * 1024]);
    let reference = MediaRef {
        id: MediaId::new(format!("{:x}", Sha256::digest(&bytes))).unwrap(),
        mime: "image/png".into(),
        bytes: bytes.len() as u64,
        width: 1,
        height: 1,
    };
    backend.media.objects.lock().unwrap().insert(
        reference.id.clone(),
        StoredMedia {
            reference: reference.clone(),
            bytes,
        },
    );
    cmd(
        &app,
        json!({"action":"inspect_image","pane":"main","generation":generation,"media":reference}),
    )
    .await;
    let selection =
        json!({"kind":"source","ticket":sources::panel(&app)["image_detail"]["ticket"]})
            .to_string();
    let stored = app.read_image(&selection).await.unwrap();
    assert!(
        app.read_image(&selection)
            .await
            .unwrap_err()
            .contains("bytes are busy")
    );
    assert_eq!(backend.media.reads.load(Ordering::SeqCst), 1);
    drop(stored);
    assert!(app.read_image(&selection).await.is_ok());
    assert_eq!(backend.media.reads.load(Ordering::SeqCst), 2);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn dropping_a_preview_waiter_leaves_owned_work_until_detail_cancellation() {
    let (runtime, backend, app) = sources::fixture().await;
    let generation = view(&app)["surfaces"]["main"]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let media = app
        .import_image(rsi_gui::SurfaceId::MAIN, &generation, png(2))
        .await
        .unwrap();
    let inspect =
        json!({"action":"inspect_image","pane":"main","generation":generation,"media":media});
    cmd(&app, inspect.clone()).await;
    let selection =
        json!({"kind":"source","ticket":sources::panel(&app)["image_detail"]["ticket"]})
            .to_string();
    backend.media.block_read.store(true, Ordering::SeqCst);
    let reading = app.read_image(&selection);
    until(|| backend.media.active.load(Ordering::SeqCst) == 1).await;
    drop(reading);
    assert_eq!(backend.media.active.load(Ordering::SeqCst), 1);
    sources::close_panel(&app).await;
    until(|| backend.media.active.load(Ordering::SeqCst) == 0).await;
    backend.media.block_read.store(false, Ordering::SeqCst);
    cmd(&app, inspect).await;
    let replacement =
        json!({"kind":"source","ticket":sources::panel(&app)["image_detail"]["ticket"]})
            .to_string();
    assert!(app.read_image(&replacement).await.is_ok());
    assert!(runtime.shutdown().await.is_clean());
}
