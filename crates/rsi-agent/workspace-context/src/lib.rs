//! Bounded trust-aware workspace instruction and skill snapshots.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
#[cfg(not(unix))]
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, SessionHeader, WorkspaceTrust,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// Maximum bytes read from one instruction or skill source.
pub const MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum bytes in one rendered instruction baseline, skill catalog, or body.
pub const MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES: usize = 512 * 1024;
/// Maximum instruction files in one snapshot.
pub const MAXIMUM_WORKSPACE_INSTRUCTION_FILES: usize = 64;
/// Maximum entries inspected across all skill roots.
pub const MAXIMUM_WORKSPACE_SKILL_ENTRIES: usize = 256;
const MAXIMUM_SKILL_METADATA_PREFIX_BYTES: usize = 16 * 1024;

/// One fully selected direct-user skill invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSkillInvocation {
    /// Exact validated skill name.
    pub name: String,
    /// Model-facing source path.
    pub source: String,
    /// Current bounded skill instructions.
    pub text: String,
}

/// Complete current view returned without retaining Session state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceContextSnapshot {
    /// Whether this is a complete observation safe to replace last-good state.
    pub complete: bool,
    /// Digest of the complete rendered instruction baseline, including empty.
    pub instructions_sha256: String,
    /// Nonempty rendered baseline; `None` means no active instructions.
    pub instructions: Option<String>,
    /// Digest of the complete selected skill catalog, including empty.
    pub skill_catalog_sha256: String,
    /// Nonempty rendered catalog; `None` means no available skills.
    pub skill_catalog: Option<String>,
    /// Direct Human invocations in message order with duplicate names removed.
    pub invocations: Vec<WorkspaceSkillInvocation>,
}

/// Snapshot failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkspaceContextError {
    /// The caller supplied an invalid durable or configured boundary.
    #[error("invalid workspace context: {0}")]
    Invalid(String),
    /// A required task or filesystem operation failed.
    #[error("workspace context failed: {0}")]
    Failed(String),
}

/// Process-local trust-aware workspace context source.
#[async_trait]
pub trait WorkspaceContext: fmt::Debug + Send + Sync + 'static {
    /// Reads one complete bounded snapshot for the exact Session Header and messages.
    async fn snapshot(
        &self,
        header: &SessionHeader,
        messages: &[&AgentMessage],
    ) -> Result<WorkspaceContextSnapshot, WorkspaceContextError>;
}

/// Nominal Local contract for [`WorkspaceContext`].
#[derive(Debug)]
pub struct WorkspaceContextContract;

impl LocalContract for WorkspaceContextContract {
    const KEY: &'static str = "rsi.agent.workspace-context";
    type Service = dyn WorkspaceContext;
}

/// Local filesystem source configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceContextConfig {
    /// Optional trusted user instruction file.
    pub user_instruction_file: Option<PathBuf>,
    /// Ordered trusted user skill roots, strongest first.
    #[serde(default)]
    pub user_skill_roots: Vec<PathBuf>,
}

