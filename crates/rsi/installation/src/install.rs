use crate::{
    Result, error, filesystem as fs, path_receipt,
    record::{Layout, Phase, Record, Role},
};
use std::{path::PathBuf, process::Stdio, time::Duration};

#[derive(Debug)]
pub struct InstallOptions {
    pub prefix: PathBuf,
    pub binary: PathBuf,
    pub uninstaller: PathBuf,
    pub launcher: PathBuf,
    pub no_path: bool,
}
pub async fn probe(path: &std::path::Path) -> Result<()> {
    probe_candidate(path, true).await
}
pub(crate) async fn probe_candidate(path: &std::path::Path, protocol: bool) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new(path);
    command
        .arg(if protocol {
            "--installation-protocol"
        } else {
            "--version"
        })
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for key in ["SystemRoot", "WINDIR", "PATH", "PATHEXT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|e| error(format!("candidate executable could not start: {e}")))?;
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| error("probe stdout unavailable"))?
        .take(4097);
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        output.read_to_end(&mut bytes).await?;
        if bytes.len() > 4096 {
            return Err(error("installation probe output exceeds bound"));
        }
        if !child.wait().await?.success() {
            return Err(error("candidate executable failed its bounded probe"));
        }
        if !protocol {
            return Ok(());
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
        if value != serde_json::from_str::<serde_json::Value>(crate::PROTOCOL).map_err(error)? {
            return Err(error("incompatible installation protocol"));
        }
        Ok(())
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result.map_err(|_| error("installation probe timed out"))?
}
#[allow(clippy::too_many_lines)] // One ownership scope spans staging, publication, PATH receipts and failure cleanup.
pub async fn install(options: InstallOptions) -> Result<()> {
    let layout = Layout::new(&options.prefix)?;
    layout.preflight()?;
    for input in [&options.binary, &options.uninstaller, &options.launcher] {
        fs::source(input)?;
    }
    eprintln!("[rsi] Verifying core and uninstaller protocols...");
    probe(&options.binary).await?;
    probe(&options.uninstaller).await?;
    let bin = fs::Lease::acquire(&layout.bin, true, false)?;
    let control = fs::Lease::acquire(&layout.control, true, false)?;
    layout.preflight()?;
    layout.reject_pending()?;
    eprintln!("[rsi] Copying installation files...");
    let id = fs::id()?;
    let mut staged = Vec::new();
    let mut published = Vec::new();
    let mut owns_record = false;
    let result = (|| -> Result<Record> {
        let record = Record {
            format: 1,
            id: id.clone(),
            prefix: layout.prefix.clone(),
            phase: Phase::Installing,
            core: fs::digest_file(fs::source(&options.binary)?)?,
            uninstaller: fs::digest_file(fs::source(&options.uninstaller)?)?,
            launcher: fs::digest_file(fs::source(&options.launcher)?)?,
            path_edits: vec![],
        };
        record.save(&control.directory, false)?;
        owns_record = true;
        for (role, input) in [
            (Role::Uninstaller, &options.uninstaller),
            (Role::Launcher, &options.launcher),
            (Role::Core, &options.binary),
        ] {
            let dir = if role == Role::Core {
                &bin.directory
            } else {
                &control.directory
            };
            let stage = format!(".rsi-install-{id}-{}", role.name());
            let copied = fs::copy(dir, &stage, input, role != Role::Launcher || cfg!(unix))?;
            staged.push((role, stage.clone()));
            let expected = match role {
                Role::Core => &record.core,
                Role::Uninstaller => &record.uninstaller,
                _ => &record.launcher,
            };
            if &copied != expected {
                return Err(error(
                    "source changed after installation intent was recorded",
                ));
            }
        }
        for (role, stage) in &staged {
            let dir = if *role == Role::Core {
                &bin.directory
            } else {
                &control.directory
            };
            fs::rename(dir, stage, role.name(), false)?;
            published.push(*role);
            dir.sync_all()?;
        }
        Ok(record)
    })();
    for (role, stage) in &staged {
        let dir = if *role == Role::Core {
            &bin.directory
        } else {
            &control.directory
        };
        if fs::kind(dir, stage)?.is_some() {
            dir.remove_child(std::path::Path::new(stage))?;
        }
    }
    let mut record = match result {
        Ok(record) => record,
        Err(failure) => {
            for role in published.into_iter().rev() {
                let dir = if role == Role::Core {
                    &bin.directory
                } else {
                    &control.directory
                };
                if let Err(cleanup) = dir.remove_child(std::path::Path::new(role.name())) {
                    return Err(error(format!(
                        "{failure}; cleanup of {} failed: {cleanup}; retain the record and retry uninstall",
                        role.name()
                    )));
                }
            }
            if owns_record {
                control
                    .directory
                    .remove_child(std::path::Path::new(crate::record::RECORD))?;
            }
            return Err(failure);
        }
    };
    record.phase = Phase::Installed;
    record.save(&control.directory, true)?;
    if !options.no_path {
        eprintln!("[rsi] Updating user PATH...");
        match path_receipt::planned(&record.id, &layout.bin) {
            Ok(edits) => {
                record.path_edits = edits;
                record.save(&control.directory, true)?;
                for index in 0..record.path_edits.len() {
                    match path_receipt::apply(&record.path_edits[index], false) {
                        Ok(()) => {
                            record.path_edits[index].applied = true;
                            record.save(&control.directory, true)?;
                        }
                        Err(e) => eprintln!(
                            "Installed; PATH setup skipped: {e}. Add {} manually.",
                            layout.bin.display()
                        ),
                    }
                }
            }
            Err(e) => eprintln!(
                "Installed; PATH setup unavailable: {e}. Add {} manually.",
                layout.bin.display()
            ),
        }
    }
    println!(
        "Installed {}\nUninstall with {}",
        layout.target(Role::Core).display(),
        layout.target(Role::Launcher).display()
    );
    Ok(())
}
