use getrandom::fill;
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use thiserror::Error;

/// Session Host wire protocol epoch.
pub const SESSION_HOST_PROTOCOL_EPOCH: u32 = 1;
static SESSION_HOST_PRODUCT_BUILD: LazyLock<Result<String, String>> =
    LazyLock::new(executable_product_build);

/// Returns the exact running executable identity used by local compatibility checks.
pub fn session_host_product_build() -> Result<&'static str, SessionHostError> {
    SESSION_HOST_PRODUCT_BUILD
        .as_ref()
        .map(String::as_str)
        .map_err(|error| SessionHostError::Io(error.clone()))
}

const OWNER_DIRECTORY: &str = "session-host";
const OWNER_LOCK_FILE: &str = "owner.lock";
const OWNER_METADATA_FILE: &str = "owner.json";
const OWNER_LOG_FILE: &str = "owner.log";
const FALLBACK_RUNTIME_DIRECTORY: &str = "runtime";
const SOCKET_FILE: &str = "host.sock";
#[cfg(unix)]
const MAXIMUM_UNIX_SOCKET_PATH_BYTES: usize = 107;

/// One random process-generation identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct HostEpoch(String);

impl<'de> Deserialize<'de> for HostEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl HostEpoch {
    /// Allocates one unpredictable generation identity from OS entropy.
    pub fn generate() -> Result<Self, SessionHostError> {
        let mut entropy = [0_u8; 16];
        fill(&mut entropy)
            .map_err(|error| SessionHostError::Io(format!("OS entropy failed: {error}")))?;
        Ok(Self(hex::encode(entropy)))
    }

    /// Parses the exact lower-case 128-bit hexadecimal representation.
    pub fn parse(value: impl Into<String>) -> Result<Self, SessionHostError> {
        let value = value.into();
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SessionHostError::Invalid(
                "Host epoch must be 32 lower-case hexadecimal bytes".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Returns the wire representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Active owner execution mode.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOwnerMode {
    /// Private in-process owner without an endpoint.
    Embedded,
    /// Explicit foreground/background daemon with a same-user endpoint.
    Daemon,
}

/// Supported daemon-control signals after PID start-token verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSignal {
    /// Ask the daemon to drain and shut down.
    Stop,
    /// Immediately terminate a stuck daemon after exact generation validation.
    ForceStop,
    /// Ask the daemon to rebuild its complete Profile source program.
    Reload,
}

/// Strict recoverable process-owner description.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostOwnerMetadata {
    /// Schema revision.
    pub format: u8,
    /// Owning process id.
    pub pid: u32,
    /// Platform process-start token fencing PID reuse.
    pub process_start_token: String,
    /// Embedded or daemon ownership.
    pub mode: HostOwnerMode,
    /// Exact launch identity.
    pub launch_key: String,
    /// Exact wire protocol epoch.
    pub protocol_epoch: u32,
    /// Exact product build identity.
    pub product_build: String,
    /// Random process generation.
    pub host_epoch: HostEpoch,
    /// Published endpoint for daemon mode only.
    pub socket_path: Option<PathBuf>,
}

impl HostOwnerMetadata {
    /// Builds and validates metadata for the current process.
    pub fn current(
        mode: HostOwnerMode,
        launch_key: impl Into<String>,
        host_epoch: HostEpoch,
        socket_path: Option<PathBuf>,
    ) -> Result<Self, SessionHostError> {
        let metadata = Self {
            format: 1,
            pid: std::process::id(),
            process_start_token: current_process_start_token()?,
            mode,
            launch_key: launch_key.into(),
            protocol_epoch: SESSION_HOST_PROTOCOL_EPOCH,
            product_build: session_host_product_build()?.into(),
            host_epoch,
            socket_path,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Validates all durable bounds and cross-field invariants.
    ///
    /// This deliberately does not require compatibility with the reading
    /// executable. Lifecycle clients must be able to inspect an older live
    /// generation after their own binary has been replaced.
    pub fn validate(&self) -> Result<(), SessionHostError> {
        if self.format != 1 {
            return Err(SessionHostError::Invalid(
                "unsupported Session Host owner metadata format".into(),
            ));
        }
        if self.pid == 0
            || self.process_start_token.is_empty()
            || self.process_start_token.len() > 128
        {
            return Err(SessionHostError::Invalid(
                "invalid Session Host process identity".into(),
            ));
        }
        validate_launch_key(&self.launch_key)?;
        if self.protocol_epoch == 0 {
            return Err(SessionHostError::Invalid(
                "Session Host protocol epoch must be nonzero".into(),
            ));
        }
        validate_product_build(&self.product_build)?;
        match (self.mode, &self.socket_path) {
            (HostOwnerMode::Embedded, None) => {}
            (HostOwnerMode::Daemon, Some(path)) => validate_socket_path(path)?,
            _ => {
                return Err(SessionHostError::Invalid(
                    "only daemon metadata may publish exactly one socket path".into(),
                ));
            }
        }
        Ok(())
    }

    /// Returns whether this structurally valid generation can handshake with
    /// the current executable.
    pub fn is_compatible_with_current(&self) -> Result<bool, SessionHostError> {
        self.validate()?;
        Ok(self.protocol_epoch == SESSION_HOST_PROTOCOL_EPOCH
            && self.product_build == session_host_product_build()?)
    }
}

pub(crate) fn validate_launch_key(value: &str) -> Result<(), SessionHostError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SessionHostError::Invalid(
            "Host launch key must be 64 lower-case hexadecimal bytes".into(),
        ));
    }
    Ok(())
}