impl WorkspaceContextConfig {
    fn validate(&self) -> Result<(), WorkspaceContextError> {
        if self.user_skill_roots.len() > 32 {
            return Err(WorkspaceContextError::Invalid(
                "user skill roots exceed 32".into(),
            ));
        }
        for path in self
            .user_instruction_file
            .iter()
            .chain(self.user_skill_roots.iter())
        {
            if !path.is_absolute() {
                return Err(WorkspaceContextError::Invalid(format!(
                    "configured context path is not absolute: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

/// Ordinary filesystem-backed source.
#[derive(Clone, Debug)]
pub struct LocalWorkspaceContext {
    config: WorkspaceContextConfig,
}

impl LocalWorkspaceContext {
    /// Creates a validated source.
    pub fn new(config: WorkspaceContextConfig) -> Result<Self, WorkspaceContextError> {
        config.validate()?;
        Ok(Self { config })
    }
}

#[derive(Clone, Debug)]
struct SelectedSkill {
    name: String,
    description: String,
    source: String,
    path: PathBuf,
    project_authority: Option<Arc<ProjectAuthority>>,
    model_invocable: bool,
    user_invocable: bool,
}

#[derive(Clone, Copy, Debug)]
enum SkillEntryKind {
    Directory,
    File,
    Other,
}

type SkillEntries = (Vec<(PathBuf, SkillEntryKind)>, bool);

#[derive(Debug)]
struct ProjectAuthority {
    root: PathBuf,
    directory: Dir,
}

impl ProjectAuthority {
    fn open(root: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        let directory = open_absolute_directory_no_follow(root)?;
        #[cfg(not(unix))]
        let directory = Dir::open_ambient_dir(root, ambient_authority())?;
        Ok(Self {
            root: root.to_owned(),
            directory,
        })
    }

    fn open_regular_file(&self, path: &Path) -> std::io::Result<Option<File>> {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Ok(None);
        };
        match self.directory.symlink_metadata(relative) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        let file = match open_relative_file_no_follow(&self.directory, relative) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if is_link_rejection(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        #[cfg(not(unix))]
        let file = match self.directory.open(relative) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        #[cfg(not(unix))]
        let file = file.into_std();
        Ok(file.metadata()?.is_file().then_some(file))
    }

    fn skill_entries(&self, path: &Path, maximum: usize) -> std::io::Result<Option<SkillEntries>> {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Ok(None);
        };
        #[cfg(unix)]
        let directory = match open_relative_directory_no_follow(&self.directory, relative) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if is_link_rejection(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        #[cfg(not(unix))]
        let directory = {
            let metadata = match self.directory.symlink_metadata(relative) {
                Ok(metadata) if metadata.is_dir() => metadata,
                Ok(_) => return Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let _ = metadata;
            self.directory.open_dir(relative)?
        };
        let mut entries = Vec::new();
        for entry in directory.entries()?.take(maximum.saturating_add(1)) {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let kind = if file_type.is_dir() {
                SkillEntryKind::Directory
            } else if file_type.is_file() {
                SkillEntryKind::File
            } else {
                SkillEntryKind::Other
            };
            entries.push((path.join(entry.file_name()), kind));
        }
        let overflow = entries.len() > maximum;
        entries.truncate(maximum);
        Ok(Some((entries, overflow)))
    }
}

#[cfg(unix)]
fn open_absolute_directory_no_follow(path: &Path) -> std::io::Result<Dir> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::path::Component;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "project root is not absolute",
        ));
    }
    let root = openat(
        rustix::fs::CWD,
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut directory = Dir::from_std_file(root.into());
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => {
                directory = open_relative_directory_no_follow(&directory, Path::new(component))?;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "project root is not normalized",
                ));
            }
        }
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_relative_directory_no_follow(directory: &Dir, path: &Path) -> std::io::Result<Dir> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::os::fd::AsFd as _;
    use std::path::Component;

    let mut current = directory.try_clone()?;
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "project-relative directory is not normalized",
            ));
        };
        let next = openat(
            current.as_fd(),
            Path::new(component),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        current = Dir::from_std_file(next.into());
    }
    Ok(current)
}

#[cfg(unix)]
fn open_relative_file_no_follow(directory: &Dir, path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::os::fd::AsFd as _;

    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "file path has no name")
    })?;
    let parent = open_relative_directory_no_follow(directory, parent)?;
    let file = openat(
        parent.as_fd(),
        Path::new(name),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    Ok(file.into())
}

#[cfg(unix)]
fn is_link_rejection(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| code == libc::ELOOP || code == libc::ENOTDIR)
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
    #[serde(default = "default_true", rename = "user-invocable")]
    user_invocable: bool,
}

const fn default_true() -> bool {
    true
}

#[async_trait]
impl WorkspaceContext for LocalWorkspaceContext {
    async fn snapshot(
        &self,
        header: &SessionHeader,
        messages: &[&AgentMessage],
    ) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        let config = self.config.clone();
        let cwd = PathBuf::from(header.canonical_cwd());
        let workspace_trust = header.workspace_trust();
        let invocations = invoked_names(messages);
        tokio::task::spawn_blocking(move || {
            snapshot_blocking(&config, &cwd, workspace_trust, &invocations)
        })
        .await
        .map_err(|error| WorkspaceContextError::Failed(error.to_string()))?
    }
}

