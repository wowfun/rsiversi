//! Bounded product launch inputs, separate from provider-owned Settings.

use crate::{ApplicationProfileId, HostProfileId, RsiError};
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

/// Authoritative document name inside the selected configuration home.
pub const LAUNCH_FILE: &str = "rsi.toml";
/// Maximum launch document size before parsing.
pub const MAXIMUM_LAUNCH_BYTES: usize = 1024 * 1024;

/// Interfaces implemented by this product, independent from optional package contents.
pub fn product_protocols() -> Vec<rsi_api_protocol::ProtocolDescriptor> {
    vec![
        rsi_api_protocol::connection_protocol(),
        rsi_api_device_api::protocol_descriptor(),
        rsi_acp_api::protocol_descriptor(),
        rsi_ai_models_api::protocol_descriptor(),
        rsi_automation_api::protocol_descriptor(),
        rsi_configuration_api::protocol_descriptor(),
        rsi_directory_picker_api::protocol_descriptor(),
        rsi_workspace_default_api::protocol_descriptor(),
        rsi_history_api::protocol_descriptor(),
        rsi_inspector::protocol_descriptor(),
        rsi_media_api::protocol_descriptor(),
        rsi_native_addons_api::protocol_descriptor(),
        rsi_navigation_api::protocol_descriptor(),
        rsi_process_output_api::protocol_descriptor(),
        rsi_session_api::protocol_descriptor(),
        rsi_session_files::protocol_descriptor(),
        rsi_settings_api::protocol_descriptor(),
        rsi_ui_api::protocol_descriptor(),
        rsi_workspace_api::protocol_descriptor(),
        rsi_workspace_review_api::protocol_descriptor(),
        #[cfg(target_os = "linux")]
        rsi_ssh_protocol::protocol_descriptor(),
    ]
}

/// Explicit final roots; absent entries retain ordinary platform defaults.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HomeOverrides {
    /// User configuration and credentials home.
    pub config: Option<PathBuf>,
    /// Durable product state home.
    pub state: Option<PathBuf>,
    /// Rebuildable cache home.
    pub cache: Option<PathBuf>,
}

impl HomeOverrides {
    /// Resolves lexical relative roots without creating directories or loading configuration.
    pub fn resolve(&self, defaults: &HostPaths, cwd: &Path) -> crate::Result<HostPaths> {
        if !cwd.is_absolute() {
            return Err(invalid("invoking directory must be absolute"));
        }
        let resolve = |selected: &Option<PathBuf>, fallback: &Path| {
            normalize_path(selected.as_deref().unwrap_or(fallback), cwd)
        };
        HostPaths::new(
            resolve(&self.config, defaults.config())?,
            resolve(&self.state, defaults.state())?,
            resolve(&self.cache, defaults.cache())?,
        )
        .map_err(invalid)
    }
}

/// A reference to the existing Profile language, never an inline plugin tree.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchProfile {
    /// Linked or user catalog identity.
    Catalog {
        /// Profile identity validated at document admission.
        id: String,
    },
    /// Explicit source file relative to its declaring document.
    File {
        /// Profile file; include resolution remains owned by `ProfileProgram`.
        path: PathBuf,
    },
    /// Profile exported by the selected installed package.
    Package {
        /// Package identity.
        package: String,
        /// Manifest export identity.
        export: String,
    },
}

impl Default for LaunchProfile {
    fn default() -> Self {
        Self::Catalog {
            id: "standard".into(),
        }
    }
}

/// One explicitly selected, trusted source build.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSource {
    /// Source root relative to the launch document.
    pub directory: PathBuf,
    /// Executable and its literal arguments; not a shell command string.
    pub command: Vec<String>,
    /// Package manifest produced relative to the source root.
    pub output: PathBuf,
    /// Source-relative inputs observed by a watch lease.
    pub inputs: Vec<PathBuf>,
    /// Optional supervised frontend, using this source directory and caller environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontend: Option<LaunchFrontend>,
}

/// Explicit frontend recipe; only a watch application activates it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchFrontend {
    /// Executable and literal arguments implementing the application control protocol.
    pub command: Vec<String>,
    /// Requested loopback port; zero lets the kernel select a free port.
    #[serde(default)]
    pub port: u16,
}

