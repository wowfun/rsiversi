use crate::{Layout, Record, Result, error, filesystem as fs};
use rsi_service_host::InstallationOrigin;
use std::path::Path;

/// Selects the claimed installation for an ordinary detached child to verify.
pub fn launch_hint() -> Result<Option<InstallationOrigin>> {
    if let Some(origin) = inherited()? {
        return Ok(Some(origin));
    }
    discover(&std::env::current_exe()?)?.map(origin).transpose()
}

/// Admits the running executable, including a private copy with an inherited claim.
pub fn current() -> Result<Option<InstallationOrigin>> {
    let executable = std::env::current_exe()?;
    if let Some(claim) = inherited()? {
        let record = Layout::new(&claim.prefix)?
            .record()?
            .ok_or_else(|| error("origin has no installation record"))?;
        if record.id != claim.id {
            return Err(error("installation origin does not match this executable"));
        }
        return verify(&executable, record).map(Some);
    }
    for_executable(&executable)
}

/// Admits an explicitly selected launch artifact independently of the parent's image.
pub fn for_executable(executable: &Path) -> Result<Option<InstallationOrigin>> {
    discover(executable)?
        .map(|record| verify(executable, record))
        .transpose()
}

fn inherited() -> Result<Option<InstallationOrigin>> {
    let Some(value) = std::env::var_os("RSI_INSTALLATION_ORIGIN") else {
        return Ok(None);
    };
    if value.len() > 8192 {
        return Err(error("installation origin exceeds bound"));
    }
    let origin: InstallationOrigin = serde_json::from_str(
        value
            .to_str()
            .ok_or_else(|| error("invalid origin encoding"))?,
    )
    .map_err(error)?;
    origin.validate().map_err(error)?;
    Ok(Some(origin))
}

fn discover(executable: &Path) -> Result<Option<Record>> {
    let Some(bin) = executable
        .parent()
        .filter(|dir| dir.file_name().is_some_and(|name| name == "bin"))
    else {
        return Ok(None);
    };
    Layout::new(
        bin.parent()
            .ok_or_else(|| error("missing installation prefix"))?,
    )?
    .record()
}

fn origin(record: Record) -> Result<InstallationOrigin> {
    let origin = InstallationOrigin {
        id: record.id,
        prefix: record.prefix,
    };
    origin.validate().map_err(error)?;
    Ok(origin)
}

fn verify(executable: &Path, record: Record) -> Result<InstallationOrigin> {
    if record.core != fs::digest_file(fs::source(executable)?)? {
        return Err(error("installation origin does not match this executable"));
    }
    origin(record)
}
