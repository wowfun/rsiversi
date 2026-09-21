use super::{
    Arc, Arguments, Deserialize, FileKind, Files, FilesBinding, FilesCaller, FilesError,
    MAXIMUM_FILE_PATH_BYTES, ReadOwner, RelativePath, ToolContent, ToolDefinition, ToolError,
    ToolExecution, ToolExecutor, ToolRegistration, ToolResult, ToolScheduling, ToolTimeoutPolicy,
    Value, async_trait, error, json, safe_tool_text, workspace_path,
};
use serde::Serialize;

/// Maximum encoded canonical file declaration, independent of file contents.
pub const MAXIMUM_PRESENTED_FILES_BYTES: usize = 160 * 1024;

/// One existing file declared by the tool, relative to its pinned cwd.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresentedFile {
    /// Exact relative path bytes, encoded as hex on the wire.
    pub path_hex: RelativePath,
    /// Human explanation of the file's role, at most 256 UTF-8 bytes.
    pub description: String,
    /// Length observed when the tool opened the regular file.
    pub length: u64,
}

/// Validated version-one declaration. This contains no captured file contents.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresentedFilesV1 {
    /// Exact supported version, one.
    pub version: u32,
    /// One through eight existing regular files.
    pub files: Vec<PresentedFile>,
}
impl PresentedFilesV1 {
    /// Decodes recorded JSON after bounding its allocating fields.
    pub fn decode(value: &Value) -> rsi_files_protocol::Result<Self> {
        let entries = value
            .get("files")
            .and_then(Value::as_array)
            .ok_or(FilesError::Invalid)?;
        if entries.is_empty()
            || entries.len() > 8
            || entries.iter().any(|entry| {
                entry
                    .get("path_hex")
                    .and_then(Value::as_str)
                    .is_none_or(|path| path.len() > MAXIMUM_FILE_PATH_BYTES * 2)
                    || entry
                        .get("description")
                        .and_then(Value::as_str)
                        .is_none_or(|text| !valid_description(text))
            })
        {
            return Err(FilesError::Invalid);
        }
        let result = Self::deserialize(value).map_err(|_| FilesError::Invalid)?;
        result.validate()?;
        Ok(result)
    }
    /// Checks the durable value before rendering or resolving a recorded entry.
    pub fn validate(&self) -> rsi_files_protocol::Result<()> {
        if self.version != 1
            || self.files.is_empty()
            || self.files.len() > 8
            || self.files.iter().any(|file| {
                file.path_hex.as_bytes().is_empty() || !valid_description(&file.description)
            })
            || serde_json::to_vec(self)
                .map_err(|_| FilesError::Invalid)?
                .len()
                > MAXIMUM_PRESENTED_FILES_BYTES
        {
            return Err(FilesError::Invalid);
        }
        Ok(())
    }
}
fn valid_description(text: &str) -> bool {
    text.len() <= 256 && !text.chars().any(|character| {
        character.is_control() || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    files: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: Option<String>,
    path_hex: Option<RelativePath>,
    #[serde(default)]
    description: String,
}
pub(super) fn registration(files: Arc<dyn Files>) -> rsi_tools_protocol::Result<ToolRegistration> {
    Ok(ToolRegistration {
        output: None,
        definition: ToolDefinition::new("present", "Present one to eight existing regular workspace files to the user. Paths are cwd-relative UTF-8 or exact path_hex bytes. The user can open current file contents; this declaration does not freeze the bytes.", json!({
            "type":"object", "properties":{"files":{"type":"array","minItems":1,"maxItems":8,"items":{
                "type":"object","properties":{
                    "path":{"type":"string","minLength":1,"maxLength":MAXIMUM_FILE_PATH_BYTES},
                    "path_hex":{"type":"string","minLength":2,"maxLength":MAXIMUM_FILE_PATH_BYTES * 2},
                    "description":{"type":"string","maxLength":256}
                },"oneOf":[{"required":["path"]},{"required":["path_hex"]}],"additionalProperties":false
            }}},"required":["files"],"additionalProperties":false
        }))?.with_scheduling(ToolScheduling::ParallelSafe),
        timeout: ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
        executor: Arc::new(Present { files }),
    })
}
#[derive(Debug)]
struct Present {
    files: Arc<dyn Files>,
}
impl Present {
    async fn length(
        &self,
        binding: FilesBinding,
        path: RelativePath,
        execution: &ToolExecution,
    ) -> rsi_files_protocol::Result<u64> {
        let opened = self
            .files
            .open(
                binding.clone(),
                path,
                FileKind::File,
                execution.cancellation.clone(),
            )
            .await?;
        let length = opened.length;
        self.files.release(&binding, &opened.token)?;
        Ok(length)
    }
}
#[async_trait]
impl ToolExecutor for Present {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let input: Input = match serde_json::from_value(arguments) {
            Ok(input) => input,
            Err(_) => return error(&FilesError::Invalid),
        };
        if input.files.is_empty() || input.files.len() > 8 {
            return error(&FilesError::Invalid);
        }
        let mut declared = PresentedFilesV1 {
            version: 1,
            files: Vec::with_capacity(input.files.len()),
        };
        for entry in input.files {
            let args = Arguments {
                path: entry.path,
                path_hex: entry.path_hex,
                offset: 0,
                maximum: None,
            };
            let path = match args.path(FileKind::File) {
                Ok(path) => path,
                Err(failure) => return error(&failure),
            };
            declared.files.push(PresentedFile {
                path_hex: path,
                description: entry.description,
                length: 0,
            });
        }
        if let Err(failure) = declared.validate() {
            return error(&failure);
        }
        let scope = execution.workspace_read().await?;
        let prefix = scope
            .cwd()
            .strip_prefix(scope.workspace())
            .ok()
            .and_then(|path| path.to_str())
            .ok_or_else(|| ToolError::Execution("invalid workspace read scope".into()))?;
        let prefix = match RelativePath::new(prefix.as_bytes()) {
            Ok(prefix) => prefix,
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
        for file in &mut declared.files {
            let path = match workspace_path(&prefix, &file.path_hex) {
                Ok(path) => path,
                Err(failure) => return error(&failure),
            };
            file.length = match self.length(binding.clone(), path, &execution).await {
                Ok(length) => length,
                Err(failure) => return error(&failure),
            };
        }
        if let Err(failure) = declared.validate() {
            return error(&failure);
        }
        let text = declared
            .files
            .iter()
            .map(|file| {
                format!(
                    "{} ({} bytes){}{}",
                    safe_tool_text(file.path_hex.as_bytes()),
                    file.length,
                    if file.description.is_empty() {
                        ""
                    } else {
                        ": "
                    },
                    file.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        ToolResult::new(
            json!({"presented": declared}),
            vec![ToolContent::Text { text }],
            false,
        )
    }
}

#[cfg(test)]
mod description_tests {
    use super::*;
    #[test]
    fn descriptions_preserve_joiners_and_normal_multilingual_text() {
        assert!(valid_description("报告 · 👩‍💻 · العربية"));
        for text in ["\u{202e}txt.exe", "\u{2066}hidden\u{2069}", "\n"] {
            assert!(!valid_description(text));
        }
    }
}
