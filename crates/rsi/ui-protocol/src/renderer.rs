use crate::{ModelSchema, PRESENTATION_ABI, ProtocolError, Result, bounded, name_valid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Document mount location, independent of business target identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererSurface {
    /// Application root.
    Root,
    /// Independently bound pane.
    Pane,
    /// Auxiliary side panel.
    Sidebar,
    /// Transient detail or form.
    Dialog,
}
/// One digest-pinned file in a renderer's complete import graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererFile {
    /// Flat bundle filename, resolved within an acquired generation.
    pub name: String,
    /// Lowercase SHA-256 of the exact bytes.
    pub sha256: String,
}
/// Executable admission metadata, separate from model data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererManifest {
    /// Nominal renderer selected by a model.
    pub id: String,
    /// Mount/update/dispose ABI version.
    pub abi: u16,
    /// Flat JavaScript entry exporting mount.
    pub entry: String,
    /// Complete files, including lazy imports, CSS and WASM.
    pub files: Vec<RendererFile>,
    /// Exact supported model schemas.
    pub schemas: Vec<ModelSchema>,
    /// Requested bound-host capabilities.
    pub capabilities: Vec<String>,
    /// Supported document locations.
    pub surfaces: Vec<RendererSurface>,
}
/// Renderer metadata stored as `ui-renderers.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererCatalog {
    /// Explicit catalog format, currently one.
    pub format: u16,
    /// At most 32 independently named renderers.
    pub renderers: Vec<RendererManifest>,
}
impl RendererCatalog {
    /// Validates metadata; the asset owner separately checks referenced bytes.
    pub fn validate(&self) -> Result<()> {
        let invalid = || ProtocolError("invalid renderer catalog".into());
        if self.format != 1 || self.renderers.is_empty() || self.renderers.len() > 32 {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        for renderer in &self.renderers {
            if !name_valid(&renderer.id)
                || !ids.insert(&renderer.id)
                || renderer.abi != PRESENTATION_ABI
                || renderer.files.is_empty()
                || renderer.files.len() > 128
                || renderer.schemas.is_empty()
                || renderer.schemas.len() > 32
                || renderer.surfaces.is_empty()
                || renderer.surfaces.len() > 4
                || renderer.capabilities.len() > 8
            {
                return Err(invalid());
            }
            let mut names = BTreeSet::new();
            for file in &renderer.files {
                if !asset_name_valid(&file.name)
                    || !names.insert(&file.name)
                    || !digest_valid(&file.sha256)
                {
                    return Err(invalid());
                }
            }
            if std::path::Path::new(&renderer.entry)
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                != Some("js")
                || !names.contains(&renderer.entry)
            {
                return Err(invalid());
            }
            let mut schemas = BTreeSet::new();
            for schema in &renderer.schemas {
                if !name_valid(&schema.name)
                    || schema.version == 0
                    || !schemas.insert((&schema.name, schema.version))
                {
                    return Err(invalid());
                }
            }
            let mut capabilities = BTreeSet::new();
            for capability in &renderer.capabilities {
                if !matches!(
                    capability.as_str(),
                    "invoke" | "source" | "clipboard" | "focus"
                ) || !capabilities.insert(capability)
                {
                    return Err(invalid());
                }
            }
            for (index, surface) in renderer.surfaces.iter().enumerate() {
                if renderer.surfaces[..index].contains(surface) {
                    return Err(invalid());
                }
            }
        }
        bounded(self, crate::MAXIMUM_VIEW_BYTES)
    }
}
/// Checks a flat same-generation asset filename.
pub fn asset_name_valid(value: &str) -> bool {
    name_valid(value) && !value.starts_with('.')
}
/// Checks a canonical SHA-256 text identity.
pub fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
