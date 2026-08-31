//! Local Linux sandbox backend selection and process planning.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, MAXIMUM_SANDBOX_ARGUMENTS, MAXIMUM_SANDBOX_PLAN_BYTES,
    MAXIMUM_SANDBOX_WRAPPER_BYTES, ProcessRequest, Result, Sandbox, SandboxBackend,
    SandboxContract, SandboxError, SandboxFileSystem, SandboxMode, SandboxNetwork, SandboxScratch,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_SUCCESS_CODE: i32 = 23;
const BUBBLEWRAP_PROBE_ARGUMENTS: &[&str] = &[
    "--die-with-parent",
    "--new-session",
    "--unshare-all",
    "--share-net",
    "--ro-bind",
    "/",
    "/",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "--",
    "/bin/sh",
    "-c",
    "exit 23",
];
const LANDLOCK_PROBE_ARGUMENTS: &[&str] = &["--rsi-landlock-probe", "23"];

/// Feature-probe seam injectable in deterministic tests.
#[async_trait]
pub trait SandboxProbe: fmt::Debug + Send + Sync + 'static {
    /// Returns whether this exact executable succeeds with the probe operation.
    async fn available(&self, path: &Path, arguments: &[&str]) -> Result<bool>;
}

/// Tokio process-backed system probe.
#[derive(Clone, Debug, Default)]
pub struct SystemSandboxProbe;

#[async_trait]
impl SandboxProbe for SystemSandboxProbe {
    async fn available(&self, path: &Path, arguments: &[&str]) -> Result<bool> {
        let mut command = tokio::process::Command::new(path);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(PROBE_TIMEOUT, command.status()).await {
            Ok(Ok(status)) => Ok(status.code() == Some(PROBE_SUCCESS_CODE)),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Ok(Err(error)) => Err(SandboxError::Probe(error.to_string())),
            Err(_) => Err(SandboxError::Probe(format!(
                "probe timed out for {}",
                path.display()
            ))),
        }
    }
}

/// Explicit candidate paths accepted by [`SandboxLocalFactory`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxLocalConfig {
    /// bubblewrap candidates in preference order.
    #[serde(default)]
    pub bubblewrap: Vec<PathBuf>,
    /// Landlock runner candidates in preference order.
    #[serde(default)]
    pub landlock: Vec<PathBuf>,
}

