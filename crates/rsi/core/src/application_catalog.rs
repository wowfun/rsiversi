//! Frozen application metadata and the application-owned assembly seam.
use crate::{ApplicationProfileId, StandardAddonSet, StandardComposition};
use rsi_application::ApplicationDiagnostic;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    sync::Arc,
};

/// Immutable application input shared by native clients and Service daemons.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplicationCatalogMetadata {
    plugins: Arc<BTreeSet<String>>,
    profiles: Arc<BTreeMap<ApplicationProfileId, Vec<u8>>>,
}

impl ApplicationCatalogMetadata {
    /// Freezes bounded declarations, rejecting duplicate IDs before composition.
    pub fn new(
        plugins: impl IntoIterator<Item = String>,
        profiles: impl IntoIterator<Item = (ApplicationProfileId, Vec<u8>)>,
    ) -> rsi_host::Result<Self> {
        let failure = |message: &str| rsi_host::HostError::Bootstrap(message.into());
        let mut ids = BTreeSet::new();
        for id in plugins {
            if ids.len() >= 4096
                || id.is_empty()
                || id.len() > 256
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                || !ids.insert(id)
            {
                return Err(failure(
                    "invalid, duplicate or excessive application plugin IDs",
                ));
            }
        }
        let mut documents = BTreeMap::new();
        let mut total = 0_usize;
        for (id, bytes) in profiles {
            total = total.saturating_add(bytes.len());
            if documents.len() >= 4096
                || bytes.len() > crate::MAXIMUM_PROFILE_DOCUMENT_BYTES
                || total > 16 * 1024 * 1024
                || id.as_str() == "session"
            {
                return Err(failure(
                    "invalid or excessive built-in Application Profiles",
                ));
            }
            let _: toml::Table = toml::from_slice(&bytes)
                .map_err(|e| failure(&format!("invalid built-in Application Profile: {e}")))?;
            if documents.insert(id, bytes).is_some() {
                return Err(failure("duplicate built-in Application Profile"));
            }
        }
        Ok(Self {
            plugins: Arc::new(ids),
            profiles: Arc::new(documents),
        })
    }

    /// Reserved linked application plugin identities, without factory construction.
    pub fn plugins(&self) -> &BTreeSet<String> {
        &self.plugins
    }

    pub(crate) fn profiles(&self) -> &BTreeMap<ApplicationProfileId, Vec<u8>> {
        &self.profiles
    }

    /// Stable identity for Service-side Profile management and plugin reservations.
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"rsi.application.catalog.v1");
        for id in self.plugins.iter() {
            hash.update(b"plugin");
            hash.update((id.len() as u64).to_be_bytes());
            hash.update(id);
        }
        for (id, bytes) in self.profiles.iter() {
            hash.update(b"profile");
            hash.update((id.as_str().len() as u64).to_be_bytes());
            hash.update(id.as_str());
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        format!("{:x}", hash.finalize())
    }
}

/// One pure application catalog assembly and its consuming diagnostic ports.
#[derive(Debug)]
pub struct ApplicationCatalog {
    /// Frozen application declarations; no factory has been activated.
    pub addons: StandardAddonSet,
    /// Diagnostic priority is the supplied order.
    pub diagnostics: Vec<Arc<dyn ApplicationDiagnostic>>,
}

/// Application-owned selection, rebuilt against each staged Service composition.
pub trait ApplicationCatalogProvider: std::fmt::Debug + Send + Sync {
    /// Metadata must remain fixed for this provider's lifetime.
    fn metadata(&self) -> &ApplicationCatalogMetadata;
    /// Constructs fresh factories without starting runtime work.
    fn build(
        &self,
        service: &StandardComposition,
        arguments: Vec<OsString>,
    ) -> crate::Result<ApplicationCatalog>;
}
