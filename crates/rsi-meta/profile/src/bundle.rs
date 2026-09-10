use super::{ProfileError, ProfileLimits, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Bounded immutable UTF-8 source documents, independent of native files.
#[derive(Clone, PartialEq)]
pub struct ProfileBundle {
    root: String,
    documents: Arc<BTreeMap<String, Arc<[u8]>>>,
}

impl std::fmt::Debug for ProfileBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProfileBundle")
            .field("root", &self.root)
            .field("documents", &self.documents.keys())
            .finish_non_exhaustive()
    }
}

impl ProfileBundle {
    /// Validates every retained identity and document before publishing a bundle.
    pub fn new(
        root: impl Into<String>,
        documents: BTreeMap<String, Vec<u8>>,
        limits: &ProfileLimits,
    ) -> Result<Self> {
        let root = root.into();
        validate_documents(
            &root,
            documents
                .iter()
                .map(|(id, bytes)| (id.as_str(), bytes.as_slice())),
            limits,
        )?;
        Ok(Self {
            root,
            documents: Arc::new(
                documents
                    .into_iter()
                    .map(|(id, bytes)| (id, bytes.into()))
                    .collect(),
            ),
        })
    }

    /// Exact root document identity inside the bundle.
    pub fn root(&self) -> &str {
        &self.root
    }

    pub(super) fn validate(&self, limits: &ProfileLimits) -> Result<()> {
        validate_documents(
            &self.root,
            self.documents
                .iter()
                .map(|(id, bytes)| (id.as_str(), bytes.as_ref())),
            limits,
        )
    }

    pub(super) fn read(&self, requested: &Path) -> Result<(PathBuf, Arc<[u8]>)> {
        let id = requested.to_str().ok_or_else(invalid_path)?;
        let bytes = self.documents.get(id).ok_or_else(|| ProfileError::Source {
            message: "required bundle document is absent".into(),
        })?;
        Ok((PathBuf::from(id), bytes.clone()))
    }
}

fn invalid_path() -> ProfileError {
    ProfileError::Source {
        message: "bundle source identity must remain within its relative root".into(),
    }
}

fn validate_documents<'a>(
    root: &str,
    documents: impl Iterator<Item = (&'a str, &'a [u8])>,
    limits: &ProfileLimits,
) -> Result<()> {
    limits.validate()?;
    let mut count = 0_usize;
    let mut total = 0_usize;
    let mut found_root = false;
    for (id, bytes) in documents {
        if id.is_empty()
            || id.len() > limits.maximum_identifier_bytes
            || id.contains(['\\', ':'])
            || id.chars().any(char::is_control)
            || id.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(invalid_path());
        }
        count = count.checked_add(1).ok_or(ProfileError::CapacityExceeded {
            resource: "bundle sources",
            maximum: limits.maximum_source_files,
        })?;
        total = total
            .checked_add(bytes.len())
            .ok_or(ProfileError::CapacityExceeded {
                resource: "bundle bytes",
                maximum: limits.maximum_source_bytes,
            })?;
        for (actual, maximum, resource) in [
            (count, limits.maximum_source_files, "bundle sources"),
            (total, limits.maximum_source_bytes, "bundle bytes"),
            (bytes.len(), limits.maximum_document_bytes, "document bytes"),
        ] {
            if actual > maximum {
                return Err(ProfileError::CapacityExceeded { resource, maximum });
            }
        }
        std::str::from_utf8(bytes).map_err(|_| ProfileError::Source {
            message: "bundle source is not UTF-8".into(),
        })?;
        found_root |= id == root;
    }
    if !found_root {
        return Err(ProfileError::Source {
            message: "bundle root is absent".into(),
        });
    }
    Ok(())
}

pub(super) fn resolve_include(base: &Path, requested: &str) -> Result<PathBuf> {
    if requested.is_empty()
        || requested.starts_with('/')
        || requested.contains(['\\', ':'])
        || requested.chars().any(char::is_control)
    {
        return Err(invalid_path());
    }
    let base = base.to_str().ok_or_else(invalid_path)?;
    let mut parts = base
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for part in requested.split('/') {
        match part {
            "" => return Err(invalid_path()),
            "." => {}
            ".." => {
                parts.pop().ok_or_else(invalid_path)?;
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(invalid_path());
    }
    Ok(PathBuf::from(parts.join("/")))
}
