use crate::{Layout, Result, error, filesystem as fs};
use rsi_service_host::InstallationOrigin;
/// Validates the original source even when the running image is a runtime copy.
pub fn current() -> Result<Option<InstallationOrigin>> {
    let executable = std::env::current_exe()?;
    let inherited = std::env::var_os("RSI_INSTALLATION_ORIGIN");
    let proposed: Option<InstallationOrigin> = if let Some(value) = inherited {
        if value.len() > 8192 {
            return Err(error("installation origin exceeds bound"));
        }
        Some(
            serde_json::from_str(
                value
                    .to_str()
                    .ok_or_else(|| error("invalid origin encoding"))?,
            )
            .map_err(error)?,
        )
    } else {
        discover(&executable)?
    };
    admit(&executable, proposed)
}
/// Admits an explicitly selected launch artifact independently of the parent's image.
pub fn for_executable(executable: &std::path::Path) -> Result<Option<InstallationOrigin>> {
    admit(executable, discover(executable)?)
}
fn discover(executable: &std::path::Path) -> Result<Option<InstallationOrigin>> {
    Ok(
        if executable
            .parent()
            .and_then(std::path::Path::file_name)
            .is_some_and(|name| name == "bin")
        {
            let layout = Layout::new(
                executable
                    .parent()
                    .and_then(std::path::Path::parent)
                    .ok_or_else(|| error("missing installation prefix"))?,
            )?;
            layout.record()?.map(|record| InstallationOrigin {
                id: record.id,
                prefix: layout.prefix,
            })
        } else {
            None
        },
    )
}
fn admit(
    executable: &std::path::Path,
    proposed: Option<InstallationOrigin>,
) -> Result<Option<InstallationOrigin>> {
    if let Some(origin) = proposed {
        origin.validate().map_err(error)?;
        let layout = Layout::new(&origin.prefix)?;
        let record = layout
            .record()?
            .ok_or_else(|| error("origin has no installation record"))?;
        if record.id != origin.id || record.core != fs::digest_file(fs::source(executable)?)? {
            return Err(error("installation origin does not match this executable"));
        }
        return Ok(Some(InstallationOrigin {
            id: origin.id,
            prefix: layout.prefix,
        }));
    }
    Ok(None)
}
