use crate::agent_preset::{
    DEFAULT_AGENT_PRESET_ID, standard_agent_profile_compiler, user_agent_preset_root,
};
use crate::profiles::{CodingToolsLaunchIdentity, HostLaunchKey, HostProfileDocument};
use crate::settings::{AgentSettingsContract, AgentSettingsFactory, SETTINGS_FACTORY};
use async_trait::async_trait;
use rsi_agent_composition::{AgentCompositionFactory, AgentContributionCatalog};
use rsi_agent_composition_protocol::AgentCompositionContract;
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetId, AgentPresetLaunchIdentity,
    EXECUTOR_FACTORY, KERNEL_FACTORY, SQLITE_STORE_FACTORY, SessionAgentConfig, session_fragment,
};
use rsi_agent_store_protocol::SessionStoreContract;
use rsi_agent_turn_protocol::{
    TurnCompletionBlocker, TurnExecutionContract, TurnFinalizationContext,
    TurnFinalizationContract, TurnFinalizationError, TurnFinalizationReport, TurnFinalizer,
    TurnServiceContract,
};
use rsi_ai_protocol::{ImageCallContract, LanguageCallContract};
use rsi_ai_provider::{ImageRegistrarContract, LanguageRegistrarContract};
use rsi_apply_patch::ApplyPatchToolFactory;
use rsi_approval_protocol::{ApprovalAnswerersContract, ApprovalContract};
use rsi_commands_protocol::CommandRuntimeContract;
use rsi_credentials_local::{CredentialsLocalFactory, KeyringSecretStore, SecretStore};
use rsi_credentials_protocol::{CredentialsAdminContract, CredentialsResolveContract, SecretValue};
use rsi_host::{Host, HostBuilder, HostPaths, ProfileEntry, ProfileFragment};
use rsi_jobs::{Jobs, JobsContract};
use rsi_jobs_tools::JobsToolsFactory;
use rsi_media_protocol::{MediaBackendContract, MediaContract, MediaReadContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, ResolvedFactory,
    UpdateMode,
};
use rsi_meta_scope::ScopeRoot;
use rsi_permission_presets::PermissionPresetsContract;
use rsi_process::ProcessContract;
use rsi_projection::ProjectionRegistryContract;
use rsi_sandbox::SandboxContract;
use rsi_settings_protocol::{SettingsContract, SettingsProviderContract};
use rsi_shell_bash::{BashJobProducerFactory, BashToolFactory};
use rsi_storage::StorageHubContract;
use rsi_storage_domain::DomainFacilityContract;
use rsi_tools_protocol::{ToolCatalogProviderContract, ToolRegistrarContract};
use rsi_workspace::WorkspaceRegistryContract;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::File;
#[cfg(not(unix))]
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) const OPENAI_FACTORY: &str = "rsi.ai.provider.openai";
pub(crate) const OPENAI_COMPATIBLE_FACTORY: &str = "rsi.ai.provider.openai-compatible";
pub(crate) const DEEPSEEK_FACTORY: &str = "rsi.ai.provider.deepseek";

const STORAGE_FACTORY: &str = "rsi.storage";
const STORAGE_SQLITE_FACTORY: &str = "rsi.storage.sqlite";
const STORAGE_DOMAIN_FACTORY: &str = "rsi.storage.domain";
const SETTINGS_LOCAL_FACTORY: &str = "rsi.settings.local";
const SETTINGS_CORE_FACTORY: &str = "rsi.settings";
const CREDENTIALS_FACTORY: &str = "rsi.credentials.local";
const MEDIA_LOCAL_FACTORY: &str = "rsi.media.local";
const MEDIA_FACTORY: &str = "rsi.media";
const APPROVAL_FACTORY: &str = "rsi.approval";
const PERMISSIONS_FACTORY: &str = "rsi.permission-presets";
const SANDBOX_FACTORY: &str = "rsi.sandbox.local";
const PROCESS_FACTORY: &str = "rsi.process.local";
const COMMANDS_FACTORY: &str = "rsi.commands";
const JOBS_FACTORY: &str = "rsi.jobs.local";
const JOBS_FINALIZER_FACTORY: &str = "rsi.session.jobs-finalizer";
const PROJECTION_FACTORY: &str = "rsi.projection";
const WORKSPACE_FACTORY: &str = "rsi.workspace";
const TOOLS_FACTORY: &str = "rsi.tools";
const BASH_PRODUCER_FACTORY: &str = "rsi.shell.bash.producer";
const BASH_TOOL_FACTORY: &str = "rsi.shell.bash.tool";
const JOBS_TOOLS_FACTORY: &str = "rsi.jobs.tools";
const APPLY_PATCH_FACTORY: &str = "rsi.apply-patch";
const STANDARD_AGENT_CONTRIBUTION_IDS: &[&str] =
    &[BASH_TOOL_FACTORY, JOBS_TOOLS_FACTORY, APPLY_PATCH_FACTORY];
const PORTABLE_AGENT_CONTRIBUTION_IDS: &[&str] = &[JOBS_TOOLS_FACTORY];
const AGENT_COMPOSITION_FACTORY: &str = "rsi.agent.composition";
const LANGUAGE_FACTORY: &str = "rsi.ai.language";
const IMAGE_FACTORY: &str = "rsi.ai.image";

pub(crate) fn standard_agent_contribution_ids(
    linux_tools_enabled: bool,
) -> &'static [&'static str] {
    if linux_tools_enabled {
        STANDARD_AGENT_CONTRIBUTION_IDS
    } else {
        PORTABLE_AGENT_CONTRIBUTION_IDS
    }
}

const STANDARD_AGENT_PROFILE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../plugins/rsi-agent-presets/standard/agent.profile.toml"
));
const STANDARD_AGENT_METADATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../plugins/rsi-agent-presets/standard/preset.toml"
));

/// Frozen inputs used to construct the standard linked catalog and fragments.
#[derive(Clone, Debug)]
pub struct StandardComposition {
    paths: HostPaths,
    captured_environment: BTreeMap<String, SecretValue>,
    credential_store: Arc<dyn SecretStore>,
    coding_tools: Option<StandardCodingTools>,
    agent_presets: Option<AgentPresetCatalog>,
}

