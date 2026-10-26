//! Publish the executable before spawning so a resident Host never locks build output.
use rsi::RsiError;
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

// Linux embeds DWARF in the debug ELF; Windows normally keeps it in a sidecar PDB.
const MAXIMUM_EXECUTABLE: u64 = 4 * 1024 * 1024 * 1024;

#[cfg(windows)]
pub(super) fn publish(paths: &rsi_host::HostPaths) -> rsi::Result<PathBuf> {
    Ok(publish_before(
        paths,
        None,
        std::time::Instant::now() + rsi::APPLICATION_TRANSITION_TIMEOUT,
    )?
    .path)
}
pub(super) struct PublishedRuntime {
    pub(super) path: PathBuf,
    generation: Option<rsi_build_info::GenerationLease>,
}
impl PublishedRuntime {
    pub(super) fn collect(&self, paths: &rsi_host::HostPaths) -> rsi::Result<()> {
        collect(
            paths,
            self.generation.is_none().then_some(self.path.as_path()),
        )
    }
}
pub(super) fn publish_before(
    paths: &rsi_host::HostPaths,
    source: Option<&Path>,
    deadline: std::time::Instant,
) -> rsi::Result<PublishedRuntime> {
    let source = source
        .map_or_else(std::env::current_exe, |path| Ok(path.to_owned()))
        .map_err(failure)?;
    let source = std::fs::canonicalize(source).map_err(failure)?;
    check_deadline(deadline)?;
    let parent = source
        .parent()
        .ok_or_else(|| failure("runtime source has no parent"))?;
    if let Some(generation) = rsi_build_info::GenerationLease::open(parent).map_err(failure)? {
        return Ok(PublishedRuntime {
            path: source,
            generation: Some(generation),
        });
    }
    Ok(PublishedRuntime {
        path: publish_source_before(paths, &source, deadline)?,
        generation: None,
    })
}
fn check_deadline(deadline: std::time::Instant) -> rsi::Result<()> {
    if std::time::Instant::now() >= deadline {
        return Err(failure("runtime publication deadline elapsed"));
    }
    Ok(())
}

fn digest(mut source: std::fs::File, deadline: std::time::Instant) -> rsi::Result<String> {
    let metadata = source.metadata().map_err(failure)?;
    if !metadata.is_file() {
        return Err(failure("runtime source is not a file"));
    }
    if metadata.len() > MAXIMUM_EXECUTABLE {
        return Err(failure("runtime exceeds 4 GiB"));
    }
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        check_deadline(deadline)?;
        let length = source.read(&mut buffer).map_err(failure)?;
        if length == 0 {
            break;
        }
        total += length as u64;
        if total > MAXIMUM_EXECUTABLE {
            return Err(failure("runtime exceeds 4 GiB"));
        }
        hash.update(&buffer[..length]);
    }
    Ok(hex::encode(hash.finalize()))
}

#[allow(clippy::too_many_lines)] // Keep bounded verification, staging and atomic publication in one transaction.
fn publish_source_before(
    paths: &rsi_host::HostPaths,
    source: &Path,
    deadline: std::time::Instant,
) -> rsi::Result<PathBuf> {
    let source_parent = rsi_files_native_fs::open_absolute_directory_no_follow(
        source
            .parent()
            .ok_or_else(|| failure("runtime source has no parent"))?,
    )
    .map_err(failure)?;
    let source_name = Path::new(
        source
            .file_name()
            .ok_or_else(|| failure("runtime source has no name"))?,
    );
    let source_file = || {
        rsi_files_native_fs::open_relative_file_no_follow(&source_parent, source_name)
            .map_err(failure)
    };
    let expected = digest(source_file()?, deadline)?;
    let root = paths
        .state()
        .join("session-host")
        .join("artifacts")
        .join(&expected);
    let directory =
        rsi_files_native_fs::create_absolute_directory_no_follow(&root).map_err(failure)?;
    #[cfg(windows)]
    let directory = {
        drop(directory);
        rsi_files_native_fs::set_private_directory_permissions_no_follow(&root).map_err(failure)?
    };
    #[cfg(windows)]
    let namespace = directory.pin_namespace().map_err(failure)?;
    let name = Path::new(if cfg!(windows) { "rsi.exe" } else { "rsi" });
    match rsi_files_native_fs::open_relative_file_no_follow(&directory, name) {
        Ok(file) => {
            if digest(file, deadline)? != expected {
                return Err(failure("cached runtime digest differs"));
            }
            return Ok(root.join(name));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failure(error)),
    }
    let staging = format!(
        "staging-{}",
        rsi_api_protocol::HostEpoch::generate()
            .map_err(failure)?
            .as_str()
    );
    let _staging = Staging {
        directory: &directory,
        name: &staging,
    };
    let mut output = rsi_files_native_fs::create_relative_private_file_no_follow(
        &directory,
        Path::new(&staging),
    )
    .map_err(failure)?;
    let mut input = source_file()?;
    let mut buffer = vec![0; 64 * 1024];
    let mut copied = 0_u64;
    loop {
        check_deadline(deadline)?;
        let count = input.read(&mut buffer).map_err(failure)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        if copied > MAXIMUM_EXECUTABLE {
            return Err(failure("runtime grew beyond 4 GiB"));
        }
        output.write_all(&buffer[..count]).map_err(failure)?;
    }
    output.flush().map_err(failure)?;
    output.sync_all().map_err(failure)?;
    drop(output);
    let staged = rsi_files_native_fs::open_relative_file_no_follow(&directory, Path::new(&staging))
        .map_err(failure)?;
    if digest(staged, deadline)? != expected {
        return Err(failure("runtime source changed during publication"));
    }
    #[cfg(windows)]
    let published = namespace.publish_child(
        Path::new(&staging),
        name,
        rsi_files_native_fs::EntryKind::File,
        false,
    );
    #[cfg(unix)]
    let published: std::io::Result<()> = {
        use std::os::{fd::AsFd as _, unix::fs::PermissionsExt as _};
        let file =
            rsi_files_native_fs::open_relative_file_no_follow(&directory, Path::new(&staging))
                .map_err(failure)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(failure)?;
        rustix::fs::renameat_with(
            directory.as_fd(),
            &staging,
            directory.as_fd(),
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    };
    match published {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let winner = rsi_files_native_fs::open_relative_file_no_follow(&directory, name)
                .map_err(failure)?;
            if digest(winner, deadline)? != expected {
                return Err(failure("concurrent runtime digest differs"));
            }
        }
        Err(error) => return Err(failure(error)),
    }
    directory.sync_all().map_err(failure)?;
    Ok(root.join(name))
}