fn validate_product_build(value: &str) -> Result<(), SessionHostError> {
    let Some((version, digest)) = value.rsplit_once("+sha256:") else {
        return Err(SessionHostError::Invalid(
            "Session Host product build has no executable digest".into(),
        ));
    };
    if version.is_empty()
        || version.len() > 128
        || !version.bytes().all(|byte| byte.is_ascii_graphic())
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SessionHostError::Invalid(
            "Session Host product build is malformed".into(),
        ));
    }
    Ok(())
}

/// Returns whether the metadata still names the same live process generation.
pub fn owner_process_is_current(metadata: &HostOwnerMetadata) -> Result<bool, SessionHostError> {
    #[cfg(target_os = "linux")]
    {
        Ok(process_start_token(metadata.pid)?
            .is_some_and(|token| token == metadata.process_start_token))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = metadata;
        Err(SessionHostError::Unsupported)
    }
}

/// Signals an exact daemon generation only after rechecking its start token.
pub fn signal_owner(
    metadata: &HostOwnerMetadata,
    signal: HostSignal,
) -> Result<(), SessionHostError> {
    if metadata.mode != HostOwnerMode::Daemon {
        return Err(SessionHostError::Invalid(
            "only a daemon owner accepts Host lifecycle signals".into(),
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let raw_pid = i32::try_from(metadata.pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| SessionHostError::Invalid("owner PID is out of range".into()))?;
        let pidfd = rustix::process::pidfd_open(raw_pid, rustix::process::PidfdFlags::empty())
            .map_err(|error| {
                SessionHostError::Io(format!("failed to open Session Host owner pidfd: {error}"))
            })?;
        if process_start_token(metadata.pid)?
            .is_none_or(|token| token != metadata.process_start_token)
        {
            return Err(SessionHostError::Invalid(
                "owner PID no longer has the recorded process start token".into(),
            ));
        }
        let signal = match signal {
            HostSignal::Stop => rustix::process::Signal::TERM,
            HostSignal::ForceStop => rustix::process::Signal::KILL,
            HostSignal::Reload => rustix::process::Signal::HUP,
        };
        rustix::process::pidfd_send_signal(pidfd, signal).map_err(|error| {
            SessionHostError::Io(format!("failed to signal Session Host owner: {error}"))
        })?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = signal;
        Err(SessionHostError::Unsupported)
    }
}

/// Frozen persistent and runtime paths for one canonical standard Host identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHostPaths {
    owner_directory: PathBuf,
    owner_lock: PathBuf,
    owner_metadata: PathBuf,
    owner_log: PathBuf,
    runtime_directory: PathBuf,
    socket: PathBuf,
}

impl SessionHostPaths {
    /// Resolves the environment-selected runtime path without creating files.
    pub fn from_host_paths(paths: &HostPaths) -> Result<Self, SessionHostError> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        Self::from_host_paths_with_runtime(paths, runtime.as_deref())
    }

    /// Resolves paths with an explicit runtime root, primarily for deterministic hosts and tests.
    pub fn from_host_paths_with_runtime(
        paths: &HostPaths,
        xdg_runtime_directory: Option<&Path>,
    ) -> Result<Self, SessionHostError> {
        let owner_directory = paths.state().join(OWNER_DIRECTORY);
        let runtime_directory = xdg_runtime_directory.map_or_else(
            || owner_directory.join(FALLBACK_RUNTIME_DIRECTORY),
            |root| root.join("rsi").join(state_root_digest(paths.state())),
        );
        let socket = runtime_directory.join(SOCKET_FILE);
        Ok(Self {
            owner_lock: owner_directory.join(OWNER_LOCK_FILE),
            owner_metadata: owner_directory.join(OWNER_METADATA_FILE),
            owner_log: owner_directory.join(OWNER_LOG_FILE),
            owner_directory,
            runtime_directory,
            socket,
        })
    }

    /// Persistent directory containing the lease and recoverable metadata.
    pub fn owner_directory(&self) -> &Path {
        &self.owner_directory
    }

    /// Persistent owner lease path.
    pub fn owner_lock(&self) -> &Path {
        &self.owner_lock
    }

    /// Recoverable owner metadata path.
    pub fn owner_metadata(&self) -> &Path {
        &self.owner_metadata
    }

    /// Detached daemon log path.
    pub fn owner_log(&self) -> &Path {
        &self.owner_log
    }

    /// Private runtime directory containing the published endpoint.
    pub fn runtime_directory(&self) -> &Path {
        &self.runtime_directory
    }

    /// Published daemon endpoint.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Validates that this process's preferred endpoint can be bound.
    ///
    /// Metadata readers do not need this check because a live daemon's recorded
    /// endpoint, rather than the reader's environment-derived candidate, is
    /// authoritative.
    pub fn validate_daemon_endpoint(&self) -> Result<(), SessionHostError> {
        validate_socket_path(&self.socket)
    }

    /// Reads and strictly validates recoverable metadata when present.
    pub fn read_metadata(&self) -> Result<Option<HostOwnerMetadata>, SessionHostError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let file = match options.open(&self.owner_metadata) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(error)),
        };
        validate_open_regular_file(&self.owner_metadata, &file, "owner metadata")?;
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > 16 * 1024 {
            return Err(SessionHostError::Invalid(
                "owner metadata exceeds 16 KiB".into(),
            ));
        }
        let metadata: HostOwnerMetadata = serde_json::from_slice(&bytes)
            .map_err(|error| SessionHostError::Invalid(error.to_string()))?;
        metadata.validate()?;
        Ok(Some(metadata))
    }
}