fn snapshot_blocking(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    workspace_trust: WorkspaceTrust,
    invoked_names: &[String],
) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
    let mut complete = true;
    let project_root = if workspace_trust == WorkspaceTrust::Trusted {
        find_project_root(cwd, &mut complete)
    } else {
        None
    };
    let project_authority = project_root.as_deref().and_then(|root| {
        ProjectAuthority::open(root).map_or_else(
            |_| {
                complete = false;
                None
            },
            |authority| Some(Arc::new(authority)),
        )
    });
    let mut user_instruction_sections = Vec::new();
    if let Some(path) = &config.user_instruction_file
        && let Some(text) = read_bounded_utf8(path, None, &mut complete)
    {
        user_instruction_sections.push((display_path(path), text));
    }
    let mut project_instruction_sections = Vec::new();
    if let (Some(root), Some(authority)) = (&project_root, &project_authority) {
        for directory in directories_between(root, cwd)? {
            let path = directory.join("AGENTS.md");
            if let Some(text) = read_bounded_utf8(&path, Some(authority), &mut complete) {
                project_instruction_sections.push((display_project_path(root, &path), text));
            }
        }
    }
    let instructions =
        render_instructions(&user_instruction_sections, &project_instruction_sections);

    let mut skills = BTreeMap::new();
    let mut inspected = 0_usize;
    for root in &config.user_skill_roots {
        discover_skills(root, None, &mut skills, &mut inspected, &mut complete);
    }
    if let (Some(root), Some(authority)) = (&project_root, &project_authority) {
        discover_skills(
            &root.join(".agents/skills"),
            Some(authority),
            &mut skills,
            &mut inspected,
            &mut complete,
        );
    }
    let selected = skills.into_values().collect::<Vec<_>>();
    let skill_catalog = render_skill_catalog(&selected);
    let invocations = invoked_names
        .iter()
        .filter_map(|name| {
            selected
                .iter()
                .find(|skill| skill.name == name.as_str() && skill.user_invocable)
                .and_then(|skill| {
                    let raw = read_bounded_utf8(
                        &skill.path,
                        skill.project_authority.as_deref(),
                        &mut complete,
                    )?;
                    selected_skill_invocation(skill, &raw, &mut complete)
                })
        })
        .collect();
    Ok(WorkspaceContextSnapshot {
        complete,
        instructions_sha256: digest(instructions.as_deref().unwrap_or("")),
        instructions,
        skill_catalog_sha256: digest(skill_catalog.as_deref().unwrap_or("")),
        skill_catalog,
        invocations,
    })
}

fn find_project_root(cwd: &Path, complete: &mut bool) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        match fs::symlink_metadata(current.join(".git")) {
            Ok(_) => return Some(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => *complete = false,
        }
        if !current.pop() {
            return None;
        }
    }
}

fn directories_between(root: &Path, cwd: &Path) -> Result<Vec<PathBuf>, WorkspaceContextError> {
    let relative = cwd.strip_prefix(root).map_err(|_| {
        WorkspaceContextError::Invalid("project root does not contain Session cwd".into())
    })?;
    let mut directories = vec![root.to_path_buf()];
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        directories.push(current.clone());
    }
    if directories.len() > MAXIMUM_WORKSPACE_INSTRUCTION_FILES {
        directories.drain(..directories.len() - MAXIMUM_WORKSPACE_INSTRUCTION_FILES);
    }
    Ok(directories)
}

fn read_bounded_utf8(
    path: &Path,
    project_authority: Option<&ProjectAuthority>,
    complete: &mut bool,
) -> Option<String> {
    let mut file = match open_contained_regular_file(path, project_authority) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(_) => {
            *complete = false;
            return None;
        }
    };
    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take(u64::try_from(MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES).ok()? + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        *complete = false;
        return None;
    }
    if bytes.len() > MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.trim().is_empty() && session_safe_text(&text)).then_some(text)
}

fn read_skill_metadata_prefix(
    path: &Path,
    project_authority: Option<&ProjectAuthority>,
    complete: &mut bool,
) -> Option<String> {
    let mut file = match open_contained_regular_file(path, project_authority) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(_) => {
            *complete = false;
            return None;
        }
    };
    match file.metadata() {
        Ok(metadata) if metadata.len() > MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES as u64 => {
            return None;
        }
        Ok(_) => {}
        Err(_) => {
            *complete = false;
            return None;
        }
    }
    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take(u64::try_from(MAXIMUM_SKILL_METADATA_PREFIX_BYTES).ok()?)
        .read_to_end(&mut bytes)
        .is_err()
    {
        *complete = false;
        return None;
    }
    let valid_len = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return None,
    };
    bytes.truncate(valid_len);
    let text = String::from_utf8(bytes).ok()?;
    session_safe_text(&text).then_some(text)
}

