use std::{ffi::OsString, path::PathBuf};

pub(super) const HELP: &str = "RSIversi Desktop\nLaunch with rsi desktop. Internal invocation:\nrsi-desktop --config-home DIR --state-home DIR --cache-home DIR [--assets DIR]\nAll directories must be absolute; the selected Host must already be running.";
pub(super) struct Options {
    pub frontend: bool,
    pub paths: rsi_host::HostPaths,
    pub assets: rsi_web_control::Assets,
    package: Option<(String, String, String)>,
}
pub(super) fn parse(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Option<Options>, String> {
    let args: Vec<_> = arguments.into_iter().collect();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        return Ok(None);
    }
    let mut values = std::collections::BTreeMap::new();
    let mut package = None;
    let mut export = None;
    let mut digest = None;
    let mut frontend = false;
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| "Invalid Desktop option")?;
        if key == "--frontend" {
            if frontend {
                return Err("Duplicate frontend mode".into());
            }
            frontend = true;
            continue;
        }
        if matches!(key.as_str(), "--package" | "--export" | "--package-digest") {
            let selected = match key.as_str() {
                "--package" => &mut package,
                "--export" => &mut export,
                _ => &mut digest,
            };
            if selected.is_some() {
                return Err("Duplicate executable package option".into());
            }
            *selected = Some(
                args.next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or("Package option requires an identity")?,
            );
            continue;
        }
        if !matches!(
            key.as_str(),
            "--config-home" | "--state-home" | "--cache-home" | "--assets"
        ) || values.contains_key(&key)
        {
            return Err("Unknown or duplicate Desktop option; use rsi desktop".into());
        }
        let mut path = PathBuf::from(args.next().ok_or("Desktop option requires a directory")?);
        if key == "--assets" && !path.is_absolute() {
            path = std::env::current_dir().map_err(crate::error)?.join(path);
        }
        if !path.is_absolute() {
            return Err("Desktop requires resolved absolute directories".into());
        }
        values.insert(key, path);
    }
    let mut required = |key| {
        values
            .remove(key)
            .ok_or_else(|| format!("{key} is required; launch through rsi desktop"))
    };
    let paths = rsi_host::HostPaths::new(
        required("--config-home")?,
        required("--state-home")?,
        required("--cache-home")?,
    )
    .map_err(crate::error)?;
    let assets = values
        .remove("--assets")
        .map_or_else(rsi_web_control::Assets::default, |path| {
            rsi_web_control::Assets::Directory { path }
        });
    let package = match (package, export, digest) {
        (None, None, None) => None,
        (Some(package), Some(export), Some(digest)) => Some((package, export, digest)),
        _ => return Err("Executable package, digest and export must be supplied together".into()),
    };
    if frontend && package.is_none() {
        return Err("Frontend mode requires a managed Desktop package".into());
    }
    Ok(Some(Options {
        frontend,
        paths,
        assets,
        package,
    }))
}
impl Options {
    pub fn managed(&self) -> bool {
        self.package.is_some()
    }
    pub fn pin_package(&self) -> Result<Option<rsi_packages::PackageLease>, String> {
        let Some((package, export, digest)) = &self.package else {
            return Ok(None);
        };
        let lease = rsi_packages::PackageStore::open(&self.paths.state().join("packages"))
            .map_err(crate::error)?
            .acquire_exact(package, digest)
            .map_err(crate::error)?;
        let executable = lease
            .export(export, rsi_packages::ExportKind::Executable)
            .map_err(crate::error)?;
        if executable.canonicalize().map_err(crate::error)?
            != std::env::current_exe()
                .map_err(crate::error)?
                .canonicalize()
                .map_err(crate::error)?
        {
            return Err("Desktop executable is not the selected package export".into());
        }
        Ok(Some(lease))
    }
}
#[cfg(any(windows, test))]
pub(super) fn require_webview(
    version: Result<String, impl std::fmt::Display>,
) -> Result<(), String> {
    version.map(|_|()).map_err(|error| format!("Microsoft Edge WebView2 Runtime is unavailable: {error}. Install the Evergreen Runtime from https://developer.microsoft.com/microsoft-edge/webview2/ and launch RSIversi Desktop again."))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_does_not_require_homes_and_partial_or_duplicate_homes_fail() {
        assert!(parse(["--help".into()]).unwrap().is_none());
        assert!(parse([]).is_err());
        assert!(parse(["--state-home".into(), "relative".into()]).is_err());
        let root = std::env::current_dir().unwrap();
        assert!(
            parse([
                "--state-home".into(),
                root.clone().into_os_string(),
                "--state-home".into(),
                root.into_os_string()
            ])
            .is_err()
        );
    }
    #[test]
    fn webview_failure_has_recovery_guidance() {
        assert!(
            require_webview(Err::<String, _>("missing"))
                .unwrap_err()
                .contains("Evergreen Runtime")
        );
    }
}