/// Exclusive owner lease shared by embedded and daemon execution.
#[derive(Debug)]
pub struct HostOwnerLease {
    file: File,
    paths: SessionHostPaths,
    published_epoch: Option<HostEpoch>,
}

impl HostOwnerLease {
    /// Attempts to acquire the single owner lease without waiting.
    pub fn try_acquire(paths: SessionHostPaths) -> Result<Self, SessionHostError> {
        create_private_directories(paths.owner_directory())?;
        reject_symlink(paths.owner_lock(), "owner lease")?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let file = options.open(paths.owner_lock()).map_err(io_error)?;
        validate_open_regular_file(paths.owner_lock(), &file, "owner lease")?;
        #[cfg(unix)]
        set_file_mode(&file, 0o600)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(SessionHostError::OwnerActive),
            Err(std::fs::TryLockError::Error(error)) => return Err(io_error(error)),
        }
        validate_open_regular_file(paths.owner_lock(), &file, "owner lease")?;
        Ok(Self {
            file,
            paths,
            published_epoch: None,
        })
    }

    /// Atomically publishes recoverable metadata for this owner generation.
    pub fn publish(&mut self, metadata: &HostOwnerMetadata) -> Result<(), SessionHostError> {
        if !metadata.is_compatible_with_current()? {
            return Err(SessionHostError::Invalid(
                "cannot publish owner metadata for an incompatible protocol or product build"
                    .into(),
            ));
        }
        create_private_directories(self.paths.owner_directory())?;
        let bytes = serde_json::to_vec(metadata)
            .map_err(|error| SessionHostError::Invalid(error.to_string()))?;
        atomic_private_write(self.paths.owner_metadata(), &bytes)?;
        self.published_epoch = Some(metadata.host_epoch.clone());
        Ok(())
    }
}

