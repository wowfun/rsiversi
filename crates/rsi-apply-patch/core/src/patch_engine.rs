use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

mod file_update;
mod parser;
mod preflight;

use parser::{ParsedOperation, parse_patch};
use preflight::preflight;

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
    pub kind: PatchEffectKind,
    pub path: String,
    pub bytes_before: Option<usize>,
    pub bytes_after: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PatchEffectKind {
    Add,
    Update,
    Delete,
    MoveWrite,
    MoveDelete,
    Mkdir,
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
        if self
            .effects
            .iter()
            .any(|effect| effect.path.len() > MAXIMUM_PATCH_PATH_BYTES)
            || self
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
                        kind: PatchEffectKind::Add,
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
                        kind: PatchEffectKind::Update,
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
                        kind: PatchEffectKind::MoveWrite,
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
                        kind: PatchEffectKind::MoveDelete,
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
                        kind: PatchEffectKind::Delete,
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
        kind: PatchEffectKind::Mkdir,
        path,
        bytes_before: None,
        bytes_after: None,
    }));
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
mod tests;
