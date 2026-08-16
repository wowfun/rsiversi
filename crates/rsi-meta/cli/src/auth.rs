use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::io::Write;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use uuid::Uuid;

const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;
const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const TOKEN_FILE_FORMAT_VERSION: u32 = 0;
const MAX_TOKEN_FILE_BYTES: u64 = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TokenFileEnvelope {
    format_version: u32,
    generation: u64,
    token: String,
}

#[derive(Clone)]
pub struct BearerToken(String);

impl BearerToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

#[derive(Clone)]
pub struct AuthState {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    token_file: PathBuf,
    digest: RwLock<[u8; 32]>,
    generation: AtomicU64,
    generation_tx: watch::Sender<u64>,
    rotation: Mutex<()>,
}

impl fmt::Debug for AuthState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthState")
            .field("token_file", &self.inner.token_file)
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl AuthState {
    /// Loads a secure versioned token file, or creates it on first use.
    pub fn initialize(token_file: impl Into<PathBuf>) -> Result<Self> {
        let token_file = token_file.into();
        let parent = token_parent(&token_file)?;
        ensure_private_directory(parent)?;
        let (token, generation) = match fs::symlink_metadata(&token_file) {
            Ok(_) => read_token_envelope(&token_file)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let token = generate_token();
                write_token_file(&token_file, 0, &token)?;
                (token, 0)
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect token file {}", token_file.display()));
            }
        };
        let (generation_tx, _) = watch::channel(generation);
        Ok(Self {
            inner: Arc::new(AuthInner {
                token_file,
                digest: RwLock::new(token_digest(token.expose())),
                generation: AtomicU64::new(generation),
                generation_tx,
                rotation: Mutex::new(()),
            }),
        })
    }

    #[cfg(test)]
    fn with_token(token_file: impl Into<PathBuf>, token: &str) -> Result<Self> {
        let token_file = token_file.into();
        let token = BearerToken(token.to_owned());
        write_token_file(&token_file, 0, &token)?;
        let (generation_tx, _) = watch::channel(0);
        Ok(Self {
            inner: Arc::new(AuthInner {
                token_file,
                digest: RwLock::new(token_digest(token.expose())),
                generation: AtomicU64::new(0),
                generation_tx,
                rotation: Mutex::new(()),
            }),
        })
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    pub fn subscribe_generation(&self) -> watch::Receiver<u64> {
        self.inner.generation_tx.subscribe()
    }

    #[cfg(test)]
    pub fn authorize(&self, authorization: &str) -> bool {
        self.authorize_generation(authorization).is_some()
    }

    /// Authenticates a bearer and snapshots its generation atomically with
    /// respect to rotation.
    pub fn authorize_generation(&self, authorization: &str) -> Option<u64> {
        let _rotation = self
            .inner
            .rotation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let candidate = authorization.strip_prefix("Bearer ")?;
        if candidate.is_empty() || candidate.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return None;
        }
        let candidate_digest = token_digest(candidate);
        let expected = self
            .inner
            .digest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        constant_time_eq(&candidate_digest, &expected).then(|| self.generation())
    }

    /// Applies a durable token generation exactly once.
    ///
    /// The token is published on disk before the in-memory generation advances.
    /// Replaying the same or an older durable outcome is a no-op; advancing it
    /// disconnects existing authenticated browser sessions.
    pub fn rotate_to(&self, target_generation: u64) -> Result<bool> {
        let _rotation = self
            .inner
            .rotation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.generation() >= target_generation {
            return Ok(false);
        }
        let token = generate_token();
        write_token_file(&self.inner.token_file, target_generation, &token)?;
        *self
            .inner
            .digest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token_digest(token.expose());
        self.inner
            .generation
            .store(target_generation, Ordering::Release);
        self.inner.generation_tx.send_replace(target_generation);
        Ok(true)
    }

    /// Reconciles the credential file with the durable core generation during
    /// daemon startup.
    ///
    /// A lagging file is repaired with a fresh bearer before remote admission.
    /// An ahead-of-core file indicates that the database and credential state
    /// were paired incorrectly or rolled back, so startup must fail closed.
    pub fn reconcile_generation(&self, persisted_generation: u64) -> Result<bool> {
        let file_generation = self.generation();
        if file_generation > persisted_generation {
            bail!(
                "token file generation {file_generation} is ahead of durable core generation {persisted_generation}"
            );
        }
        self.rotate_to(persisted_generation)
    }
}

#[cfg(test)]
pub fn read_token_file(path: &Path) -> Result<BearerToken> {
    read_token_envelope(path).map(|(token, _)| token)
}

