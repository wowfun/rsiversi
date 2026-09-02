use rsi_host::HostPaths;
use rsi_session_host::SESSION_HOST_PROTOCOL_EPOCH;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

/// Directory containing user-authored Application Profiles.
pub const APPLICATION_PROFILE_DIRECTORY: &str = "application-profiles";
/// File name owned by one Application Profile directory.
pub const APPLICATION_PROFILE_FILE: &str = "application.toml";
/// Directory containing user-authored Host Profiles.
pub const HOST_PROFILE_DIRECTORY: &str = "host-profiles";
/// File name owned by one Host Profile directory.
pub const HOST_PROFILE_FILE: &str = "host.profile.toml";

const PROFILE_FORMAT: u32 = 1;
const MAXIMUM_PROFILE_BYTES: usize = 1024 * 1024;
const MAXIMUM_PROFILE_ENTRIES: usize = 4096;
const SESSION_PROFILE: &str = "session";
const HEADLESS_PROFILE: &str = "headless";
const STANDARD_HOST_PROFILE: &str = "standard";
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

macro_rules! profile_id {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Validated ", $kind, " identifier.")]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Validates one ", $kind, " identifier.")]
            pub fn new(value: impl Into<String>) -> Result<Self, ProfileCatalogError> {
                let value = value.into();
                validate_id($kind, &value)?;
                Ok(Self(value))
            }

            /// Returns the validated wire value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

profile_id!(ApplicationProfileId, "Application Profile");
profile_id!(HostProfileId, "Host Profile");

/// Product application selected by an Application Profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplicationKind {
    /// Interactive line-oriented Session application.
    Session,
    /// Single-submission non-interactive application.
    Headless,
}

/// Exact `application.toml` schema.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProfile {
    format: u32,
    application: ApplicationKind,
    host_profile: HostProfileId,
}

impl ApplicationProfile {
    /// Creates the current schema after validating the Host Profile identity.
    pub fn new(application: ApplicationKind, host_profile: HostProfileId) -> Self {
        Self {
            format: PROFILE_FORMAT,
            application,
            host_profile,
        }
    }

    /// Returns the selected application.
    pub const fn application(&self) -> ApplicationKind {
        self.application
    }

    /// Returns the selected Host Profile identity.
    pub const fn host_profile(&self) -> &HostProfileId {
        &self.host_profile
    }

    fn validate(&self) -> Result<(), ProfileCatalogError> {
        if self.format != PROFILE_FORMAT {
            return Err(ProfileCatalogError::UnsupportedFormat {
                kind: "Application Profile",
                observed: self.format,
            });
        }
        Ok(())
    }
}

/// Origin of one selected profile document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileSource {
    /// Immutable product-owned definition.
    Builtin,
    /// User-owned file under the standard configuration root.
    User,
}

/// Loaded Application Profile with resolved provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationProfileDocument {
    /// Selected identity.
    pub id: ApplicationProfileId,
    /// Parsed strict document.
    pub profile: ApplicationProfile,
    /// Resolved source class.
    pub source: ProfileSource,
    /// Exact source path for a user document.
    pub path: Option<PathBuf>,
}

/// Loaded Host Profile source with resolved provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProfileDocument {
    /// Selected identity.
    pub id: HostProfileId,
    /// Resolved source class.
    pub source: ProfileSource,
    /// Exact source path for a user document.
    pub path: Option<PathBuf>,
    /// Complete bounded Profile source bytes.
    pub contents: Vec<u8>,
}

/// Canonical identity of one standard Host process generation.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HostLaunchKey(String);