/// Product launch choices. Mutable user Settings are deliberately absent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchConfiguration {
    /// Launch document format.
    pub format: u32,
    /// Application selected by a bare `rsi`.
    pub default_application: String,
    /// Single shared Service Profile source.
    pub service: LaunchProfile,
    /// Additional named Application Profile references.
    pub applications: BTreeMap<String, LaunchProfile>,
    /// Explicit source recipes, inert without a build/watch request.
    pub sources: BTreeMap<String, LaunchSource>,
    /// Explicit repository trust anchor; absent uses the release's compiled identity.
    pub repository: Option<LaunchRepository>,
}

/// Operator-selected download authority, distinct from local package provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRepository {
    /// Stable authority used by rollback protection.
    pub name: String,
    /// Repository root URL; transport policy is checked by the downloader.
    pub url: String,
    /// Pinned 32-byte Ed25519 public key in lowercase hex.
    pub public_key: String,
}

impl Default for LaunchConfiguration {
    fn default() -> Self {
        Self {
            format: 1,
            default_application: "tui".into(),
            service: LaunchProfile::default(),
            applications: BTreeMap::new(),
            sources: BTreeMap::new(),
            repository: None,
        }
    }
}

impl LaunchConfiguration {
    /// Resolves the selected Service Profile using the owning catalog/file parser.
    pub fn service_profile(
        &self,
        catalog: &crate::ProfileCatalog,
    ) -> crate::Result<crate::HostProfileDocument> {
        match &self.service {
            LaunchProfile::Catalog { id } => catalog
                .host(&HostProfileId::new(id).map_err(invalid)?)
                .map_err(invalid),
            LaunchProfile::File { path } => {
                let path = normalize_path(path, catalog.paths().config())?;
                let contents = crate::profiles::read_regular_file(
                    &path,
                    crate::MAXIMUM_PROFILE_DOCUMENT_BYTES,
                )
                .map_err(invalid)?;
                Ok(crate::HostProfileDocument {
                    package_lease: None,
                    id: HostProfileId::new("launch").map_err(invalid)?,
                    source: crate::ProfileSource::User,
                    path: Some(path),
                    contents,
                })
            }
            LaunchProfile::Package { package, export } => {
                let (lease, path, contents) = package_profile(
                    catalog,
                    package,
                    export,
                    rsi_packages::ExportKind::ServiceProfile,
                )?;
                Ok(crate::HostProfileDocument {
                    id: HostProfileId::new("launch").map_err(invalid)?,
                    source: crate::ProfileSource::User,
                    path: Some(path),
                    contents,
                    package_lease: Some(lease),
                })
            }
        }
    }

    /// Resolves an Application reference without introducing a second Profile grammar.
    pub fn application_profile(
        &self,
        catalog: &crate::ProfileCatalog,
        id: &ApplicationProfileId,
    ) -> crate::Result<crate::ApplicationProfileDocument> {
        match self.applications.get(id.as_str()) {
            None => catalog.application(id).map_err(invalid),
            Some(LaunchProfile::Catalog { id }) => catalog
                .application(&ApplicationProfileId::new(id).map_err(invalid)?)
                .map_err(invalid),
            Some(LaunchProfile::File { path }) => {
                let path = normalize_path(path, catalog.paths().config())?;
                let contents = crate::profiles::read_regular_file(
                    &path,
                    crate::MAXIMUM_PROFILE_DOCUMENT_BYTES,
                )
                .map_err(invalid)?;
                Ok(crate::ApplicationProfileDocument {
                    package_lease: None,
                    id: id.clone(),
                    source: crate::ProfileSource::User,
                    path: Some(path),
                    contents,
                })
            }
            Some(LaunchProfile::Package { package, export }) => {
                let (lease, path, contents) = package_profile(
                    catalog,
                    package,
                    export,
                    rsi_packages::ExportKind::ApplicationProfile,
                )?;
                Ok(crate::ApplicationProfileDocument {
                    id: id.clone(),
                    source: crate::ProfileSource::User,
                    path: Some(path),
                    contents,
                    package_lease: Some(lease),
                })
            }
        }
    }