fn read_token_envelope(path: &Path) -> Result<(BearerToken, u64)> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect token file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("token path {} is not a regular file", path.display());
    }
    require_current_owner(&metadata, path)?;
    let mode = metadata.mode() & 0o777;
    if mode != PRIVATE_FILE_MODE {
        bail!(
            "token file {} must have mode 0600, found {mode:04o}",
            path.display()
        );
    }
    if metadata.len() > MAX_TOKEN_FILE_BYTES {
        bail!("token file {} is unexpectedly large", path.display());
    }

    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .with_context(|| format!("open token file {}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("inspect opened token file {}", path.display()))?;
    if !opened_metadata.is_file() {
        bail!("opened token path {} is not a regular file", path.display());
    }
    require_current_owner(&opened_metadata, path)?;
    let opened_mode = opened_metadata.mode() & 0o777;
    if opened_mode != PRIVATE_FILE_MODE {
        bail!(
            "opened token file {} must have mode 0600, found {opened_mode:04o}",
            path.display()
        );
    }
    if opened_metadata.len() > MAX_TOKEN_FILE_BYTES {
        bail!("opened token file {} is unexpectedly large", path.display());
    }
    let mut value = String::new();
    file.take(MAX_TOKEN_FILE_BYTES + 1)
        .read_to_string(&mut value)
        .with_context(|| format!("read token file {}", path.display()))?;
    if value.len() as u64 > MAX_TOKEN_FILE_BYTES {
        bail!("token file {} is unexpectedly large", path.display());
    }
    let envelope: TokenFileEnvelope = serde_json::from_str(&value).with_context(|| {
        format!(
            "token file {} must be a versioned JSON envelope; legacy raw-token files are not accepted",
            path.display()
        )
    })?;
    if envelope.format_version != TOKEN_FILE_FORMAT_VERSION {
        bail!(
            "token file {} has unsupported format_version {}, expected {}",
            path.display(),
            envelope.format_version,
            TOKEN_FILE_FORMAT_VERSION
        );
    }
    validate_token(&envelope.token)?;
    Ok((BearerToken(envelope.token), envelope.generation))
}

pub fn ensure_private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(PRIVATE_DIR_MODE);
    builder
        .create(path)
        .with_context(|| format!("create runtime directory {}", path.display()))?;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let directory = options.open(path).with_context(|| {
        format!(
            "open runtime directory {} without following symlinks",
            path.display()
        )
    })?;
    let metadata = directory
        .metadata()
        .with_context(|| format!("inspect opened runtime directory {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("runtime path {} is not a directory", path.display());
    }
    require_current_owner(&metadata, path)?;
    let mode = metadata.mode() & 0o777;
    if mode != PRIVATE_DIR_MODE {
        bail!(
            "runtime directory {} must have mode 0700, found {mode:04o}",
            path.display()
        );
    }
    Ok(())
}

fn generate_token() -> BearerToken {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    BearerToken(hex::encode(bytes))
}

fn validate_token(token: &str) -> Result<()> {
    if token.len() != TOKEN_HEX_LEN {
        bail!("token must contain exactly 32 bytes encoded as hexadecimal");
    }
    let decoded = hex::decode(token).context("token is not hexadecimal")?;
    if decoded.len() != TOKEN_BYTES {
        bail!("token must contain exactly 32 bytes");
    }
    Ok(())
}

fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn write_token_file(path: &Path, generation: u64, token: &BearerToken) -> Result<()> {
    validate_token(token.expose())?;
    let parent = token_parent(path)?;
    ensure_private_directory(parent)?;
    validate_existing_token_target(path)?;
    let file_name = path
        .file_name()
        .context("token file must have a file name")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let encoded = serde_json::to_vec(&TokenFileEnvelope {
        format_version: TOKEN_FILE_FORMAT_VERSION,
        generation,
        token: token.expose().to_owned(),
    })?;

    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create token file {}", temporary.display()))?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("publish token file {}", path.display()))?;
        File::open(parent)?.sync_all()?;
        verify_private_file(path)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn token_parent(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("token file must have a parent directory")
}

fn validate_existing_token_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "refusing to replace non-regular token path {}",
                    path.display()
                );
            }
            require_current_owner(&metadata, path)?;
            let mode = metadata.mode() & 0o777;
            if mode != PRIVATE_FILE_MODE {
                bail!(
                    "refusing to replace token file {} with insecure mode {mode:04o}",
                    path.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect token path {}", path.display())),
    }
}

fn verify_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "published token path {} is not a regular file",
            path.display()
        );
    }
    require_current_owner(&metadata, path)?;
    let mode = metadata.mode() & 0o777;
    if mode != PRIVATE_FILE_MODE {
        bail!(
            "published token file {} has insecure mode {mode:04o}",
            path.display()
        );
    }
    Ok(())
}

