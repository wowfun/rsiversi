//! Read-only Tool contributions using the invocation's pinned Sandbox scope.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_files_protocol::{
    FileKind, Files, FilesBinding, FilesCaller, FilesContract, FilesError,
    MAXIMUM_DIRECTORY_ENTRIES, MAXIMUM_DIRECTORY_PAGE_ENTRIES, MAXIMUM_FILE_PAGE_BYTES,
    MAXIMUM_FILE_PATH_BYTES, PREFERRED_DIRECTORY_PAGE_ENTRIES, PREFERRED_FILE_PAGE_BYTES,
    RelativePath,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolScheduling, ToolTimeoutPolicy, safe_tool_text,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Ordinary staged contribution of read-only filesystem Tools.
#[derive(Clone, Debug, Default)]
pub struct FilesToolsFactory;
#[async_trait]
impl PluginFactory for FilesToolsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Files Tools configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<FilesContract>()
            .requiring_local::<ToolRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let files = plan.local::<FilesContract>()?;
        let registrar = plan.local::<ToolRegistrarContract>()?;
        let lease = registrar
            .register_batch(
                registrations(&files).map_err(|error| MetaError::Activation(error.to_string()))?,
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release Files Tool contribution",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }
}
fn registrations(files: &Arc<dyn Files>) -> rsi_tools_protocol::Result<Vec<ToolRegistration>> {
    [FileKind::File, FileKind::Directory].into_iter().map(|kind| {
        let (name, description, maximum) = match kind {
            FileKind::File => ("file_read", format!("Read one fresh byte page from a regular file beneath the current workspace. path is cwd-relative UTF-8; path_hex is its exact byte alternative. No symlinks or parent traversal. offset is zero-based bytes; maximum defaults to {PREFERRED_FILE_PAGE_BYTES}."), MAXIMUM_FILE_PAGE_BYTES),
            FileKind::Directory => ("directory_list", format!("Read one page of a fresh directory snapshot beneath the current workspace. path or path_hex is cwd-relative; omit both to list cwd. Symlinks and special files are not readable. offset is zero-based entries; maximum defaults to {PREFERRED_DIRECTORY_PAGE_ENTRIES}. Later calls observe a fresh snapshot."), MAXIMUM_DIRECTORY_PAGE_ENTRIES),
        };
        let mut schema = json!({"type":"object", "properties": {
            "path":{"type":"string","maxLength":MAXIMUM_FILE_PATH_BYTES},
            "path_hex":{"type":"string","maxLength":MAXIMUM_FILE_PATH_BYTES * 2},
            "offset":{"type":"integer","minimum":0},
            "maximum":{"type":"integer","minimum":1,"maximum":maximum}
        }, "not":{"required":["path","path_hex"]}, "additionalProperties":false});
        if kind == FileKind::File { schema["anyOf"] = json!([{"required":["path"]},{"required":["path_hex"]}]); }
        Ok(ToolRegistration { definition: ToolDefinition::new(name, description, schema)?.with_scheduling(ToolScheduling::ParallelSafe), timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 }, executor: Arc::new(ReadTool { files: files.clone(), kind }) })
    }).collect()
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: Option<String>,
    path_hex: Option<RelativePath>,
    #[serde(default)]
    offset: u64,
    maximum: Option<usize>,
}
impl Arguments {
    fn page_limit(&self, kind: FileKind) -> rsi_files_protocol::Result<usize> {
        let (preferred, maximum) = match kind {
            FileKind::File => (PREFERRED_FILE_PAGE_BYTES, MAXIMUM_FILE_PAGE_BYTES),
            FileKind::Directory => (
                PREFERRED_DIRECTORY_PAGE_ENTRIES,
                MAXIMUM_DIRECTORY_PAGE_ENTRIES,
            ),
        };
        let page_limit = self.maximum.unwrap_or(preferred);
        if page_limit == 0
            || page_limit > maximum
            || (kind == FileKind::Directory && self.offset > MAXIMUM_DIRECTORY_ENTRIES as u64)
        {
            return Err(FilesError::Invalid);
        }
        Ok(page_limit)
    }

