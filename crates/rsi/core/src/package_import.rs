use rsi_host::{HostBuilder, HostPaths, Profile, ProfileEntry};
use rsi_meta::UpdateMode;
use rsi_packages::{InstallSource, Manifest, PackageStore, Provenance, Resolution};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// A caller's explicit importer and workspace authority.
#[derive(Debug)]
pub struct PackageImport {
    /// Installed importer package.
    pub package: String,
    /// Exact importer for a configured profile; CLI invocations use the selected generation.
    pub digest: Option<String>,
    /// Named importer export.
    pub export: String,
    /// Independently resolved executable.
    pub program: PathBuf,
    /// Invoking workspace used to resolve explicit relative sources.
    pub workspace: PathBuf,
    /// Ordered package specifications.
    pub specifications: Vec<String>,
    /// Optional configuration interpreted by the selected importer.
    pub options: Option<serde_json::Value>,
}
impl PackageImport {
    fn validate(&self) -> crate::Result<()> {
        if let Some(options) = &self.options
            && (!options.is_object()
                || serde_json::to_vec(options).map_err(error)?.len() > 64 * 1024)
        {
            return Err(error("invalid importer options"));
        }
        rsi_packages::validate_identifier(&self.package).map_err(error)?;
        rsi_packages::validate_identifier(&self.export).map_err(error)?;
        if !self.workspace.is_absolute()
            || !self.program.is_absolute()
            || self.specifications.is_empty()
            || self.specifications.len() > 128
            || self
                .specifications
                .iter()
                .any(|s| s.is_empty() || s.contains('\0'))
            || self.specifications.iter().map(String::len).sum::<usize>() > 64 * 1024
        {
            return Err(error("invalid package import selection"));
        }
        Ok(())
    }
}
/// Published resolution and names produced by this importer operation.
#[derive(Debug)]
pub struct PackageAdmission {
    /// Complete atomic package resolution.
    pub resolution: Resolution,
    /// Admitted packages from the importer, in staging order.
    pub names: Vec<String>,
}
fn error(value: impl std::fmt::Display) -> crate::RsiError {
    crate::RsiError::Boot(format!("plugin: {value}"))
}
/// Reads the bounded manifest from an explicit local install source.
pub fn local_package_source(path: &Path, cwd: &Path) -> crate::Result<InstallSource> {
    let directory = crate::normalize_launch_path(path, cwd)?;
    let input =
        rsi_files_native_fs::open_absolute_directory_no_follow(&directory).map_err(error)?;
    let mut manifest = Vec::new();
    rsi_files_native_fs::open_relative_file_no_follow(&input, Path::new("package.json"))
        .map_err(error)?
        .take(rsi_packages::MAX_DOCUMENT as u64 + 1)
        .read_to_end(&mut manifest)
        .map_err(error)?;
    Manifest::parse(&manifest).map_err(error)?;
    Ok(InstallSource {
        directory,
        manifest,
        provenance: Provenance::Local,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Imported {
    format: u32,
    directories: Vec<String>,
}
fn imported(value: serde_json::Value) -> crate::Result<Imported> {
    let result: Imported = serde_json::from_value(value).map_err(error)?;
    if result.format != 1 || result.directories.is_empty() || result.directories.len() > 128 {
        return Err(error("import result format or capacity"));
    }
    let mut names = BTreeSet::new();
    for name in &result.directories {
        rsi_packages::validate_identifier(name).map_err(error)?;
        if !names.insert(name) {
            return Err(error("duplicate import directory"));
        }
    }
    Ok(result)
}

/// Resolves one installed importer and atomically publishes its complete result.
pub async fn import_packages(
    store: Arc<PackageStore>,
    invocation: PackageImport,
    protocols: Vec<rsi_api_protocol::ProtocolDescriptor>,
    stop: CancellationToken,
) -> crate::Result<PackageAdmission> {
    let prepared = prepare_import(&store, invocation, &stop).await?;
    prepared
        .publish(move |sources, names, target| {
            let resolution = store
                .install_checked(sources, &[], &protocols, target, || producer_active(&stop))?;
            Ok(PackageAdmission { resolution, names })
        })
        .await
}

/// Resolves the same installed importer but reserves its outputs in the invoking profile.
pub async fn import_profile_packages(
    store: Arc<PackageStore>,
    invocation: PackageImport,
    protocols: Vec<rsi_api_protocol::ProtocolDescriptor>,
    stop: CancellationToken,
    profile: String,
    expected: BTreeMap<String, String>,
) -> crate::Result<rsi_packages::ProfileAdmission> {
    let prepared = prepare_import(&store, invocation, &stop).await?;
    prepared
        .publish(move |sources, _, target| {
            store.stage_for_profile(&profile, &expected, sources, &protocols, target, || {
                producer_active(&stop)
            })
        })
        .await
}
fn producer_active(stop: &CancellationToken) -> rsi_packages::Result<()> {
    if stop.is_cancelled() {
        Err(rsi_packages::Error::Invalid("import cancelled".into()))
    } else {
        Ok(())
    }
}
struct PreparedImport {
    scratch: tempfile::TempDir,
    lease: Arc<rsi_packages::ResolvedProcessExtension>,
    sources: Vec<InstallSource>,
    names: Vec<String>,
}
impl PreparedImport {
    async fn publish<T: Send + 'static>(
        self,
        publish: impl FnOnce(&[InstallSource], Vec<String>, &str) -> rsi_packages::Result<T>
        + Send
        + 'static,
    ) -> crate::Result<T> {
        tokio::task::spawn_blocking(move || {
            let _resources = (self.scratch, self.lease);
            publish(
                &self.sources,
                self.names,
                &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            )
        })
        .await
        .map_err(error)?
        .map_err(error)
    }
}
async fn prepare_import(
    store: &Arc<PackageStore>,
    invocation: PackageImport,
    stop: &CancellationToken,
) -> crate::Result<PreparedImport> {
    invocation.validate()?;
    let selected = match invocation.digest {
        Some(digest) => digest,
        None => {
            store
                .resolution()
                .map_err(error)?
                .packages
                .remove(&invocation.package)
                .ok_or_else(|| error("importer package is not installed"))?
                .digest
        }
    };
    let scratch = tempfile::Builder::new()
        .prefix("rsi-import-")
        .tempdir()
        .map_err(error)?;
    let root =
        crate::normalize_launch_path(scratch.path(), &std::env::current_dir().map_err(error)?)?;
    let generation = format!(
        "import-{}",
        scratch
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| error("temporary path is not UTF-8"))?
    );
    let verification = {
        let store = Arc::clone(store);
        let (name, digest, export, identity) = (
            invocation.package.clone(),
            selected.clone(),
            invocation.export.clone(),
            generation.clone(),
        );
        tokio::task::spawn_blocking(move || {
            store.package_importer(&name, &digest, &export, &identity)
        })
    };
    let resolved = Arc::new(verification.await.map_err(error)?.map_err(error)?);
    if stop.is_cancelled() {
        return Err(error("import cancelled"));
    }
    let stage = root.join("staging");
    std::fs::create_dir(&stage).map_err(error)?;
    let launch = rsi_extension::Launch {
        program: invocation.program,
        arguments: resolved.arguments.clone(),
        cwd: resolved.cwd.clone(),
        environment: private_environment(&root)?,
        generation,
    };
    launch.validate().map_err(error)?;
    let running = start_importer(&root, launch, resolved.clone()).await?;
    let result = async {
        let connection = running.lookup_local::<rsi_extension::ExtensionContract>()
            .ok_or_else(|| error("importer failed to activate"))?;
        let cancellation = stop.child_token();
        let mut payload = json!({"format":1,"staging":stage,
            "workspace":invocation.workspace,"specifications":invocation.specifications,
            "importer":{"package":invocation.package,"digest":selected,"dependencies":resolved.package.manifest().dependencies}});
        if let Some(options) = invocation.options { payload["options"] = options; }
        let request = connection.request("package.import", payload, cancellation.clone());
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_mins(10), request) => result.map_err(error).and_then(|result| result.map_err(error)),
            () = stop.cancelled() => Err(error("import cancelled")),
        };
        cancellation.cancel();
        imported(result?)
    }.await;
    if !running.shutdown().await.is_clean() {
        let retained = scratch.keep();
        return Err(error(format!(
            "importer did not settle; staging retained at {}",
            retained.display()
        )));
    }
    let imported = result?;
    let (sources, names) = imported_sources(&imported, &stage, &root)?;
    if stop.is_cancelled() {
        return Err(error("import cancelled"));
    }
    Ok(PreparedImport {
        scratch,
        lease: resolved,
        sources,
        names,
    })
}