fn require_current_owner(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    let effective_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != effective_uid {
        bail!(
            "path {} is owned by uid {}, expected effective uid {effective_uid}",
            path.display(),
            metadata.uid()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_file_is_private_and_authenticates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run").join("daemon.token");
        let state = AuthState::initialize(&path).unwrap();
        let token = read_token_file(&path).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        let envelope: TokenFileEnvelope =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(envelope.format_version, TOKEN_FILE_FORMAT_VERSION);
        assert_eq!(envelope.generation, 0);
        assert!(state.authorize(&format!("Bearer {}", token.expose())));
        assert!(!state.authorize(token.expose()));
        assert!(!state.authorize("Bearer wrong"));
    }

    #[test]
    fn insecure_token_file_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run").join("daemon.token");
        let state = AuthState::initialize(&path).unwrap();
        let token = read_token_file(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(read_token_file(&path).is_err());
        assert!(state.authorize(&format!("Bearer {}", token.expose())));
    }

    #[test]
    fn token_publication_refuses_a_symlink_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let run = directory.path().join("run");
        ensure_private_directory(&run).unwrap();
        let target = run.join("target");
        fs::write(&target, "unchanged").unwrap();
        let path = run.join("daemon.token");
        symlink(&target, &path).unwrap();

        assert!(AuthState::initialize(&path).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "unchanged");
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }

    #[test]
    fn existing_runtime_directory_must_already_be_private() {
        let directory = tempfile::tempdir().unwrap();
        let run = directory.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(ensure_private_directory(&run).is_err());
        assert_eq!(fs::metadata(run).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn rotation_invalidates_the_old_token_and_notifies_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run").join("daemon.token");
        let old = "00".repeat(TOKEN_BYTES);
        let state = AuthState::with_token(&path, &old).unwrap();
        let mut generation = state.subscribe_generation();

        assert!(state.authorize(&format!("Bearer {old}")));
        assert!(state.rotate_to(1).unwrap());
        generation.mark_changed();
        assert_eq!(*generation.borrow_and_update(), 1);
        assert!(!state.authorize(&format!("Bearer {old}")));
        let new = read_token_file(&path).unwrap();
        assert!(state.authorize(&format!("Bearer {}", new.expose())));
    }

    #[test]
    fn replayed_or_stale_generation_does_not_rotate_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run").join("daemon.token");
        let state = AuthState::initialize(&path).unwrap();

        assert!(state.rotate_to(7).unwrap());
        let at_seven = read_token_file(&path).unwrap();
        assert!(!state.rotate_to(7).unwrap());
        assert!(!state.rotate_to(6).unwrap());
        let after_replay = read_token_file(&path).unwrap();

        assert_eq!(state.generation(), 7);
        assert_eq!(at_seven.expose(), after_replay.expose());
    }

    #[test]
    fn restart_reuses_generation_and_old_replay_is_a_noop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run").join("daemon.token");
        let first = AuthState::initialize(&path).unwrap();
        assert!(first.rotate_to(7).unwrap());
        let before_restart = read_token_file(&path).unwrap();
        drop(first);

        let restarted = AuthState::initialize(&path).unwrap();
        let generation = restarted.subscribe_generation();
        assert_eq!(restarted.generation(), 7);
        assert!(!restarted.rotate_to(7).unwrap());
        assert!(!generation.has_changed().unwrap());
        let after_replay = read_token_file(&path).unwrap();
        assert_eq!(before_restart.expose(), after_replay.expose());
        assert!(restarted.authorize(&format!("Bearer {}", after_replay.expose())));
    }

    #[test]
    fn startup_reconciliation_repairs_a_lagging_file_and_rejects_an_ahead_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run").join("daemon.token");
        let state = AuthState::initialize(&path).unwrap();
        let old = read_token_file(&path).unwrap();

        assert!(state.reconcile_generation(4).unwrap());
        assert_eq!(state.generation(), 4);
        assert!(!state.authorize(&format!("Bearer {}", old.expose())));
        assert!(!state.reconcile_generation(4).unwrap());

        let error = state.reconcile_generation(3).unwrap_err();
        assert!(error.to_string().contains("ahead of durable core"));
        assert_eq!(state.generation(), 4);
    }

    #[test]
    fn legacy_raw_token_file_is_rejected_explicitly() {
        let directory = tempfile::tempdir().unwrap();
        let run = directory.path().join("run");
        ensure_private_directory(&run).unwrap();
        let path = run.join("daemon.token");
        let legacy = format!("{}\n", "00".repeat(TOKEN_BYTES));
        fs::write(&path, &legacy).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();

        let error = AuthState::initialize(&path).unwrap_err();
        assert!(error.to_string().contains("versioned JSON envelope"));
        assert_eq!(fs::read_to_string(path).unwrap(), legacy);
    }
}
