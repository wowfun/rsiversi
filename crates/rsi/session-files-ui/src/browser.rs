use crate::{
    DIRECTORY_PAGE_ENTRIES, FILE_PAGE_BYTES, FilesBrowserContract, INPUT_PATH_BYTES, view,
};
use futures_util::future::BoxFuture;
use rsi_files_protocol::{FileKind, FilesError, OpenedFile, RelativePath};
use rsi_session_files::{SessionFiles, SessionFilesError};
use rsi_session_protocol::{SessionHandle, SessionTarget};
use rsi_ui::{ActionInput, ActionTarget, Result, UiAction, UiError, UiView};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub(crate) struct Opened {
    pub target: SessionTarget,
    pub file: OpenedFile,
}
#[derive(Debug)]
pub(crate) struct State {
    pub revision: u64,
    pub opened: Option<Opened>,
}
/// Opaque browser state owned by one actual Session surface.
#[derive(Debug)]
pub struct Browser {
    session: Arc<dyn SessionHandle>,
    files: Arc<dyn SessionFiles>,
    pub(crate) state: Mutex<State>,
    slot: Arc<Semaphore>,
    stop: CancellationToken,
}
impl Browser {
    pub(crate) fn new(
        session: Arc<dyn SessionHandle>,
        files: Arc<dyn SessionFiles>,
    ) -> Result<Self> {
        Ok(Self {
            session,
            files,
            state: Mutex::new(State {
                revision: next_revision()?,
                opened: None,
            }),
            slot: Arc::new(Semaphore::new(1)),
            stop: CancellationToken::new(),
        })
    }
    pub(crate) async fn close(&self) {
        self.stop.cancel();
        let _permit = self
            .slot
            .acquire()
            .await
            .expect("browser slot stays open during drain");
        self.release().await;
    }
    async fn release(&self) {
        let opened = self
            .state
            .lock()
            .expect("Files browser state poisoned")
            .opened
            .take();
        if let Some(opened) = opened {
            // A disconnected or expired remote binding cannot promise early release.
            // The server's bounded token lease remains the final owner.
            let _ = self.files.release(opened.target, opened.file.token).await;
        }
    }
    async fn target(&self) -> Result<SessionTarget> {
        let header = self
            .session
            .header()
            .await
            .map_err(|e| UiError::Action(e.to_string()))?;
        Ok(SessionTarget {
            session_id: header.session_id().clone(),
            header_key: header
                .fingerprint()
                .map_err(|e| UiError::Action(e.to_string()))?,
        })
    }
    async fn open(&self, path: RelativePath, kind: FileKind) -> Result<()> {
        let target = self.target().await?;
        self.release().await;
        let file = self
            .files
            .open(target.clone(), path, kind)
            .await
            .map_err(action_error)?;
        self.state
            .lock()
            .expect("Files browser state poisoned")
            .opened = Some(Opened { target, file });
        Ok(())
    }
    async fn page(&self, offset: u64, hex: bool, revision: u64) -> Result<UiView> {
        let opened = self
            .state
            .lock()
            .expect("Files browser state poisoned")
            .opened
            .clone()
            .ok_or_else(|| UiError::Invalid("Open a file or directory first".into()))?;
        if self.target().await? != opened.target {
            return Err(action_error(FilesError::Changed.into()));
        }
        match opened.file.kind {
            FileKind::File => {
                let page = self
                    .files
                    .read(opened.target, opened.file.clone(), offset, FILE_PAGE_BYTES)
                    .await
                    .map_err(action_error)?;
                Ok(view::file(revision, &opened.file, &page, hex))
            }
            FileKind::Directory => {
                let page = self
                    .files
                    .list(
                        opened.target,
                        opened.file.clone(),
                        usize::try_from(offset).map_err(|_| {
                            UiError::Invalid("Directory offset is too large".into())
                        })?,
                        DIRECTORY_PAGE_ENTRIES,
                    )
                    .await
                    .map_err(action_error)?;
                Ok(view::directory(revision, &opened.file, page))
            }
        }
    }
    async fn perform(
        &self,
        operation: Operation,
        fields: BTreeMap<String, String>,
        revision: u64,
    ) -> Result<UiView> {
        match operation {
            Operation::Input { kind } => {
                let text = fields.get("path").map_or("", String::as_str);
                let bytes = if text == "." && kind == FileKind::Directory {
                    b""
                } else {
                    text.as_bytes()
                };
                let path = RelativePath::new(bytes).map_err(|e| action_error(e.into()))?;
                self.open(path, kind).await?;
            }
            Operation::Open { path, kind } => self.open(path, kind).await?,
            Operation::Page { offset, hex } => return self.page(offset, hex, revision).await,
            Operation::Refresh => {
                let file = self
                    .state
                    .lock()
                    .expect("Files browser state poisoned")
                    .opened
                    .as_ref()
                    .map(|o| o.file.clone())
                    .ok_or_else(|| UiError::Invalid("Open a file or directory first".into()))?;
                self.open(file.path, file.kind).await?;
            }
            Operation::Release => {
                self.release().await;
                return Ok(view::initial(revision, false));
            }
        }
        self.page(0, false, revision).await
    }
    async fn invoke(&self, target: ActionTarget, input: ActionInput) -> Result<UiView> {
        if input.fields.len() > 1
            || input
                .fields
                .iter()
                .any(|(key, value)| key != "path" || value.len() > INPUT_PATH_BYTES)
        {
            return Err(UiError::Invalid(format!(
                "Files accepts one path of at most {INPUT_PATH_BYTES} UTF-8 bytes"
            )));
        }
        let request: Request =
            serde_json::from_value(input.value).map_err(|e| UiError::Invalid(e.to_string()))?;
        let _permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| UiError::Capacity)?;
        if self.stop.is_cancelled() || target.is_cancelled() {
            return Err(UiError::Retired);
        }
        let revision = {
            let mut state = self.state.lock().expect("Files browser state poisoned");
            if request.revision != state.revision.to_string() {
                return Err(UiError::Invalid(
                    "This Files view changed; reopen it".into(),
                ));
            }
            state.revision = next_revision()?;
            state.revision
        };
        let result = tokio::select! { biased;
            () = self.stop.cancelled() => return Err(UiError::Retired),
            () = target.cancelled() => return Err(UiError::Retired),
            () = target.view_closed() => return Err(UiError::Retired),
            result = self.perform(request.operation, input.fields, revision) => result,
        };
        match result {
            Ok(view) => Ok(view),
            Err(error) => {
                let opened = self
                    .state
                    .lock()
                    .expect("Files browser state poisoned")
                    .opened
                    .is_some();
                Ok(view::failure(revision, opened, &error.to_string()))
            }
        }
    }
}
fn next_revision() -> Result<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        value.checked_add(1)
    })
    .map_err(|_| UiError::Capacity)
}
fn action_error(error: SessionFilesError) -> UiError {
    let text = match error {
        SessionFilesError::Files(FilesError::Changed) => {
            "The file or directory changed. Refresh to open a new snapshot.".into()
        }
        SessionFilesError::Files(FilesError::Unavailable) => {
            "The Session, file or snapshot is unavailable. Refresh or select another item.".into()
        }
        other => other.to_string(),
    };
    UiError::Action(text)
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub revision: String,
    pub operation: Operation,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    Input {
        #[serde(rename = "file_kind")]
        kind: FileKind,
    },
    Open {
        path: RelativePath,
        #[serde(rename = "file_kind")]
        kind: FileKind,
    },
    Page {
        #[serde(with = "decimal_offset")]
        offset: u64,
        hex: bool,
    },
    Refresh,
    Release,
}
mod decimal_offset {
    use serde::{Deserialize, Deserializer, Serializer};
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "Serde with serializer signature"
    )]
    pub fn serialize<S: Serializer>(
        value: &u64,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<u64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
#[derive(Debug)]
pub(crate) struct Browse;
impl UiAction for Browse {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            let browser = target
                .context()
                .lookup_local::<FilesBrowserContract>()
                .ok_or(UiError::Retired)?;
            browser.invoke(target, input).await
        })
    }
}
