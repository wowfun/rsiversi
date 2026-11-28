//! Resolve a detached Host executable and hold its managed generation through readiness.
use rsi::RsiError;
use std::path::{Path, PathBuf};

pub(super) fn executable(
    selected: Option<&Path>,
) -> rsi::Result<(PathBuf, Option<rsi_build_info::GenerationLease>)> {
    let Some(selected) = selected else {
        // main already holds the invoking executable's lease.
        return Ok((
            std::env::current_exe()
                .map(rsi_workspace_path::canonical_native_path)
                .map_err(failure)?,
            None,
        ));
    };
    let path = rsi_workspace_path::canonical_native_path(selected.canonicalize().map_err(failure)?);
    let parent = path
        .parent()
        .ok_or_else(|| failure("executable has no parent"))?;
    let lease = rsi_build_info::GenerationLease::open(parent).map_err(failure)?;
    Ok((path, lease))
}

#[cfg(windows)]
pub(super) fn detach(command: &mut std::process::Command) -> rsi::Result<()> {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, DETACHED_PROCESS};
    rsi_process_native::clear_standard_handle_inheritance().map_err(failure)?;
    command.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
    Ok(())
}

fn failure(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("Service Host executable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmanaged_host_runs_the_selected_executable_in_place() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("rsi.exe");
        std::fs::write(&source, b"unmanaged runtime").unwrap();
        let (path, lease) = executable(Some(&source)).unwrap();
        assert_eq!(path.canonicalize().unwrap(), source.canonicalize().unwrap());
        assert!(lease.is_none());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    }

    #[test]
    fn managed_runtime_stays_in_publication_and_pins_until_handoff() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("generations/00000000000000000001/bundle");
        std::fs::create_dir_all(bundle.join("nested")).unwrap();
        std::fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
        let source = bundle.join("rsi.exe");
        std::fs::write(&source, b"managed executable").unwrap();
        assert!(executable(Some(&source)).is_err());
        std::fs::create_dir(temp.path().join("leases")).unwrap();
        let file = std::fs::File::create(temp.path().join("leases/00000000000000000001")).unwrap();
        let (path, lease) = executable(Some(&bundle.join("nested/../rsi.exe"))).unwrap();
        assert_eq!(path.canonicalize().unwrap(), source.canonicalize().unwrap());
        assert!(matches!(
            file.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(lease);
        file.try_lock().unwrap();
    }
}