    fn path(&self, kind: FileKind) -> rsi_files_protocol::Result<RelativePath> {
        let path = match (&self.path, &self.path_hex) {
            (Some(path), None) if path == "." && kind == FileKind::Directory => {
                RelativePath::default()
            }
            (Some(path), None) => RelativePath::new(path.as_bytes())?,
            (None, Some(path)) => path.clone(),
            (None, None) if kind == FileKind::Directory => RelativePath::default(),
            (Some(_), Some(_)) | (None, None) => return Err(FilesError::Invalid),
        };
        if kind == FileKind::File && path.as_bytes().is_empty() {
            return Err(FilesError::Invalid);
        }
        Ok(path)
    }
}
struct ReadOwner {
    files: Arc<dyn Files>,
    caller: FilesCaller,
}
impl Drop for ReadOwner {
    fn drop(&mut self) {
        self.files.release_caller(&self.caller);
    }
}
#[derive(Debug)]
struct ReadTool {
    files: Arc<dyn Files>,
    kind: FileKind,
}
#[async_trait]
impl ToolExecutor for ReadTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let arguments: Arguments = match serde_json::from_value(arguments) {
            Ok(value) => value,
            Err(_) => return error(&FilesError::Invalid),
        };
        let relative = match arguments.path(self.kind) {
            Ok(value) => value,
            Err(failure) => return error(&failure),
        };
        let page_limit = match arguments.page_limit(self.kind) {
            Ok(value) => value,
            Err(failure) => return error(&failure),
        };
        let scope = execution.workspace_read().await?;
        let prefix = scope
            .cwd()
            .strip_prefix(scope.workspace())
            .map_err(|_| ToolError::Execution("invalid workspace read scope".into()))?;
        let prefix = prefix
            .to_str()
            .ok_or_else(|| ToolError::Execution("workspace cwd is not UTF-8".into()))?;
        let prefix = match RelativePath::new(prefix.as_bytes()) {
            Ok(value) => value,
            Err(failure) => return error(&failure),
        };
        let mut bytes = prefix.as_bytes().to_vec();
        if !bytes.is_empty() && !relative.as_bytes().is_empty() {
            bytes.push(b'/');
        }
        bytes.extend_from_slice(relative.as_bytes());
        let path = match RelativePath::new(&bytes) {
            Ok(value) => value,
            Err(failure) => return error(&failure),
        };
        let owner = ReadOwner {
            files: self.files.clone(),
            caller: FilesCaller::default(),
        };
        let binding = FilesBinding::new(
            owner.caller.clone(),
            &execution.call_id,
            "workspace-read",
            scope.workspace().to_owned(),
        )
        .map_err(|_| ToolError::Execution("invalid Files invocation binding".into()))?;
        let opened = match self
            .files
            .open(
                binding.clone(),
                path,
                self.kind,
                execution.cancellation.clone(),
            )
            .await
        {
            Ok(value) => value,
            Err(failure) => return error(&failure),
        };
        match self.kind {
            FileKind::File => {
                let page = match self
                    .files
                    .read(
                        binding,
                        opened.token,
                        arguments.offset,
                        page_limit,
                        execution.cancellation.clone(),
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(failure) => return error(&failure),
                };
                file_result(&relative, &page)
            }
            FileKind::Directory => {
                let offset = usize::try_from(arguments.offset).map_err(|_| {
                    ToolError::InvalidInput("directory offset exceeds host limits".into())
                })?;
                let mut page = match self
                    .files
                    .list(
                        binding,
                        opened.token,
                        offset,
                        page_limit,
                        execution.cancellation.clone(),
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(failure) => return error(&failure),
                };
                directory_result(&relative, &prefix, &mut page)
            }
        }
    }
}
fn error(failure: &FilesError) -> rsi_tools_protocol::Result<ToolResult> {
    if *failure == FilesError::Cancelled {
        return Err(ToolError::Cancelled);
    }
    ToolResult::new(
        json!({"error":failure}),
        vec![ToolContent::Text {
            text: failure.to_string(),
        }],
        true,
    )
}

fn file_result(
    relative: &RelativePath,
    page: &rsi_files_protocol::FilePage,
) -> rsi_tools_protocol::Result<ToolResult> {
    let bytes = hex::decode(&page.bytes_hex)
        .map_err(|_| ToolError::Execution("invalid trusted Files byte page".into()))?;
    let text = safe_tool_text(&bytes);
    let changed = text.as_bytes() != bytes;
    let next = page.offset + bytes.len() as u64;
    let value = json!({"path_hex":relative,"offset":page.offset,"total":page.total,"next_offset":next,"has_more":next < page.total,"text":text,"text_changed":changed,"bytes_hex":page.bytes_hex});
    let mut display = format!(
        "File {:?}, bytes {}..{next}/{} (fresh snapshot)\n{text}",
        safe_tool_text(relative.as_bytes()),
        page.offset,
        page.total
    );
    if changed {
        display.push_str("\nExact bytes (hex): ");
        display.push_str(&page.bytes_hex);
    }
    ToolResult::new(value, vec![ToolContent::Text { text: display }], false)
}

fn directory_result(
    relative: &RelativePath,
    prefix: &RelativePath,
    page: &mut rsi_files_protocol::DirectoryPage,
) -> rsi_tools_protocol::Result<ToolResult> {
    for entry in &mut page.entries {
        if !prefix.as_bytes().is_empty() {
            let bytes = entry
                .path
                .as_bytes()
                .strip_prefix(prefix.as_bytes())
                .and_then(|rest| rest.strip_prefix(b"/"))
                .ok_or_else(|| {
                    ToolError::Execution("invalid trusted Files directory prefix".into())
                })?;
            entry.path = RelativePath::new(bytes)
                .map_err(|_| ToolError::Execution("invalid trusted Files directory path".into()))?;
        }
    }
    let next = page.offset + page.entries.len();
    let value = json!({"path_hex":relative,"offset":page.offset,"total":page.total,"next_offset":next,"has_more":next < page.total,"entries":page.entries,"snapshot":"fresh_per_invocation"});
    let display = serde_json::to_string_pretty(&value)
        .map_err(|error| ToolError::Execution(error.to_string()))?;
    ToolResult::new(
        value,
        vec![ToolContent::Text {
            text: safe_tool_text(display.as_bytes()),
        }],
        false,
    )
}
