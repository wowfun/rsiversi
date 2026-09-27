//! Product-only bootstrap admission; renderer publication remains independent.
use super::{AssetError, AssetResult, Config, WebAssetsFactory, open_file, publication::BOOTSTRAP};
use async_trait::async_trait;
use rsi_api_http::HttpAsset;
use rsi_application::{ApplicationDiagnostic, RsiError};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read as _, path::Path, sync::Mutex};

const RECEIPT: &str = "rsi-build.json";
const MAXIMUM_RECEIPT_BYTES: u64 = 32 * 1024;
const REBUILD: &str = "Build and launch the paired output with: pnpm -C apps/web build";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    format: u8,
    family_sha256: String,
    bootstrap: BTreeMap<String, String>,
}

/// Official product asset owner; expected identity cannot come from Profile config.
#[derive(Debug)]
pub struct PairedWebAssetsFactory {
    family: Option<&'static str>,
    diagnostic: Mutex<Option<RsiError>>,
}
impl PairedWebAssetsFactory {
    /// Supplies the native executable's compiled family, never an operator override.
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
        config.pairing = Some(family.to_owned());
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

pub(super) fn verify(
    directory: &File,
    path: &Path,
    expected: &str,
    files: &BTreeMap<String, HttpAsset>,
) -> AssetResult<()> {
    let failure = |detail: String| {
        AssetError::Invalid(format!(
            "Web pairing: {detail}; expected family {expected}. {REBUILD}"
        ))
    };
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
    if receipt.format != 1 || receipt.family_sha256 != expected {
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
    Ok(())
}

/// Creates a receipt from admitted bootstrap bytes for an explicit build producer.
/// This function does not write files or confer distributor authenticity.
pub fn pairing_receipt(directory: &Path, family: &str) -> AssetResult<Vec<u8>> {
    if family.len() != 64 || !family.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AssetError::Invalid("invalid build family digest".into()));
    }
    let config = Config {
        directory: directory.to_owned(),
        files: BOOTSTRAP.iter().map(|s| (*s).to_owned()).collect(),
        watch: false,
        pairing: None,
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
    let bootstrap = BOOTSTRAP
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                format!(
                    "{:x}",
                    Sha256::digest(files[&format!("/{name}")].bytes.as_bytes())
                ),
            )
        })
        .collect();
    serde_json::to_vec(&Receipt {
        format: 1,
        family_sha256: family.to_owned(),
        bootstrap,
    })
    .map_err(|e| AssetError::Invalid(e.to_string()))
}
