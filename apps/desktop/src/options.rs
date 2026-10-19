use std::{ffi::OsString, path::PathBuf};

pub(super) const HELP: &str = "RSIversi Desktop\nUsage: rsi-desktop [--assets ABSOLUTE_DIRECTORY] [--host-profile standard]\nAssets default to the assets directory beside this executable.\nWindows requires the Microsoft Edge WebView2 Runtime.";

pub(super) fn parse(
    arguments: impl IntoIterator<Item = OsString>,
    executable: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<Option<(PathBuf, rsi::HostProfileId)>, String> {
    let arguments: Vec<_> = arguments.into_iter().collect();
    if arguments
        .iter()
        .any(|value| value == "--help" || value == "-h")
    {
        return Ok(None);
    }
    let mut arguments = arguments.into_iter();
    let mut assets = None;
    let mut host_profile = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--assets") if assets.is_none() => {
                let path = PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--assets requires an absolute directory")?,
                );
                if !path.is_absolute() {
                    return Err("--assets requires an absolute directory".into());
                }
                assets = Some(path);
            }
            Some("--host-profile") if host_profile.is_none() => {
                host_profile = Some(
                    arguments
                        .next()
                        .and_then(|v| v.into_string().ok())
                        .ok_or("--host-profile requires an identity")?,
                );
            }
            _ => return Err("Unknown or duplicate desktop argument; use --help".into()),
        }
    }
    let assets = match assets {
        Some(path) => path,
        None => executable()?
            .parent()
            .ok_or("Desktop executable directory is absent")?
            .join("assets"),
    };
    let profile = rsi::HostProfileId::new(host_profile.unwrap_or_else(|| "standard".into()))
        .map_err(|e| e.to_string())?;
    Ok(Some((assets, profile)))
}

pub(super) fn require_assets(assets: &std::path::Path) -> Result<(), String> {
    if !assets.is_dir() {
        return Err(format!(
            "Desktop assets directory is missing or inaccessible: {}. Use a complete Desktop publication, or --assets ABSOLUTE_DIRECTORY.",
            assets.display()
        ));
    }
    Ok(())
}

#[cfg(any(windows, test))]
pub(super) fn require_webview(
    version: Result<String, impl std::fmt::Display>,
) -> Result<(), String> {
    version.map(|_| ()).map_err(|e| format!("Microsoft Edge WebView2 Runtime is unavailable: {e}. Install the Evergreen Runtime from https://developer.microsoft.com/microsoft-edge/webview2/ and launch RSIversi Desktop again."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_non_directory_assets_fail_with_recovery_guidance() {
        let root = tempfile::tempdir().unwrap();
        let executable = || Ok(root.path().join("rsi-desktop"));
        for as_file in [false, true] {
            if as_file {
                std::fs::write(root.path().join("assets"), "not a directory").unwrap();
            }
            for arguments in [
                vec![],
                vec![
                    "--assets".into(),
                    root.path().join("assets").into_os_string(),
                ],
            ] {
                let assets = parse(arguments, executable).unwrap().unwrap().0;
                let error = require_assets(&assets).unwrap_err();
                assert!(error.contains("Desktop assets directory") && error.contains("--assets"));
                assert!(error.contains(root.path().to_str().unwrap()));
            }
        }
        assert!(!root.path().join("desktop-webview").exists());
    }

    #[test]
    fn default_assets_are_executable_relative_and_overrides_are_absolute() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("desktop options space 中文");
        std::fs::create_dir_all(directory.join("assets")).unwrap();
        let executable = || Ok(directory.join("rsi-desktop"));
        assert_eq!(
            parse([], executable).unwrap().unwrap().0,
            directory.join("assets")
        );
        assert_eq!(
            parse(
                ["--assets".into(), directory.clone().into_os_string()],
                executable
            )
            .unwrap()
            .unwrap()
            .0,
            directory
        );
        assert!(parse(["--assets".into(), "relative".into()], executable).is_err());
        for flag in ["--assets", "--host-profile"] {
            let value = if flag == "--assets" {
                directory.as_os_str().to_owned()
            } else {
                "standard".into()
            };
            assert!(parse([flag.into(), value.clone(), flag.into(), value], executable).is_err());
        }
    }

    #[test]
    fn help_precedes_executable_and_runtime_requirements() {
        assert!(
            parse(["--unknown".into(), "--help".into()], || panic!(
                "help resolved executable"
            ))
            .unwrap()
            .is_none()
        );
        assert!(require_webview(Ok::<_, String>("1.2.3".into())).is_ok());
        let error = require_webview(Err::<String, _>("not installed")).unwrap_err();
        assert!(error.contains("WebView2") && error.contains("Evergreen Runtime"));
    }
}
