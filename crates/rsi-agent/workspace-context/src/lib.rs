//! Bounded workspace instruction and skill snapshots.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
#[cfg(not(unix))]
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, SessionHeader,
};
#[cfg(unix)]
use rsi_files_native_fs::{
    is_link_rejection, open_absolute_directory_no_follow, open_relative_file_no_follow,
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
use tokio_util::sync::CancellationToken;
mod agents;
pub use agents::WorkspaceAgentDefinition;
mod budget;
mod observation;
use observation::Observation;
mod requests;
pub mod skill_input;
pub use requests::WorkspaceSkillRequests;
mod contributor;
mod skill_files;
mod skills;
use budget::{SnapshotBudget, SnapshotOwner};
pub use contributor::WorkspaceContributorFactory;
use skill_files::{SkillDiscovery, SkillSource};
pub use skills::{SkillAudience, WorkspaceSkillToolsFactory};

/// Maximum bytes read from one instruction or skill source.
pub const MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum bytes in one rendered instruction baseline, skill catalog, or body.
pub const MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES: usize = 512 * 1024;
/// Maximum instruction files in one snapshot.
pub const MAXIMUM_WORKSPACE_INSTRUCTION_FILES: usize = 64;
/// Maximum entries inspected across all skill roots.
pub const MAXIMUM_WORKSPACE_SKILL_ENTRIES: usize = 256;
const MAXIMUM_SKILL_METADATA_PREFIX_BYTES: usize = 16 * 1024;
const INSTRUCTIONS_PREAMBLE: &str = "The following workspace instructions apply. More specific entries take precedence and none override system or direct user instructions.\n";

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
    /// First source failure, with bounded escaped path text; absent on complete observations.
    pub diagnostic: Option<String>,
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
    /// Snapshot exceeds its aggregate retained-byte admission.
    #[error("workspace context capacity exhausted")]
    Capacity,
    /// Source withdrawal closed snapshot admission.
    #[error("workspace context source is closed")]
    Closed,
    /// The caller supplied an invalid durable or configured boundary.
    #[error("invalid workspace context: {0}")]
    Invalid(String),
    /// A required task or filesystem operation failed.
    #[error("workspace context failed: {0}")]
    Failed(String),
}

/// Process-local workspace context source.
#[async_trait]
pub trait WorkspaceContext: fmt::Debug + Send + Sync + 'static {
    /// Reads current Markdown agents or one exact winning name. Up to 32 reserved
    /// names are checked in the same walk and returned as unavailable collisions.
    async fn agents(
        &self,
        header: &SessionHeader,
        id: Option<&str>,
        reserved_names: &BTreeSet<String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<WorkspaceAgentDefinition>, WorkspaceContextError>;
    /// Reads an explicitly selected skill or its catalog under independent invocation flags.
    async fn skills(
        &self,
        header: &SessionHeader,
        id: Option<&str>,
        audience: SkillAudience,
        cancellation: CancellationToken,
    ) -> Result<rsi_agent_session_protocol::SessionResourceValue, WorkspaceContextError>;
    /// Reads one complete bounded snapshot for the exact Header and selected skill names.
    async fn snapshot(
        &self,
        header: &SessionHeader,
        requests: &WorkspaceSkillRequests,
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
    /// Optional configured user instruction file.
    pub user_instruction_file: Option<PathBuf>,
    /// Ordered configured user skill roots, strongest first.
    #[serde(default)]
    pub user_skill_roots: Vec<PathBuf>,
    /// Ordered personal agent definition roots, strongest first.
    #[serde(default)]
    pub user_agent_roots: Vec<PathBuf>,
}

impl WorkspaceContextConfig {
    fn validate(&self) -> Result<(), WorkspaceContextError> {
        if self.user_skill_roots.len() > 32 || self.user_agent_roots.len() > 32 {
            return Err(WorkspaceContextError::Invalid(
                "user skill or agent roots exceed 32".into(),
            ));
        }
        for path in self
            .user_instruction_file
            .iter()
            .chain(self.user_skill_roots.iter())
            .chain(self.user_agent_roots.iter())
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
    config: Arc<WorkspaceContextConfig>,
    owner: Arc<SnapshotOwner>,
}

impl LocalWorkspaceContext {
    /// Creates a validated source.
    pub fn new(config: WorkspaceContextConfig) -> Result<Self, WorkspaceContextError> {
        config.validate()?;
        budget::config_retained_bytes(&config)?;
        Ok(Self {
            config: Arc::new(config),
            owner: Arc::new(SnapshotOwner::default()),
        })
    }
}

#[derive(Clone, Debug)]
struct SelectedSkill {
    name: String,
    description: String,
    source: String,
    file: SkillSource,
    model_invocable: bool,
    user_invocable: bool,
}

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
        open_directory_regular_file(&self.directory, relative)
    }
}

