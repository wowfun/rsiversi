use crate::{Interface, Result, invalid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Exact dependency edge, with no version-range solver.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// Selected package identity.
    pub name: String,
    /// Exact manifest digest.
    pub digest: String,
}
/// One declared file; arbitrary source trees are never copied.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    /// Portable relative path.
    pub path: String,
    /// Exact file length.
    pub bytes: u64,
    /// Exact SHA-256.
    pub digest: String,
    /// Whether the published file may be executed.
    pub executable: bool,
}
/// Supported named exports, resolved only after package admission.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    /// Directory of declared Web document, Worker and WASM files.
    WebAssets,
    /// Native launcher or helper executable.
    Executable,
    /// Existing Application Profile language.
    ApplicationProfile,
    /// Existing Service Profile language.
    ServiceProfile,
    /// Native ABI addon.
    NativeAddon,
}
/// A named path and role; it grants no activation authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Export role.
    pub kind: ExportKind,
    /// File or assets directory relative to the package.
    pub path: String,
}
/// Provenance remains distinct from integrity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provenance {
    /// Explicitly admitted user-owned local source.
    Local,
    /// Metadata authenticated under an operator-pinned repository key.
    Signed {
        /// Repository authority.
        repository: String,
        /// Verified publication sequence.
        sequence: u64,
        /// Signed payload digest.
        payload: String,
    },
}
/// Bounded immutable package manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Format revision.
    pub format: u32,
    /// Stable package name.
    pub name: String,
    /// Human-readable release version.
    pub version: String,
    /// OS-architecture or `portable`.
    pub target: String,
    /// Exact dependencies.
    pub dependencies: Vec<Dependency>,
    /// Declared files.
    pub files: Vec<FileRecord>,
    /// Named exports.
    pub exports: BTreeMap<String, Export>,
    /// Required wire interfaces.
    pub requires: Vec<Interface>,
}
impl Manifest {
    /// Bounds bytes before parsing, then validates all cross-field rules.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        if bytes.len() > crate::MAX_DOCUMENT {
            return Err(invalid("manifest exceeds 1 MiB"));
        }
        let manifest: Self = serde_json::from_slice(bytes).map_err(|error| {
            if serde_json::from_slice::<Format>(bytes).is_ok_and(|format| format.format != 2) {
                invalid("unsupported package manifest format; expected 2")
            } else {
                invalid(error.to_string())
            }
        })?;
        if manifest.format != 2 {
            return Err(invalid("unsupported package manifest format; expected 2"));
        }
        manifest.validate()?;
        Ok(manifest)
    }
    /// Validates typed input as strictly as decoded metadata.
    pub fn validate(&self) -> Result<()> {
        crate::identifier(&self.name)?;
        if self.format != 2
            || self.version.is_empty()
            || self.version.len() > 128
            || !self.version.bytes().all(|b| b.is_ascii_graphic())
            || !matches!(
                self.target.as_str(),
                "portable"
                    | "windows-x86_64"
                    | "windows-aarch64"
                    | "linux-x86_64"
                    | "linux-aarch64"
                    | "macos-x86_64"
                    | "macos-aarch64"
            )
            || self.files.is_empty()
            || self.files.len() > crate::MAX_FILES
            || self.dependencies.len() > 128
            || self.exports.len() > 128
            || self.requires.len() > 128
        {
            return Err(invalid("manifest bounds, format or target"));
        }
        let mut paths = BTreeSet::new();
        let mut total = 0_u64;
        for file in &self.files {
            relative(&file.path)?;
            crate::hash(&file.digest)?;
            if file.bytes > crate::MAX_FILE_BYTES || !paths.insert(file.path.to_ascii_lowercase()) {
                return Err(invalid("duplicate file or file size"));
            }
            total = total
                .checked_add(file.bytes)
                .ok_or_else(|| invalid("package size overflow"))?;
        }
        if total > crate::MAX_STORE_BYTES {
            return Err(invalid("package exceeds store capacity"));
        }
        for file in &self.files {
            let components: Vec<_> = file.path.split('/').collect();
            for end in 1..components.len() {
                if paths.contains(&components[..end].join("/").to_ascii_lowercase()) {
                    return Err(invalid("file is also a directory"));
                }
            }
        }
        for (name, export) in &self.exports {
            crate::identifier(name)?;
            relative(&export.path)?;
            let valid = if export.kind == ExportKind::WebAssets {
                self.files
                    .iter()
                    .any(|file| file.path.starts_with(&format!("{}/", export.path)))
            } else {
                self.files.iter().any(|file| {
                    file.path == export.path
                        && (export.kind != ExportKind::Executable || file.executable)
                })
            };
            if !valid {
                return Err(invalid("export has no declared content"));
            }
        }
        let mut names = BTreeSet::new();
        for dependency in &self.dependencies {
            crate::identifier(&dependency.name)?;
            crate::hash(&dependency.digest)?;
            if dependency.name == self.name || !names.insert(&dependency.name) {
                return Err(invalid("self or duplicate dependency"));
            }
        }
        let mut interfaces = BTreeSet::new();
        for required in &self.requires {
            required.validate().map_err(|e| invalid(e.to_string()))?;
            if required.version == 0 || !interfaces.insert(&required.name) {
                return Err(invalid("invalid or duplicate interface"));
            }
        }
        Ok(())
    }
    /// Rejects missing and mismatched required interfaces; unrelated exports are ignored.
    pub fn admit_interfaces(&self, offered: &[Interface]) -> Result<()> {
        for required in &self.requires {
            if !offered.contains(required) {
                return Err(invalid(format!(
                    "required interface {} v{} is unavailable",
                    required.name, required.version
                )));
            }
        }
        Ok(())
    }
}
pub(crate) fn relative(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 1024 || !path.is_ascii() || path.split('/').count() > 128 {
        return Err(invalid("invalid portable path"));
    }
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.len() > 200
            || part.ends_with('.')
            || part.ends_with(' ')
            || part.starts_with('.')
            || part.bytes().any(|b| b < 32 || b"\\:*?\"<>|".contains(&b))
        {
            return Err(invalid("escaping or ambiguous path"));
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid("reserved device path"));
        }
    }
    Ok(())
}
