use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::{LoaderError, MANIFEST_FORMAT_VERSION, PluginManifest};

const MAX_ARTIFACTS_PER_PACKAGE: usize = 32;
const MAX_CONTRACTS_PER_PACKAGE: usize = 256;
const MAX_CAPABILITIES_PER_PACKAGE: usize = 256;

pub(super) fn validate_manifest_shape(manifest: &PluginManifest) -> Result<(), LoaderError> {
    if manifest.format_version != MANIFEST_FORMAT_VERSION {
        return Err(LoaderError::UnsupportedManifestVersion {
            found: manifest.format_version,
            expected: MANIFEST_FORMAT_VERSION,
        });
    }
    if !valid_package_id(&manifest.package.id) {
        return Err(LoaderError::InvalidPackageId);
    }
    if manifest.package.version.is_empty() {
        return Err(LoaderError::EmptyPackageVersion);
    }
    if manifest.artifacts.is_empty() {
        return Err(LoaderError::EmptyManifestValue { field: "artifacts" });
    }
    require_limit(
        "artifacts",
        manifest.artifacts.len(),
        MAX_ARTIFACTS_PER_PACKAGE,
    )?;
    require_limit(
        "provides",
        manifest.provides.len(),
        MAX_CONTRACTS_PER_PACKAGE,
    )?;
    require_limit("injects", manifest.injects.len(), MAX_CONTRACTS_PER_PACKAGE)?;
    require_limit(
        "capabilities",
        manifest.capabilities.len(),
        MAX_CAPABILITIES_PER_PACKAGE,
    )?;
    for artifact in &manifest.artifacts {
        if artifact.target.is_empty() {
            return Err(LoaderError::EmptyManifestValue {
                field: "artifacts.target",
            });
        }
        validate_relative_path("artifacts.path", &artifact.path)?;
    }
    validate_unique_contracts("provides", manifest.provides.iter().map(String::as_str))?;
    validate_unique_contracts(
        "injects.contract",
        manifest
            .injects
            .iter()
            .map(|inject| inject.contract.as_str()),
    )?;
    validate_unique_nonempty(
        "capabilities",
        manifest.capabilities.iter().map(String::as_str),
    )?;
    if let Some(config_schema) = &manifest.config_schema {
        validate_relative_path("config_schema", config_schema)?;
    }
    Ok(())
}

fn require_limit(field: &'static str, actual: usize, maximum: usize) -> Result<(), LoaderError> {
    if actual > maximum {
        return Err(LoaderError::ManifestCollectionLimit {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn valid_package_id(id: &str) -> bool {
    if id.len() > 255 {
        return false;
    }
    let mut bytes = id.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_lowercase())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
}

fn validate_unique_nonempty<'value>(
    field: &'static str,
    values: impl Iterator<Item = &'value str>,
) -> Result<(), LoaderError> {
    let mut seen = HashSet::new();
    for value in values {
        if value.is_empty() {
            return Err(LoaderError::EmptyManifestValue { field });
        }
        if !seen.insert(value) {
            return Err(LoaderError::DuplicateManifestValue {
                field,
                value: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_unique_contracts<'value>(
    field: &'static str,
    values: impl Iterator<Item = &'value str>,
) -> Result<(), LoaderError> {
    let mut seen = HashSet::new();
    for value in values {
        if !valid_contract_name(value) {
            return Err(LoaderError::InvalidContractName { field });
        }
        if !seen.insert(value) {
            return Err(LoaderError::DuplicateManifestValue {
                field,
                value: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn valid_contract_name(value: &str) -> bool {
    if value.len() > 255 {
        return false;
    }
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphanumeric())
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

pub(super) fn validate_relative_path(field: &'static str, path: &Path) -> Result<(), LoaderError> {
    let valid = !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(LoaderError::UnsafeManifestPath {
            field,
            path: PathBuf::from(path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiVersion, ArtifactManifest, HostApiRequirement, InjectionManifest, PackageIdentity,
    };

    fn manifest() -> PluginManifest {
        PluginManifest {
            format_version: MANIFEST_FORMAT_VERSION,
            package: PackageIdentity {
                id: "test.package".to_owned(),
                version: "0.0.0".to_owned(),
                process_fixed: false,
            },
            host_api: HostApiRequirement {
                major: ApiVersion::CURRENT.major,
                minimum_minor: ApiVersion::CURRENT.minor,
            },
            artifacts: vec![ArtifactManifest {
                target: "test-target".to_owned(),
                path: "artifact.so".into(),
            }],
            provides: Vec::new(),
            injects: Vec::new(),
            capabilities: Vec::new(),
            config_schema: None,
        }
    }

    #[test]
    fn collection_limits_are_checked_before_per_entry_validation() {
        let mut manifest = manifest();
        manifest.injects = (0..=MAX_CONTRACTS_PER_PACKAGE)
            .map(|_| InjectionManifest {
                contract: String::new(),
                required: true,
            })
            .collect();
        assert!(matches!(
            validate_manifest_shape(&manifest),
            Err(LoaderError::ManifestCollectionLimit {
                field: "injects",
                actual,
                maximum: MAX_CONTRACTS_PER_PACKAGE,
            }) if actual == MAX_CONTRACTS_PER_PACKAGE + 1
        ));
    }
}
