use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

use super::MAXIMUM_APPLY_PATCH_BYTES as MAXIMUM_PATCH_BYTES;
const MAXIMUM_PATCH_OPERATIONS: usize = 256;
const MAXIMUM_PATCH_PATH_BYTES: usize = 16 * 1024;
const MAXIMUM_PATCH_PATH_BYTES_TOTAL: usize = 1024 * 1024;
const MAXIMUM_PATCH_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_PATCH_CONTENT_BYTES_TOTAL: usize = 16 * 1024 * 1024;
const MAXIMUM_PATCH_EFFECTS: usize = MAXIMUM_PATCH_OPERATIONS * 3;
const MAXIMUM_PATCH_FUZZY_MATCHES: usize = MAXIMUM_PATCH_OPERATIONS * 4;
const MAXIMUM_PATCH_FAILURE_CODE_BYTES: usize = 128;
const MAXIMUM_PATCH_FAILURE_MESSAGE_BYTES: usize = 32 * 1024;
const MAXIMUM_PATCH_RESPONSE_BYTES: usize = rsi_process::MAXIMUM_PROCESS_STREAM_BYTES - 1;
const DEFAULT_NEW_FILE_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::RGRP)
    .union(Mode::ROTH);
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PatchStatus {
    Applied,
    Rejected,
    Partial,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchEffect {
    pub operation: usize,
    pub kind: String,
    pub path: String,
    pub bytes_before: Option<usize>,
    pub bytes_after: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MatchKind {
    Exact,
    Rstrip,
    Trim,
    Unicode,
}

impl MatchKind {
    const fn is_exact(self) -> bool {
        matches!(self, Self::Exact)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchFuzzyMatch {
    pub operation: usize,
    pub hunk: usize,
    pub path: String,
    kind: MatchKind,
    pub source_line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchFailure {
    pub operation: Option<usize>,
    pub hunk: Option<usize>,
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchHelperResponse {
    pub status: PatchStatus,
    pub delta_exact: bool,
    pub effects: Vec<PatchEffect>,
    pub fuzzy_matches: Vec<PatchFuzzyMatch>,
    pub failure: Option<PatchFailure>,
}

impl PatchHelperResponse {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self.status {
            PatchStatus::Applied if self.failure.is_none() && !self.effects.is_empty() => {}
            PatchStatus::Rejected if self.failure.is_some() && self.effects.is_empty() => {}
            PatchStatus::Partial if self.failure.is_some() && !self.effects.is_empty() => {}
            _ => return Err("helper response status disagrees with effects/failure".into()),
        }
        if self.effects.len() > MAXIMUM_PATCH_EFFECTS
            || self.fuzzy_matches.len() > MAXIMUM_PATCH_FUZZY_MATCHES
        {
            return Err("helper response exceeds bounded effect metadata".into());
        }
        if self.effects.iter().any(|effect| {
            effect.kind.len() > MAXIMUM_PATCH_FAILURE_CODE_BYTES
                || effect.path.len() > MAXIMUM_PATCH_PATH_BYTES
        }) || self
            .fuzzy_matches
            .iter()
            .any(|audit| audit.path.len() > MAXIMUM_PATCH_PATH_BYTES)
            || self.failure.as_ref().is_some_and(|failure| {
                failure.code.len() > MAXIMUM_PATCH_FAILURE_CODE_BYTES
                    || failure.message.len() > MAXIMUM_PATCH_FAILURE_MESSAGE_BYTES
                    || failure
                        .path
                        .as_ref()
                        .is_some_and(|path| path.len() > MAXIMUM_PATCH_PATH_BYTES)
            })
        {
            return Err("helper response exceeds bounded string metadata".into());
        }
        Ok(())
    }
}

impl PatchHelperResponse {
    fn rejected(failure: PatchFailure, fuzzy_matches: Vec<PatchFuzzyMatch>) -> Self {
        let mut response = Self {
            status: PatchStatus::Rejected,
            delta_exact: true,
            effects: Vec::new(),
            fuzzy_matches,
            failure: Some(failure),
        };
        if !response.fits_capture() {
            response.fuzzy_matches.clear();
        }
        debug_assert!(response.fits_capture());
        response
    }

    fn fits_capture(&self) -> bool {
        self.validate().is_ok()
            && serde_json::to_vec(self)
                .is_ok_and(|bytes| bytes.len() <= MAXIMUM_PATCH_RESPONSE_BYTES)
    }
}

pub(crate) fn validate_patch_document(patch: &str) -> Result<(), PatchFailure> {
    validate_patch_text(patch).map_err(EngineFailure::response)
}

pub(crate) fn rejection(code: &str, message: impl Into<String>) -> PatchHelperResponse {
    PatchHelperResponse::rejected(EngineFailure::new(code, message).response(), Vec::new())
}

#[derive(Clone, Debug)]
enum ParsedOperation {
    Add {
        path: SafePath,
        content: Vec<u8>,
    },
    Update {
        path: SafePath,
        move_path: Option<SafePath>,
        chunks: Vec<UpdateChunk>,
    },
    Delete {
        path: SafePath,
    },
}

#[derive(Clone, Debug)]
struct UpdateChunk {
    context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    context_indices: Vec<(usize, usize)>,
    eof: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SafePath {
    text: String,
    components: Vec<OsString>,
}

impl SafePath {
    fn parse(text: &str) -> Result<Self, EngineFailure> {
        if text.is_empty() || text.len() > MAXIMUM_PATCH_PATH_BYTES || text.contains('\0') {
            return Err(EngineFailure::new(
                "invalid_path",
                format!("patch path must be nonempty and within {MAXIMUM_PATCH_PATH_BYTES} bytes"),
            ));
        }
        let path = Path::new(text);
        if path.is_absolute() {
            return Err(EngineFailure::new(
                "invalid_path",
                "patch paths must be relative to the tool cwd",
            ));
        }
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(component) => components.push(component.to_os_string()),
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => {
                    return Err(EngineFailure::new(
                        "invalid_path",
                        "patch paths cannot contain '.', '..', roots, or prefixes",
                    ));
                }
            }
        }
        if components.is_empty() {
            return Err(EngineFailure::new(
                "invalid_path",
                "patch path must name a file",
            ));
        }
        let text = components
            .iter()
            .map(|component| component.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        Ok(Self { text, components })
    }

    fn name(&self) -> &OsString {
        self.components.last().expect("safe path has a name")
    }

    fn parents(&self) -> &[OsString] {
        &self.components[..self.components.len() - 1]
    }
}

#[derive(Debug)]
struct EngineFailure {
    operation: Option<usize>,
    hunk: Option<usize>,
    code: String,
    message: String,
    path: Option<String>,
}

impl EngineFailure {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            operation: None,
            hunk: None,
            code: truncate_utf8(code.to_owned(), MAXIMUM_PATCH_FAILURE_CODE_BYTES),
            message: truncate_utf8(message.into(), MAXIMUM_PATCH_FAILURE_MESSAGE_BYTES),
            path: None,
        }
    }

    fn at(mut self, operation: usize, path: &SafePath) -> Self {
        self.operation = Some(operation);
        self.path = Some(path.text.clone());
        self
    }

    fn at_hunk(mut self, operation: usize, hunk: usize, path: &SafePath) -> Self {
        self.operation = Some(operation);
        self.hunk = Some(hunk);
        self.path = Some(path.text.clone());
        self
    }

    fn response(self) -> PatchFailure {
        PatchFailure {
            operation: self.operation,
            hunk: self.hunk,
            code: self.code,
            message: self.message,
            path: self.path,
        }
    }
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

#[derive(Debug)]
enum PreparedKind {
    Add,
    Update,
    Move {
        destination_parent: Option<DirectoryIdentity>,
    },
    Delete,
}

#[derive(Debug)]
struct PreparedOperation {
    index: usize,
    path: SafePath,
    expected: Option<Vec<u8>>,
    expected_mode: Option<Mode>,
    expected_identity: Option<FileIdentity>,
    parent: Option<DirectoryIdentity>,
    publish_mode: Option<Mode>,
    content: Option<Vec<u8>>,
    kind: PreparedKind,
    move_path: Option<SafePath>,
    prospective_directories: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug)]
struct VirtualContent {
    bytes: Vec<u8>,
    mode: Mode,
    identity: Option<FileIdentity>,
}

#[derive(Clone, Copy, Debug)]
struct PathExpectation<'a> {
    bytes: Option<&'a [u8]>,
    mode: Option<Mode>,
    identity: Option<FileIdentity>,
    parent: Option<DirectoryIdentity>,
}

#[derive(Clone, Copy, Debug)]
enum ParentCreation<'a> {
    Forbidden,
    Planned(&'a [String]),
}

pub(crate) fn apply_patch(root_path: &Path, patch: &str) -> PatchHelperResponse {
    apply_patch_before_commit(root_path, patch, |_| {})
}

fn apply_patch_before_commit(
    root_path: &Path,
    patch: &str,
    mut before_operation: impl FnMut(usize),
) -> PatchHelperResponse {
    if let Err(failure) = validate_patch_text(patch) {
        return PatchHelperResponse::rejected(failure.response(), Vec::new());
    }
    let operations = match parse_patch(patch) {
        Ok(operations) => operations,
        Err(failure) => return PatchHelperResponse::rejected(failure.response(), Vec::new()),
    };
    let root = match Root::open(root_path) {
        Ok(root) => root,
        Err(failure) => return PatchHelperResponse::rejected(failure.response(), Vec::new()),
    };
    let new_file_mode = if operations
        .iter()
        .any(|operation| matches!(operation, ParsedOperation::Add { .. }))
    {
        match effective_new_file_mode() {
            Ok(mode) => mode,
            Err(failure) => return PatchHelperResponse::rejected(failure.response(), Vec::new()),
        }
    } else {
        DEFAULT_NEW_FILE_MODE
    };
    let (prepared, fuzzy_matches) = match preflight(&root, operations, new_file_mode) {
        Ok(preflight) => preflight,
        Err((failure, fuzzy_matches)) => {
            return PatchHelperResponse::rejected(failure.response(), fuzzy_matches);
        }
    };
    commit(&root, prepared, fuzzy_matches, &mut before_operation)
}

fn effective_new_file_mode() -> Result<Mode, EngineFailure> {
    let mut status = String::new();
    File::open("/proc/self/status")
        .and_then(|file| file.take(64 * 1024).read_to_string(&mut status))
        .map_err(|error| {
            EngineFailure::new(
                "umask_unavailable",
                format!("failed to read the helper process umask: {error}"),
            )
        })?;
    let umask = status
        .lines()
        .find_map(|line| line.strip_prefix("Umask:"))
        .map(str::trim)
        .and_then(|value| u32::from_str_radix(value, 8).ok())
        .filter(|value| *value <= 0o777)
        .ok_or_else(|| {
            EngineFailure::new(
                "umask_unavailable",
                "helper process status omitted a valid umask",
            )
        })?;
    Ok(Mode::from_raw_mode(
        DEFAULT_NEW_FILE_MODE.as_raw_mode() & !umask,
    ))
}

fn validate_patch_text(patch: &str) -> Result<(), EngineFailure> {
    if patch.len() > MAXIMUM_PATCH_BYTES {
        return Err(EngineFailure::new(
            "patch_too_large",
            format!("patch exceeds {MAXIMUM_PATCH_BYTES} UTF-8 bytes"),
        ));
    }
    if patch.chars().any(|character| {
        (character <= '\u{1f}' && !matches!(character, '\t' | '\n' | '\r')) || character == '\u{7f}'
    }) {
        return Err(EngineFailure::new(
            "invalid_patch_text",
            "patch contains a disallowed control character",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // One parser state machine owns the complete bounded patch grammar.
fn parse_patch(document: &str) -> Result<Vec<ParsedOperation>, EngineFailure> {
    let normalized = document.replace("\r\n", "\n");
    let mut lines = normalized.lines().map(str::to_owned).collect::<Vec<_>>();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.first().map(String::as_str) != Some("*** Begin Patch") {
        return Err(EngineFailure::new(
            "invalid_patch",
            "first line must be '*** Begin Patch'",
        ));
    }
    if lines.last().map(String::as_str) != Some("*** End Patch") {
        return Err(EngineFailure::new(
            "invalid_patch",
            "last line must be '*** End Patch'",
        ));
    }
    let mut operations = Vec::new();
    let mut path_bytes = 0_usize;
    let mut cursor = 1;
    while cursor < lines.len() - 1 {
        if operations.len() >= MAXIMUM_PATCH_OPERATIONS {
            return Err(EngineFailure::new(
                "too_many_operations",
                format!("patch exceeds {MAXIMUM_PATCH_OPERATIONS} operations"),
            ));
        }
        let header = &lines[cursor];
        let operation = operations.len();
        if let Some(path) = header.strip_prefix("*** Add File: ") {
            let path = parse_bounded_path(path, &mut path_bytes)?;
            cursor += 1;
            let mut content = String::new();
            while cursor < lines.len() - 1 && !is_operation_header(&lines[cursor]) {
                let Some(line) = lines[cursor].strip_prefix('+') else {
                    return Err(EngineFailure::new(
                        "invalid_add_hunk",
                        format!("add operation {operation} contains a non-'+' line"),
                    )
                    .at(operation, &path));
                };
                content.push_str(line);
                content.push('\n');
                cursor += 1;
            }
            if content.is_empty() {
                return Err(EngineFailure::new(
                    "invalid_add_hunk",
                    "add operation must contain at least one '+' line",
                )
                .at(operation, &path));
            }
            operations.push(ParsedOperation::Add {
                path,
                content: content.into_bytes(),
            });
        } else if let Some(path) = header.strip_prefix("*** Delete File: ") {
            let path = parse_bounded_path(path, &mut path_bytes)?;
            cursor += 1;
            operations.push(ParsedOperation::Delete { path });
        } else if let Some(path) = header.strip_prefix("*** Update File: ") {
            let path = parse_bounded_path(path, &mut path_bytes)?;
            cursor += 1;
            let move_path = if cursor < lines.len() - 1 {
                if let Some(destination) = lines[cursor].strip_prefix("*** Move to: ") {
                    cursor += 1;
                    Some(parse_bounded_path(destination, &mut path_bytes)?)
                } else {
                    None
                }
            } else {
                None
            };
            let mut chunks = Vec::new();
            while cursor < lines.len() - 1 && !is_operation_header(&lines[cursor]) {
                if !lines[cursor].starts_with("@@") {
                    return Err(EngineFailure::new(
                        "invalid_update_hunk",
                        format!("update operation {operation} expected an '@@' hunk marker"),
                    )
                    .at(operation, &path));
                }
                let context = match lines[cursor].as_str() {
                    "@@" => None,
                    marker if marker.starts_with("@@ ") => Some(marker[3..].to_owned()),
                    _ => {
                        return Err(EngineFailure::new(
                            "invalid_update_hunk",
                            "invalid '@@' hunk marker",
                        )
                        .at(operation, &path));
                    }
                };
                cursor += 1;
                let mut chunk = UpdateChunk {
                    context,
                    old_lines: Vec::new(),
                    new_lines: Vec::new(),
                    context_indices: Vec::new(),
                    eof: false,
                };
                let mut changed = false;
                while cursor < lines.len() - 1
                    && !is_operation_header(&lines[cursor])
                    && !lines[cursor].starts_with("@@")
                {
                    if lines[cursor] == "*** End of File" {
                        chunk.eof = true;
                        cursor += 1;
                        break;
                    }
                    let line = &lines[cursor];
                    let Some(prefix) = line.as_bytes().first().copied() else {
                        return Err(EngineFailure::new(
                            "invalid_update_hunk",
                            "empty patch line must carry a ' ', '+', or '-' prefix",
                        )
                        .at(operation, &path));
                    };
                    let text = line[1..].to_owned();
                    match prefix {
                        b' ' => {
                            chunk
                                .context_indices
                                .push((chunk.old_lines.len(), chunk.new_lines.len()));
                            chunk.old_lines.push(text.clone());
                            chunk.new_lines.push(text);
                        }
                        b'-' => {
                            changed = true;
                            chunk.old_lines.push(text);
                        }
                        b'+' => {
                            changed = true;
                            chunk.new_lines.push(text);
                        }
                        _ => {
                            return Err(EngineFailure::new(
                                "invalid_update_hunk",
                                "update lines must start with ' ', '+', or '-'",
                            )
                            .at(operation, &path));
                        }
                    }
                    cursor += 1;
                }
                if !changed {
                    return Err(EngineFailure::new(
                        "invalid_update_hunk",
                        "update hunk contains no added or removed line",
                    )
                    .at(operation, &path));
                }
                chunks.push(chunk);
            }
            if chunks.is_empty() {
                return Err(EngineFailure::new(
                    "invalid_update_hunk",
                    "update operation contains no hunks",
                )
                .at(operation, &path));
            }
            operations.push(ParsedOperation::Update {
                path,
                move_path,
                chunks,
            });
        } else {
            return Err(EngineFailure::new(
                "invalid_patch",
                format!("unknown operation header at patch line {}", cursor + 1),
            ));
        }
    }
    if operations.is_empty() {
        return Err(EngineFailure::new(
            "empty_patch",
            "patch must contain at least one operation",
        ));
    }
    Ok(operations)
}

fn parse_bounded_path(path: &str, path_bytes: &mut usize) -> Result<SafePath, EngineFailure> {
    *path_bytes = path_bytes
        .checked_add(path.len())
        .ok_or_else(|| EngineFailure::new("path_budget", "patch path budget overflow"))?;
    if *path_bytes > MAXIMUM_PATCH_PATH_BYTES_TOTAL {
        return Err(EngineFailure::new(
            "path_budget",
            format!("patch paths exceed {MAXIMUM_PATCH_PATH_BYTES_TOTAL} total bytes"),
        ));
    }
    SafePath::parse(path)
}

fn is_operation_header(line: &str) -> bool {
    line.starts_with("*** Add File: ")
        || line.starts_with("*** Update File: ")
        || line.starts_with("*** Delete File: ")
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_lines,
    clippy::type_complexity
)] // The error carries the exact fuzzy prefix; one transaction derives every operation before mutation.
fn preflight(
    root: &Root,
    operations: Vec<ParsedOperation>,
    new_file_mode: Mode,
) -> Result<(Vec<PreparedOperation>, Vec<PatchFuzzyMatch>), (EngineFailure, Vec<PatchFuzzyMatch>)> {
    let mut virtual_files: HashMap<SafePath, Option<VirtualContent>> = HashMap::new();
    let mut prepared = Vec::with_capacity(operations.len());
    let mut fuzzy_matches = Vec::new();
    let mut content_bytes = 0_usize;
    for (index, operation) in operations.into_iter().enumerate() {
        let result = match operation {
            ParsedOperation::Add { path, content } => {
                let expected = virtual_content(root, &mut virtual_files, &path)
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                if expected.is_some() {
                    return Err((
                        EngineFailure::new("already_exists", "add target already exists")
                            .at(index, &path),
                        fuzzy_matches,
                    ));
                }
                account_content(&mut content_bytes, content.len())
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                let (parent, prospective_directories) = root
                    .parent_plan(&path)
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                virtual_files.insert(
                    path.clone(),
                    Some(VirtualContent {
                        bytes: content.clone(),
                        mode: new_file_mode,
                        identity: None,
                    }),
                );
                PreparedOperation {
                    index,
                    path,
                    expected: None,
                    expected_mode: None,
                    expected_identity: None,
                    parent,
                    publish_mode: Some(new_file_mode),
                    content: Some(content),
                    kind: PreparedKind::Add,
                    move_path: None,
                    prospective_directories,
                }
            }
            ParsedOperation::Delete { path } => {
                let expected = virtual_content(root, &mut virtual_files, &path)
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?
                    .ok_or_else(|| {
                        (
                            EngineFailure::new("not_found", "delete target does not exist")
                                .at(index, &path),
                            fuzzy_matches.clone(),
                        )
                    })?;
                account_content(&mut content_bytes, expected.bytes.len())
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                let parent = root
                    .parent_identity(&path)
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                virtual_files.insert(path.clone(), None);
                PreparedOperation {
                    index,
                    path,
                    expected: Some(expected.bytes),
                    expected_mode: Some(expected.mode),
                    expected_identity: expected.identity,
                    parent,
                    publish_mode: None,
                    content: None,
                    kind: PreparedKind::Delete,
                    move_path: None,
                    prospective_directories: Vec::new(),
                }
            }
            ParsedOperation::Update {
                path,
                move_path,
                chunks,
            } => {
                let expected = virtual_content(root, &mut virtual_files, &path)
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?
                    .ok_or_else(|| {
                        (
                            EngineFailure::new("not_found", "update target does not exist")
                                .at(index, &path),
                            fuzzy_matches.clone(),
                        )
                    })?;
                account_content(&mut content_bytes, expected.bytes.len())
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                let text = std::str::from_utf8(&expected.bytes).map_err(|_| {
                    (
                        EngineFailure::new("non_utf8_file", "update target is not UTF-8")
                            .at(index, &path),
                        fuzzy_matches.clone(),
                    )
                })?;
                let content = derive_update(text, &path, &chunks, index, &mut fuzzy_matches)
                    .map_err(|failure| (failure, fuzzy_matches.clone()))?
                    .into_bytes();
                account_content(&mut content_bytes, content.len())
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                let parent = root
                    .parent_identity(&path)
                    .map_err(|failure| (failure.at(index, &path), fuzzy_matches.clone()))?;
                let (kind, prospective_directories) = if let Some(destination) = &move_path {
                    if destination == &path {
                        return Err((
                            EngineFailure::new(
                                "invalid_move",
                                "move destination must differ from the source",
                            )
                            .at(index, &path),
                            fuzzy_matches,
                        ));
                    }
                    if virtual_content(root, &mut virtual_files, destination)
                        .map_err(|failure| (failure.at(index, destination), fuzzy_matches.clone()))?
                        .is_some()
                    {
                        return Err((
                            EngineFailure::new("already_exists", "move destination already exists")
                                .at(index, destination),
                            fuzzy_matches,
                        ));
                    }
                    let (destination_parent, prospective_directories) =
                        root.parent_plan(destination).map_err(|failure| {
                            (failure.at(index, destination), fuzzy_matches.clone())
                        })?;
                    virtual_files.insert(path.clone(), None);
                    virtual_files.insert(
                        destination.clone(),
                        Some(VirtualContent {
                            bytes: content.clone(),
                            mode: expected.mode,
                            identity: None,
                        }),
                    );
                    (
                        PreparedKind::Move { destination_parent },
                        prospective_directories,
                    )
                } else {
                    virtual_files.insert(
                        path.clone(),
                        Some(VirtualContent {
                            bytes: content.clone(),
                            mode: expected.mode,
                            identity: None,
                        }),
                    );
                    (PreparedKind::Update, Vec::new())
                };
                PreparedOperation {
                    index,
                    path,
                    expected: Some(expected.bytes),
                    expected_mode: Some(expected.mode),
                    expected_identity: expected.identity,
                    parent,
                    publish_mode: Some(expected.mode),
                    content: Some(content),
                    kind,
                    move_path,
                    prospective_directories,
                }
            }
        };
        prepared.push(result);
    }
    validate_effect_budget(&prepared).map_err(|failure| (failure, fuzzy_matches.clone()))?;
    validate_response_budget(&prepared, &fuzzy_matches).map_err(|failure| (failure, Vec::new()))?;
    Ok((prepared, fuzzy_matches))
}

fn virtual_content(
    root: &Root,
    virtual_files: &mut HashMap<SafePath, Option<VirtualContent>>,
    path: &SafePath,
) -> Result<Option<VirtualContent>, EngineFailure> {
    if let Some(content) = virtual_files.get(path) {
        return Ok(content.clone());
    }
    let content = root.read_optional(path)?;
    virtual_files.insert(path.clone(), content.clone());
    Ok(content)
}

fn validate_effect_budget(prepared: &[PreparedOperation]) -> Result<(), EngineFailure> {
    let mut effects = 0_usize;
    let mut potential_directories = BTreeSet::new();
    for operation in prepared {
        effects = effects
            .checked_add(if matches!(operation.kind, PreparedKind::Move { .. }) {
                2
            } else {
                1
            })
            .ok_or_else(|| EngineFailure::new("effect_budget", "patch effect budget overflow"))?;
        if effects > MAXIMUM_PATCH_EFFECTS {
            return Err(effect_budget_failure());
        }
        for directory in &operation.prospective_directories {
            if !potential_directories.contains(directory) {
                if effects >= MAXIMUM_PATCH_EFFECTS {
                    return Err(effect_budget_failure());
                }
                potential_directories.insert(directory.clone());
                effects += 1;
            }
        }
    }
    Ok(())
}

fn effect_budget_failure() -> EngineFailure {
    EngineFailure::new(
        "effect_budget",
        format!("patch may produce more than {MAXIMUM_PATCH_EFFECTS} filesystem effects"),
    )
}

fn validate_response_budget(
    prepared: &[PreparedOperation],
    fuzzy_matches: &[PatchFuzzyMatch],
) -> Result<(), EngineFailure> {
    if fuzzy_matches.len() > MAXIMUM_PATCH_FUZZY_MATCHES {
        return Err(EngineFailure::new(
            "response_budget",
            format!("patch may report more than {MAXIMUM_PATCH_FUZZY_MATCHES} fuzzy matches"),
        ));
    }
    let effects = projected_effects(prepared)?;
    let response = PatchHelperResponse {
        status: PatchStatus::Partial,
        delta_exact: true,
        effects,
        fuzzy_matches: fuzzy_matches.to_vec(),
        failure: Some(PatchFailure {
            operation: Some(usize::MAX),
            hunk: Some(usize::MAX),
            code: "\0".repeat(MAXIMUM_PATCH_FAILURE_CODE_BYTES),
            message: "\0".repeat(MAXIMUM_PATCH_FAILURE_MESSAGE_BYTES),
            path: Some("\0".repeat(MAXIMUM_PATCH_PATH_BYTES)),
        }),
    };
    if !response.fits_capture() {
        return Err(EngineFailure::new(
            "response_budget",
            format!(
                "patch effect metadata cannot fit the {MAXIMUM_PATCH_RESPONSE_BYTES}-byte helper response"
            ),
        ));
    }
    Ok(())
}

fn projected_effects(prepared: &[PreparedOperation]) -> Result<Vec<PatchEffect>, EngineFailure> {
    let mut directories = BTreeMap::new();
    let mut effects = Vec::new();
    for operation in prepared {
        for directory in &operation.prospective_directories {
            directories
                .entry(directory.clone())
                .or_insert(operation.index);
        }
        let effect = |kind: &str,
                      path: &SafePath,
                      bytes_before: Option<usize>,
                      bytes_after: Option<usize>| PatchEffect {
            operation: operation.index,
            kind: kind.into(),
            path: path.text.clone(),
            bytes_before,
            bytes_after,
        };
        match &operation.kind {
            PreparedKind::Add => effects.push(effect(
                "add",
                &operation.path,
                None,
                operation.content.as_ref().map(Vec::len),
            )),
            PreparedKind::Update => effects.push(effect(
                "update",
                &operation.path,
                operation.expected.as_ref().map(Vec::len),
                operation.content.as_ref().map(Vec::len),
            )),
            PreparedKind::Move { .. } => {
                effects.push(effect(
                    "move_write",
                    operation.move_path.as_ref().expect("move has destination"),
                    None,
                    operation.content.as_ref().map(Vec::len),
                ));
                effects.push(effect(
                    "move_delete",
                    &operation.path,
                    operation.expected.as_ref().map(Vec::len),
                    None,
                ));
            }
            PreparedKind::Delete => effects.push(effect(
                "delete",
                &operation.path,
                operation.expected.as_ref().map(Vec::len),
                None,
            )),
        }
    }
    effects.extend(
        directories
            .into_iter()
            .map(|(path, operation)| PatchEffect {
                operation,
                kind: "mkdir".into(),
                path,
                bytes_before: None,
                bytes_after: None,
            }),
    );
    if effects.len() > MAXIMUM_PATCH_EFFECTS {
        return Err(EngineFailure::new(
            "effect_budget",
            format!("patch may produce more than {MAXIMUM_PATCH_EFFECTS} filesystem effects"),
        ));
    }
    Ok(effects)
}

fn account_content(total: &mut usize, bytes: usize) -> Result<(), EngineFailure> {
    if bytes > MAXIMUM_PATCH_FILE_BYTES {
        return Err(EngineFailure::new(
            "file_too_large",
            format!("file exceeds {MAXIMUM_PATCH_FILE_BYTES} bytes"),
        ));
    }
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| EngineFailure::new("content_budget", "content budget overflow"))?;
    if *total > MAXIMUM_PATCH_CONTENT_BYTES_TOTAL {
        return Err(EngineFailure::new(
            "content_budget",
            format!("patch content exceeds {MAXIMUM_PATCH_CONTENT_BYTES_TOTAL} aggregate bytes"),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Exact partial-effect accounting remains adjacent to each fallible filesystem operation.
fn commit(
    root: &Root,
    prepared: Vec<PreparedOperation>,
    fuzzy_matches: Vec<PatchFuzzyMatch>,
    before_operation: &mut impl FnMut(usize),
) -> PatchHelperResponse {
    let mut effects = Vec::new();
    for operation in prepared {
        before_operation(operation.index);
        let result = match &operation.kind {
            PreparedKind::Add => {
                let mut directories = Vec::new();
                let result = root.atomic_write(
                    &operation.path,
                    PathExpectation {
                        bytes: operation.expected.as_deref(),
                        mode: operation.expected_mode,
                        identity: operation.expected_identity,
                        parent: operation.parent,
                    },
                    operation.content.as_deref().expect("add has content"),
                    operation.publish_mode.expect("add has a publish mode"),
                    ParentCreation::Planned(&operation.prospective_directories),
                    &mut directories,
                );
                push_directory_effects(&mut effects, operation.index, directories);
                result.map(|()| {
                    effects.push(PatchEffect {
                        operation: operation.index,
                        kind: "add".into(),
                        path: operation.path.text.clone(),
                        bytes_before: None,
                        bytes_after: operation.content.as_ref().map(Vec::len),
                    });
                })
            }
            PreparedKind::Update => {
                let mut directories = Vec::new();
                let result = root.atomic_write(
                    &operation.path,
                    PathExpectation {
                        bytes: operation.expected.as_deref(),
                        mode: operation.expected_mode,
                        identity: operation.expected_identity,
                        parent: operation.parent,
                    },
                    operation.content.as_deref().expect("update has content"),
                    operation.publish_mode.expect("update has a publish mode"),
                    ParentCreation::Forbidden,
                    &mut directories,
                );
                push_directory_effects(&mut effects, operation.index, directories);
                result.map(|()| {
                    effects.push(PatchEffect {
                        operation: operation.index,
                        kind: "update".into(),
                        path: operation.path.text.clone(),
                        bytes_before: operation.expected.as_ref().map(Vec::len),
                        bytes_after: operation.content.as_ref().map(Vec::len),
                    });
                })
            }
            PreparedKind::Move { destination_parent } => {
                let destination = operation
                    .move_path
                    .as_ref()
                    .expect("move has a destination");
                let mut directories = Vec::new();
                let result = root
                    .atomic_write(
                        destination,
                        PathExpectation {
                            bytes: None,
                            mode: None,
                            identity: None,
                            parent: *destination_parent,
                        },
                        operation.content.as_deref().expect("move has content"),
                        operation.publish_mode.expect("move has a publish mode"),
                        ParentCreation::Planned(&operation.prospective_directories),
                        &mut directories,
                    )
                    .map_err(|mut failure| {
                        failure.path.get_or_insert(destination.text.clone());
                        failure
                    });
                push_directory_effects(&mut effects, operation.index, directories);
                result.and_then(|()| {
                    effects.push(PatchEffect {
                        operation: operation.index,
                        kind: "move_write".into(),
                        path: destination.text.clone(),
                        bytes_before: None,
                        bytes_after: operation.content.as_ref().map(Vec::len),
                    });
                    root.delete(
                        &operation.path,
                        PathExpectation {
                            bytes: operation.expected.as_deref(),
                            mode: operation.expected_mode,
                            identity: operation.expected_identity,
                            parent: operation.parent,
                        },
                    )?;
                    effects.push(PatchEffect {
                        operation: operation.index,
                        kind: "move_delete".into(),
                        path: operation.path.text.clone(),
                        bytes_before: operation.expected.as_ref().map(Vec::len),
                        bytes_after: None,
                    });
                    Ok(())
                })
            }
            PreparedKind::Delete => root
                .delete(
                    &operation.path,
                    PathExpectation {
                        bytes: operation.expected.as_deref(),
                        mode: operation.expected_mode,
                        identity: operation.expected_identity,
                        parent: operation.parent,
                    },
                )
                .map(|()| {
                    effects.push(PatchEffect {
                        operation: operation.index,
                        kind: "delete".into(),
                        path: operation.path.text.clone(),
                        bytes_before: operation.expected.as_ref().map(Vec::len),
                        bytes_after: None,
                    });
                }),
        };
        if let Err(mut failure) = result {
            failure.operation = Some(operation.index);
            failure.path.get_or_insert(operation.path.text);
            return PatchHelperResponse {
                status: if effects.is_empty() {
                    PatchStatus::Rejected
                } else {
                    PatchStatus::Partial
                },
                delta_exact: true,
                effects,
                fuzzy_matches,
                failure: Some(failure.response()),
            };
        }
    }
    PatchHelperResponse {
        status: PatchStatus::Applied,
        delta_exact: true,
        effects,
        fuzzy_matches,
        failure: None,
    }
}

fn push_directory_effects(
    effects: &mut Vec<PatchEffect>,
    operation: usize,
    directories: Vec<String>,
) {
    effects.extend(directories.into_iter().map(|path| PatchEffect {
        operation,
        kind: "mkdir".into(),
        path,
        bytes_before: None,
        bytes_after: None,
    }));
}

#[derive(Clone, Copy, Debug)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    const fn text(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

#[derive(Clone, Debug)]
struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

#[derive(Debug)]
struct SourceFile {
    lines: Vec<SourceLine>,
    preferred: LineEnding,
}

type Replacement = (usize, usize, Vec<String>);

impl SourceFile {
    fn parse(contents: &str) -> Self {
        let mut lines = Vec::new();
        let mut preferred = None;
        let mut start = 0;
        let mut cursor = 0;
        while cursor < contents.len() {
            let (ending, width) = match contents.as_bytes()[cursor] {
                b'\r' if contents.as_bytes().get(cursor + 1) == Some(&b'\n') => {
                    (LineEnding::CrLf, 2)
                }
                b'\r' => (LineEnding::Cr, 1),
                b'\n' => (LineEnding::Lf, 1),
                _ => {
                    cursor += 1;
                    continue;
                }
            };
            preferred.get_or_insert(ending);
            lines.push(SourceLine {
                text: contents[start..cursor].to_owned(),
                ending: Some(ending),
            });
            cursor += width;
            start = cursor;
        }
        if start < contents.len() {
            lines.push(SourceLine {
                text: contents[start..].to_owned(),
                ending: None,
            });
        }
        Self {
            lines,
            preferred: preferred.unwrap_or(LineEnding::Lf),
        }
    }

    fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.text.clone()).collect()
    }

    fn apply(mut self, replacements: &[Replacement]) -> String {
        let mut source = self.lines.drain(..);
        let mut output = Vec::new();
        let mut source_index = 0;
        for (start, old_len, new_lines) in replacements {
            debug_assert!(*start >= source_index);
            output.extend(source.by_ref().take(*start - source_index));
            source.by_ref().take(*old_len).for_each(drop);
            output.extend(new_lines.iter().map(|text| SourceLine {
                text: text.clone(),
                ending: Some(self.preferred),
            }));
            source_index = *start + *old_len;
        }
        output.extend(source);
        if !output.is_empty() {
            for line in &mut output {
                line.ending.get_or_insert(self.preferred);
            }
        }
        let mut content = String::new();
        for line in output {
            content.push_str(&line.text);
            if let Some(ending) = line.ending {
                content.push_str(ending.text());
            }
        }
        content
    }
}

fn derive_update(
    contents: &str,
    path: &SafePath,
    chunks: &[UpdateChunk],
    operation: usize,
    fuzzy_matches: &mut Vec<PatchFuzzyMatch>,
) -> Result<String, EngineFailure> {
    let source = SourceFile::parse(contents);
    let lines = source.texts();
    let views = LineViews::new(&lines);
    let mut replacements = Vec::new();
    let mut line_index = 0;
    let mut previous_end = 0;
    for (hunk, chunk) in chunks.iter().enumerate() {
        if let Some(context) = &chunk.context {
            let (index, kind) =
                seek_sequence(&views, std::slice::from_ref(context), line_index, false)
                    .ok_or_else(|| {
                        EngineFailure::new(
                            "context_not_found",
                            format!("failed to find hunk context in {}", path.text),
                        )
                        .at_hunk(operation, hunk, path)
                    })?;
            audit_match(fuzzy_matches, operation, hunk, path, index, kind)?;
            line_index = index + 1;
        }
        let (start, kind) = if chunk.old_lines.is_empty() {
            (
                if chunk.context.is_some() {
                    line_index
                } else {
                    lines.len()
                },
                MatchKind::Exact,
            )
        } else {
            seek_sequence(&views, &chunk.old_lines, line_index, chunk.eof).ok_or_else(|| {
                EngineFailure::new(
                    "expected_lines_not_found",
                    format!("failed to find expected lines in {}", path.text),
                )
                .at_hunk(operation, hunk, path)
            })?
        };
        if start < previous_end {
            return Err(EngineFailure::new(
                "overlapping_hunks",
                "update hunks are not source-ordered and non-overlapping",
            )
            .at_hunk(operation, hunk, path));
        }
        audit_match(fuzzy_matches, operation, hunk, path, start, kind)?;
        let mut old_start = 0;
        let mut new_start = 0;
        for &(old_context, new_context) in &chunk.context_indices {
            if old_start != old_context || new_start != new_context {
                replacements.push((
                    start + old_start,
                    old_context - old_start,
                    chunk.new_lines[new_start..new_context].to_vec(),
                ));
            }
            old_start = old_context + 1;
            new_start = new_context + 1;
        }
        if old_start != chunk.old_lines.len() || new_start != chunk.new_lines.len() {
            replacements.push((
                start + old_start,
                chunk.old_lines.len() - old_start,
                chunk.new_lines[new_start..].to_vec(),
            ));
        }
        previous_end = start + chunk.old_lines.len();
        line_index = previous_end;
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    for pair in replacements.windows(2) {
        if pair[0].0 + pair[0].1 > pair[1].0 {
            return Err(
                EngineFailure::new("overlapping_hunks", "derived replacements overlap")
                    .at(operation, path),
            );
        }
    }
    Ok(source.apply(&replacements))
}

fn audit_match(
    matches: &mut Vec<PatchFuzzyMatch>,
    operation: usize,
    hunk: usize,
    path: &SafePath,
    source_index: usize,
    kind: MatchKind,
) -> Result<(), EngineFailure> {
    if !kind.is_exact() {
        if matches.len() >= MAXIMUM_PATCH_FUZZY_MATCHES {
            return Err(EngineFailure::new(
                "response_budget",
                format!("patch may report more than {MAXIMUM_PATCH_FUZZY_MATCHES} fuzzy matches"),
            )
            .at_hunk(operation, hunk, path));
        }
        matches.push(PatchFuzzyMatch {
            operation,
            hunk,
            path: path.text.clone(),
            kind,
            source_line: source_index + 1,
        });
    }
    Ok(())
}

struct LineViews<'a> {
    exact: Vec<&'a str>,
    rstrip: OnceLock<Vec<&'a str>>,
    trim: OnceLock<Vec<&'a str>>,
    unicode: OnceLock<Vec<String>>,
}

impl<'a> LineViews<'a> {
    fn new(lines: &'a [String]) -> Self {
        Self {
            exact: lines.iter().map(String::as_str).collect(),
            rstrip: OnceLock::new(),
            trim: OnceLock::new(),
            unicode: OnceLock::new(),
        }
    }
}

fn seek_sequence(
    lines: &LineViews<'_>,
    pattern: &[String],
    start: usize,
    eof: bool,
) -> Option<(usize, MatchKind)> {
    if pattern.is_empty() {
        return Some((start, MatchKind::Exact));
    }
    if pattern.len() > lines.exact.len() || start > lines.exact.len().saturating_sub(pattern.len())
    {
        return None;
    }
    let exact_pattern = pattern.iter().map(String::as_str).collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(&lines.exact, &exact_pattern, start, eof) {
        return Some((index, MatchKind::Exact));
    }
    let rstrip_lines = lines
        .rstrip
        .get_or_init(|| lines.exact.iter().map(|line| line.trim_end()).collect());
    let rstrip_pattern = pattern
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(rstrip_lines, &rstrip_pattern, start, eof) {
        return Some((index, MatchKind::Rstrip));
    }
    let trim_lines = lines
        .trim
        .get_or_init(|| lines.exact.iter().map(|line| line.trim()).collect());
    let trim_pattern = pattern.iter().map(|line| line.trim()).collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(trim_lines, &trim_pattern, start, eof) {
        return Some((index, MatchKind::Trim));
    }
    let unicode_lines = lines.unicode.get_or_init(|| {
        lines
            .exact
            .iter()
            .map(|line| normalize_unicode(line))
            .collect()
    });
    let unicode_pattern = pattern
        .iter()
        .map(|line| normalize_unicode(line))
        .collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(unicode_lines, &unicode_pattern, start, eof) {
        return Some((index, MatchKind::Unicode));
    }
    None
}

fn seek_sequence_layer<T: Eq>(
    lines: &[T],
    pattern: &[T],
    start: usize,
    eof: bool,
) -> Option<usize> {
    let end = lines.len().checked_sub(pattern.len())?;
    if eof {
        return (end >= start && lines[end..] == *pattern).then_some(end);
    }
    let mut prefix = vec![0_usize; pattern.len()];
    let mut matched = 0;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
            prefix[index] = matched;
        }
    }
    matched = 0;
    for (offset, line) in lines.iter().enumerate().skip(start) {
        while matched > 0 && line != &pattern[matched] {
            matched = prefix[matched - 1];
        }
        if line == &pattern[matched] {
            matched += 1;
            if matched == pattern.len() {
                return Some(offset + 1 - pattern.len());
            }
        }
    }
    None
}

fn normalize_unicode(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[derive(Debug)]
struct Root {
    directory: File,
}

impl Root {
    fn open(path: &Path) -> Result<Self, EngineFailure> {
        let directory = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| io_failure("open_workspace", "failed to open tool cwd", error))?;
        Ok(Self { directory })
    }

    fn read_optional(&self, path: &SafePath) -> Result<Option<VirtualContent>, EngineFailure> {
        let parent = match self.open_parent(path, ParentCreation::Forbidden, &mut Vec::new()) {
            Ok(parent) => parent,
            Err(failure) if failure.code == "not_found" => return Ok(None),
            Err(failure) => return Err(failure),
        };
        Self::read_optional_at(&parent, path.name())
    }

    fn read_optional_at(
        parent: &File,
        name: &std::ffi::OsStr,
    ) -> Result<Option<VirtualContent>, EngineFailure> {
        let descriptor = match rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(io_failure(
                    "unsafe_or_unreadable_path",
                    "target is not a readable symlink-free regular file",
                    error,
                ));
            }
        };
        let mut file = File::from(descriptor);
        let stat = rustix::fs::fstat(&file)
            .map_err(|error| io_failure("metadata_failed", "failed to inspect target", error))?;
        if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file() {
            return Err(EngineFailure::new(
                "special_file",
                "patch target must be a regular file",
            ));
        }
        let file_len = u64::try_from(stat.st_size).map_err(|_| {
            EngineFailure::new("file_too_large", "target size cannot be represented")
        })?;
        if file_len > MAXIMUM_PATCH_FILE_BYTES as u64 {
            return Err(EngineFailure::new(
                "file_too_large",
                format!("target exceeds {MAXIMUM_PATCH_FILE_BYTES} bytes"),
            ));
        }
        let capacity = usize::try_from(file_len).map_err(|_| {
            EngineFailure::new("file_too_large", "target size cannot be represented")
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        std::io::Read::by_ref(&mut file)
            .take((MAXIMUM_PATCH_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                EngineFailure::new("read_failed", format!("target read failed: {error}"))
            })?;
        if bytes.len() > MAXIMUM_PATCH_FILE_BYTES {
            return Err(EngineFailure::new(
                "file_too_large",
                format!("target exceeds {MAXIMUM_PATCH_FILE_BYTES} bytes"),
            ));
        }
        Ok(Some(VirtualContent {
            bytes,
            mode: Mode::from_raw_mode(stat.st_mode),
            identity: Some(FileIdentity {
                device: stat.st_dev as u64,
                inode: stat.st_ino as u64,
            }),
        }))
    }

    fn parent_identity(&self, path: &SafePath) -> Result<Option<DirectoryIdentity>, EngineFailure> {
        let parent = match self.open_parent(path, ParentCreation::Forbidden, &mut Vec::new()) {
            Ok(parent) => parent,
            Err(failure) if failure.code == "not_found" => return Ok(None),
            Err(failure) => return Err(failure),
        };
        directory_identity(&parent).map(Some)
    }

    fn parent_plan(
        &self,
        path: &SafePath,
    ) -> Result<(Option<DirectoryIdentity>, Vec<String>), EngineFailure> {
        let mut directory = self.directory.try_clone().map_err(|error| {
            EngineFailure::new(
                "open_workspace",
                format!("failed to clone cwd handle: {error}"),
            )
        })?;
        let mut prefix = Vec::new();
        let mut missing = false;
        let mut prospective = Vec::new();
        for component in path.parents() {
            prefix.push(component.to_string_lossy().into_owned());
            if missing {
                prospective.push(prefix.join("/"));
                continue;
            }
            match rustix::fs::openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(opened) => directory = File::from(opened),
                Err(rustix::io::Errno::NOENT) => {
                    missing = true;
                    prospective.push(prefix.join("/"));
                }
                Err(error) => {
                    return Err(io_failure(
                        "unsafe_parent",
                        "parent path is not a symlink-free directory",
                        error,
                    ));
                }
            }
        }
        if missing {
            Ok((None, prospective))
        } else {
            directory_identity(&directory).map(|identity| (Some(identity), prospective))
        }
    }

    fn atomic_write(
        &self,
        path: &SafePath,
        expected: PathExpectation<'_>,
        content: &[u8],
        publish_mode: Mode,
        parent_creation: ParentCreation<'_>,
        created_directories: &mut Vec<String>,
    ) -> Result<(), EngineFailure> {
        let parent = self.open_parent(path, parent_creation, created_directories)?;
        validate_parent_identity(&parent, expected.parent)?;
        Self::revalidate_at(
            &parent,
            path.name(),
            expected.bytes,
            expected.mode,
            expected.identity,
        )?;
        let mut temporary = None;
        for _ in 0..128 {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                ".rsi-apply-patch-{}-{sequence:016x}.tmp",
                std::process::id()
            );
            match rustix::fs::openat(
                &parent,
                &name,
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(file) => {
                    temporary = Some((name, File::from(file)));
                    break;
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(io_failure(
                        "temporary_create_failed",
                        "failed to allocate private temporary file",
                        error,
                    ));
                }
            }
        }
        let (temporary_name, mut temporary_file) = temporary.ok_or_else(|| {
            EngineFailure::new(
                "temporary_create_failed",
                "could not allocate a private temporary file name",
            )
        })?;
        let cleanup = |name: &str| {
            let _ = rustix::fs::unlinkat(&parent, name, AtFlags::empty());
        };
        if let Err(error) = temporary_file
            .write_all(content)
            .and_then(|()| {
                rustix::fs::fchmod(&temporary_file, publish_mode).map_err(std::io::Error::from)
            })
            .and_then(|()| temporary_file.sync_all())
        {
            cleanup(&temporary_name);
            return Err(EngineFailure::new(
                "temporary_write_failed",
                format!("failed to write private temporary file: {error}"),
            ));
        }
        drop(temporary_file);
        if let Err(failure) = Self::revalidate_at(
            &parent,
            path.name(),
            expected.bytes,
            expected.mode,
            expected.identity,
        ) {
            cleanup(&temporary_name);
            return Err(failure);
        }
        let rename = if expected.bytes.is_none() {
            rustix::fs::renameat_with(
                &parent,
                &temporary_name,
                &parent,
                path.name(),
                RenameFlags::NOREPLACE,
            )
        } else {
            rustix::fs::renameat(&parent, &temporary_name, &parent, path.name())
        };
        if let Err(error) = rename {
            cleanup(&temporary_name);
            return Err(io_failure(
                "rename_failed",
                "failed to atomically publish updated file",
                error,
            ));
        }
        Ok(())
    }

    fn delete(&self, path: &SafePath, expected: PathExpectation<'_>) -> Result<(), EngineFailure> {
        let parent = self.open_parent(path, ParentCreation::Forbidden, &mut Vec::new())?;
        validate_parent_identity(&parent, expected.parent)?;
        Self::revalidate_at(
            &parent,
            path.name(),
            expected.bytes,
            expected.mode,
            expected.identity,
        )?;
        rustix::fs::unlinkat(&parent, path.name(), AtFlags::empty())
            .map_err(|error| io_failure("delete_failed", "failed to delete target file", error))
    }

    fn revalidate_at(
        parent: &File,
        name: &std::ffi::OsStr,
        expected: Option<&[u8]>,
        expected_mode: Option<Mode>,
        expected_identity: Option<FileIdentity>,
    ) -> Result<(), EngineFailure> {
        let current = Self::read_optional_at(parent, name)?;
        if current.as_ref().map(|current| current.bytes.as_slice()) != expected
            || current.as_ref().map(|current| current.mode) != expected_mode
            || (expected_identity.is_some()
                && current.as_ref().and_then(|current| current.identity) != expected_identity)
        {
            return Err(EngineFailure::new(
                "changed_since_preflight",
                "target changed after patch preflight",
            ));
        }
        Ok(())
    }

    fn open_parent(
        &self,
        path: &SafePath,
        parent_creation: ParentCreation<'_>,
        created: &mut Vec<String>,
    ) -> Result<File, EngineFailure> {
        let mut directory = self.directory.try_clone().map_err(|error| {
            EngineFailure::new(
                "open_workspace",
                format!("failed to clone cwd handle: {error}"),
            )
        })?;
        let mut prefix = Vec::new();
        for component in path.parents() {
            prefix.push(component.to_string_lossy().into_owned());
            let opened = rustix::fs::openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            );
            directory = match opened {
                Ok(opened) => File::from(opened),
                Err(rustix::io::Errno::NOENT)
                    if matches!(parent_creation, ParentCreation::Planned(_)) =>
                {
                    let path = prefix.join("/");
                    let ParentCreation::Planned(prospective_directories) = parent_creation else {
                        unreachable!("guard admits planned parent creation only");
                    };
                    if !prospective_directories.contains(&path) {
                        return Err(EngineFailure::new(
                            "changed_since_preflight",
                            "an existing parent disappeared after patch preflight",
                        ));
                    }
                    match rustix::fs::mkdirat(
                        &directory,
                        component,
                        Mode::RUSR
                            | Mode::WUSR
                            | Mode::XUSR
                            | Mode::RGRP
                            | Mode::XGRP
                            | Mode::ROTH
                            | Mode::XOTH,
                    ) {
                        Ok(()) => created.push(path),
                        Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => {
                            return Err(io_failure(
                                "mkdir_failed",
                                "failed to create a parent directory",
                                error,
                            ));
                        }
                    }
                    rustix::fs::openat(
                        &directory,
                        component,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map(File::from)
                    .map_err(|error| {
                        io_failure(
                            "unsafe_parent",
                            "created parent could not be reopened without following links",
                            error,
                        )
                    })?
                }
                Err(rustix::io::Errno::NOENT) => {
                    return Err(EngineFailure::new(
                        "not_found",
                        "a parent directory does not exist",
                    ));
                }
                Err(error) => {
                    return Err(io_failure(
                        "unsafe_parent",
                        "parent path is not a symlink-free directory",
                        error,
                    ));
                }
            };
        }
        Ok(directory)
    }
}

fn directory_identity(directory: &File) -> Result<DirectoryIdentity, EngineFailure> {
    let stat = rustix::fs::fstat(directory)
        .map_err(|error| io_failure("metadata_failed", "failed to inspect parent", error))?;
    Ok(DirectoryIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    })
}

fn validate_parent_identity(
    parent: &File,
    expected: Option<DirectoryIdentity>,
) -> Result<(), EngineFailure> {
    if let Some(expected) = expected
        && directory_identity(parent)? != expected
    {
        return Err(EngineFailure::new(
            "changed_since_preflight",
            "target parent changed after patch preflight",
        ));
    }
    Ok(())
}

fn io_failure(code: &str, context: &str, error: rustix::io::Errno) -> EngineFailure {
    EngineFailure::new(code, format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::wildcard_imports)]

    use super::*;
    use std::fmt::Write as _;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn patch(root: &Path, body: &str) -> PatchHelperResponse {
        apply_patch(root, &format!("*** Begin Patch\n{body}\n*** End Patch\n"))
    }

    #[test]
    fn applies_add_update_move_delete_after_full_preflight() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("old.txt"), "old\n").unwrap();
        fs::write(root.path().join("delete.txt"), "gone\n").unwrap();
        let result = patch(
            root.path(),
            "*** Add File: nested/add.txt\n+added\n*** Update File: old.txt\n*** Move to: moved/new.txt\n@@\n-old\n+new\n*** Delete File: delete.txt",
        );
        assert_eq!(result.status, PatchStatus::Applied);
        assert!(result.delta_exact);
        assert_eq!(
            fs::read(root.path().join("nested/add.txt")).unwrap(),
            b"added\n"
        );
        assert_eq!(
            fs::read(root.path().join("moved/new.txt")).unwrap(),
            b"new\n"
        );
        assert!(!root.path().join("old.txt").exists());
        assert!(!root.path().join("delete.txt").exists());
    }

    #[test]
    fn preflight_failure_has_no_file_effects() {
        let root = tempfile::tempdir().unwrap();
        let result = patch(
            root.path(),
            "*** Add File: created.txt\n+created\n*** Update File: missing.txt\n@@\n-old\n+new",
        );
        assert_eq!(result.status, PatchStatus::Rejected);
        assert!(result.effects.is_empty());
        assert!(!root.path().join("created.txt").exists());
        assert_eq!(result.failure.unwrap().operation, Some(1));
    }

    #[test]
    fn matcher_uses_global_layer_priority_and_audits_only_fuzzy_matches() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("match.txt"), "target   \nother\ntarget\n").unwrap();
        let result = patch(
            root.path(),
            "*** Update File: match.txt\n@@\n-target\n+changed",
        );
        assert_eq!(result.status, PatchStatus::Applied);
        assert!(result.fuzzy_matches.is_empty());
        assert_eq!(
            fs::read_to_string(root.path().join("match.txt")).unwrap(),
            "target   \nother\nchanged\n"
        );

        fs::write(root.path().join("unicode.txt"), "say “hello”\n").unwrap();
        let fuzzy = patch(
            root.path(),
            "*** Update File: unicode.txt\n@@\n-say \"hello\"\n+done",
        );
        assert_eq!(fuzzy.status, PatchStatus::Applied);
        assert_eq!(fuzzy.fuzzy_matches.len(), 1);
        assert_eq!(fuzzy.fuzzy_matches[0].kind, MatchKind::Unicode);
        assert!(fuzzy.delta_exact);
    }

    #[test]
    fn normalizes_mixed_line_endings_and_terminates_nonempty_output() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("mixed.txt"), b"one\r\ntwo\rthree\nfour").unwrap();
        let result = patch(
            root.path(),
            "*** Update File: mixed.txt\n@@\n one\n two\n-three\n+THREE\n four",
        );
        assert_eq!(result.status, PatchStatus::Applied);
        assert_eq!(
            fs::read(root.path().join("mixed.txt")).unwrap(),
            b"one\r\ntwo\rTHREE\r\nfour\r\n"
        );
    }

    #[test]
    fn rejects_parent_escape_symlinks_special_files_and_controls() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        symlink(outside.path(), root.path().join("link")).unwrap();
        #[cfg(unix)]
        {
            let symlink_result = patch(root.path(), "*** Add File: link/escape.txt\n+bad");
            assert_eq!(symlink_result.status, PatchStatus::Rejected);
            assert!(!outside.path().join("escape.txt").exists());
        }
        let escape = patch(root.path(), "*** Add File: ../escape.txt\n+bad");
        assert_eq!(escape.failure.unwrap().code, "invalid_path");
        let control = apply_patch(root.path(), "*** Begin Patch\n\0*** End Patch\n");
        assert_eq!(control.failure.unwrap().code, "invalid_patch_text");
    }

    #[test]
    fn repeated_operations_use_virtual_preflight_state() {
        let root = tempfile::tempdir().unwrap();
        let result = patch(
            root.path(),
            "*** Add File: same.txt\n+one\n*** Update File: same.txt\n@@\n-one\n+two",
        );
        assert_eq!(result.status, PatchStatus::Applied);
        assert_eq!(fs::read(root.path().join("same.txt")).unwrap(), b"two\n");
    }

    #[test]
    fn commit_failure_reports_the_exact_applied_prefix_without_replay_guessing() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("second.txt"), "old\n").unwrap();
        let document = concat!(
            "*** Begin Patch\n",
            "*** Add File: first.txt\n",
            "+first\n",
            "*** Update File: second.txt\n",
            "@@\n",
            "-old\n",
            "+new\n",
            "*** End Patch\n"
        );
        let result = apply_patch_before_commit(root.path(), document, |operation| {
            if operation == 1 {
                fs::write(root.path().join("second.txt"), "raced\n").unwrap();
            }
        });
        assert_eq!(result.status, PatchStatus::Partial);
        assert!(result.delta_exact);
        assert_eq!(result.effects.len(), 1);
        assert_eq!(result.effects[0].kind, "add");
        let failure = result.failure.unwrap();
        assert_eq!(failure.operation, Some(1));
        assert_eq!(failure.code, "changed_since_preflight");
        assert_eq!(fs::read(root.path().join("first.txt")).unwrap(), b"first\n");
        assert_eq!(
            fs::read(root.path().join("second.txt")).unwrap(),
            b"raced\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_files_respect_the_helper_process_umask() {
        const CHILD: &str = "RSI_APPLY_PATCH_UMASK_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let root = tempfile::tempdir().unwrap();
            let result = patch(root.path(), "*** Add File: private.txt\n+secret");
            assert_eq!(result.status, PatchStatus::Applied);
            assert_eq!(
                fs::metadata(root.path().join("private.txt"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            return;
        }

        let current = std::env::current_exe().unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "umask 077; exec \"$1\" \"$2\" --exact --nocapture",
                "rsi-apply-patch-umask",
            ])
            .arg(current)
            .arg("patch_engine::tests::new_files_respect_the_helper_process_umask")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "umask child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn update_preserves_executable_mode() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("script.sh");
        fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        let result = patch(
            root.path(),
            "*** Update File: script.sh\n@@\n-exit 0\n+exit 1",
        );

        assert_eq!(result.status, PatchStatus::Applied);
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn preflight_rejects_a_path_whose_directory_effects_exceed_the_response_bound() {
        let root = tempfile::tempdir().unwrap();
        let path = format!("{}file.txt", "d/".repeat(MAXIMUM_PATCH_OPERATIONS * 3));
        let result = patch(root.path(), &format!("*** Add File: {path}\n+x"));

        assert_eq!(result.status, PatchStatus::Rejected);
        assert!(result.effects.is_empty());
        assert_eq!(result.failure.unwrap().code, "effect_budget");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn preflight_rejects_effect_metadata_that_cannot_fit_the_helper_capture() {
        let root = tempfile::tempdir().unwrap();
        let component = "d".repeat(230);
        let mut document = String::from("*** Begin Patch\n");
        for operation in 0..11 {
            let mut components = vec![format!("root-{operation}")];
            components.extend((0..67).map(|index| format!("{index:02}-{component}")));
            components.push("file.txt".into());
            write!(document, "*** Add File: {}\n+value\n", components.join("/")).unwrap();
        }
        document.push_str("*** End Patch\n");

        let result = apply_patch(root.path(), &document);

        assert_eq!(result.status, PatchStatus::Rejected);
        assert!(result.effects.is_empty());
        assert_eq!(result.failure.unwrap().code, "response_budget");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn preflight_rejects_more_fuzzy_audits_than_the_response_contract_can_carry() {
        let root = tempfile::tempdir().unwrap();
        let source = (0..=MAXIMUM_PATCH_FUZZY_MATCHES).fold(String::new(), |mut source, index| {
            writeln!(source, "line-{index}   ").unwrap();
            source
        });
        fs::write(root.path().join("many.txt"), &source).unwrap();
        let mut document = String::from("*** Begin Patch\n*** Update File: many.txt\n");
        for index in 0..=MAXIMUM_PATCH_FUZZY_MATCHES {
            write!(document, "@@\n-line-{index}\n+changed-{index}\n").unwrap();
        }
        document.push_str("*** End Patch\n");

        let result = apply_patch(root.path(), &document);

        assert_eq!(result.status, PatchStatus::Rejected);
        assert!(result.effects.is_empty());
        assert_eq!(result.failure.unwrap().code, "response_budget");
        assert_eq!(
            fs::read_to_string(root.path().join("many.txt")).unwrap(),
            source
        );
    }

    #[test]
    fn fuzzy_audit_budget_rejects_before_cloning_the_next_path() {
        let path = SafePath::parse("bounded.txt").unwrap();
        let mut matches = vec![
            PatchFuzzyMatch {
                operation: 0,
                hunk: 0,
                path: "bounded.txt".into(),
                kind: MatchKind::Trim,
                source_line: 1,
            };
            MAXIMUM_PATCH_FUZZY_MATCHES
        ];

        let failure = audit_match(&mut matches, 1, 2, &path, 3, MatchKind::Trim).unwrap_err();

        assert_eq!(failure.code, "response_budget");
        assert_eq!(matches.len(), MAXIMUM_PATCH_FUZZY_MATCHES);
    }

    #[test]
    fn commit_rejects_a_replaced_parent_directory_even_with_identical_target_bytes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("parent")).unwrap();
        fs::write(root.path().join("parent/file.txt"), "old\n").unwrap();
        let document = concat!(
            "*** Begin Patch\n",
            "*** Update File: parent/file.txt\n",
            "@@\n",
            "-old\n",
            "+new\n",
            "*** End Patch\n"
        );

        let result = apply_patch_before_commit(root.path(), document, |_| {
            fs::rename(
                root.path().join("parent"),
                root.path().join("original-parent"),
            )
            .unwrap();
            fs::create_dir(root.path().join("parent")).unwrap();
            fs::write(root.path().join("parent/file.txt"), "old\n").unwrap();
        });

        assert_eq!(result.status, PatchStatus::Rejected);
        assert_eq!(result.failure.unwrap().code, "changed_since_preflight");
        assert_eq!(
            fs::read(root.path().join("parent/file.txt")).unwrap(),
            b"old\n"
        );
        assert_eq!(
            fs::read(root.path().join("original-parent/file.txt")).unwrap(),
            b"old\n"
        );
    }

    #[test]
    fn commit_does_not_recreate_a_parent_that_existed_during_preflight() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("parent")).unwrap();
        let document = concat!(
            "*** Begin Patch\n",
            "*** Add File: parent/file.txt\n",
            "+value\n",
            "*** End Patch\n"
        );

        let result = apply_patch_before_commit(root.path(), document, |_| {
            fs::remove_dir(root.path().join("parent")).unwrap();
        });

        assert_eq!(result.status, PatchStatus::Rejected);
        assert!(result.effects.is_empty());
        assert_eq!(result.failure.unwrap().code, "changed_since_preflight");
        assert!(!root.path().join("parent").exists());
    }

    #[test]
    fn commit_rejects_a_replaced_target_even_with_identical_bytes_and_mode() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file.txt"), "old\n").unwrap();
        let document = concat!(
            "*** Begin Patch\n",
            "*** Update File: file.txt\n",
            "@@\n",
            "-old\n",
            "+new\n",
            "*** End Patch\n"
        );

        let result = apply_patch_before_commit(root.path(), document, |_| {
            fs::rename(
                root.path().join("file.txt"),
                root.path().join("original.txt"),
            )
            .unwrap();
            fs::write(root.path().join("file.txt"), "old\n").unwrap();
        });

        assert_eq!(result.status, PatchStatus::Rejected);
        assert_eq!(result.failure.unwrap().code, "changed_since_preflight");
        assert_eq!(fs::read(root.path().join("file.txt")).unwrap(), b"old\n");
        assert_eq!(
            fs::read(root.path().join("original.txt")).unwrap(),
            b"old\n"
        );
    }

    #[test]
    fn matcher_audits_rstrip_trim_and_eof_priority() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("rstrip.txt"), "alpha   \n").unwrap();
        let rstrip = patch(root.path(), "*** Update File: rstrip.txt\n@@\n-alpha\n+one");
        assert_eq!(rstrip.fuzzy_matches[0].kind, MatchKind::Rstrip);

        fs::write(root.path().join("trim.txt"), "  beta  \n").unwrap();
        let trim = patch(root.path(), "*** Update File: trim.txt\n@@\n-beta\n+two");
        assert_eq!(trim.fuzzy_matches[0].kind, MatchKind::Trim);

        fs::write(root.path().join("eof.txt"), "same\nmiddle\nsame\n").unwrap();
        let eof = patch(
            root.path(),
            "*** Update File: eof.txt\n@@\n-same\n+last\n*** End of File",
        );
        assert_eq!(eof.status, PatchStatus::Applied);
        assert_eq!(
            fs::read_to_string(root.path().join("eof.txt")).unwrap(),
            "same\nmiddle\nlast\n"
        );
    }

    #[test]
    fn eof_marker_rejects_expected_lines_that_do_not_end_the_source() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("eof.txt"), "target\ntrailing\n").unwrap();

        let result = patch(
            root.path(),
            "*** Update File: eof.txt\n@@\n-target\n+changed\n*** End of File",
        );

        assert_eq!(result.status, PatchStatus::Rejected);
        assert_eq!(
            fs::read_to_string(root.path().join("eof.txt")).unwrap(),
            "target\ntrailing\n"
        );
    }

    #[test]
    fn sequence_matcher_handles_long_self_overlapping_prefixes() {
        let mut lines = vec!["repeat".to_owned(); 10_000];
        lines.push("needle".to_owned());
        let mut pattern = vec!["repeat".to_owned(); 5_000];
        pattern.push("needle".to_owned());
        let views = LineViews::new(&lines);

        assert_eq!(
            seek_sequence(&views, &pattern, 0, false),
            Some((5_000, MatchKind::Exact))
        );
    }

    #[test]
    fn sequence_matcher_lazily_reuses_whole_file_normalizations() {
        let lines = vec!["exact".to_owned(), "rstrip  ".to_owned()];
        let views = LineViews::new(&lines);

        assert_eq!(
            seek_sequence(&views, &["exact".to_owned()], 0, false),
            Some((0, MatchKind::Exact))
        );
        assert!(views.rstrip.get().is_none());
        assert_eq!(
            seek_sequence(&views, &["rstrip".to_owned()], 0, false),
            Some((1, MatchKind::Rstrip))
        );
        let normalized = views.rstrip.get().unwrap().as_ptr();
        assert_eq!(
            seek_sequence(&views, &["rstrip".to_owned()], 0, false),
            Some((1, MatchKind::Rstrip))
        );
        assert_eq!(views.rstrip.get().unwrap().as_ptr(), normalized);
        assert!(views.trim.get().is_none());
        assert!(views.unicode.get().is_none());
    }

    #[test]
    fn pure_addition_appends_and_deletion_only_can_produce_an_empty_file() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("append.txt"), "one\n").unwrap();
        let appended = patch(
            root.path(),
            "*** Update File: append.txt\n@@\n+two\n*** End of File",
        );
        assert_eq!(appended.status, PatchStatus::Applied);
        assert_eq!(
            fs::read(root.path().join("append.txt")).unwrap(),
            b"one\ntwo\n"
        );

        fs::write(root.path().join("empty.txt"), "remove\n").unwrap();
        let emptied = patch(root.path(), "*** Update File: empty.txt\n@@\n-remove");
        assert_eq!(emptied.status, PatchStatus::Applied);
        assert!(fs::read(root.path().join("empty.txt")).unwrap().is_empty());
    }

    #[test]
    fn pure_addition_with_context_inserts_after_the_anchor() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("anchored.txt"), "one\nanchor\nthree\n").unwrap();

        let result = patch(
            root.path(),
            "*** Update File: anchored.txt\n@@ anchor\n+inserted",
        );

        assert_eq!(result.status, PatchStatus::Applied);
        assert_eq!(
            fs::read(root.path().join("anchored.txt")).unwrap(),
            b"one\nanchor\ninserted\nthree\n"
        );
    }

    #[test]
    fn parser_preserves_a_lone_carriage_return_inside_added_content() {
        let root = tempfile::tempdir().unwrap();
        let result = apply_patch(
            root.path(),
            "*** Begin Patch\n*** Add File: carriage.txt\n+left\rright\n*** End Patch\n",
        );

        assert_eq!(result.status, PatchStatus::Applied);
        assert_eq!(
            fs::read(root.path().join("carriage.txt")).unwrap(),
            b"left\rright\n"
        );
    }

    #[test]
    fn move_write_failure_identifies_the_destination_path() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("source.txt"), "old\n").unwrap();
        fs::create_dir(root.path().join("destination")).unwrap();
        let result = apply_patch_before_commit(
            root.path(),
            "*** Begin Patch\n*** Update File: source.txt\n*** Move to: destination/moved.txt\n@@\n-old\n+new\n*** End Patch\n",
            |_| {
                fs::rename(
                    root.path().join("destination"),
                    root.path().join("original-destination"),
                )
                .unwrap();
                fs::create_dir(root.path().join("destination")).unwrap();
            },
        );

        assert_eq!(result.status, PatchStatus::Rejected);
        assert_eq!(
            result.failure.unwrap().path.as_deref(),
            Some("destination/moved.txt")
        );
        assert_eq!(fs::read(root.path().join("source.txt")).unwrap(), b"old\n");
    }

    #[test]
    fn overlapping_hunks_existing_add_non_utf8_and_special_targets_are_rejected_without_effects() {
        #[cfg(unix)]
        use std::os::unix::net::UnixListener;

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("overlap.txt"), "one\n").unwrap();
        let overlap = patch(
            root.path(),
            "*** Update File: overlap.txt\n@@\n-one\n+first\n@@\n-one\n+second\n*** End of File",
        );
        assert_eq!(overlap.status, PatchStatus::Rejected);
        assert!(overlap.effects.is_empty());
        assert_eq!(fs::read(root.path().join("overlap.txt")).unwrap(), b"one\n");

        let existing = patch(root.path(), "*** Add File: overlap.txt\n+replacement");
        assert_eq!(existing.failure.unwrap().code, "already_exists");
        assert_eq!(fs::read(root.path().join("overlap.txt")).unwrap(), b"one\n");

        fs::write(root.path().join("bytes.bin"), [0xff, b'\n']).unwrap();
        let non_utf8 = patch(root.path(), "*** Update File: bytes.bin\n@@\n-old\n+new");
        assert_eq!(non_utf8.failure.unwrap().code, "non_utf8_file");

        #[cfg(unix)]
        {
            let _listener = UnixListener::bind(root.path().join("socket")).unwrap();
            let special = patch(root.path(), "*** Delete File: socket");
            assert_eq!(special.status, PatchStatus::Rejected);
            assert!(special.effects.is_empty());
        }
    }

    #[test]
    fn operation_path_and_file_budgets_reject_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let mut operations = String::from("*** Begin Patch\n");
        for index in 0..=MAXIMUM_PATCH_OPERATIONS {
            writeln!(&mut operations, "*** Add File: file-{index}\n+x").unwrap();
        }
        operations.push_str("*** End Patch\n");
        let too_many = apply_patch(root.path(), &operations);
        assert_eq!(too_many.failure.unwrap().code, "too_many_operations");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);

        let long_path = "x".repeat(MAXIMUM_PATCH_PATH_BYTES + 1);
        let path_result = patch(root.path(), &format!("*** Add File: {long_path}\n+x"));
        assert_eq!(path_result.failure.unwrap().code, "invalid_path");

        fs::write(
            root.path().join("large.txt"),
            vec![b'x'; MAXIMUM_PATCH_FILE_BYTES + 1],
        )
        .unwrap();
        let large = patch(root.path(), "*** Update File: large.txt\n@@\n-x\n+y");
        assert_eq!(large.failure.unwrap().code, "file_too_large");
        assert!(large.effects.is_empty());
    }
}
