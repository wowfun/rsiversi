use super::*;
use rsi_files_protocol::{
    DirectoryEntry, DirectoryPage, FileKind, FilePage, FileToken, FilesError, OpenedFile,
    RelativePath,
};
use rsi_session_files::{SessionFiles, SessionFilesContract};
use serde_json::{Value, json};
use sources::view;
use std::fmt::Write as _;
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
    contents: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
    block_path: Mutex<Option<Vec<u8>>>,
    missing_path: Mutex<Option<Vec<u8>>>,
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
        if self.missing_path.lock().unwrap().as_deref() == Some(path.as_bytes()) {
            return Err(FilesError::Unavailable.into());
        }
        let opened = OpenedFile {
            path: path.clone(),
            kind,
            length: self
                .contents
                .lock()
                .unwrap()
                .get(path.as_bytes())
                .map_or(if kind == FileKind::File { 8200 } else { 20 }, |bytes| {
                    bytes.len() as u64
                }),
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
        if self.block.load(Ordering::SeqCst)
            || self.block_path.lock().unwrap().as_deref() == Some(file.path.as_bytes())
        {
            std::future::pending::<()>().await;
        }
        if self.changed.load(Ordering::SeqCst) {
            return Err(FilesError::Changed.into());
        }
        assert!(self.opened.lock().unwrap().contains_key(&file.token));
        let maximum = maximum.min(usize::try_from(file.length - offset).unwrap());
        if let Some(bytes) = self.contents.lock().unwrap().get(file.path.as_bytes()) {
            let start = usize::try_from(offset).unwrap();
            return Ok(FilePage {
                offset,
                total: file.length,
                bytes_hex: bytes[start..start + maximum].iter().fold(
                    String::new(),
                    |mut output, byte| {
                        write!(output, "{byte:02x}").unwrap();
                        output
                    },
                ),
            });
        }
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
    Arc<rsi_gui::GuiApplication>,
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
        ("web", Arc::new(rsi_gui::GuiApplicationFactory)),
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
        .lookup_local::<rsi_gui::GuiApplicationContract>()
        .unwrap();
    app.command(r#"{"action":"create","pane":"main","workspace":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#).await.unwrap();
    (runtime, reader, app, files_fiber.unwrap())
}
async fn open_card(app: &Arc<rsi_gui::GuiApplication>, pane: &str) {
    let current = view(app);
    let pane_view = &current["surfaces"][pane];
    let menu = pane_view["ui_surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"] == "Workspace files")
        .unwrap();
    app.command(&json!({"action":"ui_surface","pane":pane,"generation":pane_view["generation"],"reference":menu["reference"]}).to_string()).await.unwrap();
}
async fn click(app: &Arc<rsi_gui::GuiApplication>, label: &str) -> Value {
    let command = ui::button(&view(app)["ui_detail"], Some(label));
    app.command(&command.to_string()).await.unwrap();
    view(app)["ui_detail"].clone()
}
fn shown(detail: &Value) -> String {
    detail["model"]["standard_view"].to_string()
}

