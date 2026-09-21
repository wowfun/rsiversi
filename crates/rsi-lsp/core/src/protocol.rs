use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
/// Operator-selected server configuration; never merged with ambient credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Absolute executable, invoked without a shell.
    pub program: PathBuf,
    /// Explicit arguments.
    pub arguments: Vec<String>,
    /// Complete environment.
    pub environment: BTreeMap<String, String>,
    /// Lowercase extensions including dot, mapped to LSP language IDs.
    pub languages: BTreeMap<String, String>,
    /// Bounded initialization options.
    pub initialization_options: serde_json::Value,
    /// Bounded answer for workspace/configuration items.
    pub configuration: serde_json::Value,
}
impl Config {
    /// Validates operator input before any process is admitted.
    pub fn validate(&self) -> Result<()> {
        if !self.program.is_absolute()
            || self.program.as_os_str().len() > 4096
            || self.arguments.len() > 32
            || self.arguments.iter().map(String::len).sum::<usize>() > 16384
            || self.arguments.iter().any(|s| s.contains('\0'))
            || self.environment.len() > 32
            || self
                .environment
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                > 32768
            || self
                .environment
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
            || self.languages.is_empty()
            || self.languages.len() > 32
            || self.languages.iter().any(|(k, v)| {
                !k.starts_with('.')
                    || k.len() < 2
                    || k.len() > 32
                    || !k[1..]
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    || v.is_empty()
                    || v.len() > 64
                    || !v
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            })
            || serde_json::to_vec(&(&self.initialization_options, &self.configuration))
                .map_err(|_| Error::Invalid)?
                .len()
                > 16384
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// Closed read-only language operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Go to definition.
    Definition,
    /// Find references, including declaration.
    References,
    /// Go to implementation.
    Implementation,
    /// Read hover text.
    Hover,
}
impl Operation {
    pub(crate) fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
            Self::Implementation => "textDocument/implementation",
            Self::Hover => "textDocument/hover",
        }
    }
    pub(crate) fn capability(self) -> &'static str {
        match self {
            Self::Definition => "definitionProvider",
            Self::References => "referencesProvider",
            Self::Implementation => "implementationProvider",
            Self::Hover => "hoverProvider",
        }
    }
}
/// A source location supplied by the caller; workspace is independent authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    /// Exact semantic operation.
    pub operation: Operation,
    /// Safe relative file path.
    pub path: String,
    /// One-based line.
    pub line: u32,
    /// One-based Unicode-scalar column (not bytes or UTF-16 units).
    pub column: u32,
}
impl Query {
    /// Bounds a query before file/process access.
    pub fn validate(&self) -> Result<()> {
        relative(&self.path)?;
        if self.line == 0 || self.line > 1_048_577 || self.column == 0 || self.column > 1_048_577 {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// LSP-native zero-based UTF-16 position.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// Zero-based line.
    pub line: u32,
    /// UTF-16 units from line start.
    pub character: u32,
}
/// Half-open LSP range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    /// Inclusive beginning.
    pub start: Position,
    /// Exclusive end.
    pub end: Position,
}
impl Range {
    /// Rejects inverted or oversized source coordinates.
    pub fn validate(self) -> Result<()> {
        if self.start > self.end
            || [
                self.start.line,
                self.start.character,
                self.end.line,
                self.end.character,
            ]
            .into_iter()
            .any(|n| n > 1_048_576)
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }
}
/// A contained file location that can be opened under current workspace authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// Workspace-relative path.
    pub path: String,
    /// Reported zero-based UTF-16 range.
    pub range: Range,
}
/// Normalized, finite semantic result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryResult {
    /// Definition/reference/implementation locations.
    Locations {
        /// At most 128 contained file positions.
        locations: Vec<Location>,
    },
    /// Plain or Markdown source shown as plain text.
    Hover {
        /// Empty when the server reports no hover.
        text: String,
        /// Optional source range.
        range: Option<Range>,
    },
}
/// Shared typed Tool value; preserves exact caller coordinate semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Exact query.
    pub query: Query,
    /// Normalized server result.
    pub result: QueryResult,
}
/// Reject path traversal before Files access or URI construction.
pub fn relative(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\0', '\\', ':'])
        || value
            .split('/')
            .any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub(crate) fn file_uri(workspace: &Path, path: &str) -> Result<String> {
    url::Url::from_file_path(workspace.join(path))
        .map(Into::into)
        .map_err(|()| Error::Invalid)
}
fn contained_path(workspace: &Path, uri: &str) -> Result<String> {
    let uri = url::Url::parse(uri).map_err(|_| Error::Protocol)?;
    if uri.query().is_some()
        || uri.fragment().is_some()
        || uri.host_str().is_some_and(|h| h != "localhost")
    {
        return Err(Error::Protocol);
    }
    let path = uri.to_file_path().map_err(|()| Error::Protocol)?;
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::Protocol);
    }
    let path = path
        .strip_prefix(workspace)
        .map_err(|_| Error::Protocol)?
        .to_str()
        .ok_or(Error::Protocol)?
        .replace(std::path::MAIN_SEPARATOR, "/");
    relative(&path).map_err(|_| Error::Protocol)?;
    Ok(path)
}
fn part(value: &serde_json::Value) -> Result<&str> {
    value
        .as_str()
        .or_else(|| value.get("value").and_then(serde_json::Value::as_str))
        .ok_or(Error::Protocol)
}
pub(crate) fn normalize(
    workspace: &Path,
    operation: Operation,
    value: serde_json::Value,
) -> Result<QueryResult> {
    if operation == Operation::Hover {
        if value.is_null() {
            return Ok(QueryResult::Hover {
                text: String::new(),
                range: None,
            });
        }
        let contents = value.get("contents").ok_or(Error::Protocol)?;
        let text = if let Some(values) = contents.as_array() {
            if values.len() > 128 {
                return Err(Error::Limit);
            }
            values
                .iter()
                .map(part)
                .collect::<Result<Vec<_>>>()?
                .join("\n\n")
        } else {
            part(contents)?.into()
        };
        if text.len() > 16384 {
            return Err(Error::Limit);
        }
        let range = value
            .get("range")
            .map(|v| serde_json::from_value::<Range>(v.clone()).map_err(|_| Error::Protocol))
            .transpose()?;
        if let Some(r) = range {
            r.validate()?;
        }
        return Ok(QueryResult::Hover { text, range });
    }
    let values = match value {
        serde_json::Value::Null => vec![],
        serde_json::Value::Array(values) => values,
        value => vec![value],
    };
    if values.len() > 128 {
        return Err(Error::Limit);
    }
    let mut locations = Vec::with_capacity(values.len());
    let mut bytes = 0;
    for value in values {
        let (uri, range) = if value.get("targetUri").is_some() {
            (value.get("targetUri"), value.get("targetSelectionRange"))
        } else {
            (value.get("uri"), value.get("range"))
        };
        let path = contained_path(
            workspace,
            uri.and_then(serde_json::Value::as_str)
                .ok_or(Error::Protocol)?,
        )?;
        bytes += path.len();
        if bytes > 16384 {
            return Err(Error::Limit);
        }
        let range: Range = serde_json::from_value(range.ok_or(Error::Protocol)?.clone())
            .map_err(|_| Error::Protocol)?;
        range.validate()?;
        locations.push(Location { path, range });
    }
    Ok(QueryResult::Locations { locations })
}
/// Convert one-based Unicode-scalar input into an exact UTF-16 wire position.
pub fn position(text: &str, line: u32, column: u32) -> Result<Position> {
    let row = text
        .split('\n')
        .nth(
            usize::try_from(line.checked_sub(1).ok_or(Error::Invalid)?)
                .map_err(|_| Error::Invalid)?,
        )
        .ok_or(Error::Invalid)?
        .trim_end_matches('\r');
    let index = usize::try_from(column.checked_sub(1).ok_or(Error::Invalid)?)
        .map_err(|_| Error::Invalid)?;
    let mut units = 0;
    let mut chars = row.chars();
    for _ in 0..index {
        units += chars.next().ok_or(Error::Invalid)?.len_utf16();
    }
    Ok(Position {
        line: line - 1,
        character: u32::try_from(units).map_err(|_| Error::Limit)?,
    })
}
/// Convert a wire position to a scalar boundary in the current UTF-8 file.
pub fn byte_offset(text: &str, position: Position) -> Result<usize> {
    let mut base = 0;
    let mut lines = text.split('\n');
    for _ in 0..position.line {
        base += lines.next().ok_or(Error::Invalid)?.len() + 1;
    }
    let row = lines.next().ok_or(Error::Invalid)?.trim_end_matches('\r');
    let mut units = 0;
    for (byte, c) in row.char_indices() {
        if units == position.character {
            return Ok(base + byte);
        }
        units += if c.len_utf16() == 1 { 1 } else { 2 };
        if units > position.character {
            return Err(Error::Invalid);
        }
    }
    if units == position.character {
        Ok(base + row.len())
    } else {
        Err(Error::Invalid)
    }
}