impl HostLaunchKey {
    /// Builds the product launch identity from already frozen, non-secret inputs.
    pub(crate) fn from_components(
        host_composition_digest: &str,
        host_profile: &HostProfileDocument,
        agent_presets: &rsi_agent_presets::AgentPresetLaunchIdentity,
        coding_tools: Option<CodingToolsLaunchIdentity<'_>>,
    ) -> Result<Self, ProfileCatalogError> {
        let mut digest = Sha256::new();
        hash_component(&mut digest, b"domain", b"rsi.session-host.launch-key.v1");
        hash_component(
            &mut digest,
            b"protocol-epoch",
            &SESSION_HOST_PROTOCOL_EPOCH.to_be_bytes(),
        );
        hash_component(
            &mut digest,
            b"product-build",
            env!("CARGO_PKG_VERSION").as_bytes(),
        );
        hash_component(&mut digest, b"composition-epoch", &1_u32.to_be_bytes());
        hash_component(
            &mut digest,
            b"host-composition",
            host_composition_digest.as_bytes(),
        );
        hash_component(
            &mut digest,
            b"host-profile-id",
            host_profile.id.as_str().as_bytes(),
        );
        match &host_profile.path {
            Some(path) => {
                let profile_directory =
                    path.parent()
                        .ok_or_else(|| ProfileCatalogError::InvalidDocument {
                            path: path.clone(),
                            message: "Host Profile source path has no catalog authority root"
                                .into(),
                        })?;
                let root = profile_directory.parent().ok_or_else(|| {
                    ProfileCatalogError::InvalidDocument {
                        path: path.clone(),
                        message: "Host Profile source path has no catalog authority root".into(),
                    }
                })?;
                if !path.is_absolute()
                    || path.file_name() != Some(OsStr::new(HOST_PROFILE_FILE))
                    || profile_directory.file_name() != Some(OsStr::new(host_profile.id.as_str()))
                    || root.file_name() != Some(OsStr::new(HOST_PROFILE_DIRECTORY))
                {
                    return Err(ProfileCatalogError::InvalidDocument {
                        path: path.clone(),
                        message: format!(
                            "Host Profile source path must end in {HOST_PROFILE_DIRECTORY}/{}/{HOST_PROFILE_FILE}",
                            host_profile.id.as_str()
                        ),
                    });
                }
                hash_path(&mut digest, b"host-profile-root", root);
            }
            None => hash_component(&mut digest, b"host-profile-root", b"builtin"),
        }
        hash_component(
            &mut digest,
            b"agent-preset-base-default",
            agent_presets.base_default.as_str().as_bytes(),
        );
        for root in &agent_presets.roots {
            hash_path(&mut digest, b"agent-preset-root", &root.path);
            hash_component(
                &mut digest,
                b"agent-preset-exact-id",
                root.exact_id
                    .as_ref()
                    .map_or(b"".as_slice(), |id| id.as_str().as_bytes()),
            );
            hash_component(
                &mut digest,
                b"agent-preset-source",
                match root.source {
                    rsi_agent_presets::AgentPresetSource::System => b"system",
                    rsi_agent_presets::AgentPresetSource::Configured => b"configured",
                    rsi_agent_presets::AgentPresetSource::User => b"user",
                },
            );
            hash_component(
                &mut digest,
                b"agent-preset-trust",
                match root.trust {
                    rsi_agent_presets::AgentPresetTrust::System => b"system",
                    rsi_agent_presets::AgentPresetTrust::User => b"user",
                },
            );
            hash_component(
                &mut digest,
                b"agent-preset-writable",
                &[u8::from(root.writable)],
            );
        }
        if let Some(coding) = coding_tools {
            hash_component(&mut digest, b"coding-tools", b"enabled");
            hash_path(&mut digest, b"coding-bash", coding.bash);
            hash_path(&mut digest, b"coding-apply-patch", coding.apply_patch);
        } else {
            hash_component(&mut digest, b"coding-tools", b"disabled");
        }
        Ok(Self(hex::encode(digest.finalize())))
    }

    /// Returns the lower-case SHA-256 wire value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostLaunchKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(crate) struct CodingToolsLaunchIdentity<'a> {
    pub(crate) bash: &'a Path,
    pub(crate) apply_patch: &'a Path,
}

/// One deterministic catalog listing row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileRow<I> {
    /// Validated profile identity.
    pub id: I,
    /// Effective source class.
    pub source: ProfileSource,
}