fn open_directory_regular_file(directory: &Dir, relative: &Path) -> std::io::Result<Option<File>> {
    match directory.symlink_metadata(relative) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    let file = match open_relative_file_no_follow(directory, relative) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_link_rejection(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    #[cfg(not(unix))]
    let file = {
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        match directory.open_with(relative, &options) {
            Ok(file) => file.into_std(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    };
    Ok(file.metadata()?.is_file().then_some(file))
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
    async fn agents(
        &self,
        header: &SessionHeader,
        id: Option<&str>,
        reserved_names: &BTreeSet<String>,
        cancellation: CancellationToken,
    ) -> Result<Vec<WorkspaceAgentDefinition>, WorkspaceContextError> {
        if id.is_some_and(|id| !valid_skill_name(id)) {
            return Err(WorkspaceContextError::Invalid("invalid agent name".into()));
        }
        if reserved_names.len() > 32 || reserved_names.iter().any(|name| !valid_skill_name(name)) {
            return Err(WorkspaceContextError::Invalid(
                "invalid reserved agent names".into(),
            ));
        }
        let stop = self.owner.cancellation.child_token();
        let _guard = stop.clone().drop_guard();
        let lease = cancellation
            .run_until_cancelled(self.owner.acquire())
            .await
            .ok_or(WorkspaceContextError::Closed)??;
        let config = self.config.clone();
        let cwd = PathBuf::from(header.canonical_cwd());
        let budget = SnapshotBudget::new(&config, &cwd, stop)?;
        let id = id.map(str::to_owned);
        let reserved_names = reserved_names.clone();
        cancellation
            .run_until_cancelled(lease.run(move || {
                agents::read_agents(&config, &cwd, id.as_deref(), &reserved_names, budget)
            }))
            .await
            .ok_or(WorkspaceContextError::Closed)?
    }
    async fn skills(
        &self,
        header: &SessionHeader,
        id: Option<&str>,
        audience: SkillAudience,
        cancellation: CancellationToken,
    ) -> Result<rsi_agent_session_protocol::SessionResourceValue, WorkspaceContextError> {
        if id.is_some_and(|id| !valid_skill_name(id)) {
            return Err(WorkspaceContextError::Invalid("invalid skill name".into()));
        }
        let stop = self.owner.cancellation.child_token();
        let _guard = stop.clone().drop_guard();
        let lease = cancellation
            .run_until_cancelled(self.owner.acquire())
            .await
            .ok_or(WorkspaceContextError::Closed)??;
        let config = Arc::clone(&self.config);
        let cwd = PathBuf::from(header.canonical_cwd());
        let budget = SnapshotBudget::new(&config, &cwd, stop)?;
        let id = id.map(str::to_owned);
        cancellation
            .run_until_cancelled(
                lease.run(move || {
                    skills::read_skills(&config, &cwd, id.as_deref(), audience, budget)
                }),
            )
            .await
            .ok_or(WorkspaceContextError::Closed)?
    }
    async fn snapshot(
        &self,
        header: &SessionHeader,
        requests: &WorkspaceSkillRequests,
    ) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        let lease = self.owner.acquire().await?;
        let config = Arc::clone(&self.config);
        let budget = SnapshotBudget::new(
            &config,
            Path::new(header.canonical_cwd()),
            self.owner.cancellation.clone(),
        )?;
        let cwd = PathBuf::from(header.canonical_cwd());
        let invocations = requests.names().to_vec();
        lease
            .run(move || snapshot_with_budget(&config, &cwd, &invocations, budget))
            .await
    }
}

#[cfg(test)]
fn snapshot_blocking(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    invoked_names: &[String],
) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
    let budget = SnapshotBudget::new(config, cwd, CancellationToken::new())?;
    snapshot_with_budget(config, cwd, invoked_names, budget)
}

fn snapshot_with_budget(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    invoked_names: &[String],
    mut budget: SnapshotBudget,
) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
    let mut observation = Observation::default();
    let (project_root, git_root) = skills::project_boundary(cwd, &mut observation);
    let project_authority = project_root
        .as_deref()
        .filter(|_| git_root)
        .and_then(|root| {
            ProjectAuthority::open(root).map_or_else(
                |error| {
                    observation.io(root, "open instruction root", &error);
                    None
                },
                Some,
            )
        });
    let instructions = read_instructions(
        config,
        cwd,
        project_root.as_deref().filter(|_| git_root),
        project_authority.as_ref(),
        &budget,
        &mut observation,
    )?;
    let selected = skills::discover_selected(
        config,
        cwd,
        project_root.as_deref(),
        &mut observation,
        &mut budget,
    )?;
    let skill_catalog = render_skill_catalog(&selected);
    let mut invocations = Vec::new();
    for name in invoked_names {
        budget.check()?;
        let Ok(index) = selected.binary_search_by(|skill| skill.name.cmp(name)) else {
            continue;
        };
        let skill = &selected[index];
        if !skill.user_invocable {
            continue;
        }
        let reservation = MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
            .checked_add(skill.name.len())
            .and_then(|bytes| bytes.checked_add(skill.source.len()))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<WorkspaceSkillInvocation>()))
            .ok_or(WorkspaceContextError::Capacity)?;
        budget.reserve(reservation)?;
        let invocation = read_skill_invocation(skill, &mut observation, &budget.cancellation);
        let retained = invocation.as_ref().map_or(0, |invocation| {
            invocation.text.capacity()
                + invocation.name.capacity()
                + invocation.source.capacity()
                + std::mem::size_of::<WorkspaceSkillInvocation>()
        });
        if retained > reservation {
            return Err(WorkspaceContextError::Capacity);
        }
        budget.release(reservation - retained);
        if let Some(invocation) = invocation {
            invocations.push(invocation);
        }
    }
    budget.check()?;
    Ok(WorkspaceContextSnapshot {
        complete: observation.is_complete(),
        diagnostic: observation.diagnostic,
        instructions_sha256: digest(instructions.as_deref().unwrap_or("")),
        instructions,
        skill_catalog_sha256: digest(skill_catalog.as_deref().unwrap_or("")),
        skill_catalog,
        invocations,
    })
}

