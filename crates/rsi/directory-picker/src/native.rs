use cap_std::fs::{Dir, MetadataExt as _};
use rsi_directory_picker_api::{
    CreateRequest, Created, Entry, Failure, ListRequest, Listing, MAXIMUM_ENTRIES, MAXIMUM_REPLY,
    Result, validate_name, validate_path,
};
use rsi_files_native_fs::{
    create_directory_no_follow, directory_entries, open_absolute_directory_no_follow,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;
#[allow(clippy::needless_pass_by_value)] // map_err consumes the I/O error at this boundary.
fn io(error: std::io::Error) -> Failure {
    Failure::Io {
        message: error.kind().to_string(),
    }
}
fn path_text(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or(Failure::Invalid)?;
    validate_path(value)?;
    Ok(value.into())
}
fn check(stop: &CancellationToken) -> Result<()> {
    if stop.is_cancelled() {
        Err(Failure::Cancelled)
    } else {
        Ok(())
    }
}
fn physical(path: &Path, stop: &CancellationToken) -> Result<PathBuf> {
    check(stop)?;
    std::fs::canonicalize(path).map_err(io)
}
fn identity(directory: &Dir, path: &Path) -> Result<()> {
    let owned = directory.dir_metadata().map_err(io)?;
    let named = open_absolute_directory_no_follow(path)
        .map_err(io)?
        .dir_metadata()
        .map_err(io)?;
    if owned.dev() != named.dev() || owned.ino() != named.ino() {
        return Err(Failure::Io {
            message: "directory changed; read it again".into(),
        });
    }
    Ok(())
}
#[allow(clippy::needless_pass_by_value)] // Own the cancellation token throughout the actual filesystem task.
pub(super) fn list(
    request: ListRequest,
    home: Option<PathBuf>,
    stop: CancellationToken,
) -> Result<Listing> {
    if let Some(path) = &request.path {
        validate_path(path)?;
    }
    let requested = request
        .path
        .as_deref()
        .map(Path::new)
        .or(home.as_deref())
        .ok_or(Failure::HomeUnavailable)?;
    let canonical = physical(requested, &stop)?;
    let path = path_text(&canonical)?;
    check(&stop)?;
    let directory = open_absolute_directory_no_follow(&canonical).map_err(io)?;
    identity(&directory, &canonical)?;
    let home = home
        .and_then(|path| physical(&path, &stop).ok())
        .and_then(|path| path_text(&path).ok());
    check(&stop)?;
    let mut listing = Listing {
        requested: request.path,
        path: path.clone(),
        home,
        breadcrumbs: path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
        entries: Vec::new(),
        truncated: false,
        unrepresentable: false,
    };
    let base = serde_json::to_vec(&listing)
        .map_err(|_| Failure::Invalid)?
        .len();
    let mut entries = BTreeMap::<String, (Entry, usize)>::new();
    let mut bytes = base;
    check(&stop)?;
    for entry in directory_entries(&directory).map_err(io)? {
        check(&stop)?;
        let entry = entry.map_err(io)?;
        check(&stop)?;
        let kind = entry.file_type().map_err(io)?;
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            listing.unrepresentable = true;
            continue;
        };
        let target = match physical(&canonical.join(name), &stop) {
            Ok(path) => path,
            Err(Failure::Cancelled) => return Err(Failure::Cancelled),
            Err(_) => continue,
        };
        check(&stop)?;
        if !std::fs::metadata(&target).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        let Ok(target) = path_text(&target) else {
            listing.unrepresentable = true;
            continue;
        };
        let row = Entry {
            name: name.into(),
            path: target,
            hidden: name.starts_with('.'),
            symlink: kind.is_symlink(),
        };
        let size = serde_json::to_vec(&row)
            .map_err(|_| Failure::Invalid)?
            .len()
            + 1;
        if let Some((_, old)) = entries.insert(name.into(), (row, size)) {
            bytes -= old;
        }
        bytes += size;
        while entries.len() > MAXIMUM_ENTRIES {
            let (_, (_, size)) = entries.pop_last().ok_or(Failure::Invalid)?;
            bytes -= size;
            listing.truncated = true;
        }
    }
    check(&stop)?;
    identity(&directory, &canonical)?;
    while bytes > MAXIMUM_REPLY - 32 {
        let (_, (_, size)) = entries.pop_last().ok_or(Failure::Invalid)?;
        bytes -= size;
        listing.truncated = true;
    }
    listing.entries = entries.into_values().map(|(entry, _)| entry).collect();
    listing.validate(&ListRequest {
        path: listing.requested.clone(),
    })?;
    Ok(listing)
}
#[allow(clippy::needless_pass_by_value)] // Same task-owned cancellation lifetime as list.
pub(super) fn create(request: CreateRequest, stop: CancellationToken) -> Result<Created> {
    validate_path(&request.parent)?;
    validate_name(&request.name)?;
    let canonical = physical(Path::new(&request.parent), &stop)?;
    let parent = path_text(&canonical)?;
    let result = canonical.join(&request.name);
    let path = path_text(&result)?;
    check(&stop)?;
    let directory = open_absolute_directory_no_follow(&canonical).map_err(io)?;
    create_at(&directory, &canonical, &request.name, &stop)?;
    Ok(Created {
        requested_parent: request.parent,
        parent,
        name: request.name,
        path,
    })
}
fn create_at(
    directory: &Dir,
    canonical: &Path,
    name: &str,
    stop: &CancellationToken,
) -> Result<()> {
    check(stop)?;
    identity(directory, canonical)?;
    check(stop)?;
    let created =
        create_directory_no_follow(directory, std::ffi::OsStr::new(name)).map_err(|error| {
            match error {
                rsi_files_native_fs::DirectoryCreationError::NotCreated(error) => io(error),
                rsi_files_native_fs::DirectoryCreationError::Created(_) => Failure::OutcomeUnknown,
            }
        })?;
    // mkdir has succeeded. Subsequent failures cannot claim the operation was undone.
    check(stop)
        .and_then(|()| identity(directory, canonical))
        .and_then(|()| identity(&created, &canonical.join(name)))
        .map_err(|_| Failure::OutcomeUnknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;
    #[test]
    fn aliases_hidden_unicode_and_single_creation_return_physical_paths() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let physical = root_path.join("home");
        std::fs::create_dir(&physical).unwrap();
        for name in ["zebra", ".hidden", "中文", "Alpha"] {
            std::fs::create_dir(physical.join(name)).unwrap();
        }
        let alias = root_path.join("alias");
        symlink(&physical, &alias).unwrap();
        symlink(physical.join("Alpha"), physical.join("linked")).unwrap();
        symlink("cycle", physical.join("cycle")).unwrap();
        #[cfg(target_os = "linux")]
        std::fs::create_dir(physical.join(std::ffi::OsString::from_vec(vec![255]))).unwrap();
        let listing = list(
            ListRequest { path: None },
            Some(alias.clone()),
            CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(listing.path, physical.to_str().unwrap());
        assert_eq!(listing.home.as_deref(), physical.to_str());
        assert_eq!(listing.unrepresentable, cfg!(target_os = "linux"));
        assert!(!listing.truncated);
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            [".hidden", "Alpha", "linked", "zebra", "中文"]
        );
        assert!(listing.entries[0].hidden);
        assert!(listing.entries[2].symlink);
        assert_eq!(
            listing.entries[2].path,
            physical.join("Alpha").to_str().unwrap()
        );
        let request = CreateRequest {
            parent: alias.to_str().unwrap().into(),
            name: "created".into(),
        };
        let created = create(request.clone(), CancellationToken::new()).unwrap();
        assert_eq!(created.path, physical.join("created").to_str().unwrap());
        assert!(create(request, CancellationToken::new()).is_err());
        for name in ["", ".", "..", "a/b", "a\\b", "nul\0x"] {
            assert!(
                create(
                    CreateRequest {
                        parent: physical.to_str().unwrap().into(),
                        name: name.into()
                    },
                    CancellationToken::new()
                )
                .is_err()
            );
        }
    }
    #[test]
    fn physical_target_bytes_truncate_a_long_directory_window() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut parent = root_path;
        // JSON escaping exceeds the reply budget without exceeding macOS PATH_MAX.
        for _ in 0..3 {
            parent.push("\u{1}".repeat(200));
            std::fs::create_dir(&parent).unwrap();
        }
        for index in 0..700 {
            std::fs::create_dir(parent.join(format!("entry-{index:04}"))).unwrap();
        }
        let listing = list(
            ListRequest {
                path: Some(parent.to_str().unwrap().into()),
            },
            None,
            CancellationToken::new(),
        )
        .unwrap();
        assert!(listing.truncated);
        assert!(listing.entries.len() < 700);
        assert!(serde_json::to_vec(&listing).unwrap().len() <= MAXIMUM_REPLY);
        assert_eq!(listing.entries.first().unwrap().name, "entry-0000");
    }
    #[test]
    fn enumeration_is_sorted_bounded_and_cancellation_and_parent_replacement_are_explicit() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let parent = root_path.join("parent");
        std::fs::create_dir(&parent).unwrap();
        for index in (0..1005).rev() {
            std::fs::create_dir(parent.join(format!("dir-{index:04}"))).unwrap();
        }
        let request = ListRequest {
            path: Some(parent.to_str().unwrap().into()),
        };
        let listing = list(request.clone(), None, CancellationToken::new()).unwrap();
        assert_eq!(listing.entries.len(), 1000);
        assert!(listing.truncated);
        assert_eq!(listing.entries.last().unwrap().name, "dir-0999");
        let stop = CancellationToken::new();
        stop.cancel();
        assert_eq!(list(request, None, stop).unwrap_err(), Failure::Cancelled);
        let retained = open_absolute_directory_no_follow(&parent).unwrap();
        std::fs::rename(&parent, root_path.join("renamed")).unwrap();
        std::fs::create_dir(&parent).unwrap();
        assert!(
            create_at(
                &retained,
                &parent,
                "never-created",
                &CancellationToken::new()
            )
            .is_err()
        );
        assert!(!parent.join("never-created").exists());
        assert!(!root_path.join("renamed/never-created").exists());
    }
}