/// Product-owned profile catalogs rooted in one frozen Host path set.
#[derive(Clone, Debug)]
pub struct ProfileCatalog {
    paths: HostPaths,
}

impl ProfileCatalog {
    /// Creates a pure catalog view without creating directories or reading files.
    pub const fn new(paths: HostPaths) -> Self {
        Self { paths }
    }

    /// Returns the frozen Host paths used by all catalog operations.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Returns the exact user path for one Application Profile.
    pub fn application_path(&self, id: &ApplicationProfileId) -> PathBuf {
        self.paths
            .config()
            .join(APPLICATION_PROFILE_DIRECTORY)
            .join(id.as_str())
            .join(APPLICATION_PROFILE_FILE)
    }

    /// Returns the exact user path for one Host Profile.
    pub fn host_path(&self, id: &HostProfileId) -> PathBuf {
        self.paths
            .config()
            .join(HOST_PROFILE_DIRECTORY)
            .join(id.as_str())
            .join(HOST_PROFILE_FILE)
    }

    /// Loads one effective Application Profile and rejects attempts to shadow built-ins.
    pub fn application(
        &self,
        id: &ApplicationProfileId,
    ) -> Result<ApplicationProfileDocument, ProfileCatalogError> {
        let path = self.application_path(id);
        if let Some(profile) = builtin_application(id) {
            reject_shadow("Application Profile", id.as_str(), &path)?;
            return Ok(ApplicationProfileDocument {
                id: id.clone(),
                profile,
                source: ProfileSource::Builtin,
                path: None,
            });
        }
        let bytes = read_regular_file(&path, MAXIMUM_PROFILE_BYTES)?;
        let profile: ApplicationProfile =
            toml::from_slice(&bytes).map_err(|error| ProfileCatalogError::InvalidDocument {
                path: path.clone(),
                message: error.to_string(),
            })?;
        profile.validate()?;
        Ok(ApplicationProfileDocument {
            id: id.clone(),
            profile,
            source: ProfileSource::User,
            path: Some(path),
        })
    }

    /// Loads one effective Host Profile and rejects attempts to shadow the built-in.
    pub fn host(&self, id: &HostProfileId) -> Result<HostProfileDocument, ProfileCatalogError> {
        let path = self.host_path(id);
        if id.as_str() == STANDARD_HOST_PROFILE {
            reject_shadow("Host Profile", id.as_str(), &path)?;
            return Ok(HostProfileDocument {
                id: id.clone(),
                source: ProfileSource::Builtin,
                path: None,
                contents: b"format = 1\n".to_vec(),
            });
        }
        let contents = read_regular_file(&path, MAXIMUM_PROFILE_BYTES)?;
        Ok(HostProfileDocument {
            id: id.clone(),
            source: ProfileSource::User,
            path: Some(path),
            contents,
        })
    }

    /// Lists built-in and user Application Profiles in identity order.
    pub fn list_applications(
        &self,
    ) -> Result<Vec<ProfileRow<ApplicationProfileId>>, ProfileCatalogError> {
        let builtins = [SESSION_PROFILE, HEADLESS_PROFILE];
        let mut ids = list_user_ids::<ApplicationProfileId>(
            &self.paths.config().join(APPLICATION_PROFILE_DIRECTORY),
            APPLICATION_PROFILE_FILE,
        )?;
        for builtin in builtins {
            let id = ApplicationProfileId::new(builtin)?;
            reject_shadow("Application Profile", builtin, &self.application_path(&id))?;
            ids.insert(id);
        }
        Ok(ids
            .into_iter()
            .map(|id| ProfileRow {
                source: if builtin_application(&id).is_some() {
                    ProfileSource::Builtin
                } else {
                    ProfileSource::User
                },
                id,
            })
            .collect())
    }