struct Staging<'a> {
    directory: &'a rsi_files_native_fs::Directory,
    name: &'a str,
}
impl Drop for Staging<'_> {
    fn drop(&mut self) {
        let _ = self.directory.remove_child(Path::new(self.name));
    }
}

/// Caller retains the startup coordinator and has verified the new Host's readiness.
pub(super) fn collect(paths: &rsi_host::HostPaths, running: Option<&Path>) -> rsi::Result<()> {
    use rsi_files_native_fs::{
        EntryKind, open_absolute_directory_no_follow, open_relative_directory_no_follow,
    };
    let root = paths.state().join("session-host/artifacts");
    let directory = match open_absolute_directory_no_follow(&root) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failure(error)),
    };
    let current = running
        .map(|path| {
            path.parent()
                .and_then(Path::file_name)
                .ok_or_else(|| failure("runtime has no generation"))
        })
        .transpose()?;
    let mut unrecognized = 0;
    'generations: for (index, entry) in directory.entries().map_err(failure)?.enumerate() {
        if index >= 4096 {
            return Err(failure("runtime collection exceeds 4096 generations"));
        }
        let entry = entry.map_err(failure)?;
        let name = entry.file_name();
        let Some(text) = name.to_str().filter(|name| lower_hex(name, 64)) else {
            continue;
        };
        if entry.file_type().map_err(failure)? != EntryKind::Directory {
            unrecognized += 1;
            continue;
        }
        let generation =
            open_relative_directory_no_follow(&directory, Path::new(text)).map_err(failure)?;
        let mut children = Vec::new();
        for child in generation.entries().map_err(failure)? {
            if children.len() >= 64 {
                unrecognized += 1;
                continue 'generations;
            }
            let child = child.map_err(failure)?;
            let child_name = child.file_name();
            let recognized = child_name.to_str().is_some_and(|name| {
                matches!(name, "rsi" | "rsi.exe" | ".rsi-namespace-pin")
                    || name.strip_prefix("staging-").is_some_and(|id| {
                        !id.is_empty()
                            && id.len() <= 32
                            && id
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
            });
            if !recognized || child.file_type().map_err(failure)? != EntryKind::File {
                unrecognized += 1;
                continue 'generations;
            }
            children.push(child_name);
        }
        #[cfg(windows)]
        let unused_image = if Some(name.as_os_str()) == current {
            None
        } else {
            use std::os::windows::fs::OpenOptionsExt as _;
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(root.join(&name).join("rsi.exe"))
            {
                Ok(file) => Some(file),
                Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(failure(error)),
            }
        };
        // Releasing the probe permits namespace deletion; the startup coordinator
        // fences publication while an existing running image was detected above.
        #[cfg(windows)]
        drop(unused_image);
        for child in children {
            if Some(name.as_os_str()) != current || child.to_string_lossy().starts_with("staging-")
            {
                generation
                    .remove_child(Path::new(&child))
                    .map_err(failure)?;
            }
        }
        drop(generation);
        if Some(name.as_os_str()) != current {
            directory
                .remove_empty_directory(Path::new(&name))
                .map_err(failure)?;
        }
    }
    directory.sync_all().map_err(failure)?;
    if unrecognized != 0 {
        return Err(failure(format!(
            "retained {unrecognized} unrecognized runtime generations; other recognized generations were collected"
        )));
    }
    Ok(())
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
    RsiError::Boot(format!("Service Host runtime publication: {error}"))
}