#[tokio::test]
async fn files_cards_page_exact_bytes_paths_refresh_and_keep_same_session_panes_independent() {
    let (runtime, reader, app, _files) = fixture().await;
    open_card(&app, "main").await;
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
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(r#"{"action":"add_surface","pane":"compare"}"#)
        .await
        .unwrap();
    app.command(&json!({"action":"open","pane":"compare","session":session}).to_string())
        .await
        .unwrap();
    open_card(&app, "compare").await;
    click(&app, "Workspace root").await;
    assert_eq!(reader.opened.lock().unwrap().len(), 2);
    open_card(&app, "main").await;
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
    open_card(&app, "main").await;
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
                &ui::reference(&view(&app)["ui_detail"], &concurrent["name"]),
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
    open_card(&app, "main").await;
    let old = ui::button(&view(&app)["ui_detail"], Some("Current snapshot"));
    let session = view(&app)["surfaces"]["main"]["session"].clone();
    app.command(&json!({"action":"open","pane":"main","session":session}).to_string())
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
    open_card(&app, "main").await;
    click(&app, "Workspace root").await;
    click(&app, "00.txt").await;
    let old = ui::button(&view(&app)["ui_detail"], Some("Next page"));
    let reference = ui::reference(&view(&app)["ui_detail"], &old["name"]);
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
    open_card(&app, "main").await;
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

async fn open_path(app: &Arc<rsi_gui::GuiApplication>, path: &str) {
    open_card(app, "main").await;
    let mut command = ui::button(&view(app)["ui_detail"], Some("Read file"));
    command["input"]["fields"] = json!({"path":path});
    app.command(&command.to_string()).await.unwrap();
}
#[tokio::test]
async fn missing_html_resources_are_diagnostics_not_empty_image_sources() {
    let (runtime, reader, app, _files) = fixture().await;
    *reader.missing_path.lock().unwrap() = Some(b"gone.png".to_vec());
    reader.contents.lock().unwrap().insert(
        b"missing.html".to_vec(),
        b"<h1>Still visible</h1><img src=gone.png alt=Missing>".to_vec(),
    );
    open_path(&app, "missing.html").await;
    let model = view(&app)["ui_detail"]["model"].clone();
    assert_eq!(model["renderer"], "rsi.file-preview");
    assert_eq!(model["sources"].as_array().unwrap().len(), 2);
    assert!(
        model["data"]["diagnostics"]
            .to_string()
            .contains("gone.png")
    );
    click(&app, "Release snapshot").await;
    assert!(reader.opened.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn rich_preview_sources_keep_version_authority_and_release_resource_tokens() {
    let (runtime, reader, app, _files) = fixture().await;
    reader.contents.lock().unwrap().extend([
        (
            b"report.md".to_vec(),
            b"# Report\n\n![diagram](diagram.svg)".to_vec(),
        ),
        (
            b"diagram.svg".to_vec(),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
        ),
    ]);
    open_path(&app, "report.md").await;
    let detail = view(&app)["ui_detail"].clone();
    assert_eq!(detail["model"]["renderer"], "rsi.file-preview");
    assert_eq!(detail["model"]["sources"].as_array().unwrap().len(), 3);
    assert_eq!(reader.opened.lock().unwrap().len(), 2);
    let ticket = detail["ticket"].as_str().unwrap();
    let source = detail["model"]["sources"][1]["name"].as_str().unwrap();
    let rendered = app.read_ui_source(ticket, source, 0, 65536).await.unwrap();
    assert!(String::from_utf8_lossy(rendered.as_bytes()).contains("<h1>Report</h1>"));
    reader.changed.store(true, Ordering::SeqCst);
    assert!(
        app.read_ui_source(ticket, source, 0, 10).await.is_err(),
        "derived bytes must revalidate their captured file"
    );
    reader.changed.store(false, Ordering::SeqCst);
    click(&app, "Refresh").await;
    assert!(
        app.read_ui_source(ticket, source, 0, 10).await.is_err(),
        "old presentation source must retire"
    );
    assert_eq!(reader.opened.lock().unwrap().len(), 2);
    click(&app, "Release snapshot").await;
    assert!(reader.opened.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn preview_paging_retains_resource_tokens_and_prepared_bytes_until_refresh() {
    let (runtime, reader, app, _files) = fixture().await;
    reader.contents.lock().unwrap().extend([
        (
            b"report.html".to_vec(),
            format!("<link rel=stylesheet href=theme.css>{}", "x".repeat(9000)).into_bytes(),
        ),
        (b"theme.css".to_vec(), b"body { color: green }".to_vec()),
    ]);
    open_path(&app, "report.html").await;
    let original = view(&app)["ui_detail"]["model"].clone();
    let opens = reader.next.load(Ordering::SeqCst);
    let reads = reader.reads.lock().unwrap().len();
    reader
        .contents
        .lock()
        .unwrap()
        .insert(b"theme.css".to_vec(), b"body { color: red }".to_vec());
    for label in ["Next page", "View exact hex", "Previous page"] {
        let detail = click(&app, label).await;
        assert_eq!(
            detail["model"]["data"]["revision"],
            original["data"]["revision"]
        );
        assert_eq!(detail["model"]["sources"], original["sources"]);
        assert_eq!(
            reader.next.load(Ordering::SeqCst),
            opens,
            "paging must not reopen resources"
        );
    }
    assert_eq!(
        reader.reads.lock().unwrap().len(),
        reads + 3,
        "only the three requested pages may be read"
    );
    let detail = view(&app)["ui_detail"].clone();
    let bytes = app
        .read_ui_source(
            detail["ticket"].as_str().unwrap(),
            original["sources"][2]["name"].as_str().unwrap(),
            0,
            65536,
        )
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(bytes.as_bytes()).contains("green"));
    let refreshed = click(&app, "Refresh").await;
    assert_ne!(
        refreshed["model"]["data"]["revision"],
        original["data"]["revision"]
    );
    assert_eq!(reader.next.load(Ordering::SeqCst), opens + 2);
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn cancelling_rich_preview_preparation_releases_already_opened_resources() {
    let (runtime, reader, app, _files) = fixture().await;
    reader.contents.lock().unwrap().extend([
        (
            b"report.html".to_vec(),
            b"<link rel=\"stylesheet\" href=\"theme.css\">".to_vec(),
        ),
        (b"theme.css".to_vec(), b"body { color: green }".to_vec()),
    ]);
    *reader.block_path.lock().unwrap() = Some(b"theme.css".to_vec());
    open_card(&app, "main").await;
    let mut command = ui::button(&view(&app)["ui_detail"], Some("Read file"));
    command["input"]["fields"] = json!({"path":"report.html"});
    let pending = app.command(&command.to_string());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if reader.active.load(Ordering::SeqCst) == 1 && reader.opened.lock().unwrap().len() == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    app.command(r#"{"action":"close_detail"}"#).await.unwrap();
    pending.await.unwrap();
    assert_eq!(reader.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        reader.opened.lock().unwrap().len(),
        1,
        "only the browser's root snapshot may remain"
    );
    assert!(runtime.shutdown().await.is_clean());
    assert!(reader.opened.lock().unwrap().is_empty());
}