    /// Lists built-in and user Host Profiles in identity order.
    pub fn list_hosts(&self) -> Result<Vec<ProfileRow<HostProfileId>>, ProfileCatalogError> {
        let mut ids = list_user_ids::<HostProfileId>(
            &self.paths.config().join(HOST_PROFILE_DIRECTORY),
            HOST_PROFILE_FILE,
        )?;
        let standard = HostProfileId::new(STANDARD_HOST_PROFILE)?;
        reject_shadow(
            "Host Profile",
            STANDARD_HOST_PROFILE,
            &self.host_path(&standard),
        )?;
        ids.insert(standard);
        Ok(ids
            .into_iter()
            .map(|id| ProfileRow {
                source: if id.as_str() == STANDARD_HOST_PROFILE {
                    ProfileSource::Builtin
                } else {
                    ProfileSource::User
                },
                id,
            })
            .collect())
    }

    /// Copies an effective Application Profile into one new user identity.
    pub fn copy_application(
        &self,
        source: &ApplicationProfileId,
        target: &ApplicationProfileId,
    ) -> Result<PathBuf, ProfileCatalogError> {
        reject_builtin_target(
            "Application Profile",
            target.as_str(),
            builtin_application(target).is_some(),
        )?;
        let source = self.application(source)?;
        let bytes = toml::to_string_pretty(&source.profile)
            .map_err(|error| ProfileCatalogError::Encode(error.to_string()))?;
        let path = self.application_path(target);
        create_new_document(&path, bytes.as_bytes())?;
        Ok(path)
    }

    /// Copies an effective Host Profile into one new user identity.
    pub fn copy_host(
        &self,
        source: &HostProfileId,
        target: &HostProfileId,
    ) -> Result<PathBuf, ProfileCatalogError> {
        reject_builtin_target(
            "Host Profile",
            target.as_str(),
            target.as_str() == STANDARD_HOST_PROFILE,
        )?;
        let source = self.host(source)?;
        let path = self.host_path(target);
        create_new_document(&path, &source.contents)?;
        Ok(path)
    }

    /// Deletes one user Application Profile directory.
    pub fn delete_application(&self, id: &ApplicationProfileId) -> Result<(), ProfileCatalogError> {
        reject_builtin_target(
            "Application Profile",
            id.as_str(),
            builtin_application(id).is_some(),
        )?;
        delete_document_directory(&self.application_path(id))
    }

    /// Deletes one user Host Profile directory.
    pub fn delete_host(&self, id: &HostProfileId) -> Result<(), ProfileCatalogError> {
        reject_builtin_target(
            "Host Profile",
            id.as_str(),
            id.as_str() == STANDARD_HOST_PROFILE,
        )?;
        delete_document_directory(&self.host_path(id))
    }
}

fn builtin_application(id: &ApplicationProfileId) -> Option<ApplicationProfile> {
    let standard = HostProfileId(STANDARD_HOST_PROFILE.to_owned());
    match id.as_str() {
        SESSION_PROFILE => Some(ApplicationProfile::new(ApplicationKind::Session, standard)),
        HEADLESS_PROFILE => Some(ApplicationProfile::new(ApplicationKind::Headless, standard)),
        _ => None,
    }
}

trait CatalogId: Ord + Sized {
    fn parse(value: String) -> Result<Self, ProfileCatalogError>;
}

impl CatalogId for ApplicationProfileId {
    fn parse(value: String) -> Result<Self, ProfileCatalogError> {
        Self::new(value)
    }
}

impl CatalogId for HostProfileId {
    fn parse(value: String) -> Result<Self, ProfileCatalogError> {
        Self::new(value)
    }
}

fn list_user_ids<I: CatalogId>(
    root: &Path,
    file_name: &str,
) -> Result<BTreeSet<I>, ProfileCatalogError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(io("list", root, error)),
    };
    let mut ids = BTreeSet::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAXIMUM_PROFILE_ENTRIES {
            return Err(ProfileCatalogError::CapacityExceeded {
                maximum: MAXIMUM_PROFILE_ENTRIES,
            });
        }
        let entry = entry.map_err(|error| io("list", root, error))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| io("inspect", &entry.path(), error))?;
        if !metadata.file_type().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(ProfileCatalogError::InvalidId {
                kind: "profile",
                value: "non-UTF-8".to_owned(),
            });
        };
        let id = I::parse(name)?;
        let document = entry.path().join(file_name);
        match fs::symlink_metadata(&document) {
            Ok(metadata) if metadata.file_type().is_file() => {
                ids.insert(id);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io("inspect", &document, error)),
        }
    }
    Ok(ids)
}

