use super::{Path, Result, fs};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    time::Duration,
};

// Application publication grammar: apps/devtools/README.md.
pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 20 && id.bytes().all(|c| c.is_ascii_digit())
}
pub(super) fn current(cache: &Path) -> Result<Option<String>> {
    let file = match open_record(&cache.join("current")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    file.take(22).read_to_string(&mut text)?;
    let id = text
        .strip_suffix('\n')
        .ok_or("current publication record must end in newline")?;
    if !valid_id(id) {
        return Err("invalid current publication identifier".into());
    }
    Ok(Some(id.into()))
}
fn open_record(path: &Path) -> std::io::Result<fs::File> {
    let invalid = || std::io::Error::other("current publication record must be an ordinary file");
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        // NONBLOCK also makes a substituted FIFO reject without waiting for a writer.
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                invalid()
            } else {
                error.into()
            }
        })?;
        fs::File::from(descriptor)
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?
    };
    #[cfg(not(any(unix, windows)))]
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.is_symlink() {
        return Err(invalid());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid());
        }
    }
    Ok(file)
}
pub(super) fn publish(cache: &Path, id: &str) -> Result<()> {
    if !valid_id(id) {
        return Err("invalid publication identifier".into());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    writeln!(temporary, "{id}")?;
    temporary.as_file().sync_all()?;
    let mut retry = temporary;
    for _ in 0..9 {
        match retry.persist(cache.join("current")) {
            Ok(_) => return Ok(()),
            Err(error)
                if matches!(
                    error.error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
                ) =>
            {
                retry = error.file;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    retry
        .persist(cache.join("current"))
        .map(|_| ())
        .map_err(|error| error.error.into())
}
pub(super) fn collect(cache: &Path) -> Result<Vec<String>> {
    let trash = cache.join("trash");
    // Classify abandoned entries before removing any recoverable generation.
    let _ = trash_entries(&trash)?;
    let generations = cache.join("generations");
    if !generations.exists() {
        for path in trash_entries(&trash)? {
            remove(&path)?;
        }
        collect_orphan_leases(cache)?;
        return Ok(Vec::new());
    }
    let mut success = Vec::new();
    let mut failed = Vec::new();
    for entry in fs::read_dir(&generations)? {
        let entry = entry?;
        let name = entry.file_name();
        if !entry.file_type()?.is_dir() || !name.to_str().is_some_and(valid_id) {
            return Err("invalid managed generation directory".into());
        }
        let id = name.to_string_lossy().into_owned();
        if entry.path().join("bundle/receipt.json").is_file()
            && !entry.path().join("failure.json").exists()
        {
            success.push(id);
        } else {
            failed.push(id);
        }
    }
    success.sort_unstable();
    success.reverse();
    failed.sort_unstable();
    failed.reverse();
    let mut retained: BTreeSet<String> = success
        .iter()
        .take(2)
        .chain(failed.iter().take(1))
        .cloned()
        .collect();
    if let Some(id) = current(cache)? {
        retained.insert(id);
    }
    fs::create_dir_all(&trash)?;
    let mut removed = Vec::new();
    for id in success.into_iter().chain(failed) {
        if retained.contains(&id) {
            continue;
        }
        let generation = generations.join(&id);
        let lock_path = cache.join("leases").join(&id);
        let lease = if lock_path.exists() {
            let lease = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)?;
            match lease.try_lock() {
                Ok(()) => (),
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
            Some(lease)
        } else {
            None
        };
        let destination = trash.join(&id);
        match fs::rename(&generation, &destination) {
            Ok(()) => {
                remove(&destination)?;
                drop(lease);
                if lock_path.exists() {
                    fs::remove_file(lock_path)?;
                }
                removed.push(id);
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => (),
            Err(error) => return Err(error.into()),
        }
    }
    for path in trash_entries(&trash)? {
        remove(&path)?;
    }
    collect_orphan_leases(cache)?;
    Ok(removed)
}
fn trash_entries(trash: &Path) -> Result<Vec<std::path::PathBuf>> {
    let entries = match fs::read_dir(trash) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    entries.map(|entry| {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(valid_id) || !entry.file_type()?.is_dir() {
            return Err(format!("invalid managed trash entry: {}; inspect and move it outside the cache before retrying GC", entry.path().display()).into());
        }
        Ok(entry.path())
    }).collect()
}
fn collect_orphan_leases(cache: &Path) -> Result<()> {
    let entries = match fs::read_dir(cache.join("leases")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let id = entry.file_name();
        if !id.to_str().is_some_and(valid_id) || !entry.file_type()?.is_file() {
            return Err("invalid managed lease entry".into());
        }
        if cache.join("generations").join(&id).exists() || cache.join("trash").join(&id).exists() {
            continue;
        }
        let lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(entry.path())?;
        match lease.try_lock() {
            Ok(()) => (),
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        drop(lease);
        fs::remove_file(entry.path())?;
    }
    Ok(())
}
fn remove(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err("unexpected link in managed trash".into());
    }
    fs::remove_dir_all(path)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_trash_is_diagnosed_before_generation_removal() {
        let root = tempfile::tempdir().unwrap();
        for id in 1..=3 {
            let bundle = root
                .path()
                .join("generations")
                .join(format!("{id:020}"))
                .join("bundle");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(bundle.join("receipt.json"), b"{}").unwrap();
        }
        fs::create_dir(root.path().join("trash")).unwrap();
        let foreign = root.path().join("trash").join("unknown");
        fs::write(&foreign, b"preserve").unwrap();
        let error = collect(root.path()).unwrap_err().to_string();
        assert!(error.contains(&foreign.display().to_string()), "{error}");
        assert!(error.contains("inspect and move"));
        assert!(
            root.path()
                .join("generations/00000000000000000001")
                .is_dir()
        );
        assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
    }
    #[cfg(unix)]
    #[test]
    fn current_rejects_a_symlink_even_when_its_bytes_are_valid() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("record"), "00000000000000000001\n").unwrap();
        std::os::unix::fs::symlink("record", root.path().join("current")).unwrap();
        assert!(
            current(root.path())
                .unwrap_err()
                .to_string()
                .contains("ordinary file")
        );
    }
    #[cfg(unix)]
    #[test]
    fn current_rejects_special_files_without_waiting_for_a_writer() {
        let root = tempfile::tempdir().unwrap();
        let record = root.path().join("current");
        nix::unistd::mkfifo(
            &record,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(
            current(root.path())
                .unwrap_err()
                .to_string()
                .contains("ordinary file")
        );
    }
    #[test]
    fn current_distinguishes_missing_and_directory_records() {
        let root = tempfile::tempdir().unwrap();
        assert!(current(root.path()).unwrap().is_none());
        fs::create_dir(root.path().join("current")).unwrap();
        assert!(current(root.path()).is_err());
    }
    #[test]
    fn abandoned_trash_is_collected_without_a_generations_directory() {
        let root = tempfile::tempdir().unwrap();
        let abandoned = root.path().join("trash/00000000000000000001");
        fs::create_dir_all(&abandoned).unwrap();
        fs::write(abandoned.join("partial-artifact"), b"unfinished cleanup").unwrap();
        fs::create_dir(root.path().join("leases")).unwrap();
        let lease = root.path().join("leases/00000000000000000001");
        fs::write(&lease, b"").unwrap();
        assert!(collect(root.path()).unwrap().is_empty());
        assert!(!abandoned.exists());
        assert!(!lease.exists());
        assert!(!root.path().join("generations").exists());
    }
    #[test]
    fn orphan_leases_are_retried_without_removing_a_live_lease() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("leases")).unwrap();
        let path = root.path().join("leases/00000000000000000001");
        let live = fs::File::create_new(&path).unwrap();
        live.lock_shared().unwrap();
        collect(root.path()).unwrap();
        assert!(path.exists());
        live.unlock().unwrap();
        drop(live);
        collect(root.path()).unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn failed_publications_with_receipts_do_not_displace_successful_generations() {
        let root = tempfile::tempdir().unwrap();
        for number in 1..=4 {
            let generation = root
                .path()
                .join("generations")
                .join(format!("{number:020}"));
            fs::create_dir_all(generation.join("bundle")).unwrap();
            fs::write(generation.join("bundle/receipt.json"), b"{}").unwrap();
            if number > 2 {
                fs::write(generation.join("failure.json"), b"{}").unwrap();
            }
        }
        publish(root.path(), "00000000000000000002").unwrap();
        assert_eq!(collect(root.path()).unwrap(), ["00000000000000000003"]);
        for id in [
            "00000000000000000001",
            "00000000000000000002",
            "00000000000000000004",
        ] {
            assert!(root.path().join("generations").join(id).is_dir());
        }
    }
    #[cfg(windows)]
    #[test]
    fn sharing_denied_lease_removal_recovers_on_later_collection() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("leases")).unwrap();
        for id in [
            "00000000000000000001",
            "00000000000000000002",
            "00000000000000000003",
        ] {
            let bundle = root.path().join("generations").join(id).join("bundle");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(bundle.join("receipt.json"), b"{}").unwrap();
            fs::write(root.path().join("leases").join(id), b"").unwrap();
        }
        let path = root.path().join("leases/00000000000000000001");
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        assert!(collect(root.path()).is_err());
        assert!(
            !root
                .path()
                .join("generations/00000000000000000001")
                .exists()
        );
        assert!(path.exists());
        drop(reader);
        collect(root.path()).unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn current_requires_exact_identifier_and_lf_without_trailing_data() {
        let root = tempfile::tempdir().unwrap();
        for record in [
            "5\n",
            "999999999999999999999999\n",
            "00000000000000000001",
            "00000000000000000001\r\n",
            "00000000000000000001\nextra",
        ] {
            fs::write(root.path().join("current"), record).unwrap();
            assert!(current(root.path()).is_err(), "accepted {record:?}");
        }
        publish(root.path(), "00000000000000000001").unwrap();
        assert!(publish(root.path(), "5").is_err());
        assert_eq!(
            current(root.path()).unwrap().as_deref(),
            Some("00000000000000000001")
        );
    }
    #[test]
    fn foreign_numeric_directories_abort_collection_before_any_removal() {
        for foreign in ["5", "999999999999999999999999"] {
            let root = tempfile::tempdir().unwrap();
            let ids = [
                "00000000000000000001",
                "00000000000000000002",
                "00000000000000000003",
                foreign,
            ];
            for id in ids {
                let bundle = root.path().join("generations").join(id).join("bundle");
                fs::create_dir_all(&bundle).unwrap();
                fs::write(bundle.join("receipt.json"), b"{}").unwrap();
            }
            publish(root.path(), ids[2]).unwrap();
            assert!(
                collect(root.path()).is_err(),
                "accepted foreign directory {foreign}"
            );
            for id in ids {
                assert!(
                    root.path().join("generations").join(id).exists(),
                    "removed {id}"
                );
            }
        }
    }
    #[cfg(windows)]
    #[test]
    fn blocked_atomic_replace_preserves_current_and_retry_succeeds_after_release() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        publish(root.path(), "00000000000000000001").unwrap();
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.path().join("current"))
            .unwrap();
        let started = std::time::Instant::now();
        assert!(publish(root.path(), "00000000000000000002").is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            current(root.path()).unwrap().as_deref(),
            Some("00000000000000000001")
        );
        drop(reader);
        publish(root.path(), "00000000000000000002").unwrap();
        assert_eq!(
            current(root.path()).unwrap().as_deref(),
            Some("00000000000000000002")
        );
    }
    #[test]
    fn publication_rejects_paths_and_keeps_previous_and_live_generations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("leases")).unwrap();
        for id in [
            "00000000000000000001",
            "00000000000000000002",
            "00000000000000000003",
            "00000000000000000004",
            "00000000000000000005",
            "00000000000000000006",
        ] {
            let bundle = root.join("generations").join(id).join("bundle");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(root.join("leases").join(id), b"").unwrap();
            if id < "00000000000000000005" {
                fs::write(bundle.join("receipt.json"), b"{}").unwrap();
            }
        }
        publish(root, "00000000000000000004").unwrap();
        let live = fs::File::open(root.join("leases/00000000000000000001")).unwrap();
        live.lock_shared().unwrap();
        let removed = collect(root).unwrap();
        assert!(
            removed.contains(&"00000000000000000002".into())
                && removed.contains(&"00000000000000000005".into())
        );
        assert!(root.join("generations/00000000000000000001").exists());
        live.unlock().unwrap();
        drop(live);
        assert_eq!(collect(root).unwrap(), ["00000000000000000001"]);
        for number in 7..10 {
            let id = format!("{number:020}");
            fs::create_dir(root.join("generations").join(&id)).unwrap();
            fs::write(root.join("leases").join(&id), b"").unwrap();
            assert_eq!(collect(root).unwrap(), [format!("{:020}", number - 1)]);
            assert!(root.join("generations").join(id).exists());
            assert_eq!(
                current(root).unwrap().as_deref(),
                Some("00000000000000000004")
            );
        }
        fs::write(root.join("current"), b"../outside\n").unwrap();
        assert!(current(root).is_err());
    }
}