fn find_project_root(cwd: &Path, observation: &mut Observation) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        match fs::symlink_metadata(current.join(".git")) {
            Ok(_) => return Some(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => observation.io(&current.join(".git"), "find project root", &error),
        }
        if !current.pop() {
            return None;
        }
    }
}

fn directories_between(root: &Path, cwd: &Path) -> Result<Vec<PathBuf>, WorkspaceContextError> {
    cwd.strip_prefix(root).map_err(|_| {
        WorkspaceContextError::Invalid("project root does not contain Session cwd".into())
    })?;
    let mut directories: Vec<_> = cwd
        .ancestors()
        .take_while(|directory| directory.starts_with(root))
        .take(MAXIMUM_WORKSPACE_INSTRUCTION_FILES)
        .map(Path::to_path_buf)
        .collect();
    directories.reverse();
    Ok(directories)
}

fn read_bounded_utf8(
    path: &Path,
    project_authority: Option<&ProjectAuthority>,
    observation: &mut Observation,
    cancellation: &CancellationToken,
) -> Option<String> {
    if cancellation.is_cancelled() {
        return None;
    }
    let file = match open_contained_regular_file(path, project_authority) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(error) => {
            observation.io(path, "open instructions", &error);
            return None;
        }
    };
    read_opened_utf8(file, path, observation, cancellation)
}

