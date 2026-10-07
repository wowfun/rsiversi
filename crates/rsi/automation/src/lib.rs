//! Durable deployment acceptance under bounded standing operator authority.
#![forbid(unsafe_code)]

mod goal;
mod intake;
mod plugin;
mod policy;
mod protocol;
mod service;
mod store;
pub use goal::{BrowserRegistryContract, BrowserToolsFactory, GoalExplorer};
pub use plugin::{AutomationContract, AutomationFactory};
pub use policy::{AutomationGrant, Policy, PolicyOwner};
pub use protocol::*;
pub use service::{AutomationService, Explorer, IngressSource, Readiness};
pub use store::{AdmissionError, Ledger};

#[cfg(test)]
fn test_directory() -> tempfile::TempDir {
    tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

pub(crate) fn now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
pub(crate) fn private_file(path: &std::path::Path) -> Result<std::fs::File, AdmissionError> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(
            i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
                .map_err(|e| AdmissionError::Invalid(e.to_string()))?,
        );
    }
    let f = o
        .open(path)
        .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
    let m = f
        .metadata()
        .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
    if !m.is_file() {
        return Err(AdmissionError::Invalid(
            "automation file is not regular".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o077 != 0 || m.nlink() != 1
        {
            return Err(AdmissionError::Invalid(
                "automation file is not private".into(),
            ));
        }
    }
    Ok(f)
}
