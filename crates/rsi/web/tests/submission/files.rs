use super::*;
use rsi_files_protocol::{
    DirectoryEntry, DirectoryPage, FileKind, FilePage, FileToken, FilesError, OpenedFile,
    RelativePath,
};
use rsi_session_files::{SessionFiles, SessionFilesContract};
use serde_json::{Value, json};
use sources::view;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct Reader {
    next: AtomicUsize,
    opened: Mutex<BTreeMap<FileToken, OpenedFile>>,
    reads: Mutex<Vec<(FileToken, u64)>>,
    released: AtomicUsize,
    active: AtomicUsize,
    block: AtomicBool,
    changed: AtomicBool,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl SessionFiles for Reader {
    async fn open(
        &self,
        _: SessionTarget,
        path: RelativePath,
        kind: FileKind,
    ) -> rsi_session_files::Result<OpenedFile> {
        let opened = OpenedFile {
            path,
            kind,
            length: if kind == FileKind::File { 8200 } else { 20 },
            token: format!("{:032x}", self.next.fetch_add(1, Ordering::SeqCst))
                .try_into()
                .unwrap(),
        };
        self.opened
            .lock()
            .unwrap()
            .insert(opened.token.clone(), opened.clone());
        Ok(opened)
    }
    async fn read(
        &self,
        _: SessionTarget,
        file: OpenedFile,
        offset: u64,
        maximum: usize,
    ) -> rsi_session_files::Result<FilePage> {
        if offset > file.length {
            return Err(FilesError::Invalid.into());
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&self.active);
        self.reads
            .lock()
            .unwrap()
            .push((file.token.clone(), offset));
        if self.block.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.changed.load(Ordering::SeqCst) {
            return Err(FilesError::Changed.into());
        }
        assert!(self.opened.lock().unwrap().contains_key(&file.token));
        let maximum = maximum.min(usize::try_from(file.length - offset).unwrap());
        // Exact control/invalid UTF-8 bytes are deliberately not display text.
        Ok(FilePage {
            offset,
            total: file.length,
            bytes_hex: "00ff".repeat(maximum / 2),
        })
    }
    async fn list(
        &self,
        _: SessionTarget,
        file: OpenedFile,
        offset: usize,
        maximum: usize,
    ) -> rsi_session_files::Result<DirectoryPage> {
        assert!(self.opened.lock().unwrap().contains_key(&file.token));
        let entries = (offset..(offset + maximum).min(20))
            .map(|index| {
                let bytes = if index == 19 {
                    vec![0xff]
                } else {
                    format!("{index:02}.txt").into_bytes()
                };
                DirectoryEntry {
                    name: String::from_utf8_lossy(&bytes).into(),
                    path: RelativePath::new(&bytes).unwrap(),
                    kind: Some(FileKind::File),
                }
            })
            .collect();
        Ok(DirectoryPage {
            offset,
            total: 20,
            entries,
        })
    }
    async fn release(&self, _: SessionTarget, token: FileToken) -> rsi_session_files::Result<()> {
        if self.opened.lock().unwrap().remove(&token).is_some() {
            self.released.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Supply(Arc<Reader>);
#[async_trait]
impl PluginFactory for Supply {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<SessionFilesContract>(self.0.clone())?;
        plan.defer(
            "withdraw fixture Files",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
async fn fixture() -> (
    Runtime,
    Arc<Reader>,
    Arc<rsi_web::WebApplication>,
    FiberHandle,
) {
    let runtime = Runtime::default();
    let backend = Arc::new(Backend::default());
    let reader = Arc::new(Reader::default());
    let mut files_fiber = None;
    for (name, factory) in [
        (
            "providers",
            Arc::new(Providers(backend)) as Arc<dyn PluginFactory>,
        ),
        ("files", Arc::new(Supply(reader.clone()))),
        ("ui", Arc::new(rsi_ui::UiFactory)),
        ("files-ui", Arc::new(rsi_session_files_ui::FilesUiFactory)),
        ("web", Arc::new(rsi_web::WebApplicationFactory)),
    ] {
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
                Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
        if name == "files" {
            files_fiber = Some(fiber);
        }
    }
    let app = runtime
        .root()
        .lookup_local::<rsi_web::WebApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"create","pane":0,"workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trust":false}"#).await.unwrap();
    (runtime, reader, app, files_fiber.unwrap())
}
async fn open_card(app: &Arc<rsi_web::WebApplication>, pane: usize) {
    let current = view(app);
    let pane_view = &current["panes"][pane];
    let menu = pane_view["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"] == "Workspace files")
        .unwrap();
    app.command(&json!({"action":"ui_surface","pane":pane,"generation":pane_view["generation"],"reference":menu["reference"]}).to_string()).await.unwrap();
}
async fn click(app: &Arc<rsi_web::WebApplication>, label: &str) -> Value {
    let command = ui::button(&view(app)["ui_detail"], Some(label));
    app.command(&command.to_string()).await.unwrap();
    view(app)["ui_detail"].clone()
}
fn shown(detail: &Value) -> String {
    detail["view"]["view"].to_string()
}

#[tokio::test]
async fn files_cards_page_exact_bytes_paths_refresh_and_keep_same_session_panes_independent() {
    let (runtime, reader, app, _files) = fixture().await;
    open_card(&app, 0).await;
    let directory = click(&app, "Workspace root").await;
    assert!(shown(&directory).contains("0–16 of 20"));
    let next = click(&app, "Next page").await;
    assert!(shown(&next).contains("16–20 of 20"));
    let file = click(&app, "�").await;
    assert!(shown(&file).contains("Path bytes (hex)"));
    assert!(shown(&file).contains("\"ff\""));
    assert!(!shown(&file).contains("\\u0000"));
    assert_eq!(reader.opened.lock().unwrap().len(), 1);
    let first_token = reader.reads.lock().unwrap()[0].0.clone();
    let next = click(&app, "Next page").await;
    assert!(shown(&next).contains("4096–8192 of 8200"));
    let hex = click(&app, "View exact hex").await;
    assert!(shown(&hex).contains("00001000  00 ff"));
    assert!(
        reader
            .reads
            .lock()
            .unwrap()
            .iter()
            .all(|(token, _)| token == &first_token)
    );
    reader.changed.store(true, Ordering::SeqCst);
    let changed = click(&app, "Next page").await;
    assert!(shown(&changed).contains("changed. Refresh"));
    let opens = reader.next.load(Ordering::SeqCst);
    reader.changed.store(false, Ordering::SeqCst);
    click(&app, "Refresh").await;
    assert_eq!(reader.next.load(Ordering::SeqCst), opens + 1);
    assert_ne!(reader.reads.lock().unwrap().last().unwrap().0, first_token);
    let session = view(&app)["panes"][0]["session"].clone();
    app.command(&json!({"action":"open","pane":1,"session":session}).to_string())
        .await
        .unwrap();
    open_card(&app, 1).await;
    click(&app, "Workspace root").await;
    assert_eq!(reader.opened.lock().unwrap().len(), 2);
    open_card(&app, 0).await;
    assert!(shown(&click(&app, "Current snapshot").await).contains("Workspace file"));
    click(&app, "Release snapshot").await;
    assert_eq!(reader.opened.lock().unwrap().len(), 1);
    let controllers = runtime.snapshot().fibers.iter().filter(|f| matches!(&f.factory, FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "rsi.client.session-controller") && f.state == FiberState::Active).count();
    assert_eq!(controllers, 2);
    assert!(runtime.shutdown().await.is_clean());
    assert!(reader.opened.lock().unwrap().is_empty());
}

#[tokio::test]
async fn closing_files_detail_cancels_read_and_replaced_surface_rejects_its_action() {
    let (runtime, reader, app, _files) = fixture().await;
    open_card(&app, 0).await;
    click(&app, "Workspace root").await;
    click(&app, "00.txt").await;
    reader.block.store(true, Ordering::SeqCst);
    let pending = app.command(&ui::button(&view(&app)["ui_detail"], Some("Next page")).to_string());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while reader.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let concurrent = ui::button(&view(&app)["ui_detail"], Some("Next page"));
    let registry = runtime.root().lookup_local::<rsi_ui::UiContract>().unwrap();
    assert!(matches!(
        registry
            .invoke(
                &serde_json::from_value(concurrent["reference"].clone()).unwrap(),
                serde_json::from_value(concurrent["input"].clone()).unwrap(),
            )
            .await,
        Err(rsi_ui::UiError::Capacity)
    ));
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    pending.await.unwrap();
    assert_eq!(reader.active.load(Ordering::SeqCst), 0);
    assert!(view(&app)["ui_detail"].is_null());
    reader.block.store(false, Ordering::SeqCst);
    open_card(&app, 0).await;
    let old = ui::button(&view(&app)["ui_detail"], Some("Current snapshot"));
    let session = view(&app)["panes"][0]["session"].clone();
    app.command(&json!({"action":"open","pane":0,"session":session}).to_string())
        .await
        .unwrap();
    let reads = reader.reads.lock().unwrap().len();
    app.command(&old.to_string()).await.unwrap();
    assert_eq!(reader.reads.lock().unwrap().len(), reads);
    assert!(reader.opened.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn replacing_only_files_provider_cannot_retarget_an_old_browser_action() {
    let (runtime, reader, app, files) = fixture().await;
    open_card(&app, 0).await;
    click(&app, "Workspace root").await;
    click(&app, "00.txt").await;
    let old = ui::button(&view(&app)["ui_detail"], Some("Next page"));
    assert!(files.dispose().await.is_clean());
    let replacement = Arc::new(Reader::default());
    let supplied = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "files",
                "replacement",
                UpdateMode::Replayable,
                Arc::new(Supply(replacement.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(supplied.snapshot().state, FiberState::Active);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let active = runtime.snapshot().fibers.iter().any(|f| f.state == FiberState::Active &&
                matches!(&f.factory, FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "rsi.session.files.ui-target"));
            if active { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let registry = runtime.root().lookup_local::<rsi_ui::UiContract>().unwrap();
    let reference = serde_json::from_value(old["reference"].clone()).unwrap();
    assert!(
        registry.is_current(&reference),
        "the actual Session UI target did not change"
    );
    assert!(
        matches!(registry.invoke(&reference, serde_json::from_value(old["input"].clone()).unwrap()).await,
        Err(rsi_ui::UiError::Invalid(message)) if message.contains("changed"))
    );
    assert_eq!(replacement.next.load(Ordering::SeqCst), 0);
    assert!(reader.opened.lock().unwrap().is_empty());
    open_card(&app, 0).await;
    let mut invalid = ui::button(&view(&app)["ui_detail"], Some("Read file"));
    invalid["input"]["fields"] = json!({"path":"sample","root":"/"});
    assert!(matches!(
        registry
            .invoke(
                &reference,
                serde_json::from_value(invalid["input"].clone()).unwrap()
            )
            .await,
        Err(rsi_ui::UiError::Invalid(_))
    ));
    invalid["input"]["fields"] =
        json!({"path":"x".repeat(rsi_session_files_ui::INPUT_PATH_BYTES + 1)});
    assert!(matches!(
        registry
            .invoke(
                &reference,
                serde_json::from_value(invalid["input"].clone()).unwrap()
            )
            .await,
        Err(rsi_ui::UiError::Invalid(_))
    ));
    assert_eq!(replacement.next.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}