fn open_contained_regular_file(
    path: &Path,
    project_authority: Option<&ProjectAuthority>,
) -> std::io::Result<Option<File>> {
    if let Some(authority) = project_authority {
        return authority.open_regular_file(path);
    }
    let link_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !link_metadata.file_type().is_file() {
        return Ok(None);
    }
    let file = match open_file_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    Ok(Some(file))
}

fn session_safe_text(text: &str) -> bool {
    !text.bytes().any(|byte| matches!(byte, b'\0' | b'\x7f'))
}

fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn render_instructions(
    user_sections: &[(String, String)],
    project_sections: &[(String, String)],
) -> Option<String> {
    if user_sections.is_empty() && project_sections.is_empty() {
        return None;
    }
    let mut rendered = String::from(
        "The following workspace instructions apply. More specific entries take precedence and none override system or direct user instructions.\n",
    );
    for (source, text) in user_sections {
        let section = format!("\nInstructions from: {source}\n\n{text}\n");
        if rendered.len().saturating_add(section.len()) <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
        {
            rendered.push_str(&section);
        }
    }
    let mut selected = Vec::new();
    let mut selected_bytes = rendered.len();
    for (source, text) in project_sections.iter().rev() {
        let section = format!("\nInstructions from: {source}\n\n{text}\n");
        if selected_bytes.saturating_add(section.len()) <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
        {
            selected_bytes += section.len();
            selected.push(section);
        }
    }
    selected.reverse();
    for section in selected {
        rendered.push_str(&section);
    }
    Some(rendered)
}

fn discover_skills(
    root: &Path,
    project_authority: Option<&Arc<ProjectAuthority>>,
    selected: &mut BTreeMap<String, SelectedSkill>,
    inspected: &mut usize,
    complete: &mut bool,
) {
    let remaining = MAXIMUM_WORKSPACE_SKILL_ENTRIES.saturating_sub(*inspected);
    let (mut entries, overflow) = if let Some(authority) = project_authority {
        match authority.skill_entries(root, remaining) {
            Ok(Some(entries)) => entries,
            Ok(None) => return,
            Err(_) => {
                *complete = false;
                return;
            }
        }
    } else {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => {
                *complete = false;
                return;
            }
        };
        let mut retained = Vec::new();
        for entry in entries.take(remaining.saturating_add(1)) {
            let Ok(entry) = entry else {
                *complete = false;
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                *complete = false;
                continue;
            };
            let kind = if file_type.is_dir() {
                SkillEntryKind::Directory
            } else if file_type.is_file() {
                SkillEntryKind::File
            } else {
                SkillEntryKind::Other
            };
            retained.push((entry.path(), kind));
        }
        let overflow = retained.len() > remaining;
        retained.truncate(remaining);
        (retained, overflow)
    };
    if overflow {
        *complete = false;
    }
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    for (path, kind) in entries {
        *inspected += 1;
        let skill_path = if matches!(kind, SkillEntryKind::Directory) {
            path.join("SKILL.md")
        } else if matches!(kind, SkillEntryKind::File)
            && path.extension().is_some_and(|extension| extension == "md")
        {
            path
        } else {
            continue;
        };
        let Some(raw) =
            read_skill_metadata_prefix(&skill_path, project_authority.map(AsRef::as_ref), complete)
        else {
            continue;
        };
        let Some(skill) = parse_skill(&skill_path, project_authority, &raw) else {
            continue;
        };
        selected.entry(skill.name.clone()).or_insert(skill);
    }
}

fn parse_skill(
    path: &Path,
    project_authority: Option<&Arc<ProjectAuthority>>,
    raw: &str,
) -> Option<SelectedSkill> {
    let (yaml, body) = split_skill(raw)?;
    let frontmatter: SkillFrontmatter = yaml_serde::from_str(yaml).ok()?;
    if !valid_skill_name(&frontmatter.name)
        || frontmatter.description.trim().is_empty()
        || body.trim().is_empty()
    {
        return None;
    }
    Some(SelectedSkill {
        name: frontmatter.name,
        description: frontmatter
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        source: project_authority.map_or_else(
            || display_path(path),
            |authority| display_project_path(&authority.root, path),
        ),
        path: path.to_owned(),
        project_authority: project_authority.cloned(),
        model_invocable: !frontmatter.disable_model_invocation,
        user_invocable: frontmatter.user_invocable,
    })
}

