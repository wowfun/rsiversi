//! Read-only Tool contributions using the invocation's pinned execution provider.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_files_protocol::{
    FileKind, Files, FilesBinding, FilesCaller, FilesContract, FilesError,
    MAXIMUM_DIRECTORY_PAGE_ENTRIES, MAXIMUM_FILE_PAGE_BYTES, MAXIMUM_FILE_PATH_BYTES,
    PREFERRED_DIRECTORY_PAGE_ENTRIES, PREFERRED_FILE_PAGE_BYTES, PathRead, ReadPage, ReadPath,
    RelativePath,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_tools_protocol::{
    ToolContent, ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolRunPolicy, ToolScheduling, safe_tool_text,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

mod present;
pub use present::{MAXIMUM_PRESENTED_FILES_BYTES, PresentedFile, PresentedFilesV1};

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
    let mut entries = [FileKind::File, FileKind::Directory].into_iter().map(|kind| {
        let (name, description, maximum) = match kind {
            FileKind::File => ("file_read", format!("Read one fresh byte page from a regular file on the execution host. path is absolute or cwd-relative UTF-8; path_hex is its exact byte alternative. Parents and symlinks are resolved by the filesystem. offset is zero-based bytes; maximum defaults to {PREFERRED_FILE_PAGE_BYTES}."), MAXIMUM_FILE_PAGE_BYTES),
            FileKind::Directory => ("directory_list", format!("Read one page of a fresh directory snapshot on the execution host. path or path_hex is absolute or cwd-relative; omit both to list cwd. Parents and symlinks are resolved by the filesystem. offset is zero-based entries; maximum defaults to {PREFERRED_DIRECTORY_PAGE_ENTRIES}. Later calls observe a fresh snapshot."), MAXIMUM_DIRECTORY_PAGE_ENTRIES),
        };
        let mut schema = json!({"type":"object", "properties": {
            "path":{"type":"string","maxLength":MAXIMUM_FILE_PATH_BYTES},
            "path_hex":{"type":"string","maxLength":MAXIMUM_FILE_PATH_BYTES * 2},
            "offset":{"type":"integer","minimum":0},
            "maximum":{"type":"integer","minimum":1,"maximum":maximum}
        }, "not":{"required":["path","path_hex"]}, "additionalProperties":false});
        if kind == FileKind::File { schema["anyOf"] = json!([{"required":["path"]},{"required":["path_hex"]}]); }
        Ok(ToolRegistration { output: None, definition: ToolDefinition::new(name, description, schema)?.with_scheduling(ToolScheduling::ParallelSafe).with_program_role(rsi_tools_protocol::ToolProgramRole::Callable), run_policy: ToolRunPolicy::execution(std::time::Duration::from_secs(30)), executor: Arc::new(ReadTool { files: files.clone(), kind }) })
    }).collect::<rsi_tools_protocol::Result<Vec<_>>>()?;
    entries.push(present::registration(files.clone())?);
    Ok(entries)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: Option<String>,
    path_hex: Option<ReadPath>,
    #[serde(default)]
    offset: u64,
    maximum: Option<usize>,
}
impl Arguments {
    fn page_limit(&self, kind: FileKind) -> usize {
        self.maximum.unwrap_or(match kind {
            FileKind::File => PREFERRED_FILE_PAGE_BYTES,
            FileKind::Directory => PREFERRED_DIRECTORY_PAGE_ENTRIES,
        })
    }

    fn path(&self, kind: FileKind) -> rsi_files_protocol::Result<ReadPath> {
        match (&self.path, &self.path_hex) {
            (Some(path), None) => ReadPath::new(path.as_bytes()),
            (None, Some(path)) => Ok(path.clone()),
            (None, None) if kind == FileKind::Directory => ReadPath::new(b"."),
            _ => Err(FilesError::Invalid),
        }
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
        let page_limit = arguments.page_limit(self.kind);
        let request = match PathRead::new(
            execution.policy().cwd.clone(),
            relative,
            self.kind,
            arguments.offset,
            page_limit,
        ) {
            Ok(request) => request,
            Err(failure) => return error(&failure),
        };
        let page = match execution
            .files(&self.files)?
            .read_path(request, execution.cancellation.clone())
            .await
        {
            Ok(page) => page,
            Err(failure) => return error(&failure),
        };
        match page.page {
            ReadPage::File(file) => file_result(&page.path, &file),
            ReadPage::Directory(directory) => directory_result(&page.path, directory),
        }
    }
}
fn error(failure: &FilesError) -> rsi_tools_protocol::Result<ToolResult> {
    if *failure == FilesError::OutcomeUnknown {
        return Err(ToolError::OutcomeUnknown);
    }
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
    relative: &ReadPath,
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
    path: &ReadPath,
    page: rsi_files_protocol::DirectoryPage,
) -> rsi_tools_protocol::Result<ToolResult> {
    let mut entries = Vec::with_capacity(page.entries.len());
    let mut bytes = 256 + path.as_bytes().len() * 2;
    for entry in page.entries {
        let mut absolute = path.as_bytes().to_vec();
        if !absolute.ends_with(b"/") {
            absolute.push(b'/');
        }
        absolute.extend_from_slice(entry.path.as_bytes());
        let cost = absolute.len() * 2 + entry.name.len() * 6 + 128;
        if absolute.len() > MAXIMUM_FILE_PATH_BYTES
            || bytes + cost > rsi_files_protocol::MAXIMUM_DIRECTORY_PAGE_BYTES
        {
            if entries.is_empty() {
                return error(&FilesError::Capacity);
            }
            break;
        }
        bytes += cost;
        entries.push(json!({"name":entry.name,"path":hex::encode(absolute),"kind":entry.kind}));
    }
    let next = page.offset + entries.len();
    let value = json!({"path_hex":path,"offset":page.offset,"total":page.total,"next_offset":next,"has_more":next < page.total,"entries":entries,"snapshot":"fresh_per_invocation"});
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
