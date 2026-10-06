use rsi_sandbox::{IsolatedProcessRequest, Result, SandboxError};
use std::{
    path::{Component, Path},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt as _;

fn unavailable(message: impl Into<String>) -> SandboxError {
    SandboxError::Probe(message.into())
}

pub(super) async fn verify(request: &IsolatedProcessRequest, user_runtime: &Path) -> Result<()> {
    request.validate()?;
    if !user_runtime.is_absolute() {
        return Err(unavailable(
            "private user-manager directory must be absolute",
        ));
    }
    let work = async {
        loop {
            let mut child =
                tokio::process::Command::new(request.supervisor.with_file_name("systemctl"))
                    .env_clear()
                    .env("XDG_RUNTIME_DIR", user_runtime)
                    .env(
                        "DBUS_SESSION_BUS_ADDRESS",
                        format!("unix:path={}/bus", user_runtime.display()),
                    )
                    .args([
                        "--user",
                        "show",
                        "--property=ActiveState,ControlGroup,KillMode,RuntimeMaxUSec",
                    ])
                    .arg(format!("{}.service", request.unit))
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .stdout(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| unavailable(e.to_string()))?;
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .ok_or_else(|| unavailable("missing supervisor response"))?
                .take(4097)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            if bytes.len() > 4096 {
                return Err(unavailable("supervisor response exceeds bound"));
            }
            let status = child.wait().await.map_err(|e| unavailable(e.to_string()))?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| unavailable("invalid supervisor response"))?;
            if status.success() && text.lines().any(|line| line == "ActiveState=active") {
                return verify_effective(Path::new("/sys/fs/cgroup"), text);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .map_err(|_| unavailable("isolated resource verification timed out"))?
}

fn verify_effective(root: &Path, properties: &str) -> Result<()> {
    let property = |name: &str| {
        properties.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key == name).then_some(value)
        })
    };
    if !root.join("cgroup.controllers").is_file()
        || property("KillMode") != Some("control-group")
        || !matches!(
            property("RuntimeMaxUSec"),
            Some("10min" | "600000000" | "600000000us")
        )
    {
        return Err(unavailable(
            "unified cgroup and bounded supervisor policy required",
        ));
    }
    let group = property("ControlGroup").ok_or_else(|| unavailable("missing owned cgroup"))?;
    if group.len() > 4096
        || !group.starts_with('/')
        || group == "/"
        || Path::new(group)
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(unavailable("invalid owned cgroup"));
    }
    for (file, ceiling) in [("memory.max", 1_073_741_824u64), ("pids.max", 256)] {
        let path = root.join(group.trim_start_matches('/')).join(file);
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(
                std::fs::File::open(path)
                    .map_err(|_| unavailable(format!("missing effective {file}")))?,
                65,
            ),
            &mut bytes,
        )
        .map_err(|_| unavailable(format!("unreadable effective {file}")))?;
        let limit = std::str::from_utf8(&bytes)
            .ok()
            .filter(|_| bytes.len() <= 64)
            .and_then(|s| s.trim().parse::<u64>().ok());
        if !limit.is_some_and(|value| value > 0 && value <= ceiling) {
            return Err(unavailable(format!(
                "effective {file} exceeds isolated bound"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_controller_limits_are_required_not_requested_properties() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("scope")).unwrap();
        let properties = "ActiveState=active\nControlGroup=/scope\nKillMode=control-group\nRuntimeMaxUSec=10min\n";
        assert!(verify_effective(root.path(), properties).is_err());
        std::fs::write(root.path().join("cgroup.controllers"), "memory pids").unwrap();
        std::fs::write(root.path().join("scope/memory.max"), "1073741824\n").unwrap();
        std::fs::write(root.path().join("scope/pids.max"), "256\n").unwrap();
        verify_effective(root.path(), properties).unwrap();
        for (name, bad) in [
            ("memory.max", "max"),
            ("memory.max", "1073741825"),
            ("pids.max", "257"),
            ("pids.max", "0"),
        ] {
            let path = root.path().join("scope").join(name);
            let old = std::fs::read(&path).unwrap();
            std::fs::write(&path, bad).unwrap();
            assert!(
                verify_effective(root.path(), properties).is_err(),
                "{name}={bad}"
            );
            std::fs::write(path, old).unwrap();
        }
        assert!(verify_effective(root.path(), &properties.replace("/scope", "/../scope")).is_err());
        assert!(verify_effective(root.path(), &properties.replace("10min", "infinity")).is_err());
        assert!(
            verify_effective(root.path(), &properties.replace("control-group", "process")).is_err()
        );
        std::fs::remove_file(root.path().join("scope/pids.max")).unwrap();
        assert!(verify_effective(root.path(), properties).is_err());
    }
}
