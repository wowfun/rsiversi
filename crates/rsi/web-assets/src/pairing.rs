//! Product-only bootstrap admission; renderer publication remains independent.
use super::{
    AssetDirectory, AssetError, AssetResult, Config, WebAssetsFactory, open_file,
    publication::BOOTSTRAP,
};
use async_trait::async_trait;
use rsi_api_http::HttpAsset;
use rsi_application::{ApplicationDiagnostic, RsiError};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, io::Read as _, path::Path, sync::Mutex};

const RECEIPT: &str = "rsi-build.json";
const MAXIMUM_RECEIPT_BYTES: u64 = 32 * 1024;
const REBUILD: &str = "Install Web with `rsi plugin install web`, then run `rsi web`; explicitly built local bundles can be selected with `rsi web --assets ABSOLUTE_DIRECTORY`";

#[derive(Clone)]
pub(super) enum Identity {
    Exact(String),
    Internal,
}

/// Independent bundle admission; package authenticity belongs to its package owner.
#[derive(Debug)]
pub struct VerifiedWebAssetsFactory;
#[async_trait]
impl PluginFactory for VerifiedWebAssetsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let mut config: Config = serde_json::from_value(desired.clone())
            .map_err(|_| MetaError::InvalidInput("invalid Web assets configuration".into()))?;
        config.validate()?;
        config.pairing = Some(Identity::Internal);
        config.discover_renderers = desired.get("files").is_none();
        Ok(PreparedActivation::with_state(
            ConfigValue::Null,
            config,
            32 * 1024,
        ))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        WebAssetsFactory.activate(plan).await
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    format: u8,
    family_sha256: String,
    bootstrap: BTreeMap<String, String>,
    desktop_bridge: rsi_api_protocol::ProtocolDescriptor,
    document: Option<Document>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    files: BTreeMap<String, String>,
    trusted_inline_styles: bool,
}

impl Receipt {
    pub(super) fn configure(&self, config: &mut Config) -> AssetResult<()> {
        if let Some(document) = &self.document {
            let declared: Vec<_> = BOOTSTRAP
                .iter()
                .map(|s| (*s).to_owned())
                .chain(document.files.keys().cloned())
                .collect();
            if config.discover_renderers {
                config.files = declared;
                config.trusted_inline_styles = document.trusted_inline_styles;
                config.discover_renderers = false;
            } else if config.trusted_inline_styles != document.trusted_inline_styles
                || config
                    .files
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    != declared.iter().collect()
            {
                return Err(AssetError::Invalid(
                    "explicit assets differ from the document receipt".into(),
                ));
            }
            config
                .validate()
                .map_err(|e| AssetError::Invalid(e.to_string()))?;
        }
        Ok(())
    }
}

/// Exact-family fixture admission; expected identity cannot come from Profile config.
#[derive(Debug)]
pub struct PairedWebAssetsFactory {
    family: Option<&'static str>,
    diagnostic: Mutex<Option<RsiError>>,
}
impl PairedWebAssetsFactory {
    /// Supplies the fixture's expected family, never an operator override.
    pub const fn new(family: Option<&'static str>) -> Self {
        Self {
            family,
            diagnostic: Mutex::new(None),
        }
    }
    fn diagnosed(&self, error: impl std::fmt::Display) -> MetaError {
        let mut message = error.to_string();
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        *self.diagnostic.lock().expect("pairing diagnostic poisoned") =
            Some(RsiError::Boot(message.clone()));
        MetaError::InvalidInput(message)
    }
}
impl ApplicationDiagnostic for PairedWebAssetsFactory {
    fn take_diagnostic(&self) -> Option<RsiError> {
        self.diagnostic
            .lock()
            .expect("pairing diagnostic poisoned")
            .take()
    }
}
#[async_trait]
impl PluginFactory for PairedWebAssetsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let family = self.family.ok_or_else(|| {
            self.diagnosed(format!(
                "Web pairing: native executable has no build family. {REBUILD}"
            ))
        })?;
        let mut config: Config = serde_json::from_value(desired.clone())
            .map_err(|_| self.diagnosed("invalid Web assets configuration"))?;
        config.validate().map_err(|e| self.diagnosed(e))?;
        config.pairing = Some(Identity::Exact(family.to_owned()));
        config.discover_renderers = desired.get("files").is_none();
        Ok(PreparedActivation::with_state(
            ConfigValue::Null,
            config,
            32 * 1024,
        ))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        WebAssetsFactory
            .activate(plan)
            .await
            .map_err(|e| self.diagnosed(e))
    }
}