/// Frozen process inputs required by the standard Linux coding-tool generation.
#[derive(Clone, Debug)]
pub struct StandardCodingTools {
    bash_producer: BashJobProducerFactory,
    bash_tool: BashToolFactory,
    apply_patch: ApplyPatchToolFactory,
}

/// Pure standard Host/Profile preview and the exact owner-generation identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardHostPreview {
    /// Product launch key used by embedded and daemon owner selection.
    pub launch_key: HostLaunchKey,
    /// Generic Host compiler/resolver evidence.
    pub profile: rsi_host::HostProfilePreview,
}

impl StandardCodingTools {
    /// Validates explicit executable paths and freezes the supplied child environment.
    pub fn new(
        bash: PathBuf,
        helper: PathBuf,
        child_environment: Vec<(OsString, OsString)>,
    ) -> crate::Result<Self> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (bash, helper, child_environment);
            return Err(crate::RsiError::Boot(
                "the standard Bash and apply-patch contributions require Linux".into(),
            ));
        }
        #[cfg(target_os = "linux")]
        {
            let bash_tool = BashToolFactory::new(bash, child_environment)
                .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
            let apply_patch = ApplyPatchToolFactory::new(helper)
                .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
            Ok(Self {
                bash_producer: BashJobProducerFactory,
                bash_tool,
                apply_patch,
            })
        }
    }
}

fn standard_agent_contributions(
    coding_tools: Option<&StandardCodingTools>,
) -> rsi_host::Result<AgentContributionCatalog> {
    let mut factories = vec![ResolvedFactory::linked(
        JOBS_TOOLS_FACTORY,
        env!("CARGO_PKG_VERSION"),
        UpdateMode::RestartRequired,
        Arc::new(JobsToolsFactory),
    )];
    if let Some(coding) = coding_tools {
        factories.extend([
            ResolvedFactory::linked(
                BASH_TOOL_FACTORY,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(coding.bash_tool.clone()),
            ),
            ResolvedFactory::linked(
                APPLY_PATCH_FACTORY,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(coding.apply_patch.clone()),
            ),
        ]);
    }
    AgentContributionCatalog::new(factories)
        .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))
}

fn materialize_standard_agent_preset(paths: &HostPaths) -> rsi_host::Result<PathBuf> {
    let cache = paths.cache().join("agent-presets").join("system");
    let digest = standard_agent_preset_digest();

    #[cfg(unix)]
    let result = materialize_standard_agent_preset_unix(&cache, &digest);
    #[cfg(not(unix))]
    let result = materialize_standard_agent_preset_portable(&cache, &digest);
    result
}

fn standard_agent_preset_digest() -> String {
    let mut digest = Sha256::new();
    digest.update(b"rsi-standard-agent-preset-v1\0");
    for bytes in [STANDARD_AGENT_PROFILE, STANDARD_AGENT_METADATA] {
        digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(bytes);
    }
    hex::encode(digest.finalize())
}

pub(crate) fn standard_agent_preset_root_candidate(paths: &HostPaths) -> PathBuf {
    paths
        .cache()
        .join("agent-presets")
        .join("system")
        .join(standard_agent_preset_digest())
}

#[cfg(not(unix))]
fn materialize_standard_agent_preset_portable(
    cache: &Path,
    digest: &str,
) -> rsi_host::Result<PathBuf> {
    create_portable_cache_tree_without_links(cache)?;
    let target = cache.join(digest);
    if portable_real_directory_exists(&target)? {
        verify_standard_agent_preset(&target)?;
        return Ok(target);
    }

    let staging = create_asset_staging(&cache)?;
    let mut cleanup = AssetStaging::new(staging.clone());
    set_owner_directory_permissions(&staging)?;
    let preset = staging.join("standard");
    fs::create_dir(&preset)
        .map_err(|error| asset_error("create preset directory", &preset, &error))?;
    set_owner_directory_permissions(&preset)?;
    write_asset_file(&preset.join("agent.profile.toml"), STANDARD_AGENT_PROFILE)?;
    write_asset_file(&preset.join("preset.toml"), STANDARD_AGENT_METADATA)?;

    match fs::rename(&staging, &target) {
        Ok(()) => cleanup.disarm(),
        Err(_) if portable_real_directory_exists(&target)? => {
            verify_standard_agent_preset(&target)?;
        }
        Err(error) => return Err(asset_error("publish preset cache", &target, &error)),
    }
    verify_standard_agent_preset(&target)?;
    Ok(target)
}

