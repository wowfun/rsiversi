//! Files-backed rich models. Every byte read retains its actual Session and version.
use crate::{
    browser::{Browser, Opened, action_error},
    markup, view,
};
use rsi_files_protocol::{FileKind, OpenedFile, RelativePath};
use rsi_ui::{ActionTarget, ModelSchema, ModelSource, Result, UiElement, UiError, UiModel, UiView};
use std::{
    collections::BTreeSet,
    sync::{Arc, LazyLock},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const TEXT_BYTES: u64 = 1024 * 1024;
const PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
// Charge raw, parsed and encoded scratch before reading. Retained leases outlive waiters.
static PREVIEW_BYTES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(256 * 1024 * 1024)));
#[derive(Debug)]
pub(crate) struct Source {
    pub name: String,
    pub mime: String,
    pub file: OpenedFile,
    pub data: Option<Arc<Vec<u8>>>,
}
impl Source {
    fn length(&self) -> u64 {
        self.data
            .as_ref()
            .map_or(self.file.length, |data| data.len() as u64)
    }
}
#[derive(Debug)]
pub(crate) struct Preview {
    pub revision: u64,
    pub opened: Opened,
    pub kind: &'static str,
    pub label: String,
    pub sources: Vec<Source>,
    pub diagnostics: Vec<String>,
    leases: Vec<OwnedSemaphorePermit>,
}
fn reserve(bytes: u64) -> Result<OwnedSemaphorePermit> {
    PREVIEW_BYTES
        .clone()
        .try_acquire_many_owned(u32::try_from(bytes).map_err(|_| UiError::Capacity)?)
        .map_err(|_| UiError::Capacity)
}
pub(crate) fn media(path: &RelativePath) -> (&'static str, &'static str) {
    let extension = path
        .as_bytes()
        .rsplit(|b| *b == b'.')
        .next()
        .unwrap_or_default();
    match String::from_utf8_lossy(extension)
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => ("markdown", "text/markdown"),
        "html" | "htm" => ("html", "text/html"),
        "png" => ("image", "image/png"),
        "jpg" | "jpeg" => ("image", "image/jpeg"),
        "gif" => ("image", "image/gif"),
        "webp" => ("image", "image/webp"),
        "bmp" => ("image", "image/bmp"),
        "ico" => ("image", "image/x-icon"),
        "svg" => ("image", "image/svg+xml"),
        "css" => ("code", "text/css"),
        "js" => ("code", "text/javascript"),
        "woff" => ("binary", "font/woff"),
        "woff2" => ("binary", "font/woff2"),
        "ttf" => ("binary", "font/ttf"),
        "otf" => ("binary", "font/otf"),
        "mp4" | "webm" | "mov" | "mkv" | "avi" | "pdf" | "zip" => {
            ("binary", "application/octet-stream")
        }
        _ => ("code", "text/plain"),
    }
}
impl Preview {
    fn matches(&self, opened: &Opened) -> bool {
        self.opened.target == opened.target && self.opened.file.token == opened.file.token
    }
    fn model(&self, fallback: UiView) -> Result<UiModel> {
        let prefix = format!("preview.{}.", self.revision);
        let mut model = UiModel::standard(fallback)?;
        model.renderer = "rsi.file-preview".into();
        model.schema = ModelSchema {
            name: "rsi.file-preview".into(),
            version: 1,
        };
        model.sources = self
            .sources
            .iter()
            .map(|source| ModelSource {
                name: format!("{prefix}{}", source.name),
                title: source.name.clone(),
                media_type: source.mime.clone(),
            })
            .collect();
        model.data = serde_json::json!({"kind":self.kind,"label":self.label,"revision":self.revision.to_string(),"diagnostics":self.diagnostics,
            "sources":self.sources.iter().map(|source|serde_json::json!({"name":source.name,"source":format!("{prefix}{}",source.name),"bytes":source.length(),"media_type":source.mime})).collect::<Vec<_>>()});
        model.validate()?;
        Ok(model)
    }
}
impl Browser {
    async fn full(&self, opened: &Opened, file: &OpenedFile) -> Result<Vec<u8>> {
        let mut bytes =
            Vec::with_capacity(usize::try_from(file.length).map_err(|_| UiError::Capacity)?);
        while (bytes.len() as u64) < file.length {
            let page = self
                .files
                .read(
                    opened.target.clone(),
                    file.clone(),
                    bytes.len() as u64,
                    64 * 1024,
                )
                .await
                .map_err(action_error)?;
            if page.offset != bytes.len() as u64
                || page.total != file.length
                || page.bytes_hex.is_empty()
            {
                return Err(UiError::Invalid("Invalid preview page".into()));
            }
            bytes.extend(view::decode(&page.bytes_hex));
        }
        Ok(bytes)
    }
    fn empty_preview(revision: u64, opened: &Opened) -> Result<Preview> {
        let (kind, mime) = media(&opened.file.path);
        if kind == "binary" {
            return Err(UiError::Invalid(
                "Preview unavailable for this format; use text or hex".into(),
            ));
        }
        let maximum = if kind == "image" {
            PACKAGE_BYTES
        } else {
            TEXT_BYTES
        };
        if opened.file.length > maximum {
            return Err(UiError::Invalid(format!(
                "Complete preview exceeds {} MiB; use paged Source or hex",
                maximum / 1024 / 1024
            )));
        }
        let preview = Preview {
            revision,
            label: view::preview(opened.file.path.as_bytes(), 1024),
            kind,
            opened: opened.clone(),
            sources: vec![Source {
                name: "main".into(),
                mime: mime.into(),
                file: opened.file.clone(),
                data: None,
            }],
            diagnostics: vec![],
            leases: vec![reserve(opened.file.length.saturating_mul(6).max(1))?],
        };
        Ok(preview)
    }
    async fn prepare_preview(&self, preview: &mut Preview) -> Result<()> {
        let opened = preview.opened.clone();
        let kind = preview.kind;
        if kind == "image" {
            return Ok(());
        }
        let raw = self.full(&opened, &opened.file).await?;
        let text = std::str::from_utf8(&raw)
            .map_err(|_| UiError::Invalid("Not UTF-8 text; use exact hex".into()))?;
        if kind == "code" {
            preview.sources[0].data = Some(Arc::new(raw));
            return Ok(());
        }
        let mut resources = markup::Resources::default();
        let rendered = if kind == "markdown" {
            markup::markdown(text, &opened.file.path, &mut resources)
        } else {
            markup::html(text, &opened.file.path, &mut resources)
        }
        .map_err(UiError::Invalid)?;
        preview.sources.push(Source {
            name: "document".into(),
            mime: "text/html".into(),
            file: opened.file.clone(),
            data: Some(Arc::new(rendered.into_bytes())),
        });
        self.prepare_resources(preview, &mut resources).await?;
        preview.diagnostics = resources.diagnostics;
        Ok(())
    }
    async fn prepare_resources(
        &self,
        preview: &mut Preview,
        resources: &mut markup::Resources,
    ) -> Result<()> {
        let opened = preview.opened.clone();
        let mut total = opened.file.length;
        let mut index = 0;
        while index < resources.entries.len() {
            let reference = resources.entries[index].clone();
            let path = match reference.location {
                markup::Location::Embedded { mime, bytes } => {
                    total = total
                        .checked_add(bytes.len() as u64)
                        .ok_or(UiError::Capacity)?;
                    if total > PACKAGE_BYTES {
                        return Err(UiError::Capacity);
                    }
                    preview
                        .leases
                        .push(reserve((bytes.len() as u64).saturating_mul(6).max(1))?);
                    preview.sources.push(Source {
                        name: format!("asset-{index}"),
                        mime,
                        file: opened.file.clone(),
                        data: Some(Arc::new(bytes)),
                    });
                    index += 1;
                    continue;
                }
                markup::Location::File(path) => path,
            };
            let file = match self
                .files
                .open(opened.target.clone(), path.clone(), FileKind::File)
                .await
            {
                Ok(file) => file,
                Err(error) => {
                    resources.diagnostic(&format!(
                        "Resource {}: {}",
                        view::preview(path.as_bytes(), 256),
                        action_error(error)
                    ));
                    index += 1;
                    continue;
                }
            };
            // Register ownership before any fallible preparation so failure releases it.
            let (_, mime) = media(&file.path);
            preview.sources.push(Source {
                name: format!("asset-{index}"),
                mime: mime.into(),
                file: file.clone(),
                data: None,
            });
            total = total.checked_add(file.length).ok_or(UiError::Capacity)?;
            if total > PACKAGE_BYTES || file.length > 4 * 1024 * 1024 {
                return Err(UiError::Invalid(
                    "HTML/Markdown resource package exceeds 32 MiB or a resource exceeds 4 MiB"
                        .into(),
                ));
            }
            preview
                .leases
                .push(reserve(file.length.saturating_mul(6).max(1))?);
            if reference.kind == "css" {
                let raw = self.full(&opened, &file).await?;
                let text = std::str::from_utf8(&raw)
                    .map_err(|_| UiError::Invalid("CSS is not UTF-8".into()))?;
                let rendered = markup::css(text, &file.path, resources);
                if rendered.len() > 8 * 1024 * 1024 {
                    return Err(UiError::Capacity);
                }
                let source = preview.sources.last_mut().expect("inserted source");
                source.mime = "text/css".into();
                source.data = Some(Arc::new(rendered.into_bytes()));
            } else if reference.kind == "script" {
                preview.sources.last_mut().expect("inserted source").mime =
                    "text/javascript".into();
            }
            index += 1;
        }
        Ok(())
    }
    pub(crate) async fn release_preview(&self, preview: &Preview) {
        let mut released = BTreeSet::new();
        for source in &preview.sources {
            if source.file.token != preview.opened.file.token
                && released.insert(serde_json::to_string(&source.file.token).expect("token"))
            {
                let _ = self
                    .files
                    .release(preview.opened.target.clone(), source.file.token.clone())
                    .await;
            }
        }
    }
    pub(crate) async fn preview_model(
        &self,
        target: ActionTarget,
        mut fallback: UiView,
    ) -> Result<UiModel> {
        let (revision, opened, cached) = {
            let state = self.state.lock().expect("Files state");
            (state.revision, state.opened.clone(), state.preview.clone())
        };
        let Some(opened) = opened.filter(|opened| opened.file.kind == FileKind::File) else {
            return Ok(UiModel::standard(fallback)?);
        };
        if let Some(preview) = cached.filter(|preview| preview.matches(&opened)) {
            return preview.model(fallback);
        }
        let mut preview = match Self::empty_preview(revision, &opened) {
            Ok(preview) => preview,
            Err(error) => {
                fallback.elements.insert(
                    0,
                    UiElement::Text {
                        text: error.to_string(),
                    },
                );
                return Ok(UiModel::standard(fallback)?);
            }
        };
        let prepared = tokio::select! {biased;
            ()=self.stop.cancelled()=>Err(UiError::Retired),
            ()=target.cancelled()=>Err(UiError::Retired),
            ()=target.view_closed()=>Err(UiError::Retired),
            prepared=self.prepare_preview(&mut preview)=>prepared,
        };
        match prepared {
            Ok(()) => {
                let preview = Arc::new(preview);
                let previous = {
                    let mut state = self.state.lock().expect("Files state");
                    debug_assert_eq!(state.revision, revision);
                    state.preview.replace(preview.clone())
                };
                if let Some(old) = previous {
                    self.release_preview(&old).await;
                }
                preview.model(fallback)
            }
            Err(error) => {
                self.release_preview(&preview).await;
                if matches!(error, UiError::Retired) {
                    return Err(error);
                }
                fallback.elements.insert(
                    0,
                    UiElement::Text {
                        text: error.to_string(),
                    },
                );
                Ok(UiModel::standard(fallback)?)
            }
        }
    }
    pub(crate) async fn preview_source(
        &self,
        target: ActionTarget,
        name: String,
        offset: u64,
        maximum: usize,
    ) -> Result<Vec<u8>> {
        let _slot = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| UiError::Capacity)?;
        let preview = {
            let state = self.state.lock().expect("Files state");
            state.preview.clone().filter(|preview| {
                state
                    .opened
                    .as_ref()
                    .is_some_and(|opened| preview.matches(opened))
            })
        }
        .ok_or(UiError::Retired)?;
        let short = name
            .strip_prefix(&format!("preview.{}.", preview.revision))
            .ok_or(UiError::Retired)?;
        let source = preview
            .sources
            .iter()
            .find(|source| source.name == short)
            .ok_or(UiError::Retired)?;
        if maximum == 0 || maximum > 64 * 1024 || offset > source.length() {
            return Err(UiError::Invalid("Invalid preview window".into()));
        }
        let work = async {
            if self.target().await? != preview.opened.target {
                return Err(UiError::Retired);
            }
            if let Some(data) = &source.data {
                // Validate current authorization and captured file version before serving derived bytes.
                self.files
                    .read(preview.opened.target.clone(), source.file.clone(), 0, 1)
                    .await
                    .map_err(action_error)?;
                let start = usize::try_from(offset).map_err(|_| UiError::Capacity)?;
                Ok(data[start..data.len().min(start + maximum)].to_vec())
            } else {
                let page = self
                    .files
                    .read(
                        preview.opened.target.clone(),
                        source.file.clone(),
                        offset,
                        maximum,
                    )
                    .await
                    .map_err(action_error)?;
                Ok(view::decode(&page.bytes_hex))
            }
        };
        tokio::select! {biased;()=self.stop.cancelled()=>Err(UiError::Retired),()=target.cancelled()=>Err(UiError::Retired),()=target.view_closed()=>Err(UiError::Retired),result=work=>result}
    }
}
pub(crate) fn source(
    target: ActionTarget,
    name: String,
    offset: u64,
    maximum: usize,
) -> futures_util::future::BoxFuture<'static, Result<Vec<u8>>> {
    Box::pin(async move {
        let browser = target
            .context()
            .lookup_local::<crate::FilesBrowserContract>()
            .ok_or(UiError::Retired)?;
        browser.preview_source(target, name, offset, maximum).await
    })
}
