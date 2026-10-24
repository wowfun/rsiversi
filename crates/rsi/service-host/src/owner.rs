use getrandom::fill;
use rsi_api_protocol::{EndpointId, HostEpoch};
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use thiserror::Error;

/// Service Host wire protocol epoch.
pub const SERVICE_HOST_PROTOCOL_EPOCH: u32 = 11;
static SERVICE_HOST_EXECUTABLE_BUILD: LazyLock<Result<String, String>> =
    LazyLock::new(executable_product_build);

/// Returns diagnostic artifact identity; wire compatibility uses protocol descriptors.
pub fn service_host_product_build() -> Result<&'static str, ServiceHostError> {
    service_host_executable_build()
}

/// Returns the exact running executable artifact identity independently of its build family.
pub fn service_host_executable_build() -> Result<&'static str, ServiceHostError> {
    SERVICE_HOST_EXECUTABLE_BUILD
        .as_ref()
        .map(String::as_str)
        .map_err(|error| ServiceHostError::Io(error.clone()))
}

const OWNER_DIRECTORY: &str = "session-host";
const OWNER_LOCK_FILE: &str = "owner.lock";
const OWNER_METADATA_FILE: &str = "owner.json";
const OWNER_LOG_FILE: &str = "owner.log";
const FALLBACK_RUNTIME_DIRECTORY: &str = "runtime";
#[cfg(unix)]
const SOCKET_FILE: &str = "host.sock";

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
    /// Canonical config/state authority pair; absent in older lifecycle records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_authority: Option<String>,
    /// Digest of the currently accepted Service Profile sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_revision: Option<String>,
    /// Exact wire protocol epoch.
    pub protocol_epoch: u32,
    /// Exact product build identity.
    pub product_build: String,
    /// Random process generation.
    pub host_epoch: HostEpoch,
    /// Persistent deployment identity; absent only in legacy lifecycle metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<EndpointId>,
    /// Published endpoint for daemon mode only.
    pub socket_path: Option<PathBuf>,
}

impl HostOwnerMetadata {
    /// Builds and validates metadata for the current process.
    pub fn current(
        mode: HostOwnerMode,
        launch_key: impl Into<String>,
        host_epoch: HostEpoch,
        endpoint_id: EndpointId,
        socket_path: Option<PathBuf>,
    ) -> Result<Self, ServiceHostError> {
        #[cfg(any(target_os = "linux", windows))]
        let process_start_token = current_process_start_token()?;
        #[cfg(not(any(target_os = "linux", windows)))]
        let process_start_token = current_process_start_token();
        let metadata = Self {
            format: 2,
            pid: std::process::id(),
            process_start_token,
            mode,
            launch_key: launch_key.into(),
            home_authority: None,
            service_revision: None,
            protocol_epoch: SERVICE_HOST_PROTOCOL_EPOCH,
            product_build: service_host_product_build()?.into(),
            host_epoch,
            endpoint_id: Some(endpoint_id),
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
    pub fn validate(&self) -> Result<(), ServiceHostError> {
        if !matches!((self.format, &self.endpoint_id), (1, None) | (2, Some(_))) {
            return Err(ServiceHostError::Invalid(
                "unsupported Service Host owner metadata format".into(),
            ));
        }
        validate_process_identity(self.pid, &self.process_start_token)?;
        validate_launch_key(&self.launch_key)?;
        for digest in [&self.home_authority, &self.service_revision]
            .into_iter()
            .flatten()
        {
            validate_launch_key(digest)?;
        }
        if self.protocol_epoch == 0 {
            return Err(ServiceHostError::Invalid(
                "Service Host protocol epoch must be nonzero".into(),
            ));
        }
        validate_product_build(&self.product_build)?;
        match (self.mode, &self.socket_path) {
            (HostOwnerMode::Embedded, None) => {}
            (HostOwnerMode::Daemon, Some(path)) => validate_socket_path(path)?,
            _ => {
                return Err(ServiceHostError::Invalid(
                    "only daemon metadata may publish exactly one socket path".into(),
                ));
            }
        }
        Ok(())
    }

    /// Returns whether the local transport format can be negotiated.
    /// Required operation descriptors are checked by each client after connecting.
    pub fn is_compatible_with_current(&self) -> Result<bool, ServiceHostError> {
        self.validate()?;
        Ok(self.format == 2 && self.protocol_epoch == SERVICE_HOST_PROTOCOL_EPOCH)
    }
}

pub(crate) fn validate_process_identity(pid: u32, token: &str) -> Result<(), ServiceHostError> {
    let native = !token.is_empty()
        && token.len() <= 20
        && token.bytes().all(|byte| byte.is_ascii_digit())
        && token.parse::<u64>().is_ok();
    if pid == 0 || !(native || token == format!("process-{pid}")) {
        return Err(ServiceHostError::Invalid("invalid process identity".into()));
    }
    Ok(())
}

pub(crate) fn validate_launch_key(value: &str) -> Result<(), ServiceHostError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ServiceHostError::Invalid(
            "Host launch key must be 64 lower-case hexadecimal bytes".into(),
        ));
    }
    Ok(())
}