fn imported_sources(
    imported: &Imported,
    stage: &Path,
    root: &Path,
) -> crate::Result<(Vec<InstallSource>, Vec<String>)> {
    let sources = imported
        .directories
        .iter()
        .map(|name| local_package_source(&stage.join(name), root))
        .collect::<crate::Result<Vec<_>>>()?;
    let names = sources
        .iter()
        .map(|source| Manifest::parse(&source.manifest).map(|m| m.name))
        .collect::<rsi_packages::Result<Vec<_>>>()
        .map_err(error)?;
    Ok((sources, names))
}

fn private_environment(root: &Path) -> crate::Result<BTreeMap<String, String>> {
    let mut environment = importer_environment(std::env::vars_os());
    for (key, directory) in [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("TMP", "temporary"),
        ("TEMP", "temporary"),
        ("TMPDIR", "temporary"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        let path = root.join(directory);
        std::fs::create_dir_all(&path).map_err(error)?;
        environment.insert(
            key.into(),
            path.into_os_string()
                .into_string()
                .map_err(|_| error("temporary path is not UTF-8"))?,
        );
    }
    Ok(environment)
}

fn importer_environment(
    variables: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> BTreeMap<String, String> {
    rsi_shell_native::scrub_child_environment(variables)
        .into_iter()
        .filter_map(|(name, value)| {
            let key = name.into_string().ok()?;
            let value = value.into_string().ok()?;
            [
                "SystemRoot",
                "WINDIR",
                "PATH",
                "LANG",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "NO_PROXY",
            ]
            .iter()
            .any(|allowed| key.eq_ignore_ascii_case(allowed))
            .then_some((key, value))
        })
        .collect()
}

async fn start_importer(
    root: &Path,
    launch: rsi_extension::Launch,
    resolved: Arc<rsi_packages::ResolvedProcessExtension>,
) -> crate::Result<rsi_host::RunningHost> {
    let mut builder = HostBuilder::new(
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache"))
            .map_err(error)?,
    );
    builder
        .register_local_contract::<rsi_process::ProcessContract>()
        .map_err(error)?;
    builder
        .register_local_contract::<rsi_process::DuplexProcessContract>()
        .map_err(error)?;
    builder
        .register_local_contract::<rsi_sandbox::SandboxContract>()
        .map_err(error)?;
    builder
        .register_local_contract::<rsi_extension::ExtensionContract>()
        .map_err(error)?;
    for (name, factory) in [
        (
            "process",
            Arc::new(rsi_process_local::ProcessLocalFactory) as Arc<dyn rsi_meta::PluginFactory>,
        ),
        (
            "sandbox",
            Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
        ),
        (
            "importer",
            Arc::new(
                rsi_extension::ExtensionFactory::new(Arc::new(rsi_extension::RejectRequests))
                    .retaining(resolved),
            ),
        ),
    ] {
        builder
            .register_linked(name, "1", UpdateMode::RestartRequired, factory)
            .map_err(error)?;
    }
    builder
        .build()
        .map_err(error)?
        .start(Profile::new([
            ProfileEntry::new("process", "process", json!({})),
            ProfileEntry::new("sandbox", "sandbox", json!({})),
            ProfileEntry::new(
                "importer",
                "importer",
                serde_json::to_value(launch).map_err(error)?,
            ),
        ]))
        .await
        .map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn importer_keeps_only_noncredential_network_settings_and_os_inputs() {
        let variables = [
            ("https_proxy", "http://127.0.0.1:8080"),
            ("HTTP_PROXY", "http://user:pass@proxy"),
            ("NO_PROXY", "localhost"),
            ("API_KEY", "unrelated"),
            ("NODE_OPTIONS", "--inspect"),
            ("PATH", "bin"),
        ];
        let env = importer_environment(variables.map(|(name, value)| (name.into(), value.into())));
        assert_eq!(env.len(), 3);
        assert_eq!(env["https_proxy"], "http://127.0.0.1:8080");
        assert_eq!(env["NO_PROXY"], "localhost");
        assert_eq!(env["PATH"], "bin");
    }
    #[test]
    fn result_refuses_escape_duplicates_and_unknown_fields() {
        for value in [
            json!({"format":1,"directories":["../outside"]}),
            json!({"format":1,"directories":["a","a"]}),
            json!({"format":1,"directories":["a"],"extra":true}),
            json!({"format":2,"directories":["a"]}),
        ] {
            assert!(imported(value).is_err());
        }
        assert_eq!(
            imported(json!({"format":1,"directories":["plugin-one"]}))
                .unwrap()
                .directories,
            ["plugin-one"]
        );
    }
}
