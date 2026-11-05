use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

/// Creates the package-backed process provider for this Host's store.
pub fn package_extension_factory(root: PathBuf) -> Arc<dyn PluginFactory> {
    Arc::new(PackageExtensionFactory { root })
}

#[derive(Debug)]
struct PackageExtensionFactory {
    root: PathBuf,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    package: String,
    digest: String,
    export: String,
    program: PathBuf,
    environment: BTreeMap<String, String>,
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[async_trait]
impl PluginFactory for PackageExtensionFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let value: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        validate(&self.root, &value)?;
        Ok(PreparedActivation::with_state(config.clone(), value, 0)
            .requiring_local::<rsi_process::DuplexProcessContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config = plan.take_state()?;
        let root = self.root.clone();
        let selected = resolve(root, config).await?;
        rsi_extension::ExtensionFactory::new(Arc::new(rsi_extension::RejectRequests))
            .retaining(selected.resolved)
            .activate_launch(plan, selected.launch)
            .await
    }
}

/// One resolved application launch and the store owning its active lease.
#[derive(Debug)]
pub struct PackageProcess {
    /// Admitted process arguments and environment.
    pub launch: rsi_extension::Launch,
    /// Exact immutable generation retained while the process runs.
    pub resolved: Arc<rsi_packages::ResolvedProcessExtension>,
    /// Store used by this generation for subsequent package operations.
    pub store: Arc<rsi_packages::PackageStore>,
}

/// Verifies the immutable closure before a separately supervised application activation.
pub async fn resolve_package_process(
    root: PathBuf,
    config: ConfigValue,
) -> rsi_meta::Result<PackageProcess> {
    let config: Config = serde_json::from_value(config).map_err(meta)?;
    validate(&root, &config)?;
    resolve(root, config).await
}
fn validate(root: &std::path::Path, value: &Config) -> rsi_meta::Result<()> {
    rsi_packages::validate_identifier(&value.package).map_err(meta)?;
    rsi_packages::validate_identifier(&value.export).map_err(meta)?;
    if value.digest.len() != 64
        || !value
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(meta("package selection requires an exact SHA-256"));
    }
    rsi_extension::Launch {
        program: value.program.clone(),
        cwd: root.to_owned(),
        arguments: vec![],
        environment: value.environment.clone(),
        generation: value.digest.clone(),
    }
    .validate()?;
    Ok(())
}
async fn resolve(root: PathBuf, config: Config) -> rsi_meta::Result<PackageProcess> {
    let mut entropy = [0_u8; 8];
    getrandom::fill(&mut entropy).map_err(meta)?;
    let generation = format!("{}-{}", config.digest, hex::encode(entropy));
    let selected_generation = generation.clone();
    // Verification and native directory handles remain owned if activation stops waiting.
    let (store, resolved) = tokio::task::spawn_blocking(move || {
        let store = Arc::new(rsi_packages::PackageStore::open(&root)?);
        let resolved = store.process_extension(
            &config.package,
            &config.digest,
            &config.export,
            &selected_generation,
        )?;
        Ok::<_, rsi_packages::Error>((store, resolved))
    })
    .await
    .map_err(meta)?
    .map_err(meta)?;
    let launch = rsi_extension::Launch {
        program: config.program,
        environment: config.environment,
        generation,
        cwd: resolved.cwd.clone(),
        arguments: resolved.arguments.clone(),
    };
    Ok(PackageProcess {
        launch,
        resolved: Arc::new(resolved),
        store,
    })
}