fn validate_product_build(value: &str) -> Result<(), ServiceHostError> {
    let Some((version, digest)) = value
        .rsplit_once("+sha256:")
        .or_else(|| value.rsplit_once("+family-sha256:"))
    else {
        return Err(ServiceHostError::Invalid(
            "Service Host product build has no executable or family digest".into(),
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
        return Err(ServiceHostError::Invalid(
            "Service Host product build is malformed".into(),
        ));
    }
    Ok(())
}

/// Returns whether the metadata still names the same live process generation.
pub fn owner_process_is_current(metadata: &HostOwnerMetadata) -> Result<bool, ServiceHostError> {
    process_generation_is_current(metadata.pid, &metadata.process_start_token)
}

pub(crate) fn process_generation_is_current(
    pid: u32,
    process_start_token: &str,
) -> Result<bool, ServiceHostError> {
    #[cfg(target_os = "linux")]
    {
        Ok(live_process_start_token(pid)?.is_some_and(|token| token == process_start_token))
    }
    #[cfg(windows)]
    {
        let process = match rsi_process_native::ProcessHandle::open(pid) {
            Ok(process) => process,
            Err(error) if error.raw_os_error() == Some(87) => return Ok(false),
            Err(error) => return Err(io_error(error)),
        };
        let current =
            rsi_process_native::ProcessHandle::open(std::process::id()).map_err(io_error)?;
        Ok(process.identity().start_token == process_start_token
            && process.identity().user_sid == current.identity().user_sid
            && process.is_running().map_err(io_error)?)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (pid, process_start_token);
        Err(ServiceHostError::Unsupported)
    }
}

/// Signals an exact daemon generation only after rechecking its start token.
pub fn signal_owner(
    metadata: &HostOwnerMetadata,
    signal: HostSignal,
) -> Result<(), ServiceHostError> {
    signal_generation(
        metadata.pid,
        &metadata.process_start_token,
        metadata.mode,
        signal,
    )
}

pub(crate) fn signal_generation(
    pid: u32,
    process_start_token: &str,
    mode: HostOwnerMode,
    signal: HostSignal,
) -> Result<(), ServiceHostError> {
    if mode != HostOwnerMode::Daemon {
        return Err(ServiceHostError::Invalid(
            "only a daemon owner accepts Host lifecycle signals".into(),
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let raw_pid = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| ServiceHostError::Invalid("owner PID is out of range".into()))?;
        let pidfd = rustix::process::pidfd_open(raw_pid, rustix::process::PidfdFlags::empty())
            .map_err(|error| {
                ServiceHostError::Io(format!("failed to open Service Host owner pidfd: {error}"))
            })?;
        if live_process_start_token(pid)?.is_none_or(|token| token != process_start_token) {
            return Err(ServiceHostError::Invalid(
                "owner PID no longer has the recorded process start token".into(),
            ));
        }
        let signal = match signal {
            HostSignal::Stop => rustix::process::Signal::TERM,
            HostSignal::ForceStop => rustix::process::Signal::KILL,
            HostSignal::Reload => rustix::process::Signal::HUP,
        };
        rustix::process::pidfd_send_signal(pidfd, signal).map_err(|error| {
            ServiceHostError::Io(format!("failed to signal Service Host owner: {error}"))
        })?;
        Ok(())
    }
    #[cfg(windows)]
    {
        if signal != HostSignal::ForceStop {
            return Err(ServiceHostError::Invalid(
                "graceful control requires Lifecycle v1".into(),
            ));
        }
        let current =
            rsi_process_native::ProcessHandle::open(std::process::id()).map_err(io_error)?;
        let expected = rsi_process_native::ProcessIdentity {
            pid,
            start_token: process_start_token.to_owned(),
            user_sid: current.identity().user_sid.clone(),
        };
        let process =
            rsi_process_native::ProcessHandle::for_termination(&expected).map_err(io_error)?;
        process.terminate(1).map_err(io_error)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = signal;
        Err(ServiceHostError::Unsupported)
    }
}

/// Frozen persistent and runtime paths for one canonical standard Host identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceHostPaths {
    config_owner_directory: PathBuf,
    owner_directory: PathBuf,
    owner_lock: PathBuf,
    owner_metadata: PathBuf,
    owner_log: PathBuf,
    runtime_directory: PathBuf,
    socket: PathBuf,
}

impl ServiceHostPaths {
    /// Stable authority of the canonical config/state pair, independent of applications.
    /// Both homes must already exist; callers may acquire the startup coordinator first.
    pub fn home_authority(&self) -> Result<String, ServiceHostError> {
        let mut digest = Sha256::new();
        digest.update(b"rsi.home-authority.v1\0");
        for directory in [&self.config_owner_directory, &self.owner_directory] {
            let root = directory
                .parent()
                .ok_or_else(|| ServiceHostError::Invalid("owner root has no home".into()))?;
            let canonical = fs::canonicalize(root).map_err(io_error)?;
            let bytes = canonical.as_os_str().as_encoded_bytes();
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        Ok(hex::encode(digest.finalize()))
    }
    /// Resolves persistent paths without depending on a login-session runtime directory.
    pub fn from_host_paths(paths: &HostPaths) -> Result<Self, ServiceHostError> {
        Self::from_host_paths_with_runtime(paths, None)
    }

    /// Resolves paths with an explicit runtime root, primarily for deterministic hosts and tests.
    pub fn from_host_paths_with_runtime(
        paths: &HostPaths,
        xdg_runtime_directory: Option<&Path>,
    ) -> Result<Self, ServiceHostError> {
        let owner_directory = paths.state().join(OWNER_DIRECTORY);
        let runtime_directory = xdg_runtime_directory.map_or_else(
            || owner_directory.join(FALLBACK_RUNTIME_DIRECTORY),
            |root| root.join("rsi").join(state_root_digest(paths.state())),
        );
        #[cfg(unix)]
        let socket = runtime_directory.join(SOCKET_FILE);
        #[cfg(windows)]
        let socket = PathBuf::from(format!(
            r"\\.\pipe\rsi-{}",
            state_root_digest(paths.state())
        ));
        Ok(Self {
            config_owner_directory: paths.config().join(OWNER_DIRECTORY),
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

    pub(crate) fn lifecycle(&self) -> Self {
        let mut paths = self.clone();
        paths.socket = lifecycle_endpoint(&self.socket);
        paths
    }

    pub(crate) fn sibling(&self, name: &str) -> Result<Self, ServiceHostError> {
        let mut paths = self.clone();
        paths.socket = sibling_endpoint(&self.socket, name)?;
        Ok(paths)
    }

    /// Validates that this process's preferred endpoint can be bound.
    ///
    /// Metadata readers do not need this check because a live daemon's recorded
    /// endpoint, rather than the reader's environment-derived candidate, is
    /// authoritative.
    pub fn validate_daemon_endpoint(&self) -> Result<(), ServiceHostError> {
        validate_socket_path(&self.socket)
    }

    /// Reads and strictly validates recoverable metadata when present.
    pub fn read_metadata(&self) -> Result<Option<HostOwnerMetadata>, ServiceHostError> {
        let Some(bytes) = self.read_owner_bytes()? else {
            return Ok(None);
        };
        let metadata: HostOwnerMetadata = serde_json::from_slice(&bytes)
            .map_err(|error| ServiceHostError::Invalid(error.to_string()))?;
        metadata.validate()?;
        self.validate_home_authority(metadata.home_authority.as_deref())?;
        Ok(Some(metadata))
    }

    /// Reads only the frozen lifecycle identity, ignoring evolving product fields.
    #[cfg(any(unix, windows))]
    pub fn read_lifecycle_identity(
        &self,
    ) -> Result<Option<crate::LifecycleIdentity>, ServiceHostError> {
        let Some(bytes) = self.read_owner_bytes()? else {
            return Ok(None);
        };
        let identity: crate::LifecycleIdentity = serde_json::from_slice(&bytes)
            .map_err(|error| ServiceHostError::Invalid(error.to_string()))?;
        identity.validate()?;
        self.validate_home_authority(identity.home_authority.as_deref())?;
        Ok(Some(identity))
    }

    fn validate_home_authority(&self, authority: Option<&str>) -> Result<(), ServiceHostError> {
        if let Some(authority) = authority
            && self.home_authority()?.as_str() != authority
        {
            return Err(ServiceHostError::Invalid(
                "the active Service Host belongs to a different config/state home pair".into(),
            ));
        }
        Ok(())
    }

    fn read_owner_bytes(&self) -> Result<Option<Vec<u8>>, ServiceHostError> {
        #[cfg(windows)]
        let opened = (|| {
            let directory =
                rsi_files_native_fs::open_absolute_directory_no_follow(&self.owner_directory)?;
            let file = rsi_files_native_fs::open_relative_file_no_follow(
                &directory,
                Path::new(OWNER_METADATA_FILE),
            )?;
            rsi_files_native_fs::verify_private_file(&file)?;
            Ok::<_, io::Error>(file)
        })();
        #[cfg(not(windows))]
        let opened = {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
            }
            options.open(&self.owner_metadata)
        };
        let file = match opened {
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
            return Err(ServiceHostError::Invalid(
                "owner metadata exceeds 16 KiB".into(),
            ));
        }
        Ok(Some(bytes))
    }
}

pub(crate) fn lifecycle_endpoint(endpoint: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        endpoint.with_file_name("control")
    }
    #[cfg(windows)]
    {
        let mut name = endpoint.as_os_str().to_owned();
        name.push("-control");
        name.into()
    }
}

pub(crate) fn sibling_endpoint(endpoint: &Path, name: &str) -> Result<PathBuf, ServiceHostError> {
    if name.is_empty()
        || name.len() > 8
        || !name.bytes().all(|byte| byte.is_ascii_lowercase())
        || matches!(name, "control" | "host")
    {
        return Err(ServiceHostError::Invalid(
            "invalid local endpoint name".into(),
        ));
    }
    #[cfg(unix)]
    let path = endpoint.with_file_name(name);
    #[cfg(windows)]
    let path = {
        let mut value = endpoint.as_os_str().to_owned();
        value.push(format!("-{name}"));
        PathBuf::from(value)
    };
    validate_socket_path(&path)?;
    Ok(path)
}

/// Exclusive owner lease shared by embedded and daemon execution.
#[derive(Debug)]
pub struct HostOwnerLease {
    _locks: Vec<HomeLock>,
    paths: ServiceHostPaths,
    published_epoch: Mutex<Option<HostEpoch>>,
    pub(crate) endpoint: tokio::sync::Mutex<Option<(String, rsi_api_protocol::EndpointId)>>,
}

impl HostOwnerLease {
    /// Acquires both home authorities in canonical order without waiting.
    pub fn try_acquire(paths: ServiceHostPaths) -> Result<Self, ServiceHostError> {
        let locks = acquire_home_locks(&paths, OWNER_LOCK_FILE)?;
        Ok(Self {
            _locks: locks,
            paths,
            published_epoch: Mutex::new(None),
            endpoint: tokio::sync::Mutex::new(None),
        })
    }

    /// Returns the exact paths protected by this owner lease.
    pub fn paths(&self) -> &ServiceHostPaths {
        &self.paths
    }

    /// Atomically publishes recoverable metadata for this owner generation.
    pub fn publish(&self, metadata: &HostOwnerMetadata) -> Result<(), ServiceHostError> {
        if !metadata.is_compatible_with_current()?
            || metadata.product_build != service_host_executable_build()?
        {
            return Err(ServiceHostError::Invalid(
                "cannot publish owner metadata for an incompatible protocol or product build"
                    .into(),
            ));
        }
        let authority = self.paths.home_authority()?;
        if metadata
            .home_authority
            .as_ref()
            .is_some_and(|value| value != &authority)
        {
            return Err(ServiceHostError::Invalid(
                "owner publication belongs to different homes".into(),
            ));
        }
        let mut metadata = metadata.clone();
        metadata.home_authority = Some(authority);
        let mut published = self
            .published_epoch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        create_private_directories(self.paths.owner_directory())?;
        let bytes = serde_json::to_vec(&metadata)
            .map_err(|error| ServiceHostError::Invalid(error.to_string()))?;
        atomic_private_write(self.paths.owner_metadata(), &bytes)?;
        *published = Some(metadata.host_epoch.clone());
        Ok(())
    }
}

/// Serializes process startup and maintenance independently of the child's owner leases.
#[derive(Debug)]
pub struct HostStartupLease {
    _locks: Vec<HomeLock>,
}

impl HostStartupLease {
    /// Attempts to coordinate both homes without waiting or opening product configuration.
    pub fn try_acquire(paths: &ServiceHostPaths) -> Result<Self, ServiceHostError> {
        Ok(Self {
            _locks: acquire_home_locks(paths, "startup.lock")?,
        })
    }
}

#[derive(Debug)]
struct HomeLock {
    _file: File,
    #[cfg(windows)]
    _namespace: rsi_files_native_fs::NamespaceLease,
}

fn acquire_home_locks(
    paths: &ServiceHostPaths,
    name: &str,
) -> Result<Vec<HomeLock>, ServiceHostError> {
    let mut ordered = std::collections::BTreeMap::new();
    for directory in [&paths.config_owner_directory, &paths.owner_directory] {
        create_private_directories(directory)?;
        // Canonical identity orders aliases consistently; acquisition still uses the
        // admitted original spelling, including Windows long-name validation.
        let identity = fs::canonicalize(directory).map_err(io_error)?;
        ordered.entry(identity).or_insert(directory);
    }
    ordered
        .into_values()
        .map(|directory| HomeLock::acquire(directory, name))
        .collect()
}

impl HomeLock {
    fn acquire(root: &Path, name: &str) -> Result<Self, ServiceHostError> {
        let path = root.join(name);
        #[cfg(windows)]
        let directory =
            rsi_files_native_fs::open_absolute_directory_no_follow(root).map_err(io_error)?;
        #[cfg(windows)]
        let namespace = directory.pin_namespace().map_err(io_error)?;
        reject_symlink(&path, "home lease")?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        #[cfg(not(windows))]
        let file = options.open(&path).map_err(io_error)?;
        #[cfg(windows)]
        let file = {
            match rsi_files_native_fs::create_relative_private_file_no_follow(
                &directory,
                Path::new(name),
            ) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    rsi_files_native_fs::set_relative_private_file_permissions_no_follow(
                        &directory,
                        Path::new(name),
                    )
                    .map_err(io_error)?;
                    rsi_files_native_fs::open_relative_private_file_no_follow(
                        &directory,
                        Path::new(name),
                    )
                    .map_err(io_error)?
                }
                Err(e) => return Err(io_error(e)),
            }
        };
        validate_open_regular_file(&path, &file, "home lease")?;
        #[cfg(unix)]
        set_file_mode(&file, 0o600)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(ServiceHostError::OwnerActive),
            Err(std::fs::TryLockError::Error(error)) => return Err(io_error(error)),
        }
        validate_open_regular_file(&path, &file, "home lease")?;
        Ok(Self {
            _file: file,
            #[cfg(windows)]
            _namespace: namespace,
        })
    }
}

impl Drop for HostOwnerLease {
    fn drop(&mut self) {
        if let Some(epoch) = self
            .published_epoch
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            && self
                .paths
                .read_lifecycle_identity()
                .ok()
                .flatten()
                .is_some_and(|metadata| metadata.host_epoch == *epoch)
        {
            let _ = fs::remove_file(self.paths.owner_metadata());
        }
    }
}

/// Service Host path, ownership, and transport failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ServiceHostError {
    /// Another embedded or daemon owner holds the standard paths.
    #[cfg_attr(
        windows,
        error(
            "another RSIversi application owns these data directories; stop the other Web, TUI, ACP or offline device process first (do not delete owner.lock)"
        )
    )]
    #[cfg_attr(not(windows), error("another Service Host owner is active"))]
    OwnerActive,
    /// Durable or wire input violated its bounded contract.
    #[error("invalid Service Host state: {0}")]
    Invalid(String),
    /// A bounded transport pool has no capacity; callers may retry.
    #[error("Service Host capacity exhausted: {0}")]
    Capacity(String),
    /// Platform I/O failed.
    #[error("Service Host I/O failed: {0}")]
    Io(String),
    /// This platform cannot publish the local daemon transport.
    #[error("Service Host daemon mode is unsupported on this platform")]
    Unsupported,
}