impl Drop for HostOwnerLease {
    fn drop(&mut self) {
        if let Some(epoch) = &self.published_epoch
            && self
                .paths
                .read_metadata()
                .ok()
                .flatten()
                .is_some_and(|metadata| metadata.host_epoch == *epoch)
        {
            let _ = fs::remove_file(self.paths.owner_metadata());
        }
        let _ = self.file.unlock();
    }
}

/// Session Host path, ownership, and transport failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SessionHostError {
    /// Another embedded or daemon owner holds the standard paths.
    #[error("another Session Host owner is active")]
    OwnerActive,
    /// Durable or wire input violated its bounded contract.
    #[error("invalid Session Host state: {0}")]
    Invalid(String),
    /// Platform I/O failed.
    #[error("Session Host I/O failed: {0}")]
    Io(String),
    /// This platform cannot publish the local daemon transport.
    #[error("Session Host daemon mode is unsupported on this platform")]
    Unsupported,
}

fn current_process_start_token() -> Result<String, SessionHostError> {
    #[cfg(target_os = "linux")]
    {
        process_start_token(std::process::id())?.ok_or_else(|| {
            SessionHostError::Io("current process disappeared while reading its start token".into())
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(format!("process-{}", std::process::id()))
    }
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Result<Option<String>, SessionHostError> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let end = stat.rfind(')').ok_or_else(|| {
        SessionHostError::Invalid("/proc/self/stat has no process-name terminator".into())
    })?;
    let token = stat[end + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| SessionHostError::Invalid("/proc/self/stat is truncated".into()))?;
    if !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SessionHostError::Invalid(
            "/proc/self/stat start token is not numeric".into(),
        ));
    }
    Ok(Some(token.into()))
}

fn validate_socket_path(path: &Path) -> Result<(), SessionHostError> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(SessionHostError::Invalid(
            "Session Host socket path must be absolute".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        if path.as_os_str().as_bytes().len() > MAXIMUM_UNIX_SOCKET_PATH_BYTES {
            return Err(SessionHostError::Invalid(format!(
                "Unix socket path exceeds {MAXIMUM_UNIX_SOCKET_PATH_BYTES} bytes: {}",
                path.display()
            )));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn create_private_directories(path: &Path) -> Result<(), SessionHostError> {
    fs::create_dir_all(path).map_err(io_error)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SessionHostError::Invalid(format!(
            "private path is not a directory: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    Ok(())
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), SessionHostError> {
    let parent = path
        .parent()
        .ok_or_else(|| SessionHostError::Invalid("metadata path has no parent".into()))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| SessionHostError::Invalid("metadata file name is invalid".into()))?;
    let mut entropy = [0_u8; 8];
    fill(&mut entropy)
        .map_err(|error| SessionHostError::Io(format!("OS entropy failed: {error}")))?;
    let temporary = parent.join(format!(".{name}.{}.tmp", hex::encode(entropy)));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary).map_err(io_error)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(io_error)?;
        #[cfg(unix)]
        set_file_mode(&file, 0o600)?;
        fs::rename(&temporary, path).map_err(io_error)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(io_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), SessionHostError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(SessionHostError::Invalid(
            format!("{label} must not be a symbolic link"),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn validate_open_regular_file(
    path: &Path,
    file: &File,
    label: &str,
) -> Result<(), SessionHostError> {
    let path_metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let file_metadata = file.metadata().map_err(io_error)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !file_metadata.file_type().is_file()
    {
        return Err(SessionHostError::Invalid(format!(
            "{label} is not a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(SessionHostError::Invalid(format!(
                "{label} changed while opening"
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: u32) -> Result<(), SessionHostError> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(io_error)
}

fn state_root_digest(path: &Path) -> String {
    let mut digest = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        digest.update(path.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    digest.update(path.to_string_lossy().as_bytes());
    hex::encode(digest.finalize())
}

fn executable_product_build() -> Result<String, String> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("resolve Session Host executable identity: {error}"))?;
    let mut file = File::open(&executable)
        .map_err(|error| format!("open Session Host executable identity: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read Session Host executable identity: {error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!(
        "{}+sha256:{}",
        env!("CARGO_PKG_VERSION"),
        hex::encode(digest.finalize())
    ))
}

#[allow(clippy::needless_pass_by_value)] // Kept as a direct `map_err` adapter.
fn io_error(error: io::Error) -> SessionHostError {
    SessionHostError::Io(error.to_string())
}