    /// Reads bounded bytes, using built-ins only when the default document is absent.
    pub fn load(home: &Path, explicit: bool) -> crate::Result<Self> {
        let path = home.join(LAUNCH_FILE);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit => {
                Ok(Self::default())
            }
            Err(error) => Err(invalid(format!(
                "cannot read {}: {error}; use `rsi --config-home {} config init`",
                path.display(),
                home.display()
            ))),
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                Err(invalid("launch document must be a regular file"))
            }
            Ok(_) => {
                let bytes = crate::profiles::read_regular_file(&path, MAXIMUM_LAUNCH_BYTES)
                    .map_err(invalid)?;
                Self::parse(&bytes)
            }
        }
    }

    /// Validates the launch envelope without activating or downloading any plugin.
    pub fn parse(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() > MAXIMUM_LAUNCH_BYTES {
            return Err(invalid("launch document exceeds 1 MiB"));
        }
        let configuration: Self = toml::from_slice(bytes).map_err(invalid)?;
        configuration.validate()?;
        Ok(configuration)
    }

    /// Validates bounded references independently from source availability.
    pub fn validate(&self) -> crate::Result<()> {
        if self.format != 1 {
            return Err(invalid("unsupported launch document format"));
        }
        ApplicationProfileId::new(&self.default_application).map_err(invalid)?;
        if let Some(repository) = &self.repository {
            ApplicationProfileId::new(&repository.name).map_err(invalid)?;
            if repository.url.is_empty()
                || repository.url.len() > 2048
                || repository.public_key.len() != 64
                || !repository
                    .public_key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid("invalid repository URL or public key"));
            }
        }
        if self.applications.len() > 256 || self.sources.len() > 64 {
            return Err(invalid("too many launch applications or sources"));
        }
        validate_reference(&self.service, true)?;
        for (id, source) in &self.applications {
            ApplicationProfileId::new(id).map_err(invalid)?;
            validate_reference(source, false)?;
        }
        for (id, source) in &self.sources {
            ApplicationProfileId::new(id).map_err(invalid)?;
            if let Some(frontend) = &source.frontend
                && (id != "web"
                    || frontend.command.is_empty()
                    || frontend.command.len() > 128
                    || frontend.command[0].is_empty()
                    || frontend
                        .command
                        .iter()
                        .any(|arg| arg.len() > 16 * 1024 || arg.contains('\0')))
            {
                return Err(invalid("invalid Web frontend recipe"));
            }
            if source.command.is_empty()
                || source.command.len() > 128
                || source
                    .command
                    .iter()
                    .any(|arg| arg.len() > 16 * 1024 || arg.contains('\0'))
                || source.command[0].is_empty()
                || source.inputs.is_empty()
                || source.inputs.len() > 256
                || source.directory.as_os_str().is_empty()
            {
                return Err(invalid("invalid or excessive source build declaration"));
            }
            rsi_packages::validate_identifier(id).map_err(invalid)?;
            crate::source_inputs::layout(&source.output, &source.inputs).map_err(invalid)?;
        }
        Ok(())
    }

    /// Creates a default document through a private directory handle; never overwrites.
    #[cfg(any(unix, windows))]
    pub fn initialize(home: &Path) -> crate::Result<PathBuf> {
        let directory =
            rsi_files_native_fs::create_absolute_directory_no_follow(home).map_err(invalid)?;
        let mut file = rsi_files_native_fs::create_relative_private_file_no_follow(
            &directory,
            Path::new(LAUNCH_FILE),
        )
        .map_err(invalid)?;
        let bytes = toml::to_string_pretty(&Self::default()).map_err(invalid)?;
        file.write_all(bytes.as_bytes()).map_err(invalid)?;
        file.sync_all().map_err(invalid)?;
        directory.sync_all().map_err(invalid)?;
        Ok(home.join(LAUNCH_FILE))
    }
}