#[cfg(target_os = "linux")]
fn current_process_start_token() -> Result<String, ServiceHostError> {
    live_process_start_token(std::process::id())?.ok_or_else(|| {
        ServiceHostError::Io("current process disappeared while reading its start token".into())
    })
}

#[cfg(windows)]
fn current_process_start_token() -> Result<String, ServiceHostError> {
    Ok(rsi_process_native::ProcessHandle::open(std::process::id())
        .map_err(io_error)?
        .identity()
        .start_token
        .clone())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn current_process_start_token() -> String {
    format!("process-{}", std::process::id())
}

#[cfg(target_os = "linux")]
fn live_process_start_token(pid: u32) -> Result<Option<String>, ServiceHostError> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let end = stat.rfind(')').ok_or_else(|| {
        ServiceHostError::Invalid("/proc/self/stat has no process-name terminator".into())
    })?;
    let mut fields = stat[end + 1..].split_whitespace();
    let state = fields
        .next()
        .ok_or_else(|| ServiceHostError::Invalid("/proc process stat has no state".into()))?;
    // Exit releases the service resources before a foreground caller reaps its child.
    // A zombie retains its start token but can no longer serve or handle a signal.
    if state == "Z" {
        return Ok(None);
    }
    let token = fields
        .nth(18)
        .ok_or_else(|| ServiceHostError::Invalid("/proc/self/stat is truncated".into()))?;
    if !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ServiceHostError::Invalid(
            "/proc/self/stat start token is not numeric".into(),
        ));
    }
    Ok(Some(token.into()))
}