fn split_skill(raw: &str) -> Option<(&str, &str)> {
    raw.strip_prefix("---\n")
        .and_then(|content| content.split_once("\n---\n"))
        .or_else(|| raw.strip_prefix("---\r\n")?.split_once("\r\n---\r\n"))
}

fn selected_skill_invocation(
    skill: &SelectedSkill,
    raw: &str,
    complete: &mut bool,
) -> Option<WorkspaceSkillInvocation> {
    let Some((_, body)) = split_skill(raw) else {
        *complete = false;
        return None;
    };
    let Some(current) = parse_skill(&skill.path, skill.project_authority.as_ref(), raw) else {
        *complete = false;
        return None;
    };
    if current.name != skill.name
        || current.description != skill.description
        || current.model_invocable != skill.model_invocable
        || current.user_invocable != skill.user_invocable
    {
        *complete = false;
        return None;
    }
    Some(WorkspaceSkillInvocation {
        name: skill.name.clone(),
        source: skill.source.clone(),
        text: render_skill_body(skill, body),
    })
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn render_skill_catalog(skills: &[SelectedSkill]) -> Option<String> {
    let visible = skills
        .iter()
        .filter(|skill| skill.model_invocable)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return None;
    }
    let mut rendered = String::from(
        "Available skills are summaries only. Load or directly invoke the exact selected skill before following it:\n<available_skills>\n",
    );
    for skill in visible {
        let description = skill.description.chars().take(500).collect::<String>();
        let line = format!("- `{}`: {}\n", skill.name, description);
        if rendered.len().saturating_add(line.len()).saturating_add(21)
            > MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
        {
            break;
        }
        rendered.push_str(&line);
    }
    rendered.push_str("</available_skills>");
    Some(rendered)
}

fn render_skill_body(skill: &SelectedSkill, body: &str) -> String {
    let mut text = format!(
        "<skill_content name=\"{}\">\nSource: {}\n\n<skill_instructions>\n",
        skill.name, skill.source
    );
    let remaining = MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES.saturating_sub(
        text.len()
            .saturating_add("\n</skill_instructions>\n</skill_content>".len()),
    );
    text.push_str(utf8_prefix(body, remaining));
    text.push_str("\n</skill_instructions>\n</skill_content>");
    text
}

