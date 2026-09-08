//! Bounded immutable Web bundle provider over the ordinary HTTP asset capability.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_api_http::{AssetType, HttpAsset, HttpAssets, HttpAssetsContract};
use rsi_api_protocol::{ApiError, ByteBudget, MAXIMUM_API_BYTES};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    directory: PathBuf,
    #[serde(default = "default_files")]
    files: Vec<String>,
}
fn default_files() -> Vec<String> {
    [
        "index.html",
        "app.js",
        "worker.js",
        "styles.css",
        "rsi_web.js",
        "rsi_web_bg.wasm",
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

#[derive(Debug)]
struct Assets {
    files: BTreeMap<String, HttpAsset>,
    stop: CancellationToken,
}
impl HttpAssets for Assets {
    fn get(&self, path: &str) -> rsi_api_protocol::Result<Option<HttpAsset>> {
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        Ok(self
            .files
            .get(if path == "/" { "/index.html" } else { path })
            .cloned())
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
        let stop = CancellationToken::new();
        let tracker = TaskTracker::new();
        let token = tracker.token();
        let retiring = stop.clone();
        plan.defer(
            "retire Web assets and join bundle reader",
            Box::new(move || {
                Box::pin(async move {
                    retiring.cancel();
                    tracker.close();
                    tracker.wait().await;
                    Ok(())
                })
            }),
        )?;
        let reading = stop.clone();
        let files = plan
            .context()
            .runtime()
            .execution()
            .prepare(move || {
                let _token = token;
                load(&config, &reading)
            })
            .await
            .map_err(|_| MetaError::Activation("Web bundle reader failed".into()))??;
        let supply = plan
            .context()
            .provide_local::<HttpAssetsContract>(Arc::new(Assets { files, stop }))?;
        plan.defer(
            "withdraw Web asset capability",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn load(
    config: &Config,
    stop: &CancellationToken,
) -> rsi_meta::Result<BTreeMap<String, HttpAsset>> {
    let directory = open_directory(&config.directory).map_err(io_error)?;
    let budget = ByteBudget::new(MAXIMUM_API_BYTES).expect("constant asset budget");
    let mut files = BTreeMap::new();
    for name in &config.files {
        if stop.is_cancelled() {
            return Err(MetaError::Activation("Web bundle load retired".into()));
        }
        let mut file = open_file(&directory, &config.directory, name).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > MAXIMUM_API_BYTES as u64 {
            return Err(MetaError::Activation(
                "Web asset must be a regular file within 64 MiB".into(),
            ));
        }
        let size = usize::try_from(metadata.len()).expect("bounded asset length");
        let reserved = budget
            .reserve(size)
            .map_err(|_| MetaError::Activation("Web bundle exceeds 64 MiB".into()))?;
        let mut bytes = vec![0; size];
        for chunk in bytes.chunks_mut(128 * 1024) {
            if stop.is_cancelled() {
                return Err(MetaError::Activation("Web bundle load retired".into()));
            }
            file.read_exact(chunk).map_err(io_error)?;
        }
        if file.read(&mut [0; 1]).map_err(io_error)? != 0 {
            return Err(MetaError::Activation("Web asset grew while loading".into()));
        }
        let bytes = reserved
            .retain_vec(bytes)
            .map_err(|_| MetaError::Activation("Web asset allocation exceeded admission".into()))?;
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
fn io_error(_: std::io::Error) -> MetaError {
    MetaError::Activation("cannot read a regular Web bundle asset".into())
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