fn read_opened_utf8(
    mut file: File,
    path: &Path,
    observation: &mut Observation,
    cancellation: &CancellationToken,
) -> Option<String> {
    let bytes = read_source_chunks(
        &mut file,
        path,
        MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES + 1,
        cancellation,
        observation,
    )?;
    if bytes.len() > MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.trim().is_empty() && session_safe_text(&text)).then_some(text)
}

fn read_skill_metadata_prefix(
    source: &SkillSource,
    observation: &mut Observation,
    cancellation: &CancellationToken,
) -> Option<String> {
    if cancellation.is_cancelled() {
        return None;
    }
    let mut file = match source.open() {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(error) => {
            observation.io(&source.logical_path, "open skill metadata", &error);
            return None;
        }
    };
    match file.metadata() {
        Ok(metadata) if metadata.len() > MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES as u64 => {
            return None;
        }
        Ok(_) => {}
        Err(error) => {
            observation.io(&source.logical_path, "stat skill metadata", &error);
            return None;
        }
    }
    let mut bytes = read_source_chunks(
        &mut file,
        &source.logical_path,
        MAXIMUM_SKILL_METADATA_PREFIX_BYTES,
        cancellation,
        observation,
    )?;
    let valid_len = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return None,
    };
    bytes.truncate(valid_len);
    let text = String::from_utf8(bytes).ok()?;
    session_safe_text(&text).then_some(text)
}

fn read_source_chunks(
    file: &mut File,
    path: &Path,
    maximum: usize,
    cancellation: &CancellationToken,
    observation: &mut Observation,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    while bytes.len() < maximum {
        if cancellation.is_cancelled() {
            return None;
        }
        let remaining = chunk.len().min(maximum - bytes.len());
        match file.read(&mut chunk[..remaining]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) => {
                observation.io(path, "read source", &error);
                return None;
            }
        }
    }
    // The collector accounts for retained source length within its scratch
    // envelope, so read capacity must not escape with each selected file.
    Some(bytes.into_boxed_slice().into_vec())
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

fn instruction_section<'a>(source: &'a str, text: &'a str) -> [&'a str; 5] {
    ["\nInstructions from: ", source, "\n\n", text, "\n"]
}

fn instruction_section_bytes(source: &str, text: &str) -> usize {
    instruction_section(source, text)
        .iter()
        .fold(0usize, |sum, part| sum.saturating_add(part.len()))
}

fn render_instructions(
    user_sections: &[(String, String)],
    project_sections: &[(String, String)],
) -> Option<String> {
    if user_sections.is_empty() && project_sections.is_empty() {
        return None;
    }
    let mut rendered = String::from(INSTRUCTIONS_PREAMBLE);
    for (source, text) in user_sections {
        if rendered
            .len()
            .saturating_add(instruction_section_bytes(source, text))
            <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
        {
            for part in instruction_section(source, text) {
                rendered.push_str(part);
            }
        }
    }
    let mut selected = Vec::new();
    let mut selected_bytes = rendered.len();
    for (source, text) in project_sections.iter().rev() {
        let bytes = instruction_section_bytes(source, text);
        if selected_bytes.saturating_add(bytes) <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES {
            selected_bytes += bytes;
            selected.push((source, text));
        }
    }
    selected.reverse();
    for (source, text) in selected {
        for part in instruction_section(source, text) {
            rendered.push_str(part);
        }
    }
    Some(rendered)
}