fn validate_id(kind: &'static str, value: &str) -> Result<(), ProfileCatalogError> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0].is_ascii_digit());
    let grammar = value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid && grammar {
        Ok(())
    } else {
        Err(ProfileCatalogError::InvalidId {
            kind,
            value: value.to_owned(),
        })
    }
}

fn reject_shadow(kind: &'static str, id: &str, path: &Path) -> Result<(), ProfileCatalogError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(ProfileCatalogError::BuiltinShadowed {
            kind,
            id: id.to_owned(),
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io("inspect", path, error)),
    }
}

fn reject_builtin_target(
    kind: &'static str,
    id: &str,
    builtin: bool,
) -> Result<(), ProfileCatalogError> {
    if builtin {
        Err(ProfileCatalogError::BuiltinImmutable {
            kind,
            id: id.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn read_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>, ProfileCatalogError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| io("open", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| io("inspect", path, error))?;
    if !metadata.is_file() {
        return Err(ProfileCatalogError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > maximum as u64 {
        return Err(ProfileCatalogError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io("read", path, error))?;
    if bytes.len() > maximum {
        return Err(ProfileCatalogError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    Ok(bytes)
}

fn create_new_document(path: &Path, bytes: &[u8]) -> Result<(), ProfileCatalogError> {
    if bytes.len() > MAXIMUM_PROFILE_BYTES {
        return Err(ProfileCatalogError::DocumentTooLarge {
            path: path.to_path_buf(),
            maximum: MAXIMUM_PROFILE_BYTES,
        });
    }
    let directory = path.parent().expect("catalog document has a parent");
    let root = directory.parent().expect("profile directory has a root");
    create_private_directories(root)?;
    match fs::symlink_metadata(directory) {
        Ok(_) => return Err(ProfileCatalogError::AlreadyExists(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io("inspect", directory, error)),
    }
    create_private_directory(directory)?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{sequence}.tmp"));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| io("create", &temporary, error))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| io("write", &temporary, error))?;
        fs::rename(&temporary, path).map_err(|error| io("install", path, error))?;
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| io("sync", directory, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_dir(directory);
    }
    result
}

fn create_private_directories(path: &Path) -> Result<(), ProfileCatalogError> {
    fs::create_dir_all(path).map_err(|error| io("create", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io("chmod", path, error))?;
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), ProfileCatalogError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| io("create", path, error))
}

fn delete_document_directory(path: &Path) -> Result<(), ProfileCatalogError> {
    let directory = path.parent().expect("catalog document has a parent");
    let directory_metadata =
        fs::symlink_metadata(directory).map_err(|error| io("inspect", directory, error))?;
    if !directory_metadata.file_type().is_dir() || directory_metadata.file_type().is_symlink() {
        return Err(ProfileCatalogError::NotRegularDirectory(
            directory.to_path_buf(),
        ));
    }
    let file_metadata = fs::symlink_metadata(path).map_err(|error| io("inspect", path, error))?;
    if !file_metadata.file_type().is_file() || file_metadata.file_type().is_symlink() {
        return Err(ProfileCatalogError::NotRegularFile(path.to_path_buf()));
    }
    let mut entries = fs::read_dir(directory).map_err(|error| io("list", directory, error))?;
    let first = entries
        .next()
        .transpose()
        .map_err(|error| io("list", directory, error))?;
    let second = entries
        .next()
        .transpose()
        .map_err(|error| io("list", directory, error))?;
    if first.as_ref().is_none_or(|entry| entry.path() != path) || second.is_some() {
        return Err(ProfileCatalogError::DirectoryNotEmpty(
            directory.to_path_buf(),
        ));
    }
    fs::remove_file(path).map_err(|error| io("delete", path, error))?;
    fs::remove_dir(directory).map_err(|error| io("delete", directory, error))
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> ProfileCatalogError {
    ProfileCatalogError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn hash_component(digest: &mut Sha256, name: &[u8], value: &[u8]) {
    digest.update(
        u64::try_from(name.len())
            .expect("field name fits u64")
            .to_be_bytes(),
    );
    digest.update(name);
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

fn hash_path(digest: &mut Sha256, name: &[u8], path: &Path) {
    hash_os(digest, name, path.as_os_str());
}

#[cfg(unix)]
fn hash_os(digest: &mut Sha256, name: &[u8], value: &std::ffi::OsStr) {
    use std::os::unix::ffi::OsStrExt as _;
    hash_component(digest, name, value.as_bytes());
}

#[cfg(windows)]
fn hash_os(digest: &mut Sha256, name: &[u8], value: &std::ffi::OsStr) {
    use std::os::windows::ffi::OsStrExt as _;
    let bytes = value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    hash_component(digest, name, &bytes);
}

#[cfg(not(any(unix, windows)))]
fn hash_os(digest: &mut Sha256, name: &[u8], value: &std::ffi::OsStr) {
    hash_component(digest, name, value.to_string_lossy().as_bytes());
}

/// Closed failure surface for profile discovery and management.
#[derive(Debug, Error)]
pub enum ProfileCatalogError {
    /// An identity violates the stable catalog grammar.
    #[error("{kind} id `{value}` must match [a-z0-9][a-z0-9-]* and contain at most 255 bytes")]
    InvalidId {
        /// Profile class.
        kind: &'static str,
        /// Rejected value.
        value: String,
    },
    /// A document uses an unsupported schema generation.
    #[error("{kind} format {observed} is unsupported; expected {PROFILE_FORMAT}")]
    UnsupportedFormat {
        /// Profile class.
        kind: &'static str,
        /// Rejected generation.
        observed: u32,
    },
    /// A user path attempts to replace a built-in identity.
    #[error("built-in {kind} `{id}` cannot be shadowed by {}", path.display())]
    BuiltinShadowed {
        /// Profile class.
        kind: &'static str,
        /// Built-in identity.
        id: String,
        /// Conflicting user path.
        path: PathBuf,
    },
    /// A management mutation targets a built-in identity.
    #[error("built-in {kind} `{id}` is immutable")]
    BuiltinImmutable {
        /// Profile class.
        kind: &'static str,
        /// Built-in identity.
        id: String,
    },
    /// Strict TOML decoding failed.
    #[error("invalid profile document {}: {message}", path.display())]
    InvalidDocument {
        /// Document path.
        path: PathBuf,
        /// Decoder detail.
        message: String,
    },
    /// TOML encoding failed.
    #[error("profile encoding failed: {0}")]
    Encode(String),
    /// A bounded document exceeded its ceiling.
    #[error("profile document {} exceeds {maximum} bytes", path.display())]
    DocumentTooLarge {
        /// Document path.
        path: PathBuf,
        /// Byte ceiling.
        maximum: usize,
    },
    /// A selected document is not one regular file.
    #[error("profile document {} is not a regular file", .0.display())]
    NotRegularFile(PathBuf),
    /// A selected profile directory is not one regular directory.
    #[error("profile directory {} is not a regular directory", .0.display())]
    NotRegularDirectory(PathBuf),
    /// A target already exists.
    #[error("profile target {} already exists", .0.display())]
    AlreadyExists(PathBuf),
    /// Delete refuses to remove unexpected sibling data.
    #[error("profile directory {} contains files not owned by the catalog", .0.display())]
    DirectoryNotEmpty(PathBuf),
    /// A listing exceeded its deterministic work bound.
    #[error("profile catalog exceeds {maximum} entries")]
    CapacityExceeded {
        /// Entry ceiling.
        maximum: usize,
    },
    /// Filesystem operation failed.
    #[error("failed to {operation} {}: {source}", path.display())]
    Io {
        /// Stable operation name.
        operation: &'static str,
        /// Operation target.
        path: PathBuf,
        /// Operating-system error.
        #[source]
        source: std::io::Error,
    },
}