#[cfg(not(unix))]
fn create_portable_cache_tree_without_links(cache: &Path) -> rsi_host::Result<()> {
    use std::path::Component;

    let mut current = PathBuf::new();
    for component in cache.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
            }
            Component::Normal(name) => {
                current.push(name);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => ensure_portable_real_directory(&current, &metadata)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        match fs::create_dir(&current) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                            Err(error) => {
                                return Err(asset_error(
                                    "create cache component",
                                    &current,
                                    &error,
                                ));
                            }
                        }
                        let metadata = fs::symlink_metadata(&current).map_err(|error| {
                            asset_error("inspect created cache component", &current, &error)
                        })?;
                        ensure_portable_real_directory(&current, &metadata)?;
                    }
                    Err(error) => {
                        return Err(asset_error("inspect cache component", &current, &error));
                    }
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err(rsi_host::HostError::Bootstrap(format!(
                    "standard Agent preset cache path is not normalized: {}",
                    cache.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn portable_real_directory_exists(path: &Path) -> rsi_host::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure_portable_real_directory(path, &metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(asset_error("inspect preset directory", path, &error)),
    }
}

#[cfg(not(unix))]
fn ensure_portable_real_directory(path: &Path, metadata: &fs::Metadata) -> rsi_host::Result<()> {
    if portable_metadata_is_link(metadata) || !metadata.file_type().is_dir() {
        return Err(rsi_host::HostError::Bootstrap(format!(
            "standard Agent preset cache `{}` is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn portable_metadata_is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(unix), not(windows)))]
fn portable_metadata_is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn materialize_standard_agent_preset_unix(cache: &Path, digest: &str) -> rsi_host::Result<PathBuf> {
    let cache_directory = open_or_create_asset_cache_unix(cache)?;
    let target = cache.join(digest);
    match open_asset_directory_unix(&cache_directory, digest) {
        Ok(directory) => {
            verify_standard_agent_preset_directory_unix(&directory, &target)?;
            return Ok(target);
        }
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(no_follow_asset_directory(&target, error)),
    }

    let mut staging = UnixAssetStaging::create(cache_directory, cache)?;
    staging.populate()?;
    verify_standard_agent_preset_directory_unix(staging.directory(), &staging.path())?;
    let _published = staging.publish(digest)?;
    let directory = open_asset_directory_unix(staging.parent(), digest)
        .map_err(|error| no_follow_asset_directory(&target, error))?;
    verify_standard_agent_preset_directory_unix(&directory, &target)?;
    Ok(target)
}

#[cfg(unix)]
fn open_or_create_asset_cache_unix(cache: &Path) -> rsi_host::Result<File> {
    use rustix::fs::{Mode, OFlags};
    use std::path::Component;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = rustix::fs::open("/", flags, Mode::empty())
        .map(File::from)
        .map_err(|error| no_follow_asset_directory(Path::new("/"), error))?;
    let mut current = PathBuf::from("/");
    for component in cache.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => name,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(rsi_host::HostError::Bootstrap(format!(
                    "standard Agent preset cache path is not normalized: {}",
                    cache.display()
                )));
            }
        };
        current.push(name);
        let (opened, created) = match rustix::fs::openat(&directory, name, flags, Mode::empty()) {
            Ok(opened) => (opened, false),
            Err(rustix::io::Errno::NOENT) => {
                let created = match rustix::fs::mkdirat(
                    &directory,
                    name,
                    Mode::RUSR | Mode::WUSR | Mode::XUSR,
                ) {
                    Ok(()) => true,
                    Err(rustix::io::Errno::EXIST) => false,
                    Err(error) => {
                        return Err(asset_errno("create cache component", &current, error));
                    }
                };
                let opened = rustix::fs::openat(&directory, name, flags, Mode::empty())
                    .map_err(|error| no_follow_asset_directory(&current, error))?;
                (opened, created)
            }
            Err(error) => return Err(no_follow_asset_directory(&current, error)),
        };
        directory = File::from(opened);
        if created {
            rustix::fs::fchmod(&directory, Mode::RUSR | Mode::WUSR | Mode::XUSR)
                .map_err(|error| asset_errno("set cache component mode", &current, error))?;
        }
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_asset_directory_unix(
    parent: &File,
    name: &str,
) -> std::result::Result<File, rustix::io::Errno> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
}

#[cfg(unix)]
fn verify_standard_agent_preset_directory_unix(
    root: &File,
    diagnostic_root: &Path,
) -> rsi_host::Result<()> {
    use rustix::fs::{FileType, Mode, OFlags};

    let preset_path = diagnostic_root.join("standard");
    let preset = rustix::fs::openat(
        root,
        "standard",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| no_follow_asset_directory(&preset_path, error))?;
    for (name, expected) in [
        ("agent.profile.toml", STANDARD_AGENT_PROFILE),
        ("preset.toml", STANDARD_AGENT_METADATA),
    ] {
        let path = preset_path.join(name);
        let mut file = rustix::fs::openat(
            &preset,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| asset_errno("open preset asset without following links", &path, error))?;
        let stat = rustix::fs::fstat(&file)
            .map_err(|error| asset_errno("inspect preset asset", &path, error))?;
        if !FileType::from_raw_mode(stat.st_mode).is_file() {
            return Err(rsi_host::HostError::Bootstrap(format!(
                "standard Agent preset asset `{}` is not a regular file",
                path.display()
            )));
        }
        let maximum = u64::try_from(expected.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut actual = Vec::with_capacity(expected.len());
        std::io::Read::by_ref(&mut file)
            .take(maximum)
            .read_to_end(&mut actual)
            .map_err(|error| asset_error("read preset asset", &path, &error))?;
        if actual != expected {
            return Err(rsi_host::HostError::Bootstrap(format!(
                "standard Agent preset asset `{}` failed byte verification",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct UnixAssetStaging {
    parent: File,
    directory: File,
    parent_path: PathBuf,
    name: String,
    published: bool,
}

#[cfg(unix)]
impl UnixAssetStaging {
    fn create(parent: File, parent_path: &Path) -> rsi_host::Result<Self> {
        use rustix::fs::{Mode, OFlags};

        for _ in 0..16 {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).map_err(|error| {
                rsi_host::HostError::Bootstrap(format!(
                    "standard Agent preset cache entropy failed: {error}"
                ))
            })?;
            let name = format!(".staging-{}", hex::encode(random));
            match rustix::fs::mkdirat(&parent, &name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => {
                    let path = parent_path.join(&name);
                    let directory = match rustix::fs::openat(
                        &parent,
                        &name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    ) {
                        Ok(directory) => File::from(directory),
                        Err(error) => {
                            let _ignored = rustix::fs::unlinkat(
                                &parent,
                                &name,
                                rustix::fs::AtFlags::REMOVEDIR,
                            );
                            return Err(no_follow_asset_directory(&path, error));
                        }
                    };
                    if let Err(error) =
                        rustix::fs::fchmod(&directory, Mode::RUSR | Mode::WUSR | Mode::XUSR)
                    {
                        let _ignored = remove_asset_tree_at_unix(&parent, name.as_ref());
                        return Err(asset_errno("set preset staging mode", &path, error));
                    }
                    return Ok(Self {
                        parent,
                        directory,
                        parent_path: parent_path.to_path_buf(),
                        name,
                        published: false,
                    });
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(asset_errno("create preset staging", parent_path, error));
                }
            }
        }
        Err(rsi_host::HostError::Bootstrap(
            "standard Agent preset cache staging names were exhausted".into(),
        ))
    }

    const fn directory(&self) -> &File {
        &self.directory
    }

    const fn parent(&self) -> &File {
        &self.parent
    }

    fn path(&self) -> PathBuf {
        self.parent_path.join(&self.name)
    }

    fn populate(&self) -> rsi_host::Result<()> {
        use rustix::fs::{Mode, OFlags};

        let preset_path = self.path().join("standard");
        rustix::fs::mkdirat(
            &self.directory,
            "standard",
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )
        .map_err(|error| asset_errno("create preset directory", &preset_path, error))?;
        let preset = rustix::fs::openat(
            &self.directory,
            "standard",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| no_follow_asset_directory(&preset_path, error))?;
        rustix::fs::fchmod(&preset, Mode::RUSR | Mode::WUSR | Mode::XUSR)
            .map_err(|error| asset_errno("set preset directory mode", &preset_path, error))?;
        write_asset_file_unix(
            &preset,
            "agent.profile.toml",
            STANDARD_AGENT_PROFILE,
            &preset_path.join("agent.profile.toml"),
        )?;
        write_asset_file_unix(
            &preset,
            "preset.toml",
            STANDARD_AGENT_METADATA,
            &preset_path.join("preset.toml"),
        )?;
        preset
            .sync_all()
            .map_err(|error| asset_error("sync preset directory", &preset_path, &error))?;
        self.directory
            .sync_all()
            .map_err(|error| asset_error("sync preset staging", &self.path(), &error))
    }

    fn publish(&mut self, target: &str) -> rsi_host::Result<bool> {
        use rustix::fs::RenameFlags;

        match rustix::fs::renameat_with(
            &self.parent,
            &self.name,
            &self.parent,
            target,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                self.published = true;
                self.parent.sync_all().map_err(|error| {
                    asset_error("sync published preset cache", &self.parent_path, &error)
                })?;
                Ok(true)
            }
            Err(rustix::io::Errno::EXIST) => Ok(false),
            Err(error) => Err(asset_errno(
                "publish preset cache",
                &self.parent_path.join(target),
                error,
            )),
        }
    }
}

#[cfg(unix)]
impl Drop for UnixAssetStaging {
    fn drop(&mut self) {
        if !self.published {
            let _ignored = remove_asset_tree_at_unix(&self.parent, self.name.as_ref());
        }
    }
}

#[cfg(unix)]
fn remove_asset_tree_at_unix(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if FileType::from_raw_mode(stat.st_mode).is_dir() {
        let directory = rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(std::io::Error::from)?;
        for entry in rustix::fs::Dir::read_from(&directory).map_err(std::io::Error::from)? {
            let entry = entry.map_err(std::io::Error::from)?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            remove_asset_tree_at_unix(&directory, OsStr::from_bytes(bytes))?;
        }
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(Into::into)
    } else {
        rustix::fs::unlinkat(parent, name, AtFlags::empty()).map_err(Into::into)
    }
}

#[cfg(unix)]
fn write_asset_file_unix(
    directory: &File,
    name: &str,
    bytes: &[u8],
    diagnostic_path: &Path,
) -> rsi_host::Result<()> {
    use rustix::fs::{Mode, OFlags};

    let mode = Mode::RUSR | Mode::WUSR;
    let mut file = rustix::fs::openat(
        directory,
        name,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )
    .map(File::from)
    .map_err(|error| asset_errno("create preset asset", diagnostic_path, error))?;
    file.write_all(bytes)
        .map_err(|error| asset_error("write preset asset", diagnostic_path, &error))?;
    rustix::fs::fchmod(&file, mode)
        .map_err(|error| asset_errno("set preset asset mode", diagnostic_path, error))?;
    file.sync_all()
        .map_err(|error| asset_error("sync preset asset", diagnostic_path, &error))
}

#[cfg(unix)]
fn no_follow_asset_directory(path: &Path, error: rustix::io::Errno) -> rsi_host::HostError {
    rsi_host::HostError::Bootstrap(format!(
        "standard Agent preset cache `{}` is not a real directory (no-follow directory required): {error}",
        path.display()
    ))
}

#[cfg(unix)]
fn asset_errno(operation: &str, path: &Path, error: rustix::io::Errno) -> rsi_host::HostError {
    let error = std::io::Error::from(error);
    asset_error(operation, path, &error)
}

/// Materializes and byte-verifies the digest-addressed standard Agent-preset root.
///
/// The returned directory is a catalog root containing the `standard` preset,
/// not the preset directory itself.
pub fn standard_agent_preset_root(paths: &HostPaths) -> rsi_host::Result<PathBuf> {
    materialize_standard_agent_preset(paths)
}

#[cfg(not(unix))]
fn create_asset_staging(root: &Path) -> rsi_host::Result<PathBuf> {
    for _ in 0..16 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|error| {
            rsi_host::HostError::Bootstrap(format!(
                "standard Agent preset cache entropy failed: {error}"
            ))
        })?;
        let path = root.join(format!(".staging-{}", hex::encode(random)));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(asset_error("create preset staging", &path, &error)),
        }
    }
    Err(rsi_host::HostError::Bootstrap(
        "standard Agent preset cache staging names were exhausted".into(),
    ))
}

#[cfg(not(unix))]
fn verify_standard_agent_preset(root: &Path) -> rsi_host::Result<()> {
    ensure_directory_without_symlink(root)?;
    let preset = root.join("standard");
    ensure_directory_without_symlink(&preset)?;
    for (path, expected) in [
        (preset.join("agent.profile.toml"), STANDARD_AGENT_PROFILE),
        (preset.join("preset.toml"), STANDARD_AGENT_METADATA),
    ] {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| asset_error("inspect preset asset", &path, &error))?;
        if !metadata.file_type().is_file() || portable_metadata_is_link(&metadata) {
            return Err(rsi_host::HostError::Bootstrap(format!(
                "standard Agent preset asset `{}` is not a regular file",
                path.display()
            )));
        }
        let mut actual = Vec::with_capacity(expected.len());
        File::open(&path)
            .and_then(|file| {
                std::io::Read::by_ref(&mut file.take(expected.len() as u64 + 1))
                    .read_to_end(&mut actual)
            })
            .map_err(|error| asset_error("read preset asset", &path, &error))?;
        if actual != expected {
            return Err(rsi_host::HostError::Bootstrap(format!(
                "standard Agent preset asset `{}` failed byte verification",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_directory_without_symlink(path: &Path) -> rsi_host::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| asset_error("inspect preset directory", path, &error))?;
    if portable_metadata_is_link(&metadata) || !metadata.file_type().is_dir() {
        return Err(rsi_host::HostError::Bootstrap(format!(
            "standard Agent preset cache `{}` is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_asset_file(path: &Path, bytes: &[u8]) -> rsi_host::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| asset_error("create preset asset", path, &error))?;
    file.write_all(bytes)
        .map_err(|error| asset_error("write preset asset", path, &error))?;
    file.sync_all()
        .map_err(|error| asset_error("sync preset asset", path, &error))?;
    set_owner_file_permissions(path)
}

#[cfg(not(unix))]
fn set_owner_directory_permissions(_path: &Path) -> rsi_host::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_file_permissions(_path: &Path) -> rsi_host::Result<()> {
    Ok(())
}

fn asset_error(operation: &str, path: &Path, error: &std::io::Error) -> rsi_host::HostError {
    rsi_host::HostError::Bootstrap(format!(
        "standard Agent preset {operation} failed for `{}`: {error}",
        path.display()
    ))
}

#[cfg(not(unix))]
struct AssetStaging {
    path: Option<PathBuf>,
}

#[cfg(not(unix))]
impl AssetStaging {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

#[cfg(not(unix))]
impl Drop for AssetStaging {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ignored = fs::remove_dir_all(path);
        }
    }
}

impl StandardComposition {
    /// Creates a standard composition from explicit Host paths and captured secrets.
    pub fn new(
        paths: HostPaths,
        captured_environment: BTreeMap<String, SecretValue>,
        coding_tools: Option<StandardCodingTools>,
    ) -> Self {
        Self {
            paths,
            captured_environment,
            credential_store: Arc::new(KeyringSecretStore),
            coding_tools,
            agent_presets: None,
        }
    }

    /// Replaces the credential store implementation for an explicit embedder.
    #[must_use]
    pub fn with_credential_store(mut self, store: Arc<dyn SecretStore>) -> Self {
        self.credential_store = store;
        self
    }

    /// Replaces the derived system/user catalog with one settings-backed catalog.
    #[must_use]
    pub fn with_agent_presets(mut self, presets: AgentPresetCatalog) -> Self {
        self.agent_presets = Some(presets);
        self
    }

    /// Returns the frozen paths used by this candidate.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Purely compiles and resolves one Host Profile and derives its owner key.
    ///
    /// This does not materialize bundled assets, prepare or activate a factory,
    /// read credentials, or acquire a Store/owner lease.
    pub fn preview_host(
        &self,
        profile: &HostProfileDocument,
    ) -> crate::Result<StandardHostPreview> {
        let (host, presets) = self
            .build_internal(false)
            .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
        let composition_digest = host
            .composition_digest()
            .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
        let launch_key = HostLaunchKey::from_components(
            &composition_digest,
            profile,
            &presets,
            self.coding_tools
                .as_ref()
                .map(|tools| CodingToolsLaunchIdentity {
                    bash: tools.bash_tool.executable(),
                    apply_patch: tools.apply_patch.executable(),
                }),
        )
        .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
        let profile = match &profile.path {
            Some(path) => host.preview_file(path),
            None => host.preview(rsi_host::Profile::default()),
        }
        .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
        Ok(StandardHostPreview {
            launch_key,
            profile,
        })
    }

    /// Builds the generic Host without reading a Host Profile or activating plugins.
    pub fn build(self) -> rsi_host::Result<Host> {
        self.build_internal(true).map(|(host, _presets)| host)
    }

    fn build_internal(
        &self,
        materialize_assets: bool,
    ) -> rsi_host::Result<(Host, AgentPresetLaunchIdentity)> {
        let linux_tools_enabled = self.coding_tools.is_some();
        let paths = self.paths.clone();
        let compiler = standard_agent_profile_compiler(&paths, linux_tools_enabled)
            .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
        let presets = if let Some(presets) = &self.agent_presets {
            presets.clone()
        } else {
            let system_root = if materialize_assets {
                materialize_standard_agent_preset(&paths)?
            } else {
                standard_agent_preset_root_candidate(&paths)
            };
            let standard_id = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID)
                .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
            AgentPresetCatalog::new(
                AgentPresetCatalogConfig::new(standard_id.clone())
                    .with_system_preset(standard_id, system_root.join(DEFAULT_AGENT_PRESET_ID))
                    .with_user_root(user_agent_preset_root(&paths)),
                compiler,
            )
            .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?
        };
        let preset_identity = presets.launch_identity();
        let contributions = standard_agent_contributions(self.coding_tools.as_ref())?;
        let agent_composition = AgentCompositionFactory::new(
            presets,
            contributions,
            ScopeRoot::new(ScopeRoot::MAXIMUM_ANCESTRY_DEPTH)
                .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?,
        );
        let mut builder = HostBuilder::new(paths.clone());
        register_contracts(&mut builder)?;
        register_factories(
            &mut builder,
            Arc::clone(&self.credential_store),
            self.captured_environment.clone(),
            self.coding_tools.clone(),
            agent_composition,
        )?;
        builder.register_fragment(base_fragment(&paths, linux_tools_enabled))?;
        let agent = SessionAgentConfig::new(paths.state().join("agent"))
            .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
        builder.register_fragment(session_fragment(&agent))?;
        builder.build().map(|host| (host, preset_identity))
    }
}

fn register_factories(
    builder: &mut HostBuilder,
    credential_store: Arc<dyn SecretStore>,
    captured_environment: BTreeMap<String, SecretValue>,
    coding_tools: Option<StandardCodingTools>,
    agent_composition: AgentCompositionFactory,
) -> rsi_host::Result<()> {
    register(
        builder,
        STORAGE_FACTORY,
        UpdateMode::Replayable,
        rsi_storage::StorageFactory,
    )?;
    register(
        builder,
        STORAGE_SQLITE_FACTORY,
        UpdateMode::RestartRequired,
        rsi_storage_sqlite::SqliteStorageFactory,
    )?;
    register(
        builder,
        STORAGE_DOMAIN_FACTORY,
        UpdateMode::Replayable,
        rsi_storage_domain::DomainFactory,
    )?;
    register(
        builder,
        SETTINGS_LOCAL_FACTORY,
        UpdateMode::RestartRequired,
        rsi_settings_local::LocalSettingsFactory,
    )?;
    register(
        builder,
        SETTINGS_CORE_FACTORY,
        UpdateMode::Replayable,
        rsi_settings::SettingsFactory,
    )?;
    register(
        builder,
        SETTINGS_FACTORY,
        UpdateMode::Replayable,
        AgentSettingsFactory,
    )?;
    register(
        builder,
        CREDENTIALS_FACTORY,
        UpdateMode::RestartRequired,
        CredentialsLocalFactory::with_store(credential_store, captured_environment),
    )?;
    register_runtime_factories(builder, coding_tools, agent_composition)?;
    register_agent_ai_factories(builder)
}

fn register_runtime_factories(
    builder: &mut HostBuilder,
    coding_tools: Option<StandardCodingTools>,
    agent_composition: AgentCompositionFactory,
) -> rsi_host::Result<()> {
    register(
        builder,
        MEDIA_LOCAL_FACTORY,
        UpdateMode::RestartRequired,
        rsi_media_local::LocalMediaBackendFactory,
    )?;
    register(
        builder,
        MEDIA_FACTORY,
        UpdateMode::Replayable,
        rsi_media::MediaFactory,
    )?;
    register(
        builder,
        APPROVAL_FACTORY,
        UpdateMode::Replayable,
        rsi_approval::ApprovalFactory,
    )?;
    register(
        builder,
        PERMISSIONS_FACTORY,
        UpdateMode::Replayable,
        rsi_permission_presets::PermissionPresetsFactory,
    )?;
    register(
        builder,
        SANDBOX_FACTORY,
        UpdateMode::RestartRequired,
        rsi_sandbox_local::SandboxLocalFactory::default(),
    )?;
    register(
        builder,
        PROCESS_FACTORY,
        UpdateMode::RestartRequired,
        rsi_process_local::ProcessLocalFactory,
    )?;
    register(
        builder,
        COMMANDS_FACTORY,
        UpdateMode::Replayable,
        rsi_commands::CommandsFactory,
    )?;
    register(
        builder,
        JOBS_FACTORY,
        UpdateMode::Replayable,
        rsi_jobs_local::JobsLocalFactory,
    )?;
    register(
        builder,
        JOBS_FINALIZER_FACTORY,
        UpdateMode::Replayable,
        SessionJobsFinalizerFactory,
    )?;
    register(
        builder,
        PROJECTION_FACTORY,
        UpdateMode::Replayable,
        rsi_projection::ProjectionFactory,
    )?;
    register(
        builder,
        WORKSPACE_FACTORY,
        UpdateMode::Replayable,
        rsi_workspace::WorkspaceFactory,
    )?;
    register(
        builder,
        TOOLS_FACTORY,
        UpdateMode::Replayable,
        rsi_tools::ToolsFactory,
    )?;
    if let Some(coding) = coding_tools {
        register(
            builder,
            BASH_PRODUCER_FACTORY,
            UpdateMode::Replayable,
            coding.bash_producer,
        )?;
    }
    register(
        builder,
        AGENT_COMPOSITION_FACTORY,
        UpdateMode::RestartRequired,
        agent_composition,
    )?;
    Ok(())
}

fn register_agent_ai_factories(builder: &mut HostBuilder) -> rsi_host::Result<()> {
    register(
        builder,
        LANGUAGE_FACTORY,
        UpdateMode::Replayable,
        rsi_ai::LanguageRouterFactory,
    )?;
    register(
        builder,
        IMAGE_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_image::ImageRouterFactory,
    )?;
    register(
        builder,
        SQLITE_STORE_FACTORY,
        UpdateMode::RestartRequired,
        rsi_agent_store_sqlite::SqliteStoreFactory,
    )?;
    register(
        builder,
        KERNEL_FACTORY,
        UpdateMode::Replayable,
        rsi_agent_kernel::KernelFactory,
    )?;
    register(
        builder,
        EXECUTOR_FACTORY,
        UpdateMode::Replayable,
        rsi_agent_executor::ExecutorFactory,
    )?;
    register(
        builder,
        OPENAI_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_openai::OpenAiFactory::default(),
    )?;
    register(
        builder,
        OPENAI_COMPATIBLE_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_openai_compatible::OpenAiCompatibleFactory::default(),
    )?;
    register(
        builder,
        DEEPSEEK_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_deepseek::DeepSeekFactory::default(),
    )?;
    Ok(())
}

fn register(
    builder: &mut HostBuilder,
    id: &'static str,
    mode: UpdateMode,
    factory: impl PluginFactory,
) -> rsi_host::Result<()> {
    builder.register_linked(id, env!("CARGO_PKG_VERSION"), mode, Arc::new(factory))?;
    Ok(())
}

fn register_contracts(builder: &mut HostBuilder) -> rsi_host::Result<()> {
    builder.register_local_contract::<StorageHubContract>()?;
    builder.register_local_contract::<DomainFacilityContract>()?;
    builder.register_local_contract::<SettingsProviderContract>()?;
    builder.register_local_contract::<SettingsContract>()?;
    builder.register_local_contract::<AgentSettingsContract>()?;
    builder.register_local_contract::<CredentialsResolveContract>()?;
    builder.register_local_contract::<CredentialsAdminContract>()?;
    builder.register_local_contract::<MediaBackendContract>()?;
    builder.register_local_contract::<MediaContract>()?;
    builder.register_local_contract::<MediaReadContract>()?;
    builder.register_local_contract::<ApprovalContract>()?;
    builder.register_local_contract::<ApprovalAnswerersContract>()?;
    builder.register_local_contract::<PermissionPresetsContract>()?;
    builder.register_local_contract::<SandboxContract>()?;
    builder.register_local_contract::<ProcessContract>()?;
    builder.register_local_contract::<CommandRuntimeContract>()?;
    builder.register_local_contract::<JobsContract>()?;
    builder.register_local_contract::<ProjectionRegistryContract>()?;
    builder.register_local_contract::<WorkspaceRegistryContract>()?;
    builder.register_local_contract::<ToolCatalogProviderContract>()?;
    builder.register_local_contract::<ToolRegistrarContract>()?;
    builder.register_local_contract::<AgentCompositionContract>()?;
    builder.register_local_contract::<LanguageCallContract>()?;
    builder.register_local_contract::<ImageCallContract>()?;
    builder.register_local_contract::<LanguageRegistrarContract>()?;
    builder.register_local_contract::<ImageRegistrarContract>()?;
    builder.register_local_contract::<SessionStoreContract>()?;
    builder.register_local_contract::<TurnServiceContract>()?;
    builder.register_local_contract::<TurnExecutionContract>()?;
    builder.register_local_contract::<TurnFinalizationContract>()?;
    Ok(())
}

fn base_fragment(paths: &HostPaths, coding_tools: bool) -> ProfileFragment {
    let mut entries = vec![
        ProfileEntry::new("rsi-storage", STORAGE_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-storage-sqlite",
            STORAGE_SQLITE_FACTORY,
            json!({ "name": "base", "path": paths.state().join("base.sqlite3") }),
        ),
        ProfileEntry::new("rsi-storage-domain", STORAGE_DOMAIN_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-settings-local",
            SETTINGS_LOCAL_FACTORY,
            json!({ "path": paths.config().join("settings.json") }),
        ),
        ProfileEntry::new("rsi-settings", SETTINGS_CORE_FACTORY, Value::Null),
        ProfileEntry::new("rsi-session-settings", SETTINGS_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-credentials",
            CREDENTIALS_FACTORY,
            json!({
                "service": "rsiversi",
                "environment": [
                    { "reference": { "owner": OPENAI_FACTORY, "slot": "default" }, "variable": "OPENAI_API_KEY" },
                    { "reference": { "owner": OPENAI_COMPATIBLE_FACTORY, "slot": "default" }, "variable": "RSI_OPENAI_COMPATIBLE_API_KEY" },
                    { "reference": { "owner": DEEPSEEK_FACTORY, "slot": "default" }, "variable": "DEEPSEEK_API_KEY" }
                ]
            }),
        ),
        ProfileEntry::new(
            "rsi-media-local",
            MEDIA_LOCAL_FACTORY,
            json!({ "root": paths.state().join("media") }),
        ),
        ProfileEntry::new("rsi-media", MEDIA_FACTORY, Value::Null),
        ProfileEntry::new("rsi-approval", APPROVAL_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-permission-presets",
            PERMISSIONS_FACTORY,
            json!({
                "read-only": { "sandbox": "read-only", "require_approval": false },
                "workspace-write": { "sandbox": "workspace-write", "require_approval": false },
                "danger-full-access": { "sandbox": "danger-full-access", "require_approval": true }
            }),
        ),
        ProfileEntry::new(
            "rsi-sandbox",
            SANDBOX_FACTORY,
            json!({
                "bubblewrap": ["/usr/bin/bwrap"],
                "landlock": []
            }),
        ),
        ProfileEntry::new("rsi-process", PROCESS_FACTORY, Value::Null),
        ProfileEntry::new("rsi-commands", COMMANDS_FACTORY, Value::Null),
        ProfileEntry::new("rsi-jobs", JOBS_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-session-jobs-finalizer",
            JOBS_FINALIZER_FACTORY,
            Value::Null,
        ),
        ProfileEntry::new("rsi-projection", PROJECTION_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-workspace",
            WORKSPACE_FACTORY,
            json!({ "backend": "base" }),
        ),
        ProfileEntry::new("rsi-tools", TOOLS_FACTORY, Value::Null),
        ProfileEntry::new(
            "rsi-agent-composition",
            AGENT_COMPOSITION_FACTORY,
            Value::Null,
        ),
        ProfileEntry::new("rsi-ai-language", LANGUAGE_FACTORY, Value::Null),
        ProfileEntry::new("rsi-ai-image", IMAGE_FACTORY, Value::Null),
    ];
    if coding_tools {
        entries.push(ProfileEntry::new(
            "rsi-shell-bash-producer",
            BASH_PRODUCER_FACTORY,
            Value::Null,
        ));
    }
    ProfileFragment::new("rsi.standard.base", entries)
}

/// Captures only the standard allowlisted credential environment variables.
pub fn capture_standard_environment() -> crate::Result<BTreeMap<String, SecretValue>> {
    let mut captured = BTreeMap::new();
    for name in [
        "OPENAI_API_KEY",
        "RSI_OPENAI_COMPATIBLE_API_KEY",
        "DEEPSEEK_API_KEY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            let value = value.into_string().map_err(|_| {
                crate::RsiError::Boot(format!(
                    "credential environment variable `{name}` is not UTF-8"
                ))
            })?;
            captured.insert(
                name.into(),
                SecretValue::new(value)
                    .map_err(|error| crate::RsiError::Boot(error.to_string()))?,
            );
        }
    }
    Ok(captured)
}

#[derive(Debug)]
struct SessionJobsFinalizerFactory;

#[derive(Debug)]
struct SessionJobsFinalizer {
    jobs: Arc<dyn Jobs>,
}

#[async_trait]
impl TurnFinalizer for SessionJobsFinalizer {
    async fn finalize(
        &self,
        context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        let scope = context
            .job_scope
            .as_ref()
            .ok_or_else(|| TurnFinalizationError::Failed {
                code: "jobs.scope".into(),
                message: "Agent executor did not provide a Jobs scope authority".into(),
            })?;
        let finalization = self.jobs.finalize_scope(scope).await.map_err(|error| {
            TurnFinalizationError::Failed {
                code: match error {
                    rsi_jobs::JobsError::CancellationTimeout => "jobs.cancellation_timeout",
                    _ => "jobs.finalization",
                }
                .into(),
                message: error.to_string(),
            }
        })?;
        if finalization.unreported.is_empty() {
            return Ok(TurnFinalizationReport::complete());
        }
        let mut identities = finalization
            .unreported
            .iter()
            .take(16)
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if finalization.unreported.len() > 16 {
            write!(
                &mut identities,
                ", and {} more",
                finalization.unreported.len() - 16
            )
            .expect("writing to a String cannot fail");
        }
        let blocker = TurnCompletionBlocker::new(
            "jobs.unreported_background_work",
            format!("background jobs completed without an explicit report: {identities}"),
        )?;
        Ok(TurnFinalizationReport::blocked(blocker))
    }
}

#[async_trait]
impl PluginFactory for SessionJobsFinalizerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Session Jobs finalizer configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null)
            .requiring_local::<JobsContract>()
            .requiring_local::<TurnFinalizationContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let finalizer = Arc::new(SessionJobsFinalizer {
            jobs: plan.local::<JobsContract>()?,
        });
        let lease = plan
            .local::<TurnFinalizationContract>()?
            .register("rsi.session.jobs".into(), finalizer)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw Session Jobs finalizer",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_meta_profile::{
        ProfileCompiler, ProfileEnvironment, ProfileLimits, ProfileProgram, ProfileResolver,
    };
    use std::fs;
    use std::sync::{Arc, Barrier};

    #[test]
    fn standard_unconfined_preset_requires_approval() {
        let paths = HostPaths::new("/config", "/state", "/cache").unwrap();
        let fragment = base_fragment(&paths, false);
        let permissions = fragment
            .entries()
            .iter()
            .find(|entry| entry.plugin().as_str() == PERMISSIONS_FACTORY)
            .expect("standard permission preset registration");
        assert_eq!(
            permissions.config()["danger-full-access"]["require_approval"],
            true
        );
    }

    #[test]
    fn standard_agent_preset_is_digest_addressed_and_byte_verified() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            temporary.path().join("cache"),
        )
        .unwrap();
        let root = standard_agent_preset_root(&paths).unwrap();
        let preset = root.join("standard");
        assert_eq!(
            fs::read(preset.join("agent.profile.toml")).unwrap(),
            STANDARD_AGENT_PROFILE
        );
        assert_eq!(
            fs::read(preset.join("preset.toml")).unwrap(),
            STANDARD_AGENT_METADATA
        );

        fs::write(preset.join("agent.profile.toml"), b"corrupt").unwrap();
        let error = standard_agent_preset_root(&paths).unwrap_err();
        assert!(error.to_string().contains("failed byte verification"));
        assert_eq!(
            fs::read(preset.join("agent.profile.toml")).unwrap(),
            b"corrupt"
        );
    }

    #[test]
    fn standard_agent_profile_compiles_to_exactly_the_platform_enabled_contributions() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            temporary.path().join("cache"),
        )
        .unwrap();
        let root = standard_agent_preset_root(&paths).unwrap();
        let profile = ProfileProgram::from_file(root.join("standard/agent.profile.toml"));
        let compiler = |linux_tools_enabled| {
            ProfileCompiler::new(
                ProfileEnvironment::new(
                    paths.config(),
                    paths.state(),
                    paths.cache(),
                    "test-platform",
                    BTreeMap::from([(
                        "standard_linux_coding_tools".to_owned(),
                        Value::Bool(linux_tools_enabled),
                    )]),
                )
                .unwrap(),
                ProfileLimits::default(),
            )
        };

        let enabled = compiler(true).compile(&profile).unwrap();
        assert_eq!(
            enabled
                .leaves()
                .iter()
                .map(|leaf| leaf.plugin().as_str())
                .collect::<Vec<_>>(),
            [BASH_TOOL_FACTORY, JOBS_TOOLS_FACTORY, APPLY_PATCH_FACTORY]
        );
        assert_eq!(
            compiler(false)
                .compile(&profile)
                .unwrap()
                .leaves()
                .iter()
                .map(|leaf| leaf.plugin().as_str())
                .collect::<Vec<_>>(),
            [JOBS_TOOLS_FACTORY]
        );

        let portable = standard_agent_contributions(None).unwrap();
        let jobs = rsi_meta::PluginId::from(JOBS_TOOLS_FACTORY);
        assert!(portable.resolve(&jobs).is_ok());
        for linux_only in [BASH_TOOL_FACTORY, APPLY_PATCH_FACTORY] {
            assert!(
                portable
                    .resolve(&rsi_meta::PluginId::from(linux_only))
                    .is_err()
            );
        }
    }

    #[test]
    fn concurrent_standard_agent_preset_materialization_converges_without_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = Arc::new(
            HostPaths::new(
                temporary.path().join("config"),
                temporary.path().join("state"),
                temporary.path().join("cache"),
            )
            .unwrap(),
        );
        let barrier = Arc::new(Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let paths = Arc::clone(&paths);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    standard_agent_preset_root(&paths).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let roots = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(roots.windows(2).all(|pair| pair[0] == pair[1]));
        let cache = paths.cache().join("agent-presets/system");
        assert!(fs::read_dir(cache).unwrap().all(|row| {
            !row.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn standard_agent_preset_rejects_an_intermediate_cache_symlink_without_writing_outside() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let cache = temporary.path().join("cache");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&cache).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, cache.join("agent-presets")).unwrap();
        let paths = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            &cache,
        )
        .unwrap();

        let error = standard_agent_preset_root(&paths).unwrap_err();

        assert!(error.to_string().contains("no-follow directory"));
        assert!(!outside.join("system").exists());
    }

    #[cfg(unix)]
    #[test]
    fn standard_agent_preset_rejects_a_symlinked_digest_cache() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(
            temporary.path().join("config"),
            temporary.path().join("state"),
            temporary.path().join("cache"),
        )
        .unwrap();
        let root = standard_agent_preset_root(&paths).unwrap();
        let outside = temporary.path().join("outside");
        fs::rename(&root, &outside).unwrap();
        symlink(&outside, &root).unwrap();
        let error = standard_agent_preset_root(&paths).unwrap_err();
        assert!(error.to_string().contains("not a real directory"));
    }
}
