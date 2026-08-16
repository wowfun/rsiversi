use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rsi_meta_loader::{ContentHash, LoaderError, read_bounded_file_following_symlinks};
use sha2::{Digest, Sha256};

use super::{CompositionFiles, MAX_COMPOSITION_DOCUMENT_BYTES};
use crate::composition::write_lock_create_new;
use crate::model::{CompositionLock, CompositionMode, GraphRevision, InstanceId, RoutingSnapshot};
use crate::protocol::{Command, CommandEnvelope, PluginInspection};
use crate::recovery::remove_file_and_sync_parent;
use crate::runtime::PluginCommandRequest;
use crate::{HostError, Result};

pub(super) struct PluginCommandRejection {
    pub(super) code: &'static str,
    pub(super) message: &'static str,
}

pub(super) fn validate_plugin_command_admission(
    snapshot: &RoutingSnapshot,
    mode: CompositionMode,
    inspections: &BTreeMap<InstanceId, PluginInspection>,
    installed: Option<&CompositionFiles>,
    request: &PluginCommandRequest,
) -> std::result::Result<CompositionFiles, PluginCommandRejection> {
    if snapshot.graph().composition_id != request.composition_id {
        return Err(PluginCommandRejection {
            code: "plugin_command_stale",
            message: "plugin command composition provenance is not current",
        });
    }
    let Some(generation) = snapshot.generation(&request.instance_id) else {
        return Err(PluginCommandRejection {
            code: "plugin_command_stale",
            message: "plugin command source instance is no longer active",
        });
    };
    if generation.id != request.generation || !generation.is_admitting() {
        return Err(PluginCommandRejection {
            code: "plugin_command_stale",
            message: "plugin command source generation is no longer admitting",
        });
    }
    if mode != CompositionMode::Development {
        return Err(PluginCommandRejection {
            code: "plugin_command_forbidden",
            message: "plugin-origin apply is disabled outside development mode",
        });
    }
    if !inspections
        .get(&request.instance_id)
        .is_some_and(|inspection| {
            inspection
                .capabilities
                .iter()
                .any(|capability| capability == "control.apply-manifest")
        })
    {
        return Err(PluginCommandRejection {
            code: "plugin_command_forbidden",
            message: "plugin source did not declare control.apply-manifest",
        });
    }
    let Command::ApplyManifestPath {
        manifest_path,
        lock_path,
    } = &request.envelope.payload
    else {
        return Err(PluginCommandRejection {
            code: "plugin_command_unsupported",
            message: "only apply_manifest_path is accepted from plugins",
        });
    };
    let Some(installed) = installed else {
        return Err(PluginCommandRejection {
            code: "plugin_command_forbidden",
            message: "plugin-origin apply requires an installed composition pair",
        });
    };
    if !same_existing_path(manifest_path, &installed.manifest_path)
        || !same_existing_path(lock_path, &installed.lock_path)
    {
        return Err(PluginCommandRejection {
            code: "plugin_command_path_mismatch",
            message: "plugin-origin apply must name the exact installed manifest and lock",
        });
    }
    Ok(installed.clone())
}

pub(super) fn plugin_provenance_command_id(
    request: &PluginCommandRequest,
    revision: GraphRevision,
) -> String {
    let provenance_hash = plugin_request_identity_hash(request);
    format!(
        "plugin-rejection:g{}:r{}:{}",
        request.generation, revision.0, provenance_hash
    )
}

pub(super) fn plugin_effect_command_id(
    request: &PluginCommandRequest,
    candidate_lock: &CompositionLock,
) -> Result<String> {
    let lock_bytes = toml::to_string_pretty(candidate_lock)
        .map_err(|error| HostError::InvalidManifest(error.to_string()))?;
    let content_hash = ContentHash::digest(lock_bytes);
    Ok(format!(
        "plugin-effect:{}:{content_hash}",
        plugin_request_identity_hash(request)
    ))
}

pub(super) fn plugin_request_identity_hash(request: &PluginCommandRequest) -> ContentHash {
    ContentHash::digest(format!(
        "{}\0{}\0{}",
        request.composition_id, request.instance_id, request.envelope.command_id
    ))
}

pub(super) fn plugin_candidate_lock_path(installed_lock: &Path, command_id: &str) -> PathBuf {
    let name_hash = ContentHash::digest(command_id.as_bytes());
    let parent = installed_lock.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".rsi-meta-plugin-candidate-{name_hash}.lock"))
}

pub(super) fn same_existing_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left)
        .and_then(|left| fs::canonicalize(right).map(|right| left == right))
        .unwrap_or(false)
}

pub(super) fn write_plugin_candidate_lock(path: &Path, lock: &CompositionLock) -> Result<()> {
    let expected = toml::to_string_pretty(lock)
        .map_err(|error| HostError::InvalidManifest(error.to_string()))?;
    match read_bounded_file_following_symlinks(
        path,
        "read plugin candidate lock",
        MAX_COMPOSITION_DOCUMENT_BYTES,
    ) {
        Ok(existing) if existing == expected.as_bytes() => return Ok(()),
        Ok(_) => remove_file_and_sync_parent(path)?,
        Err(LoaderError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    write_lock_create_new(path, lock)
}

pub(super) fn command_hash(command: &CommandEnvelope) -> Result<Vec<u8>> {
    let mut value = serde_json::to_value(command)?;
    canonicalize_json(&mut value);
    Ok(Sha256::digest(serde_json::to_vec(&value)?).to_vec())
}

const MAX_EXACT_INTEGER_F64: f64 = 9_007_199_254_740_992.0;

pub(super) fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            let mut entries: Vec<_> = std::mem::take(object).into_iter().collect();
            for (_, value) in &mut entries {
                canonicalize_json(value);
            }
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            object.extend(entries);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                canonicalize_json(value);
            }
        }
        serde_json::Value::Number(number) if number.is_f64() => {
            let Some(value) = number.as_f64() else {
                return;
            };
            if value.fract() != 0.0 || value.abs() > MAX_EXACT_INTEGER_F64 {
                return;
            }
            let integer = format!("{value:.0}");
            if let Ok(value) = integer.parse::<u64>() {
                *number = serde_json::Number::from(value);
            } else if let Ok(value) = integer.parse::<i64>() {
                *number = serde_json::Number::from(value);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}