fn parse_skill(file: SkillSource, project_root: Option<&Path>, raw: &str) -> Option<SelectedSkill> {
    let frontmatter = parse_skill_metadata(raw)?;
    let source = project_root.map_or_else(
        || display_path(&file.logical_path),
        |root| display_project_path(root, &file.logical_path),
    );
    Some(SelectedSkill {
        name: frontmatter.name,
        description: frontmatter.description,
        source,
        file,
        model_invocable: !frontmatter.disable_model_invocation,
        user_invocable: frontmatter.user_invocable,
    })
}

fn parse_skill_metadata(raw: &str) -> Option<SkillFrontmatter> {
    let (yaml, body) = split_skill(raw)?;
    let mut frontmatter: SkillFrontmatter = yaml_serde::from_str(yaml).ok()?;
    if !valid_skill_name(&frontmatter.name)
        || frontmatter.description.trim().is_empty()
        || body.trim().is_empty()
    {
        return None;
    }
    frontmatter.description = frontmatter
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(frontmatter)
}

fn read_skill_invocation(
    skill: &SelectedSkill,
    observation: &mut Observation,
    cancellation: &CancellationToken,
) -> Option<WorkspaceSkillInvocation> {
    let path = &skill.file.logical_path;
    let file = match skill.file.open() {
        Ok(Some(file)) => file,
        Ok(None) => {
            observation.fail(path, "selected skill is no longer a regular file");
            return None;
        }
        Err(error) => {
            observation.io(path, "open selected skill", &error);
            return None;
        }
    };
    // Invalid optional content is omitted; I/O and identity changes mark the
    // observation incomplete in their owning readers.
    let raw = read_opened_utf8(file, path, observation, cancellation)?;
    selected_skill_invocation(skill, &raw, observation)
}

fn split_skill(raw: &str) -> Option<(&str, &str)> {
    raw.strip_prefix("---\n")
        .and_then(|content| content.split_once("\n---\n"))
        .or_else(|| raw.strip_prefix("---\r\n")?.split_once("\r\n---\r\n"))
}

