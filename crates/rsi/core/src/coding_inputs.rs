//! Product launcher discovery, frozen before composing ordinary shell factories.

use crate::{Result, RsiError};
use std::path::{Path, PathBuf};

/// Captures the platform shell invocation without resolving executable aliases.
/// Windows prefers PowerShell 7, then PATH entries, then Windows PowerShell 5.1.
/// Linux uses `/bin/bash`. Discovery never occurs during plugin activation.
pub fn capture_standard_shell() -> Result<PathBuf> {
    #[cfg(windows)]
    let candidates = windows_shell_candidates(
        &std::env::var_os("ProgramFiles")
            .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from),
        std::env::var_os("PATH").as_deref(),
        &std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from),
    );
    #[cfg(not(windows))]
    let candidates = vec![PathBuf::from("/bin/bash")];
    candidates.into_iter().find(|path| executable_entry(path)).ok_or_else(|| {
        RsiError::Boot("standard native shell is unavailable; install PowerShell on Windows or Bash on Linux".into())
    })
}

fn executable_entry(path: &Path) -> bool {
    path.is_absolute()
        && std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file() || metadata.is_symlink())
}

#[cfg(windows)]
fn windows_shell_candidates(
    program_files: &Path,
    path: Option<&std::ffi::OsStr>,
    system_root: &Path,
) -> Vec<PathBuf> {
    let mut candidates = vec![program_files.join("PowerShell/7/pwsh.exe")];
    if let Some(path) = path {
        candidates.extend(
            std::env::split_paths(path)
                .filter(|entry| entry.is_absolute())
                .map(|entry| entry.join("pwsh.exe")),
        );
    }
    candidates.push(system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    candidates
}

/// Captures Git independently of the shell. Absence does not disable shell tools.
pub fn capture_standard_git() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use winreg::{
            RegKey,
            enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
        };
        [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
            .into_iter()
            .find_map(|hive| {
                let install = RegKey::predef(hive)
                    .open_subkey("SOFTWARE\\GitForWindows")
                    .ok()?
                    .get_value::<String, _>("InstallPath")
                    .ok()?;
                let path = PathBuf::from(install).join("bin/git.exe");
                executable_entry(&path).then_some(path)
            })
    }
    #[cfg(not(windows))]
    {
        let path = PathBuf::from("/usr/bin/git");
        executable_entry(&path).then_some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_entry_rejects_relative_paths_and_directories() {
        assert!(!executable_entry(Path::new(".")));
        let root = tempfile::tempdir().unwrap();
        assert!(!executable_entry(root.path()));
        let file = root.path().join("shell");
        assert!(!executable_entry(&file));
        std::fs::write(&file, b"fixture").unwrap();
        assert!(executable_entry(&file));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_candidates_preserve_precedence_quotes_and_alias_spelling() {
        let candidates = windows_shell_candidates(
            Path::new(r"D:\Programs"),
            Some(std::ffi::OsStr::new(
                r#";relative;"C:\User\WindowsApps";D:\Custom"#,
            )),
            Path::new(r"D:\Windows"),
        );
        assert_eq!(
            candidates,
            [
                PathBuf::from(r"D:\Programs\PowerShell\7\pwsh.exe"),
                PathBuf::from(r"C:\User\WindowsApps\pwsh.exe"),
                PathBuf::from(r"D:\Custom\pwsh.exe"),
                PathBuf::from(r"D:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "opt-in installed native PowerShell discovery and subprocess"]
    fn windows_standard_shell_runs_the_selected_native_executable() {
        let shell = capture_standard_shell().unwrap();
        assert!(shell.is_absolute());
        let output = std::process::Command::new(shell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$PSVersionTable.PSVersion.ToString()",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
    }
}