pub(crate) fn validate_socket_path(path: &Path) -> Result<(), ServiceHostError> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(ServiceHostError::Invalid(
            "Service Host socket path must be absolute".into(),
        ));
    }
    #[cfg(unix)]
    {
        std::os::unix::net::SocketAddr::from_pathname(path)
            .map(|_| ())
            .map_err(|error| {
                ServiceHostError::Invalid(format!("invalid Unix socket path: {error}"))
            })
    }
    #[cfg(windows)]
    {
        rsi_api_local_transport::PipeName::new(path)
            .map(|_| ())
            .map_err(io_error)
    }
}

fn create_private_directories(path: &Path) -> Result<(), ServiceHostError> {
    #[cfg(windows)]
    {
        let _directory = rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(path)
            .map_err(io_error)?;
        rsi_files_native_fs::set_private_directory_permissions_no_follow(path)
            .map(|_| ())
            .map_err(io_error)
    }
    #[cfg(not(windows))]
    {
        fs::create_dir_all(path).map_err(io_error)?;
        let metadata = fs::symlink_metadata(path).map_err(io_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ServiceHostError::Invalid(format!(
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
}

#[cfg(windows)]
fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), ServiceHostError> {
    let parent = path
        .parent()
        .ok_or_else(|| ServiceHostError::Invalid("metadata has no parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| ServiceHostError::Invalid("metadata has no name".into()))?;
    let directory =
        rsi_files_native_fs::open_absolute_directory_no_follow(parent).map_err(io_error)?;
    let namespace = directory.pin_namespace().map_err(io_error)?;
    let mut entropy = [0_u8; 16];
    fill(&mut entropy).map_err(|error| ServiceHostError::Io(error.to_string()))?;
    let temporary = format!(".metadata-{}", hex::encode(entropy));
    let mut file = rsi_files_native_fs::create_relative_private_file_no_follow(
        &directory,
        Path::new(&temporary),
    )
    .map_err(io_error)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error)?;
    drop(file);
    namespace
        .publish_child(
            Path::new(&temporary),
            Path::new(name),
            rsi_files_native_fs::EntryKind::File,
            true,
        )
        .map_err(io_error)?;
    directory.sync_all().map_err(io_error)
}

#[cfg(not(windows))]
fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), ServiceHostError> {
    let parent = path
        .parent()
        .ok_or_else(|| ServiceHostError::Invalid("metadata path has no parent".into()))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ServiceHostError::Invalid("metadata file name is invalid".into()))?;
    let mut entropy = [0_u8; 8];
    fill(&mut entropy)
        .map_err(|error| ServiceHostError::Io(format!("OS entropy failed: {error}")))?;
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
        #[cfg(windows)]
        return rsi_files_native_fs::sync_absolute_directory_no_follow(parent).map_err(io_error);
        #[cfg(not(windows))]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(io_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), ServiceHostError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ServiceHostError::Invalid(
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
) -> Result<(), ServiceHostError> {
    let path_metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let file_metadata = file.metadata().map_err(io_error)?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.file_type().is_file()
        || !file_metadata.file_type().is_file()
    {
        return Err(ServiceHostError::Invalid(format!(
            "{label} is not a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(ServiceHostError::Invalid(format!(
                "{label} changed while opening"
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: u32) -> Result<(), ServiceHostError> {
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
        .map_err(|error| format!("resolve Service Host executable identity: {error}"))?;
    let mut file = File::open(&executable)
        .map_err(|error| format!("open Service Host executable identity: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read Service Host executable identity: {error}"))?;
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
pub(crate) fn io_error(error: io::Error) -> ServiceHostError {
    ServiceHostError::Io(error.to_string())
}