fn package_profile(
    catalog: &crate::ProfileCatalog,
    package: &str,
    export: &str,
    kind: rsi_packages::ExportKind,
) -> crate::Result<(std::sync::Arc<rsi_packages::PackageLease>, PathBuf, Vec<u8>)> {
    let store = rsi_packages::PackageStore::open(&catalog.paths().state().join("packages"))
        .map_err(invalid)?;
    let lease = std::sync::Arc::new(store.acquire(package).map_err(invalid)?);
    lease
        .manifest()
        .admit_interfaces(&catalog.protocols())
        .map_err(invalid)?;
    let path = lease.export(export, kind).map_err(invalid)?;
    let contents = crate::profiles::read_regular_file(&path, crate::MAXIMUM_PROFILE_DOCUMENT_BYTES)
        .map_err(invalid)?;
    Ok((lease, path, contents))
}

fn validate_reference(reference: &LaunchProfile, host: bool) -> crate::Result<()> {
    match reference {
        LaunchProfile::Catalog { id } => {
            if host {
                HostProfileId::new(id).map_err(invalid)?;
            } else {
                ApplicationProfileId::new(id).map_err(invalid)?;
            }
        }
        LaunchProfile::File { path } => {
            if path.as_os_str().is_empty() {
                return Err(invalid("Profile file path is empty"));
            }
        }
        LaunchProfile::Package { package, export } => {
            ApplicationProfileId::new(package).map_err(invalid)?;
            ApplicationProfileId::new(export).map_err(invalid)?;
        }
    }
    Ok(())
}

/// Lexically resolves a path; filesystem authorities validate and canonicalize at admission.
pub fn normalize_launch_path(path: &Path, cwd: &Path) -> crate::Result<PathBuf> {
    normalize_path(path, cwd)
}

fn normalize_path(path: &Path, cwd: &Path) -> crate::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid("home path is empty"));
    }
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(invalid("path escapes its filesystem root"));
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    if !normalized.is_absolute() {
        return Err(invalid("home path is not absolute"));
    }
    Ok(normalized)
}

