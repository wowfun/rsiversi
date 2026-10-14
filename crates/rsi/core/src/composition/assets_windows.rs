//! Private Windows publication of the exact embedded preset bytes.
use super::SHIPPED_PRESETS;
use rsi_files_native_fs as native;
use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

fn boot(error: impl std::fmt::Display) -> rsi_host::HostError {
    rsi_host::HostError::Bootstrap(error.to_string())
}
pub(super) fn materialize(cache: &Path, digest: &str) -> rsi_host::Result<PathBuf> {
    materialize_before_publish(cache, digest, || {})
}

fn materialize_before_publish(
    cache: &Path,
    digest: &str,
    before_publish: impl FnOnce(),
) -> rsi_host::Result<PathBuf> {
    native::create_absolute_directory_and_sync_no_follow(cache).map_err(boot)?;
    let parent = native::set_private_directory_permissions_no_follow(cache).map_err(boot)?;
    let namespace = parent.pin_namespace().map_err(boot)?;
    let target = cache.join(digest);
    match native::open_relative_directory_no_follow(&parent, Path::new(digest)) {
        Ok(directory) => {
            verify(&directory)?;
            return Ok(target);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(boot(e)),
    }
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(cache)
        .map_err(boot)?;
    let root = native::set_private_directory_permissions_no_follow(staging.path()).map_err(boot)?;
    for (name, profile, metadata) in SHIPPED_PRESETS {
        let directory = native::create_directory_no_follow(&root, name.as_ref()).map_err(boot)?;
        for (name, bytes) in [("agent.profile.toml", *profile), ("preset.toml", *metadata)] {
            let mut file =
                native::create_relative_private_file_no_follow(&directory, Path::new(name))
                    .map_err(boot)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(boot)?;
        }
        directory.sync_all().map_err(boot)?;
    }
    root.sync_all().map_err(boot)?;
    verify(&root)?;
    before_publish();
    match namespace.publish_child(
        Path::new(staging.path().file_name().unwrap()),
        Path::new(digest),
        native::EntryKind::Directory,
        false,
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(boot(error)),
    }
    parent.sync_all().map_err(boot)?;
    let published =
        native::open_relative_directory_no_follow(&parent, Path::new(digest)).map_err(boot)?;
    verify(&published)?;
    Ok(target)
}

fn verify(root: &native::Directory) -> rsi_host::Result<()> {
    for (name, profile, metadata) in SHIPPED_PRESETS {
        let directory =
            native::open_relative_directory_no_follow(root, Path::new(name)).map_err(boot)?;
        for (name, expected) in [("agent.profile.toml", *profile), ("preset.toml", *metadata)] {
            let file =
                native::open_relative_file_no_follow(&directory, Path::new(name)).map_err(boot)?;
            let mut actual = Vec::with_capacity(expected.len());
            file.take(expected.len() as u64 + 1)
                .read_to_end(&mut actual)
                .map_err(boot)?;
            if actual != expected {
                return Err(boot(format!(
                    "standard Agent preset asset {name} failed byte verification"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_cold_publications_converge() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        materialize_before_publish(&cache, "fixture", || {
                            barrier.wait();
                        })
                        .unwrap()
                    })
                })
                .collect();
            for task in tasks {
                assert_eq!(task.join().unwrap(), cache.join("fixture"));
            }
        });
        assert!(std::fs::read_dir(cache).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging-")
        }));
    }
    #[test]
    fn native_publication_reuse_and_corruption_refusal() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let target = materialize(&cache, "fixture").unwrap();
        assert_eq!(materialize(&cache, "fixture").unwrap(), target);
        std::fs::write(target.join("standard/agent.profile.toml"), b"changed").unwrap();
        assert!(
            materialize(&cache, "fixture")
                .unwrap_err()
                .to_string()
                .contains("byte verification")
        );
        assert_eq!(std::fs::read_dir(cache).unwrap().count(), 2);
    }
}