impl SandboxLocalConfig {
    fn validate(&self) -> Result<()> {
        let count = self
            .bubblewrap
            .len()
            .checked_add(self.landlock.len())
            .ok_or_else(|| SandboxError::InvalidInput("candidate count overflow".into()))?;
        if count > 64 {
            return Err(SandboxError::InvalidInput(
                "sandbox candidate count exceeds 64".into(),
            ));
        }
        for path in self.bubblewrap.iter().chain(&self.landlock) {
            if !path.is_absolute() {
                return Err(SandboxError::InvalidInput(
                    "sandbox candidate paths must be absolute".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Service {
    backend: Option<SelectedBackend>,
    _staged: Option<tempfile::TempDir>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendKind {
    Bubblewrap,
    Landlock,
}

#[derive(Clone, Debug)]
struct SelectedBackend {
    kind: BackendKind,
    path: PathBuf,
    sha256: String,
}

#[async_trait]
impl Sandbox for Service {
    async fn confine(&self, request: ProcessRequest) -> Result<ConfinedProcess> {
        let (program, cwd, workspace) = validate_request(&request)?;
        if request.mode == SandboxMode::DangerFullAccess {
            return Ok(ConfinedProcess {
                program,
                arguments: request.arguments.into_iter().map(OsString::from).collect(),
                cwd,
                stamp: stamp(request.mode, None, workspace),
            });
        }
        let backend = self
            .backend
            .clone()
            .ok_or(SandboxError::Unsupported(request.mode))?;
        let target_program = program.into_os_string();
        let target_cwd = cwd.clone().into_os_string();
        let target_workspace = workspace.clone().into_os_string();
        let (wrapper, arguments) = match backend.kind {
            BackendKind::Bubblewrap => {
                let mut arguments: Vec<OsString> = vec![
                    "--die-with-parent".into(),
                    "--new-session".into(),
                    "--unshare-all".into(),
                    "--share-net".into(),
                    "--ro-bind".into(),
                    "/".into(),
                    "/".into(),
                    "--tmpfs".into(),
                    "/tmp".into(),
                    "--proc".into(),
                    "/proc".into(),
                    "--dev".into(),
                    "/dev".into(),
                ];
                arguments.extend([
                    if request.mode == SandboxMode::WorkspaceWrite {
                        "--bind".into()
                    } else {
                        "--ro-bind".into()
                    },
                    target_workspace.clone(),
                    target_workspace.clone(),
                ]);
                arguments.extend(["--chdir".into(), target_cwd, "--".into(), target_program]);
                arguments.extend(request.arguments.into_iter().map(OsString::from));
                (backend.path.clone(), arguments)
            }
            BackendKind::Landlock => {
                let mut arguments: Vec<OsString> = vec![
                    "--mode".into(),
                    mode_name(request.mode).into(),
                    "--workspace".into(),
                    target_workspace,
                    "--cwd".into(),
                    target_cwd,
                    "--".into(),
                    target_program,
                ];
                arguments.extend(request.arguments.into_iter().map(OsString::from));
                (backend.path.clone(), arguments)
            }
        };
        validate_plan(&wrapper, &arguments, &cwd, &workspace)?;
        Ok(ConfinedProcess {
            program: wrapper,
            arguments,
            cwd,
            stamp: stamp(request.mode, Some(&backend), workspace),
        })
    }
}

/// Ordinary plugin factory for one selected local sandbox backend.
#[derive(Clone, Debug)]
pub struct SandboxLocalFactory {
    probe: Arc<dyn SandboxProbe>,
}

impl Default for SandboxLocalFactory {
    fn default() -> Self {
        Self {
            probe: Arc::new(SystemSandboxProbe),
        }
    }
}

impl SandboxLocalFactory {
    /// Creates a factory with an injected feature probe.
    pub fn with_probe(probe: Arc<dyn SandboxProbe>) -> Self {
        Self { probe }
    }

    async fn stage_available(
        &self,
        kind: BackendKind,
        path: PathBuf,
        arguments: &'static [&'static str],
    ) -> rsi_meta::Result<Option<(SelectedBackend, tempfile::TempDir)>> {
        let staged = tokio::task::spawn_blocking(move || stage_backend(kind, &path))
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let Ok((backend, directory)) = staged else {
            return Ok(None);
        };
        if matches!(
            self.probe.available(&backend.path, arguments).await,
            Ok(true)
        ) {
            Ok(Some((backend, directory)))
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl PluginFactory for SandboxLocalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: SandboxLocalConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config
            .bubblewrap
            .iter()
            .chain(&config.landlock)
            .map(|path| path.as_os_str().len())
            .sum();
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<SandboxLocalConfig>()?;
        let mut selected = None;
        for path in config.bubblewrap {
            if let Some(staged) = self
                .stage_available(BackendKind::Bubblewrap, path, BUBBLEWRAP_PROBE_ARGUMENTS)
                .await?
            {
                selected = Some(staged);
                break;
            }
        }
        if selected.is_none() {
            for path in config.landlock {
                if let Some(staged) = self
                    .stage_available(BackendKind::Landlock, path, LANDLOCK_PROBE_ARGUMENTS)
                    .await?
                {
                    selected = Some(staged);
                    break;
                }
            }
        }
        let (backend, staged) = match selected {
            Some((backend, staged)) => (Some(backend), Some(staged)),
            None => (None, None),
        };
        let sandbox: Arc<dyn Sandbox> = Arc::new(Service {
            backend,
            _staged: staged,
        });
        let supply = plan.context().provide_local::<SandboxContract>(sandbox)?;
        plan.defer(
            "withdraw local Sandbox service",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn validate_request(request: &ProcessRequest) -> Result<(PathBuf, PathBuf, PathBuf)> {
    if request.arguments.len() > MAXIMUM_SANDBOX_ARGUMENTS {
        return Err(SandboxError::InvalidInput(
            "process argument count is too large".into(),
        ));
    }
    if !request.program.is_absolute() {
        return Err(SandboxError::InvalidInput(
            "process program must be absolute".into(),
        ));
    }
    let program = request
        .program
        .canonicalize()
        .map_err(|error| SandboxError::InvalidInput(format!("program is unavailable: {error}")))?;
    if !program.is_file() {
        return Err(SandboxError::InvalidInput(
            "process program is not a file".into(),
        ));
    }
    let cwd = canonical_directory(&request.cwd, "cwd")?;
    let workspace = canonical_directory(&request.workspace, "workspace")?;
    if request.mode != SandboxMode::DangerFullAccess {
        if workspace == Path::new("/") {
            return Err(SandboxError::InvalidInput(
                "restricted workspace cannot be the filesystem root".into(),
            ));
        }
        if workspace == Path::new("/tmp") {
            return Err(SandboxError::InvalidInput(
                "restricted workspace cannot be exactly /tmp".into(),
            ));
        }
    }
    validate_plan(&program, &request.arguments, &cwd, &workspace)?;
    Ok((program, cwd, workspace))
}

fn canonical_directory(path: &Path, kind: &str) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|error| SandboxError::InvalidInput(format!("{kind} is unavailable: {error}")))?;
    if !canonical.is_dir() {
        return Err(SandboxError::InvalidInput(format!(
            "{kind} is not a directory"
        )));
    }
    Ok(canonical)
}

fn validate_plan<T: AsRef<OsStr>>(
    program: &Path,
    arguments: &[T],
    cwd: &Path,
    workspace: &Path,
) -> Result<()> {
    let bytes = program
        .as_os_str()
        .len()
        .checked_add(cwd.as_os_str().len())
        .and_then(|total| total.checked_add(workspace.as_os_str().len()))
        .and_then(|total| {
            arguments.iter().try_fold(total, |sum, argument| {
                sum.checked_add(argument.as_ref().len())
            })
        })
        .ok_or_else(|| SandboxError::InvalidInput("sandbox plan length overflow".into()))?;
    if bytes > MAXIMUM_SANDBOX_PLAN_BYTES {
        return Err(SandboxError::InvalidInput(
            "sandbox process plan is too large".into(),
        ));
    }
    Ok(())
}

fn stage_backend(kind: BackendKind, source: &Path) -> Result<(SelectedBackend, tempfile::TempDir)> {
    let directory = tempfile::Builder::new()
        .prefix("rsi-sandbox-wrapper-")
        .tempdir()
        .map_err(|error| SandboxError::Probe(error.to_string()))?;
    let staged = directory.path().join("wrapper");
    let mut source = open_candidate(source)?;
    let mut destination = create_staged_wrapper(&staged)?;
    let copied = std::io::copy(
        &mut std::io::Read::take(
            &mut source,
            u64::try_from(MAXIMUM_SANDBOX_WRAPPER_BYTES).expect("wrapper bound fits u64") + 1,
        ),
        &mut destination,
    )
    .map_err(|error| SandboxError::Probe(error.to_string()))?;
    if copied > u64::try_from(MAXIMUM_SANDBOX_WRAPPER_BYTES).expect("wrapper bound fits u64") {
        return Err(SandboxError::Probe(format!(
            "sandbox wrapper exceeds {MAXIMUM_SANDBOX_WRAPPER_BYTES} bytes"
        )));
    }
    destination
        .sync_all()
        .map_err(|error| SandboxError::Probe(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o500))
            .map_err(|error| SandboxError::Probe(error.to_string()))?;
    }
    let bytes = std::fs::read(&staged).map_err(|error| SandboxError::Probe(error.to_string()))?;
    let backend = SelectedBackend {
        kind,
        path: staged,
        sha256: hex::encode(Sha256::digest(bytes)),
    };
    Ok((backend, directory))
}

#[cfg(unix)]
fn open_candidate(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let path_metadata =
        std::fs::symlink_metadata(path).map_err(|error| SandboxError::Probe(error.to_string()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.file_type().is_file() {
        return Err(SandboxError::Probe(
            "sandbox candidate must be a real regular file".into(),
        ));
    }
    if path_metadata.len() > MAXIMUM_SANDBOX_WRAPPER_BYTES as u64 {
        return Err(SandboxError::Probe(format!(
            "sandbox wrapper exceeds {MAXIMUM_SANDBOX_WRAPPER_BYTES} bytes"
        )));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| SandboxError::Probe(error.to_string()))?;
    let opened = file
        .metadata()
        .map_err(|error| SandboxError::Probe(error.to_string()))?;
    if !opened.file_type().is_file()
        || opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
    {
        return Err(SandboxError::Probe(
            "sandbox candidate changed while opening".into(),
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_candidate(path: &Path) -> Result<std::fs::File> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| SandboxError::Probe(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(SandboxError::Probe(
            "sandbox candidate must be a real regular file".into(),
        ));
    }
    if metadata.len() > MAXIMUM_SANDBOX_WRAPPER_BYTES as u64 {
        return Err(SandboxError::Probe(format!(
            "sandbox wrapper exceeds {MAXIMUM_SANDBOX_WRAPPER_BYTES} bytes"
        )));
    }
    std::fs::File::open(path).map_err(|error| SandboxError::Probe(error.to_string()))
}

fn create_staged_wrapper(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o500).custom_flags(libc::O_CLOEXEC);
    }
    options
        .open(path)
        .map_err(|error| SandboxError::Probe(error.to_string()))
}

fn stamp(
    mode: SandboxMode,
    backend: Option<&SelectedBackend>,
    workspace: PathBuf,
) -> EnforcementStamp {
    let filesystem = match mode {
        SandboxMode::ReadOnly => SandboxFileSystem::ReadOnly,
        SandboxMode::WorkspaceWrite => SandboxFileSystem::WorkspaceWrite,
        SandboxMode::DangerFullAccess => SandboxFileSystem::Unconfined,
    };
    let durable_backend = match backend {
        Some(SelectedBackend {
            kind: BackendKind::Bubblewrap,
            sha256,
            ..
        }) => SandboxBackend::Bubblewrap {
            sha256: sha256.clone(),
        },
        Some(SelectedBackend {
            kind: BackendKind::Landlock,
            sha256,
            ..
        }) => SandboxBackend::Landlock {
            sha256: sha256.clone(),
        },
        None => SandboxBackend::Unconfined,
    };
    let scratch = if backend.is_some_and(|backend| backend.kind == BackendKind::Bubblewrap) {
        SandboxScratch::PrivateTmp
    } else {
        SandboxScratch::Host
    };
    let stamp = EnforcementStamp {
        requested: mode,
        backend: durable_backend,
        workspace,
        filesystem,
        scratch,
        network: SandboxNetwork::Host,
    };
    debug_assert!(stamp.validate().is_ok());
    stamp
}

const fn mode_name(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::ReadOnly => "read-only",
        SandboxMode::WorkspaceWrite => "workspace-write",
        SandboxMode::DangerFullAccess => "danger-full-access",
    }
}