fn invalid(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("launch configuration: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_configuration_rejects_invalid_runtime_layouts() {
        for (output, input) in [
            ("out/package.json", "./src"),
            ("package.json", "src"),
            ("out/manifest.json", "src"),
            ("out/package.json", "out"),
            ("out/package.json", "out/input"),
        ] {
            let source = format!(
                "[sources.web]\ndirectory='.'\ncommand=['node','build.mjs']\noutput='{output}'\ninputs=['{input}']"
            );
            assert!(
                LaunchConfiguration::parse(source.as_bytes()).is_err(),
                "{source}"
            );
        }
        assert!(LaunchConfiguration::parse(b"[sources.web]\ndirectory='.'\ncommand=['node','build.mjs']\noutput='out/package.json'\ninputs=['src']").is_ok());
    }
    #[test]
    fn installed_profile_resolves_named_role_and_retains_its_publication() {
        use rsi_packages::{
            Export, ExportKind, FileRecord, InstallSource, Manifest, PackageStore, Provenance,
        };
        use sha2::{Digest as _, Sha256};
        let root = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
            std::env::temp_dir().canonicalize().unwrap(),
        ))
        .unwrap();
        let paths = HostPaths::new(
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("cache"),
        )
        .unwrap();
        let input = root.path().join("input");
        std::fs::create_dir(&input).unwrap();
        let bytes = b"format = 1\n";
        std::fs::write(input.join("main.toml"), bytes).unwrap();
        let protocol = rsi_api_protocol::ProtocolDescriptor {
            name: "fixture.application".into(),
            version: 1,
        };
        let manifest = Manifest {
            format: 2,
            name: "example".into(),
            version: "1".into(),
            target: "portable".into(),
            dependencies: vec![],
            requires: vec![protocol.clone()],
            files: vec![FileRecord {
                path: "main.toml".into(),
                bytes: bytes.len() as u64,
                digest: hex::encode(Sha256::digest(bytes)),
                executable: false,
            }],
            exports: BTreeMap::from([(
                "main".into(),
                Export {
                    kind: ExportKind::ApplicationProfile,
                    path: "main.toml".into(),
                },
            )]),
        };
        PackageStore::open(&paths.state().join("packages"))
            .unwrap()
            .install(
                &[InstallSource {
                    directory: input,
                    manifest: serde_json::to_vec(&manifest).unwrap(),
                    provenance: Provenance::Local,
                }],
                &[],
                std::slice::from_ref(&protocol),
                "portable",
            )
            .unwrap();
        let metadata = crate::ApplicationCatalogMetadata::default();
        let base_digest = metadata.digest();
        let metadata = metadata.with_protocols(vec![protocol]).unwrap();
        assert_eq!(metadata.digest(), base_digest);
        let catalog = crate::ProfileCatalog::new(paths.clone(), metadata);
        let mut configuration = LaunchConfiguration::default();
        configuration.applications.insert(
            "example".into(),
            LaunchProfile::Package {
                package: "example".into(),
                export: "main".into(),
            },
        );
        assert!(
            configuration
                .application_profile(
                    &crate::ProfileCatalog::new(
                        paths,
                        crate::ApplicationCatalogMetadata::default()
                    ),
                    &ApplicationProfileId::new("example").unwrap(),
                )
                .is_err()
        );
        let document = configuration
            .application_profile(&catalog, &ApplicationProfileId::new("example").unwrap())
            .unwrap();
        assert_eq!(document.contents, bytes);
        let observed = std::sync::Arc::downgrade(document.package_lease.as_ref().unwrap());
        let program = document.program().unwrap();
        drop(document);
        assert!(observed.upgrade().is_some());
        drop(program);
        assert!(observed.upgrade().is_none());
        configuration.service = configuration.applications["example"].clone();
        assert!(configuration.service_profile(&catalog).is_err());
    }

    #[test]
    fn home_overrides_are_final_and_independent() {
        let root = std::env::temp_dir();
        let defaults =
            HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap();
        let resolved = HomeOverrides {
            config: Some("dev/../configuration".into()),
            ..Default::default()
        }
        .resolve(&defaults, &root)
        .unwrap();
        assert_eq!(resolved.config(), root.join("configuration"));
        assert_eq!(resolved.state(), defaults.state());
        assert_eq!(resolved.cache(), defaults.cache());
        assert!(!resolved.config().ends_with("rsi"));
    }

    #[test]
    fn document_cannot_become_a_settings_or_inline_profile_backend() {
        assert!(LaunchConfiguration::parse(b"format = 1\n[settings]\nkey=1").is_err());
        assert!(
            LaunchConfiguration::parse(
                b"format = 1\n[service]\nkind='file'\npath='host.toml'\nsteps=[]"
            )
            .is_err()
        );
        assert!(LaunchConfiguration::parse(b"format = 2").is_err());
        assert!(LaunchConfiguration::parse(&vec![b' '; MAXIMUM_LAUNCH_BYTES + 1]).is_err());
        let parsed = LaunchConfiguration::parse(
            b"default_application='web'\n[service]\nkind='file'\npath='profiles/main.toml'",
        )
        .unwrap();
        assert_eq!(parsed.default_application, "web");
    }

    #[test]
    fn source_outputs_and_inputs_cannot_escape_the_declared_source() {
        for output in ["../package.toml", "/package.toml"] {
            let document = format!(
                "[sources.web]\ndirectory='.'\ncommand=['node','build.mjs']\noutput='{output}'\ninputs=['src']"
            );
            assert!(LaunchConfiguration::parse(document.as_bytes()).is_err());
        }
        assert!(LaunchConfiguration::parse(b"[sources.web]\ndirectory='.'\ncommand=[]\noutput='dist/package.toml'\ninputs=['src']").is_err());
    }

    #[test]
    fn missing_explicit_home_does_not_silently_use_daily_configuration() {
        let root = tempfile::tempdir_in(rsi_workspace_protocol::canonical_native_path(
            std::env::temp_dir().canonicalize().unwrap(),
        ))
        .unwrap();
        assert_eq!(
            LaunchConfiguration::load(root.path(), false).unwrap(),
            LaunchConfiguration::default()
        );
        assert!(LaunchConfiguration::load(root.path(), true).is_err());
        let home = root.path().join("config");
        let path = LaunchConfiguration::initialize(&home).unwrap();
        assert!(path.is_file());
        assert_eq!(
            LaunchConfiguration::load(&home, true).unwrap(),
            LaunchConfiguration::default()
        );
        assert!(LaunchConfiguration::initialize(&home).is_err());
    }
}