#[cfg(test)]
fn publish_source(paths: &rsi_host::HostPaths, source: &Path) -> rsi::Result<PathBuf> {
    publish_source_before(
        paths,
        source,
        std::time::Instant::now() + rsi::APPLICATION_TRANSITION_TIMEOUT,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_runtime_stays_in_publication_and_pins_until_handoff() {
        let temp = tempfile::tempdir().unwrap();
        let paths = rsi_host::HostPaths::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
        )
        .unwrap();
        let bundle = temp.path().join("generations/00000000000000000001/bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
        let source = bundle.join("rsi.exe");
        std::fs::write(&source, b"managed executable").unwrap();
        let deadline = std::time::Instant::now() + rsi::APPLICATION_TRANSITION_TIMEOUT;
        assert!(publish_before(&paths, Some(&source), deadline).is_err());
        std::fs::create_dir(temp.path().join("leases")).unwrap();
        let lease = std::fs::File::create(temp.path().join("leases/00000000000000000001")).unwrap();
        std::fs::create_dir(bundle.join("nested")).unwrap();
        let runtime =
            publish_before(&paths, Some(&bundle.join("nested/../rsi.exe")), deadline).unwrap();
        assert_eq!(
            runtime.path.canonicalize().unwrap(),
            source.canonicalize().unwrap()
        );
        assert!(!paths.state().exists());
        runtime.collect(&paths).unwrap();
        let ordinary = temp.path().join("ordinary");
        std::fs::write(&ordinary, b"old Cargo runtime").unwrap();
        let obsolete = publish_source(&paths, &ordinary).unwrap();
        assert!(obsolete.exists());
        runtime.collect(&paths).unwrap();
        assert!(!obsolete.parent().unwrap().exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"managed executable");
        assert!(matches!(
            lease.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(runtime);
        lease.try_lock().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn collection_preserves_windows_artifacts_held_by_a_running_image() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let paths = rsi_host::HostPaths::new(
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("cache"),
        )
        .unwrap();
        let source = root.path().join("source.exe");
        std::fs::write(&source, b"active helper").unwrap();
        let active = publish_source(&paths, &source).unwrap();
        std::fs::write(&source, b"obsolete helper").unwrap();
        let obsolete = publish_source(&paths, &source).unwrap();
        let running = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&active)
            .unwrap();
        collect(&paths, None).unwrap();
        assert!(active.exists());
        assert!(!obsolete.exists());
        drop(running);
        collect(&paths, None).unwrap();
        assert!(!active.exists());
    }

    #[test]
    fn expired_publication_deadline_does_not_publish_a_runtime() {
        let root = tempfile::tempdir().unwrap();
        let paths = rsi_host::HostPaths::new(
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("cache"),
        )
        .unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"runtime").unwrap();
        assert!(
            publish_source_before(&paths, &source, std::time::Instant::now())
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );
        assert!(!paths.state().exists());
    }
    #[test]
    fn publication_is_content_addressed_and_preserves_the_running_generation() {
        let temp = tempfile::tempdir().unwrap();
        let paths = rsi_host::HostPaths::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
        )
        .unwrap();
        let source = temp.path().join("source");
        std::fs::write(&source, b"generation one").unwrap();
        let first = publish_source(&paths, &source).unwrap();
        assert_eq!(first, publish_source(&paths, &source).unwrap());
        std::fs::write(&source, b"generation two").unwrap();
        let second = publish_source(&paths, &source).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(first).unwrap(), b"generation one");
        assert_eq!(std::fs::read(second).unwrap(), b"generation two");
    }

    #[test]
    fn concurrent_publication_and_collection_keep_the_running_generation() {
        let temp = tempfile::tempdir().unwrap();
        let paths = rsi_host::HostPaths::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
        )
        .unwrap();
        let source = temp.path().join("source");
        std::fs::write(&source, vec![7; 1024 * 1024]).unwrap();
        let published = std::thread::scope(|scope| {
            let first = scope.spawn(|| publish_source(&paths, &source));
            let second = scope.spawn(|| publish_source(&paths, &source));
            let first = first.join().unwrap().unwrap();
            assert_eq!(first, second.join().unwrap().unwrap());
            first
        });
        std::fs::write(&source, b"next").unwrap();
        let next = publish_source(&paths, &source).unwrap();
        std::fs::write(
            published.parent().unwrap().join("staging-123"),
            b"abandoned",
        )
        .unwrap();
        let unknown = next
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("0".repeat(64));
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("operator-file"), b"preserve").unwrap();
        assert!(
            collect(&paths, Some(&next))
                .unwrap_err()
                .to_string()
                .contains("unrecognized")
        );
        assert_eq!(
            std::fs::read(unknown.join("operator-file")).unwrap(),
            b"preserve"
        );
        assert!(!published.parent().unwrap().exists());
        assert_eq!(std::fs::read(&next).unwrap(), b"next");
        let modified = std::fs::metadata(&next).unwrap().modified().unwrap();
        std::fs::write(&next, b"oops").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&next)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(
            publish_source(&paths, &source).is_err(),
            "same size and mtime do not prove cached executable integrity"
        );
    }
}