pub(super) fn read(
    directory: &AssetDirectory,
    path: &Path,
    identity: &Identity,
) -> AssetResult<Receipt> {
    let failure = |detail: String| AssetError::Invalid(format!("Web bundle: {detail}. {REBUILD}"));
    let file = open_file(directory, path, RECEIPT)
        .map_err(|_| failure(format!("missing or unreadable {RECEIPT}")))?;
    let info = file
        .metadata()
        .map_err(|_| failure("cannot inspect receipt".into()))?;
    if !info.is_file() || info.len() > MAXIMUM_RECEIPT_BYTES {
        return Err(failure("invalid receipt size or type".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAXIMUM_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read receipt".into()))?;
    if bytes.len() as u64 > MAXIMUM_RECEIPT_BYTES {
        return Err(failure("receipt exceeds limit".into()));
    }
    let receipt: Receipt =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid receipt JSON".into()))?;
    if receipt.format != 3
        || receipt.family_sha256.len() != 64
        || !receipt.family_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || matches!(identity, Identity::Exact(expected) if &receipt.family_sha256 != expected)
    {
        let actual = if receipt.family_sha256.len() == 64
            && receipt.family_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            receipt.family_sha256.as_str()
        } else {
            "invalid"
        };
        return Err(failure(format!(
            "receipt format or family mismatch (actual {actual})"
        )));
    }
    Ok(receipt)
}

pub(super) fn verify(
    receipt: Receipt,
    files: &BTreeMap<String, HttpAsset>,
) -> AssetResult<rsi_api_protocol::ProtocolDescriptor> {
    let failure = |detail: String| AssetError::Invalid(format!("Web bundle: {detail}. {REBUILD}"));
    if receipt.bootstrap.len() != BOOTSTRAP.len() {
        return Err(failure("bootstrap file set differs".into()));
    }
    for name in BOOTSTRAP {
        let asset = files
            .get(&format!("/{name}"))
            .ok_or_else(|| failure(format!("missing bootstrap file {name}")))?;
        let actual = format!("{:x}", Sha256::digest(asset.bytes.as_bytes()));
        if receipt.bootstrap.get(*name) != Some(&actual) {
            return Err(failure(format!("bootstrap digest mismatch for {name}")));
        }
    }
    if let Some(document) = &receipt.document {
        for (name, digest) in &document.files {
            let asset = files
                .get(&format!("/{name}"))
                .ok_or_else(|| failure(format!("missing document asset {name}")))?;
            if format!("{:x}", Sha256::digest(asset.bytes.as_bytes())) != *digest {
                return Err(failure(format!(
                    "document asset digest mismatch for {name}"
                )));
            }
        }
    }
    let inline = receipt
        .document
        .as_ref()
        .is_some_and(|document| document.trusted_inline_styles);
    if files["/index.html"].policy != super::application_policy(inline) {
        return Err(failure("document policy differs from its receipt".into()));
    }
    receipt
        .desktop_bridge
        .validate()
        .map_err(|_| failure("invalid Desktop bridge descriptor".into()))?;
    if receipt.desktop_bridge.name != "rsi.desktop.bridge" {
        return Err(failure("unexpected Desktop bridge protocol".into()));
    }
    Ok(receipt.desktop_bridge)
}

/// Creates a receipt from admitted bootstrap bytes for an explicit build producer.
/// This function does not write files or confer distributor authenticity.
pub fn pairing_receipt(
    directory: &Path,
    family: &str,
    desktop_bridge: rsi_api_protocol::ProtocolDescriptor,
) -> AssetResult<Vec<u8>> {
    pairing_receipt_with_document(directory, family, desktop_bridge, None)
}

/// Creates a receipt binding an optional complete document closure and style policy.
/// The file list includes the fixed bootstrap and all additional assets.
pub fn pairing_receipt_with_document(
    directory: &Path,
    family: &str,
    desktop_bridge: rsi_api_protocol::ProtocolDescriptor,
    document: Option<(Vec<String>, bool)>,
) -> AssetResult<Vec<u8>> {
    desktop_bridge
        .validate()
        .map_err(|e| AssetError::Invalid(e.to_string()))?;
    if desktop_bridge.name != "rsi.desktop.bridge" {
        return Err(AssetError::Invalid(
            "unexpected Desktop bridge protocol".into(),
        ));
    }
    if family.len() != 64 || !family.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AssetError::Invalid("invalid build family digest".into()));
    }
    let config = Config {
        directory: directory.to_owned(),
        files: document.as_ref().map_or_else(
            || BOOTSTRAP.iter().map(|s| (*s).to_owned()).collect(),
            |(files, _)| files.clone(),
        ),
        watch: false,
        trusted_inline_styles: document.as_ref().is_some_and(|(_, inline)| *inline),
        pairing: None,
        discover_renderers: false,
    };
    config
        .validate()
        .map_err(|e| AssetError::Invalid(e.to_string()))?;
    let files = super::load(
        &config,
        &tokio_util::sync::CancellationToken::new(),
        &rsi_api_protocol::ByteBudget::default(),
        None,
    )?;
    if BOOTSTRAP
        .iter()
        .any(|name| !files.files.contains_key(&format!("/{name}")))
    {
        return Err(AssetError::Invalid(
            "document is missing a fixed bootstrap asset".into(),
        ));
    }
    let document = document.map(|(_, trusted_inline_styles)| Document {
        trusted_inline_styles,
        files: files
            .files
            .iter()
            .filter(|(path, _)| !BOOTSTRAP.contains(&path.trim_start_matches('/')))
            .map(|(path, asset)| {
                (
                    path.trim_start_matches('/').to_owned(),
                    format!("{:x}", Sha256::digest(asset.bytes.as_bytes())),
                )
            })
            .collect(),
    });
    let bootstrap = BOOTSTRAP
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                format!(
                    "{:x}",
                    Sha256::digest(files.files[&format!("/{name}")].bytes.as_bytes())
                ),
            )
        })
        .collect();
    serde_json::to_vec(&Receipt {
        format: 3,
        document,
        desktop_bridge,
        family_sha256: family.to_owned(),
        bootstrap,
    })
    .map_err(|e| AssetError::Invalid(e.to_string()))
}
