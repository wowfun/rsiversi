use sha2::{Digest, Sha256};

use crate::Result;
use crate::domain::InstallRequest;
use crate::host::CompositionFiles;

pub(super) fn offline_install_hash(
    request: &InstallRequest,
    files: &CompositionFiles,
) -> Result<Vec<u8>> {
    let value = serde_json::json!({
        "method": "install",
        "manifest_path": files.manifest_path,
        "lock_path": files.lock_path,
        "database_path": crate::workspace::normalize_absolute(&request.workspace.database_path)?,
        "cache_root": crate::workspace::normalize_absolute(&request.workspace.cache_root)?,
        "installed_manifest_path": crate::workspace::normalize_absolute(&request.workspace.manifest_path)?,
        "installed_lock_path": crate::workspace::normalize_absolute(&request.workspace.lock_path)?,
    });
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&value)?);
    Ok(hasher.finalize().to_vec())
}
