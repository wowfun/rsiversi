use std::path::{Path, PathBuf};

pub(super) fn prepare(state: &Path) -> Result<PathBuf, String> {
    let data = state.join("desktop-webview");
    #[cfg(windows)]
    {
        use rsi_files_native_fs::{
            DirectoryCreationError, create_directory_no_follow, open_relative_directory_no_follow,
        };
        let root = rsi_files_native_fs::create_absolute_directory_and_sync_no_follow(state)
            .map_err(super::error)?;
        verify_private(&root, state)?;
        let child = match create_directory_no_follow(&root, std::ffi::OsStr::new("desktop-webview"))
        {
            Ok(child) => {
                child.sync_all().map_err(super::error)?;
                root.sync_all().map_err(super::error)?;
                child
            }
            Err(DirectoryCreationError::NotCreated(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                open_relative_directory_no_follow(&root, Path::new("desktop-webview"))
                    .map_err(super::error)?
            }
            Err(error) => return Err(super::error(error)),
        };
        verify_private(&child, &data)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&data).map_err(super::error)?;
        if std::fs::symlink_metadata(&data)
            .map_err(super::error)?
            .is_symlink()
        {
            return Err("WebView data directory cannot be a symlink".into());
        }
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))
            .map_err(super::error)?;
    }
    Ok(data)
}

#[cfg(windows)]
fn verify_private(directory: &rsi_files_native_fs::Directory, path: &Path) -> Result<(), String> {
    directory.verify_private().map_err(|error| format!(
        "Desktop state directory {} requires the canonical protected ACL (SYSTEM, Administrators and current user; full control with object/container inheritance): {error}. Choose a fresh XDG_STATE_HOME (rsi is appended), or have the directory owner restore this ACL. Existing permissions were not changed.",
        path.display()
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn first_launch_creates_private_state_before_webview_and_preserves_it() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state/rsi");
        let data = prepare(&state).unwrap();
        for path in [&state, &data] {
            rsi_files_native_fs::open_absolute_directory_no_follow(path)
                .unwrap()
                .verify_private()
                .unwrap();
        }
        std::fs::write(data.join("retained"), b"WebView data").unwrap();
        assert_eq!(prepare(&state).unwrap(), data);
        assert_eq!(
            std::fs::read(data.join("retained")).unwrap(),
            b"WebView data"
        );
    }

    #[test]
    fn existing_nonprivate_state_is_rejected_without_creating_webview() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let error = prepare(&state).unwrap_err();
        assert!(error.contains("canonical protected ACL") && error.contains("XDG_STATE_HOME"));
        assert!(!state.join("desktop-webview").exists());
    }

    #[test]
    fn user_only_acl_is_rejected_with_guidance_without_changing_state() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("user-only 中文");
        std::fs::create_dir(&state).unwrap();
        std::fs::write(state.join("retained"), b"operator data").unwrap();
        let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let acl = |set: bool| {
            let output = std::process::Command::new(&powershell)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    r#"
$ErrorActionPreference = 'Stop'
if ($env:RSI_ACL_SET -eq '1') {
  $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $acl = [System.Security.AccessControl.DirectorySecurity]::new()
  $acl.SetSecurityDescriptorSddlForm("D:P(A;OICI;FA;;;$sid)", [System.Security.AccessControl.AccessControlSections]::Access)
  [System.IO.Directory]::SetAccessControl($env:RSI_ACL_PATH, $acl)
}
[System.IO.Directory]::GetAccessControl($env:RSI_ACL_PATH).GetSecurityDescriptorSddlForm([System.Security.AccessControl.AccessControlSections]::Access)
"#,
                ])
                .env("RSI_ACL_PATH", &state)
                .env("RSI_ACL_SET", if set { "1" } else { "0" })
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        let before = acl(true);
        let error = prepare(&state).unwrap_err();
        assert!(error.contains(state.to_str().unwrap()) && error.contains("XDG_STATE_HOME"));
        assert!(error.contains("Existing permissions were not changed"));
        assert_eq!(acl(false), before);
        assert_eq!(
            std::fs::read(state.join("retained")).unwrap(),
            b"operator data"
        );
        assert!(!state.join("desktop-webview").exists());
    }
}
