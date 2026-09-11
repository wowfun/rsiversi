//! Bounded immutable Web bundle provider over the ordinary HTTP asset capability.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_api_http::{AssetType, HttpAsset, HttpAssetsContract};
use rsi_api_protocol::{ByteBudget, MAXIMUM_API_BYTES};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek as _},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

mod publication;
pub use publication::{
    AssetCandidate, AssetError, AssetResult, BundleLease, StageTicket, WebAssetControl,
    WebAssetControlContract,
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    directory: PathBuf,
    #[serde(default = "default_files")]
    files: Vec<String>,
    #[serde(default)]
    watch: bool,
}
fn default_files() -> Vec<String> {
    [
        "index.html",
        "app.js",
        "worker.js",
        "styles.css",
        "rsi_web.js",
        "rsi_web_bg.wasm",
        "mounts.js",
        "drafts.js",
        "standard.js",
        "ui-renderers.json",
    ]
    .map(Into::into)
    .to_vec()
}
fn kind(name: &str) -> Option<AssetType> {
    match name.rsplit('.').next()? {
        "html" => Some(AssetType::Html),
        "js" => Some(AssetType::JavaScript),
        "css" => Some(AssetType::Css),
        "wasm" => Some(AssetType::Wasm),
        "json" => Some(AssetType::Json),
        "png" => Some(AssetType::Png),
        _ => None,
    }
}
impl Config {
    fn validate(&self) -> rsi_meta::Result<()> {
        let mut unique = BTreeSet::new();
        if !self.directory.is_absolute()
            || self.directory.as_os_str().len() > 16 * 1024
            || self.files.is_empty()
            || self.files.len() > 128
            || !self.files.iter().any(|file| file == "index.html")
            || self.files.iter().any(|file| {
                file.is_empty()
                    || file.len() > 128
                    || file.starts_with('.')
                    || !file.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
                    || kind(file).is_none()
                    || !unique.insert(file)
            })
        {
            return Err(MetaError::InvalidInput(
                "invalid bounded Web asset directory or file list".into(),
            ));
        }
        Ok(())
    }
}

/// Ordinary owner of one complete, bounded immutable asset generation.
#[derive(Clone, Debug, Default)]
pub struct WebAssetsFactory;
#[async_trait]
impl PluginFactory for WebAssetsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(desired.clone())
            .map_err(|_| MetaError::InvalidInput("invalid Web assets configuration".into()))?;
        config.validate()?;
        Ok(PreparedActivation::with_state(
            ConfigValue::Null,
            config,
            32 * 1024,
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let owner = WebAssetControl::new(plan.context().runtime().execution().clone());
        let cleanup = owner.clone();
        plan.defer(
            "retire Web assets and join bundle readers",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        owner
            .initialize(config.clone())
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let assets = plan
            .context()
            .provide_local::<HttpAssetsContract>(owner.clone())?;
        let control = plan
            .context()
            .provide_local::<WebAssetControlContract>(owner)?;
        plan.defer(
            "withdraw Web asset capabilities",
            Box::new(move || {
                Box::pin(async move {
                    drop((assets, control));
                    Ok(())
                })
            }),
        )
    }
}

fn load(
    config: &Config,
    stop: &CancellationToken,
    budget: &ByteBudget,
    previous: Option<&BTreeMap<String, HttpAsset>>,
) -> AssetResult<BTreeMap<String, HttpAsset>> {
    let directory = open_directory(&config.directory).map_err(io_error)?;
    let mut files = BTreeMap::new();
    for name in &config.files {
        if stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        let mut file = open_file(&directory, &config.directory, name).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > MAXIMUM_API_BYTES as u64 {
            return Err(AssetError::Invalid(
                "Web asset must be a regular file within 64 MiB".into(),
            ));
        }
        let size = usize::try_from(metadata.len()).expect("bounded asset length");
        if let Some(old) = previous.and_then(|files| files.get(&format!("/{name}"))) {
            if old.bytes.len() == size && identical(&mut file, old.bytes.as_bytes(), stop)? {
                files.insert(format!("/{name}"), old.clone());
                continue;
            }
            file.rewind().map_err(io_error)?;
        }
        let reserved = budget.reserve(size).map_err(|_| AssetError::Capacity)?;
        let mut bytes = vec![0; size];
        for chunk in bytes.chunks_mut(128 * 1024) {
            if stop.is_cancelled() {
                return Err(AssetError::Closed);
            }
            file.read_exact(chunk).map_err(io_error)?;
        }
        if file.read(&mut [0; 1]).map_err(io_error)? != 0 {
            return Err(AssetError::Invalid("Web asset grew while loading".into()));
        }
        let bytes = reserved
            .retain_vec(bytes)
            .map_err(|_| AssetError::Capacity)?;
        files.insert(
            format!("/{name}"),
            HttpAsset {
                kind: kind(name).expect("validated extension"),
                bytes,
            },
        );
    }
    Ok(files)
}
fn identical(file: &mut File, expected: &[u8], stop: &CancellationToken) -> AssetResult<bool> {
    let mut buffer = [0_u8; 8192];
    for chunk in expected.chunks(buffer.len()) {
        if stop.is_cancelled() {
            return Err(AssetError::Closed);
        }
        file.read_exact(&mut buffer[..chunk.len()])
            .map_err(io_error)?;
        if &buffer[..chunk.len()] != chunk {
            return Ok(false);
        }
    }
    Ok(file.read(&mut [0; 1]).map_err(io_error)? == 0)
}
fn io_error(_: std::io::Error) -> AssetError {
    AssetError::Invalid("cannot read a regular Web bundle asset".into())
}

#[cfg(unix)]
fn open_directory(path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    Ok(File::from(open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}
#[cfg(unix)]
fn open_file(directory: &File, _: &Path, name: &str) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    Ok(File::from(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}
#[cfg(not(unix))]
fn open_directory(path: &Path) -> std::io::Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_symlink() || !metadata.is_dir() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    Ok(path.to_owned())
}
#[cfg(not(unix))]
fn open_file(_: &Path, path: &Path, name: &str) -> std::io::Result<File> {
    let path = path.join(name);
    if std::fs::symlink_metadata(&path)?.is_symlink() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    File::open(path)
}
