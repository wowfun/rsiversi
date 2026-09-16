use super::{
    Browser, CancellationToken, DIRECTORY_PAGE_ENTRIES, Deserialize, FILE_PAGE_BYTES, FileKind,
    FilesError, RelativePath, Result, Serialize, UiError, action_error, next_revision, view,
};

/// Finite human picker operation; paging never reopens a changed file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FilePickerRequest {
    /// Opens one workspace-relative path in this actual Session.
    Open {
        /// Exact bytes, encoded as hex on the wire.
        path: RelativePath,
        /// Regular file or directory.
        file_kind: FileKind,
    },
    /// Reads another page of the retained snapshot.
    Page {
        /// Exact process-unique browser revision.
        revision: String,
        /// Decimal byte or entry offset.
        offset: String,
        /// Display file bytes as hex.
        hex: bool,
    },
    /// Releases only the snapshot displayed by this picker.
    Release {
        /// Exact process-unique browser revision.
        revision: String,
    },
}
/// Selectable directory entry; unsupported files have no open operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChoice {
    /// Exact path bytes.
    pub path: RelativePath,
    /// Bounded display name, never insertion text.
    pub name: String,
    /// None for links and special files.
    pub kind: Option<FileKind>,
}
/// Token-free projection of one bounded browser snapshot page.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePickerPage {
    /// Exact browser revision, with no JavaScript integer rounding.
    pub revision: String,
    /// Exact workspace-relative bytes.
    pub path: RelativePath,
    /// Snapshot type.
    pub kind: FileKind,
    /// Current byte or entry offset.
    pub offset: String,
    /// Next byte or entry offset.
    pub next_offset: String,
    /// Total bytes or entries.
    pub total: String,
    /// Whether the same snapshot has another page.
    pub more: bool,
    /// File-only canonical insertion text.
    pub locator: Option<String>,
    /// At most sixteen directory entries.
    pub entries: Vec<FileChoice>,
    /// Sanitized file bytes or exact hex, at most one 4 KiB raw page.
    pub text: String,
}
/// Canonical human-visible file locator, independent of display truncation.
#[expect(
    clippy::missing_panics_doc,
    reason = "Validated paths and UTF-8 strings have infallible JSON representations"
)]
pub fn file_locator(path: &RelativePath) -> String {
    match std::str::from_utf8(path.as_bytes()) {
        Ok(path) => format!(
            "@{}",
            serde_json::to_string(path).expect("UTF-8 string serialization")
        ),
        Err(_) => format!(
            "@path_hex:{}",
            serde_json::to_value(path)
                .expect("validated path")
                .as_str()
                .expect("hex path")
        ),
    }
}
impl Browser {
    /// Reuses the surface's snapshot, admission and cancellation owner for a composer picker.
    pub async fn pick(
        &self,
        request: FilePickerRequest,
        cancel: CancellationToken,
    ) -> Result<Option<FilePickerPage>> {
        let (expected, offset, hex) = match &request {
            FilePickerRequest::Open { .. } => (None, 0, false),
            FilePickerRequest::Page {
                revision,
                offset,
                hex,
            } => {
                let parsed = offset
                    .parse::<u64>()
                    .map_err(|_| UiError::Invalid("Invalid file offset".into()))?;
                if parsed.to_string() != *offset {
                    return Err(UiError::Invalid("Noncanonical file offset".into()));
                }
                (Some(revision), parsed, *hex)
            }
            FilePickerRequest::Release { revision } => (Some(revision), 0, false),
        };
        let _permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| UiError::Capacity)?;
        if self.stop.is_cancelled() || cancel.is_cancelled() {
            return Err(UiError::Retired);
        }
        let revision = {
            let mut state = self.state.lock().expect("Files browser state poisoned");
            if expected.is_some_and(|expected| *expected != state.revision.to_string()) {
                return Err(UiError::Invalid(
                    "This Files view changed; reopen it".into(),
                ));
            }
            state.revision = next_revision()?;
            state.revision
        };
        tokio::select! {biased;
            () = self.stop.cancelled() => Err(UiError::Retired),
            () = cancel.cancelled() => Err(UiError::Retired),
            result = async {
                match request {
                    FilePickerRequest::Open {path,file_kind} => self.open(path,file_kind).await?,
                    FilePickerRequest::Release {..} => {self.release().await;return Ok(None);}
                    FilePickerRequest::Page {..} => {},
                }
                self.picker_page(revision,offset,hex).await.map(Some)
            } => result,
        }
    }
    async fn picker_page(&self, revision: u64, offset: u64, hex: bool) -> Result<FilePickerPage> {
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
        let mut page = FilePickerPage {
            revision: revision.to_string(),
            path: opened.file.path.clone(),
            kind: opened.file.kind,
            offset: offset.to_string(),
            next_offset: offset.to_string(),
            total: "0".into(),
            more: false,
            locator: None,
            entries: Vec::new(),
            text: String::new(),
        };
        match opened.file.kind {
            FileKind::Directory => {
                let result = self
                    .files
                    .list(
                        opened.target,
                        opened.file,
                        usize::try_from(offset).map_err(|_| {
                            UiError::Invalid("Directory offset is too large".into())
                        })?,
                        DIRECTORY_PAGE_ENTRIES,
                    )
                    .await
                    .map_err(action_error)?;
                let next = result.offset + result.entries.len();
                page.next_offset = next.to_string();
                page.total = result.total.to_string();
                page.more = next < result.total;
                page.entries = result
                    .entries
                    .into_iter()
                    .map(|entry| FileChoice {
                        path: entry.path,
                        name: view::preview(entry.name.as_bytes(), 256),
                        kind: entry.kind,
                    })
                    .collect();
            }
            FileKind::File => {
                let result = self
                    .files
                    .read(opened.target, opened.file, offset, FILE_PAGE_BYTES)
                    .await
                    .map_err(action_error)?;
                let bytes = crate::view::decode(&result.bytes_hex);
                let next = offset + bytes.len() as u64;
                page.locator = Some(file_locator(&page.path));
                page.next_offset = next.to_string();
                page.total = result.total.to_string();
                page.more = next < result.total;
                page.text = if hex {
                    rsi_conversation::hex_window(&bytes, offset)
                        .ok_or_else(|| UiError::Invalid("File offset overflow".into()))?
                } else {
                    rsi_tools_protocol::safe_tool_text(&bytes)
                };
            }
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locators_preserve_complete_paths_and_do_not_interpret_quotes_or_non_utf8() {
        let path = RelativePath::new("folder/a \"界\".txt".as_bytes()).unwrap();
        assert_eq!(file_locator(&path), "@\"folder/a \\\"界\\\".txt\"");
        assert_eq!(
            file_locator(&RelativePath::new(b"f/\xff.txt").unwrap()),
            "@path_hex:662fff2e747874"
        );
    }
}
