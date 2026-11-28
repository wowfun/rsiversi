use crate::{Layout, Result, error, filesystem as fs};
use rsi_service_host::InstallationOrigin;
use std::path::Path;

/// Admits the running executable from its own installation prefix.
pub fn current() -> Result<Option<InstallationOrigin>> {
    for_executable(&std::env::current_exe()?)
}

/// Resolves the installation that recorded this executable.
pub fn for_executable(executable: &Path) -> Result<Option<InstallationOrigin>> {
    let Some(bin) = executable
        .parent()
        .filter(|dir| dir.file_name().is_some_and(|name| name == "bin"))
    else {
        return Ok(None);
    };
    let layout = Layout::new(
        bin.parent()
            .ok_or_else(|| error("missing installation prefix"))?,
    )?;
    let Some(record) = layout.record()? else {
        return Ok(None);
    };
    if record.core != fs::digest_file(fs::source(executable)?)? {
        return Err(error("installation origin does not match this executable"));
    }
    let origin = InstallationOrigin {
        id: record.id,
        prefix: record.prefix,
    };
    origin.validate().map_err(error)?;
    Ok(Some(origin))
}