fn utf8_prefix(text: &str, maximum_bytes: usize) -> &str {
    let mut end = text.len().min(maximum_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn invoked_names(messages: &[&AgentMessage]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for message in messages {
        if !matches!(message.source, AgentMessageSource::Human) {
            continue;
        }
        for content in &message.content {
            let AgentMessageContent::Text { text } = content else {
                continue;
            };
            let Some(first) = text.lines().find(|line| !line.trim().is_empty()) else {
                continue;
            };
            let Some(token) = first.split_whitespace().next() else {
                continue;
            };
            let Some(name) = token.strip_prefix('/') else {
                continue;
            };
            if valid_skill_name(name) && seen.insert(name.to_owned()) {
                names.push(name.to_owned());
            }
        }
    }
    names
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn display_project_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map_or_else(|_| display_path(path), display_path)
}

fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Ordinary plugin factory for [`LocalWorkspaceContext`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkspaceContextFactory;

fn workspace_context_config_retained_bytes(
    config: &WorkspaceContextConfig,
) -> rsi_meta::Result<usize> {
    config
        .user_instruction_file
        .iter()
        .chain(&config.user_skill_roots)
        .try_fold(
            std::mem::size_of::<WorkspaceContextConfig>(),
            |total, path| {
                total.checked_add(path.as_os_str().len()).ok_or_else(|| {
                    MetaError::InvalidInput(
                        "workspace-context retained byte count overflowed".into(),
                    )
                })
            },
        )
}

#[async_trait]
impl PluginFactory for WorkspaceContextFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: WorkspaceContextConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = workspace_context_config_retained_bytes(&config)?;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<WorkspaceContextConfig>()?;
        let service: Arc<dyn WorkspaceContext> = Arc::new(
            LocalWorkspaceContext::new(config)
                .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let supply = plan
            .context()
            .provide_local::<WorkspaceContextContract>(service)?;
        plan.defer(
            "withdraw Agent workspace context",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn project_authority_never_reopens_a_replaced_ambient_root() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("project");
        let held_project = temporary.path().join("held-project");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(project.join("AGENTS.md"), "PINNED INSTRUCTION").unwrap();
        fs::write(outside.join("AGENTS.md"), "OUTSIDE INSTRUCTION").unwrap();
        let authority = ProjectAuthority::open(&project).unwrap();

        fs::rename(&project, &held_project).unwrap();
        symlink(&outside, &project).unwrap();

        let mut complete = true;
        assert_eq!(
            read_bounded_utf8(&project.join("AGENTS.md"), Some(&authority), &mut complete,)
                .as_deref(),
            Some("PINNED INSTRUCTION")
        );
        assert!(complete);
    }

    #[cfg(unix)]
    #[test]
    fn project_authority_rejects_a_symlinked_path_component() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let real = temporary.path().join("real");
        let alias = temporary.path().join("alias");
        fs::create_dir_all(&real).unwrap();
        symlink(&real, &alias).unwrap();

        assert!(ProjectAuthority::open(&alias).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unexpected_filesystem_errors_make_the_snapshot_incomplete() {
        let temporary = tempfile::tempdir().unwrap();
        let invalid_source = temporary.path().join("x".repeat(300));
        let snapshot = snapshot_blocking(
            &WorkspaceContextConfig {
                user_instruction_file: Some(invalid_source),
                user_skill_roots: Vec::new(),
            },
            temporary.path(),
            WorkspaceTrust::Untrusted,
            &[],
        )
        .unwrap();

        assert!(!snapshot.complete);
        assert!(snapshot.instructions.is_none());
    }

    #[test]
    fn skill_body_rendering_truncates_on_a_utf8_byte_boundary() {
        let skill = SelectedSkill {
            name: "multibyte".into(),
            description: "bounded body".into(),
            source: "SKILL.md".into(),
            path: PathBuf::from("SKILL.md"),
            project_authority: None,
            model_invocable: true,
            user_invocable: true,
        };

        let rendered = render_skill_body(
            &skill,
            &"🦀".repeat(MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES),
        );

        assert!(rendered.len() <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES);
        assert!(rendered.is_char_boundary(rendered.len()));
        assert!(rendered.ends_with("\n</skill_instructions>\n</skill_content>"));
    }

    #[test]
    fn invoked_skill_rejects_identity_drift_after_catalog_selection() {
        let selected = SelectedSkill {
            name: "selected".into(),
            description: "selected description".into(),
            source: "SKILL.md".into(),
            path: PathBuf::from("SKILL.md"),
            project_authority: None,
            model_invocable: true,
            user_invocable: true,
        };
        let replacement =
            "---\nname: replacement\ndescription: replacement description\n---\nREPLACEMENT BODY";
        let mut complete = true;

        assert!(selected_skill_invocation(&selected, replacement, &mut complete).is_none());
        assert!(!complete);
    }

    #[test]
    fn invoked_skill_accepts_a_current_body_for_the_selected_identity() {
        let selected = SelectedSkill {
            name: "selected".into(),
            description: "selected description".into(),
            source: "SKILL.md".into(),
            path: PathBuf::from("SKILL.md"),
            project_authority: None,
            model_invocable: true,
            user_invocable: true,
        };
        let current = "---\nname: selected\ndescription: selected   description\n---\nCURRENT BODY";
        let mut complete = true;

        let invocation = selected_skill_invocation(&selected, current, &mut complete).unwrap();
        assert!(complete);
        assert!(invocation.text.contains("CURRENT BODY"));
    }

    #[test]
    fn retained_bytes_include_configured_path_storage() {
        let config = WorkspaceContextConfig {
            user_instruction_file: Some(PathBuf::from("/tmp/instructions")),
            user_skill_roots: vec![PathBuf::from("/tmp/skills"), PathBuf::from("/opt/skills")],
        };
        let path_bytes = config
            .user_instruction_file
            .iter()
            .chain(&config.user_skill_roots)
            .map(|path| path.as_os_str().len())
            .sum::<usize>();
        assert_eq!(
            workspace_context_config_retained_bytes(&config).unwrap(),
            std::mem::size_of::<WorkspaceContextConfig>() + path_bytes
        );
    }
}
