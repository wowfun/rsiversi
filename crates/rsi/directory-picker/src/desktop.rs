#[cfg(any(unix, windows))]
use rsi_directory_picker_api::{Failure, Result};
#[cfg(any(unix, windows))]
use std::{ffi::OsStr, path::Path, process::Stdio};
#[cfg(any(unix, windows))]
use tokio::process::Command;
#[cfg(any(unix, windows))]
use tokio_util::sync::CancellationToken;

fn present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

#[cfg(target_os = "linux")]
fn wsl() -> bool {
    static DETECTED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        present("WSL_INTEROP")
            || present("WSL_DISTRO_NAME")
            || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|release| wsl_kernel(&release))
    });
    *DETECTED
}

#[cfg(target_os = "linux")]
fn wsl_kernel(release: &str) -> bool {
    release.to_ascii_lowercase().contains("microsoft")
}

#[cfg(not(target_os = "linux"))]
fn wsl() -> bool {
    false
}

pub(super) fn available() -> bool {
    cfg!(any(windows, target_os = "macos"))
        || (cfg!(target_os = "linux")
            && (wsl() || present("DISPLAY") || present("WAYLAND_DISPLAY")))
}

#[cfg(any(unix, windows))]
fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for key in [
        "PATH",
        "HOME",
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
        "WSL_INTEROP",
        "WSL_DISTRO_NAME",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

#[cfg(any(unix, windows))]
fn explorer_uri(path: &str) -> Result<String> {
    use std::fmt::Write;
    let path = path.replace('\\', "/");
    let (prefix, rest) = if let Some(unc) = path.strip_prefix("//") {
        ("file://", unc)
    } else if path.as_bytes().get(1..3) == Some(b":/") && path.as_bytes()[0].is_ascii_alphabetic() {
        ("file:///", path.as_str())
    } else {
        return Err(Failure::Invalid);
    };
    let mut uri = String::from(prefix);
    for ch in rest.chars() {
        // Explorer parses comma and equals as field separators and requires literal Unicode in file URLs.
        if !ch.is_ascii() || ch.is_ascii_alphanumeric() || "-._~/:".contains(ch) {
            uri.push(ch);
        } else {
            write!(uri, "%{:02X}", u32::from(ch)).expect("writing to a string");
        }
    }
    Ok(uri)
}

#[cfg(any(unix, windows))]
fn launch_failure() -> Failure {
    Failure::Io {
        message: "The Host file manager could not reveal the directory".into(),
    }
}

#[cfg(any(unix, windows))]
pub(super) async fn reveal(path: String, stop: CancellationToken) -> Result<()> {
    let checking = stop.clone();
    let directory =
        tokio::task::spawn_blocking(move || super::native::desktop_directory(&path, &checking))
            .await
            .map_err(|_| launch_failure())??;
    if stop.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    let explorer = cfg!(windows) || wsl();
    let mut launch = if explorer {
        let (program, path) = if cfg!(windows) {
            let root = std::env::var_os("SystemRoot").ok_or(Failure::DesktopUnavailable)?;
            (
                Path::new(&root).join("explorer.exe"),
                directory.to_str().ok_or(Failure::Invalid)?.to_owned(),
            )
        } else {
            let mut translate = command("wslpath");
            translate.arg("-w").arg(&directory).stdout(Stdio::piped());
            let translated = tokio::select! {biased;
                () = stop.cancelled() => return Err(Failure::Cancelled),
                output = translate.output() => output.map_err(|_| launch_failure())?,
            };
            if !translated.status.success() {
                return Err(launch_failure());
            }
            let path = std::str::from_utf8(&translated.stdout).map_err(|_| Failure::Invalid)?;
            (
                "explorer.exe".into(),
                path.trim_end_matches(['\r', '\n']).to_owned(),
            )
        };
        let mut cmd = command(program);
        cmd.arg("/select,").arg(explorer_uri(&path)?);
        cmd
    } else if cfg!(target_os = "macos") {
        let mut cmd = command("/usr/bin/open");
        cmd.arg("-R").arg(&directory);
        cmd
    } else {
        let mut cmd = command("xdg-open");
        cmd.arg(directory.parent().unwrap_or(&directory));
        cmd
    };
    let status = tokio::select! {biased;
        () = stop.cancelled() => return Err(Failure::Cancelled),
        status = launch.status() => status.map_err(|_| launch_failure())?,
    };
    if status.success() || (explorer && status.code() == Some(1)) {
        Ok(())
    } else {
        Err(launch_failure())
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn wsl_services_without_shell_variables_use_the_windows_desktop() {
        assert!(wsl_kernel("6.6.87.2-microsoft-standard-WSL2\n"));
        assert!(wsl_kernel("4.4.0-19041-Microsoft"));
        assert!(!wsl_kernel("6.8.0-51-generic"));
    }
    #[test]
    fn explorer_receives_one_encoded_shell_item_including_wsl_and_unicode() {
        assert_eq!(
            explorer_uri(r"C:\work\资料,a=b #100%").unwrap(),
            "file:///C:/work/资料%2Ca%3Db%20%23100%25"
        );
        assert_eq!(
            explorer_uri(r"\\wsl.localhost\Ubuntu\home\my files").unwrap(),
            "file://wsl.localhost/Ubuntu/home/my%20files"
        );
        assert!(explorer_uri("relative").is_err());
    }
}