fn selected_skill_invocation(
    skill: &SelectedSkill,
    raw: &str,
    observation: &mut Observation,
) -> Option<WorkspaceSkillInvocation> {
    let Some((_, body)) = split_skill(raw) else {
        observation.fail(&skill.file.logical_path, "selected skill metadata changed");
        return None;
    };
    let Some(current) = parse_skill_metadata(raw) else {
        observation.fail(&skill.file.logical_path, "selected skill metadata changed");
        return None;
    };
    if current.name != skill.name
        || current.description != skill.description
        || current.disable_model_invocation == skill.model_invocable
        || current.user_invocable != skill.user_invocable
    {
        observation.fail(&skill.file.logical_path, "selected skill metadata changed");
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
        "Available skills are summaries only. Use skill_read with the exact selected name to load its instructions before following them:\n<available_skills>\n",
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
    let prefix = format!(
        "<skill_content name=\"{}\">\nSource: {}\n\n<skill_instructions>\n",
        skill.name, skill.source
    );
    let suffix = "\n</skill_instructions>\n</skill_content>";
    let remaining = MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES
        .saturating_sub(prefix.len().saturating_add(suffix.len()));
    let body = utf8_prefix(body, remaining);
    let mut text = String::with_capacity(prefix.len() + body.len() + suffix.len());
    text.push_str(&prefix);
    text.push_str(body);
    text.push_str(suffix);
    text
}

fn utf8_prefix(text: &str, maximum_bytes: usize) -> &str {
    let mut end = text.len().min(maximum_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
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
    budget::config_retained_bytes(config)
        .map_err(|error| MetaError::InvalidInput(error.to_string()))
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
        let service = Arc::new(
            LocalWorkspaceContext::new(config)
                .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let supply = plan
            .context()
            .provide_local::<WorkspaceContextContract>(service.clone())?;
        plan.defer(
            "withdraw Agent workspace context",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    service.owner.close().await;
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_tiny_instruction_buffers_fit_the_snapshot_scratch_budget() {
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        let mut observation = Observation::default();
        let mut texts = Vec::new();
        for index in 0..MAXIMUM_WORKSPACE_INSTRUCTION_FILES {
            let path = directory.path().join(format!("AGENTS-{index}.md"));
            fs::write(&path, "tiny instruction\n").unwrap();
            texts.push(read_bounded_utf8(&path, None, &mut observation, &cancellation).unwrap());
        }
        assert!(observation.is_complete());
        assert!(texts.iter().all(|text| text == "tiny instruction\n"));
        let retained: usize = texts.iter().map(String::capacity).sum();
        assert!(
            retained <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES,
            "tiny instruction capacities exceed the entire instruction render allowance: {retained}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_authority_never_reopens_a_replaced_ambient_root() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let canonical_root = temporary.path().canonicalize().unwrap();
        let project = canonical_root.join("project");
        let held_project = temporary.path().join("held-project");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(project.join("AGENTS.md"), "PINNED INSTRUCTION").unwrap();
        fs::write(outside.join("AGENTS.md"), "OUTSIDE INSTRUCTION").unwrap();
        let authority = ProjectAuthority::open(&project).unwrap();

        fs::rename(&project, &held_project).unwrap();
        symlink(&outside, &project).unwrap();

        let mut observation = Observation::default();
        assert_eq!(
            read_bounded_utf8(
                &project.join("AGENTS.md"),
                Some(&authority),
                &mut observation,
                &CancellationToken::new()
            )
            .as_deref(),
            Some("PINNED INSTRUCTION")
        );
        assert!(observation.is_complete());
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
                user_agent_roots: Vec::new(),
                user_instruction_file: Some(invalid_source),
                user_skill_roots: Vec::new(),
            },
            temporary.path(),
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
            file: skill_files::test_source(),
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
            file: skill_files::test_source(),
            model_invocable: true,
            user_invocable: true,
        };
        let replacement =
            "---\nname: replacement\ndescription: replacement description\n---\nREPLACEMENT BODY";
        let mut observation = Observation::default();

        assert!(selected_skill_invocation(&selected, replacement, &mut observation).is_none());
        assert!(!observation.is_complete());
    }

    #[test]
    fn invoked_skill_accepts_a_current_body_for_the_selected_identity() {
        let selected = SelectedSkill {
            name: "selected".into(),
            description: "selected description".into(),
            source: "SKILL.md".into(),
            file: skill_files::test_source(),
            model_invocable: true,
            user_invocable: true,
        };
        let current = "---\nname: selected\ndescription: selected   description\n---\nCURRENT BODY";
        let mut observation = Observation::default();

        let invocation = selected_skill_invocation(&selected, current, &mut observation).unwrap();
        assert!(observation.is_complete());
        assert!(invocation.text.contains("CURRENT BODY"));
    }

    #[test]
    fn retained_bytes_include_configured_path_storage() {
        let config = WorkspaceContextConfig {
            user_agent_roots: Vec::new(),
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
            std::mem::size_of::<WorkspaceContextConfig>()
                + path_bytes
                + 3 * std::mem::size_of::<PathBuf>()
        );
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    use rsi_agent_session_protocol::{MessageId, MessageOptions};

    #[test]
    fn all_message_tokens_are_matched_before_selecting_invoked_skills() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("real.md"),
            "---\nname: real\ndescription: Selected skill\n---\nACTUAL INSTRUCTIONS",
        )
        .unwrap();
        let messages = (0..64)
            .map(|message| AgentMessage {
                message_id: MessageId::new(format!("message-{message}")).unwrap(),
                source: AgentMessageSource::Human,
                content: (0..64)
                    .map(|block| AgentMessageContent::Text {
                        text: if message == 63 && block == 63 {
                            "/real".into()
                        } else {
                            format!("/missing-{message}-{block}")
                        },
                    })
                    .collect(),
                options: MessageOptions::default(),
            })
            .collect::<Vec<_>>();
        let requests =
            WorkspaceSkillRequests::from_messages(&messages.iter().collect::<Vec<_>>()).unwrap();
        let names = requests.names();
        assert_eq!(names.len(), 4096);
        let snapshot = snapshot_blocking(
            &WorkspaceContextConfig {
                user_agent_roots: Vec::new(),
                user_instruction_file: None,
                user_skill_roots: vec![root.path().to_owned()],
            },
            root.path(),
            names,
        )
        .unwrap();
        assert!(snapshot.complete);
        assert_eq!(snapshot.invocations.len(), 1);
        assert_eq!(snapshot.invocations[0].name, "real");
    }

    #[test]
    fn aggregate_invocation_overflow_is_capacity_instead_of_partial_success() {
        let root = tempfile::tempdir().unwrap();
        let names = (0..64)
            .map(|index| format!("skill-{index:03}"))
            .collect::<Vec<_>>();
        for name in &names {
            let source = format!(
                "---\nname: {name}\ndescription: large skill\n---\n{}",
                "x".repeat(MAXIMUM_WORKSPACE_CONTEXT_SOURCE_BYTES - 128)
            );
            fs::write(root.path().join(format!("{name}.md")), source).unwrap();
        }
        let config = WorkspaceContextConfig {
            user_agent_roots: Vec::new(),
            user_instruction_file: None,
            user_skill_roots: vec![root.path().to_owned()],
        };
        assert_eq!(
            snapshot_blocking(&config, root.path(), &names),
            Err(WorkspaceContextError::Capacity)
        );
        assert_eq!(
            snapshot_blocking(&config, root.path(), &names[..1])
                .unwrap()
                .invocations
                .len(),
            1
        );
        for name in &names {
            fs::write(
                root.path().join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: small skill\n---\nSMALL BODY"),
            )
            .unwrap();
        }
        let snapshot = snapshot_blocking(&config, root.path(), &names).unwrap();
        assert!(snapshot.complete);
        assert_eq!(snapshot.invocations.len(), names.len());
        assert!(
            snapshot
                .invocations
                .iter()
                .all(|item| item.text.contains("SMALL BODY"))
        );
    }
}

fn read_instructions(
    config: &WorkspaceContextConfig,
    cwd: &Path,
    project_root: Option<&Path>,
    project_authority: Option<&ProjectAuthority>,
    budget: &SnapshotBudget,
    observation: &mut Observation,
) -> Result<Option<String>, WorkspaceContextError> {
    let mut user_instruction_sections = Vec::new();
    if let Some(path) = &config.user_instruction_file {
        budget.check()?;
        if let Some(text) = read_bounded_utf8(path, None, observation, &budget.cancellation) {
            user_instruction_sections.push((display_path(path), text));
        }
    }
    let mut project_instruction_sections = Vec::new();
    let mut retained = user_instruction_sections.iter().fold(
        INSTRUCTIONS_PREAMBLE.len(),
        |retained, (source, text)| {
            let bytes = instruction_section_bytes(source, text);
            if retained.saturating_add(bytes) <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES {
                retained + bytes
            } else {
                retained
            }
        },
    );
    if let (Some(root), Some(authority)) = (project_root, project_authority) {
        for directory in directories_between(root, cwd)?.into_iter().rev() {
            budget.check()?;
            let path = directory.join("AGENTS.md");
            if let Some(text) =
                read_bounded_utf8(&path, Some(authority), observation, &budget.cancellation)
            {
                let source = display_project_path(root, &path);
                let bytes = instruction_section_bytes(&source, &text);
                if retained.saturating_add(bytes) <= MAXIMUM_WORKSPACE_CONTEXT_RENDERED_BYTES {
                    retained += bytes;
                    project_instruction_sections.push((source, text));
                }
            }
        }
    }
    project_instruction_sections.reverse();
    let instructions =
        render_instructions(&user_instruction_sections, &project_instruction_sections);

    Ok(instructions)
}
