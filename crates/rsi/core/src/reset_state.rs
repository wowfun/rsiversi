use rsi_agent_store_sqlite::SqliteStoreResetReceipt;

pub(super) const HELP: &str = "\nLauncher option:\n  --reset-state   Back up and clear Agent session state; preserve configuration and credentials\n                 Example: rsi tui --reset-state\n";

#[allow(
    clippy::unnecessary_debug_formatting,
    reason = "Escape filesystem paths in terminal diagnostics."
)]
pub(super) fn report(receipt: Option<&SqliteStoreResetReceipt>) {
    let Some(receipt) = receipt else {
        return;
    };
    if let Some(backup) = &receipt.backup {
        eprintln!(
            "Agent Store reset: root {:?}; previous state preserved at {:?}",
            receipt.root, backup
        );
    }
}

#[cfg(target_os = "linux")]
pub(super) const MAX_RECEIPT_BYTES: usize = 64 * 1024;

#[cfg(target_os = "linux")]
pub(super) fn inherited_pipe() -> rsi::Result<std::fs::File> {
    use std::os::fd::AsFd as _;
    let pipe = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|error| rsi::RsiError::Boot(format!("retain reset receipt pipe: {error}")))?;
    // Keep the receipt descriptor private; plugin children inherit the daemon log.
    rustix::io::fcntl_setfd(&pipe, rustix::io::FdFlags::CLOEXEC)
        .and_then(|()| rustix::stdio::dup2_stdout(std::io::stderr()))
        .map_err(|error| rsi::RsiError::Boot(format!("isolate reset receipt pipe: {error}")))?;
    Ok(pipe.into())
}

#[cfg(target_os = "linux")]
pub(super) fn publish(
    receipt: Option<&SqliteStoreResetReceipt>,
    pipe: Option<std::fs::File>,
) -> rsi::Result<()> {
    use std::io::Write as _;
    report(receipt);
    if let Some(mut pipe) = pipe {
        let bytes =
            serde_json::to_vec(&receipt).map_err(|error| rsi::RsiError::Boot(error.to_string()))?;
        if bytes.len() >= MAX_RECEIPT_BYTES {
            return Err(rsi::RsiError::Boot(
                "reset receipt exceeds startup pipe limit; inspect daemon log".into(),
            ));
        }
        pipe.write_all(&bytes)
            .and_then(|()| pipe.write_all(b"\n"))
            .and_then(|()| pipe.flush())
            .map_err(|error| rsi::RsiError::Boot(format!("send reset receipt: {error}")))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) async fn read(
    pipe: std::process::ChildStdout,
) -> rsi::Result<Option<SqliteStoreResetReceipt>> {
    let boot = |error: std::io::Error| rsi::RsiError::Boot(format!("read reset receipt: {error}"));
    let flags = rustix::fs::fcntl_getfl(&pipe).map_err(|error| boot(error.into()))?;
    rustix::fs::fcntl_setfl(&pipe, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|error| boot(error.into()))?;
    let pipe = tokio::io::unix::AsyncFd::new(pipe).map_err(boot)?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    while bytes.len() < MAX_RECEIPT_BYTES {
        let remaining = (MAX_RECEIPT_BYTES - bytes.len()).min(chunk.len());
        let mut ready = pipe.readable().await.map_err(boot)?;
        let count = match ready.try_io(|pipe| {
            rustix::io::read(pipe.get_ref(), &mut chunk[..remaining]).map_err(std::io::Error::from)
        }) {
            Ok(result) => result.map_err(boot)?,
            Err(_) => continue,
        };
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.contains(&b'\n') {
            break;
        }
    }
    if bytes.is_empty() {
        // A child can fail before reset startup exists. Let the launcher report
        // its exit status and log location through the normal readiness path.
        return Ok(None);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(rsi::RsiError::Boot(
            "daemon ended without a complete reset receipt; inspect daemon log".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| rsi::RsiError::Boot(format!("invalid reset receipt: {error}")))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_receipt_defers_to_daemon_exit_while_partial_frames_still_fail() {
        for (script, empty) in [("exit 17", true), ("printf null; exit 17", false)] {
            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", script])
                .env_clear()
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let result = read(child.stdout.take().unwrap()).await;
            assert_eq!(child.wait().unwrap().code(), Some(17));
            if empty {
                assert!(result.unwrap().is_none());
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("complete reset receipt")
                );
            }
        }
    }
}
