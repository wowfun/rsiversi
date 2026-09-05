use crate::{
    AgentPresetId, MAX_COPY_BYTES, MAX_COPY_DEPTH, MAX_COPY_ENTRIES, MAX_METADATA_BYTES,
    METADATA_FILE, PresetError, Result, clean_metadata_text,
};
#[cfg(unix)]
use crate::{open_existing_preset_root, open_or_create_preset_root};
use serde::{Deserialize, Serialize};
#[cfg(not(unix))]
use std::fs;
use std::fs::File;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MetadataWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<i64>,
}

#[derive(Debug, Default)]
struct CopyBudget {
    entries: usize,
    bytes: u64,
    source_metadata: bool,
}

pub(crate) fn copy_preset(
    source: &Path,
    user_root: &Path,
    target: &AgentPresetId,
    name: Option<String>,
) -> Result<()> {
    let name = clean_metadata_text(name);
    copy_preset_platform(source, user_root, target, name.as_deref())
}

pub(crate) fn delete_preset(user_root: &Path, target: &AgentPresetId) -> Result<()> {
    delete_preset_platform(user_root, target)
}

fn render_metadata(metadata: &MetadataWire) -> Result<Vec<u8>> {
    let input_bytes = metadata
        .name
        .as_deref()
        .map_or(0, str::len)
        .saturating_add(metadata.description.as_deref().map_or(0, str::len));
    if input_bytes > MAX_METADATA_BYTES {
        return Err(metadata_limit());
    }
    let encoded = toml::to_string(metadata)
        .map_err(|error| PresetError::UnsafeEntry {
            path: PathBuf::from(METADATA_FILE),
            reason: format!("metadata could not be encoded: {error}"),
        })?
        .into_bytes();
    if encoded.len() > MAX_METADATA_BYTES {
        return Err(metadata_limit());
    }
    Ok(encoded)
}

fn read_copy_metadata(
    input: &mut impl Read,
    path: &Path,
    name_override: Option<&str>,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input
        .take(u64::try_from(MAX_METADATA_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read source metadata", path, error))?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(metadata_limit());
    }
    let Some(name) = name_override else {
        return Ok(bytes);
    };
    let content = std::str::from_utf8(&bytes).map_err(|_| PresetError::UnsafeEntry {
        path: path.to_path_buf(),
        reason: "metadata cannot be renamed because it is not UTF-8".to_owned(),
    })?;
    let metadata =
        toml::from_str::<MetadataWire>(content).map_err(|_| PresetError::UnsafeEntry {
            path: path.to_path_buf(),
            reason: "metadata cannot be renamed because it is not valid preset metadata".to_owned(),
        })?;
    render_metadata_with_name(metadata, name)
}

fn render_metadata_with_name(mut metadata: MetadataWire, name: &str) -> Result<Vec<u8>> {
    if name.len() > MAX_METADATA_BYTES {
        return Err(metadata_limit());
    }
    metadata.name = Some(name.to_owned());
    render_metadata(&metadata)
}

fn metadata_limit() -> PresetError {
    PresetError::CopyLimit {
        resource: "metadata bytes",
        maximum: u64::try_from(MAX_METADATA_BYTES).expect("metadata bound fits u64"),
    }
}

fn account_entry(budget: &mut CopyBudget) -> Result<()> {
    budget.entries = budget.entries.saturating_add(1);
    if budget.entries > MAX_COPY_ENTRIES {
        return Err(PresetError::CopyLimit {
            resource: "filesystem entries",
            maximum: u64::try_from(MAX_COPY_ENTRIES).expect("entry bound fits u64"),
        });
    }
    Ok(())
}

fn account_bytes(budget: &mut CopyBudget, bytes: u64) -> Result<()> {
    budget.bytes = budget.bytes.saturating_add(bytes);
    if budget.bytes > MAX_COPY_BYTES {
        return Err(PresetError::CopyLimit {
            resource: "aggregate bytes",
            maximum: MAX_COPY_BYTES,
        });
    }
    Ok(())
}

fn depth_error() -> PresetError {
    PresetError::CopyLimit {
        resource: "directory depth",
        maximum: u64::try_from(MAX_COPY_DEPTH).expect("depth bound fits u64"),
    }
}

#[cfg(unix)]
fn copy_preset_platform(
    source: &Path,
    user_root: &Path,
    target: &AgentPresetId,
    name_override: Option<&str>,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags};

    let source_directory = rustix::fs::open(
        source,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| unsafe_entry(source, "source is not a no-follow directory", error))?;
    let user_directory = open_or_create_preset_root(user_root)?.into_directory();
    let mut stage = UnixStage::create(user_directory, user_root)?;
    let mut budget = CopyBudget::default();
    UnixCopy::new(&source_directory, &mut budget, source, name_override)?.copy_directory(
        &source_directory,
        &[],
        stage.directory(),
        source,
        0,
        true,
    )?;
    if let Some(name) = name_override
        && !budget.source_metadata
    {
        let metadata = render_metadata_with_name(MetadataWire::default(), name)?;
        account_entry(&mut budget)?;
        account_bytes(
            &mut budget,
            u64::try_from(metadata.len()).unwrap_or(u64::MAX),
        )?;
        write_file_unix(
            stage.directory(),
            METADATA_FILE.as_ref(),
            &metadata,
            false,
            &source.join(METADATA_FILE),
        )?;
    }
    stage.sync()?;
    stage.publish(target)
}

#[cfg(unix)]
fn delete_preset_platform(user_root: &Path, target: &AgentPresetId) -> Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_QUARANTINE: AtomicU64 = AtomicU64::new(0);
    let user_directory = open_existing_preset_root(user_root)?.into_directory();
    let target_path = user_root.join(target.as_str());
    let target_directory = rustix::fs::openat(
        &user_directory,
        target.as_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| unsafe_entry(&target_path, "target is not a no-follow directory", error))?;
    let opened = rustix::fs::fstat(&target_directory)
        .map_err(|error| io_error("inspect delete target", &target_path, error))?;
    if !FileType::from_raw_mode(opened.st_mode).is_dir() {
        return Err(PresetError::UnsafeEntry {
            path: target_path,
            reason: "delete target is not a directory".to_owned(),
        });
    }

    for _ in 0..128 {
        let sequence = NEXT_QUARANTINE.fetch_add(1, Ordering::Relaxed);
        let quarantine = format!(
            ".rsi-agent-preset-deleted-{}-{sequence:016x}.tmp",
            std::process::id()
        );
        let current =
            rustix::fs::statat(&user_directory, target.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| io_error("revalidate delete target", &target_path, error))?;
        if current.st_dev != opened.st_dev || current.st_ino != opened.st_ino {
            return Err(PresetError::UnsafeEntry {
                path: target_path,
                reason: "delete target changed before quarantine".to_owned(),
            });
        }
        match rustix::fs::renameat_with(
            &user_directory,
            target.as_str(),
            &user_directory,
            &quarantine,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                return remove_tree_at(&user_directory, quarantine.as_ref()).map_err(|error| {
                    io_error(
                        "clean quarantined preset",
                        &user_root.join(quarantine),
                        error,
                    )
                });
            }
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => {
                return Err(io_error("quarantine preset", &target_path, error));
            }
        }
    }
    Err(PresetError::Io {
        operation: "quarantine preset",
        path: target_path,
        message: "could not allocate a private quarantine name".to_owned(),
    })
}

#[cfg(unix)]
struct UnixCopy<'a> {
    source_root: &'a File,
    budget: &'a mut CopyBudget,
    ancestors: std::collections::BTreeSet<(u64, u64)>,
    name_override: Option<&'a str>,
}

#[cfg(unix)]
impl<'a> UnixCopy<'a> {
    fn new(
        source_root: &'a File,
        budget: &'a mut CopyBudget,
        path: &Path,
        name_override: Option<&'a str>,
    ) -> Result<Self> {
        let stat = rustix::fs::fstat(source_root)
            .map_err(|error| io_error("inspect source directory", path, error))?;
        Ok(Self {
            source_root,
            budget,
            ancestors: std::collections::BTreeSet::from([unix_identity(&stat, path)?]),
            name_override,
        })
    }

    fn copy_directory(
        &mut self,
        source: &File,
        source_components: &[std::ffi::OsString],
        destination: &File,
        source_path: &Path,
        depth: usize,
        root: bool,
    ) -> Result<()> {
        use rustix::fs::{FileType, Mode, OFlags};
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        if depth > MAX_COPY_DEPTH {
            return Err(depth_error());
        }
        let directory = rustix::fs::Dir::read_from(source)
            .map_err(|error| io_error("read source directory", source_path, error))?;
        for entry in directory {
            let entry =
                entry.map_err(|error| io_error("read source directory", source_path, error))?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            account_entry(self.budget)?;
            let name = OsStr::from_bytes(bytes);
            let path = source_path.join(name);
            let mut components = source_components.to_vec();
            components.push(name.to_os_string());
            let mut resolved = self.resolve_source(components, &path)?;
            let file_type = FileType::from_raw_mode(resolved.stat.st_mode);
            if root && name == OsStr::new(METADATA_FILE) {
                if !file_type.is_file() {
                    return Err(PresetError::UnsafeEntry {
                        path,
                        reason: "metadata does not resolve to a contained regular file".to_owned(),
                    });
                }
                self.budget.source_metadata = true;
                let metadata = read_copy_metadata(&mut resolved.file, &path, self.name_override)?;
                account_bytes(
                    self.budget,
                    u64::try_from(metadata.len()).unwrap_or(u64::MAX),
                )?;
                write_file_unix(destination, name, &metadata, false, &path)?;
                continue;
            }
            if file_type.is_dir() {
                let next_depth = depth.checked_add(1).ok_or_else(depth_error)?;
                if next_depth > MAX_COPY_DEPTH {
                    return Err(depth_error());
                }
                let identity = unix_identity(&resolved.stat, &path)?;
                if !self.ancestors.insert(identity) {
                    return Err(PresetError::UnsafeEntry {
                        path,
                        reason: "directory symlink cycle cannot be materialized".to_owned(),
                    });
                }
                rustix::fs::mkdirat(destination, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)
                    .map_err(|error| io_error("create staged directory", &path, error))?;
                let created = rustix::fs::openat(
                    destination,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(|error| io_error("open staged directory", &path, error))?;
                let copy = self.copy_directory(
                    &resolved.file,
                    &resolved.components,
                    &created,
                    &path,
                    next_depth,
                    false,
                );
                self.ancestors.remove(&identity);
                copy?;
                created
                    .sync_all()
                    .map_err(|error| io_error("sync staged directory", &path, error))?;
            } else if file_type.is_file() {
                copy_file_unix(
                    &mut resolved.file,
                    destination,
                    name,
                    &path,
                    resolved.stat.st_mode,
                    resolved.stat.st_size,
                    self.budget,
                )?;
            } else {
                return Err(PresetError::UnsafeEntry {
                    path,
                    reason: "special files cannot be copied".to_owned(),
                });
            }
        }
        Ok(())
    }

    fn resolve_source(
        &mut self,
        components: Vec<std::ffi::OsString>,
        diagnostic_path: &Path,
    ) -> Result<ResolvedSource> {
        resolve_source_unix(self.source_root, components, self.budget, diagnostic_path)
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ResolvedSource {
    file: File,
    stat: rustix::fs::Stat,
    components: Vec<std::ffi::OsString>,
}

#[cfg(unix)]
fn resolve_source_unix(
    source_root: &File,
    components: Vec<std::ffi::OsString>,
    budget: &mut CopyBudget,
    diagnostic_path: &Path,
) -> Result<ResolvedSource> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::collections::{BTreeSet, VecDeque};

    let mut pending = VecDeque::from(components);
    let mut resolved = Vec::new();
    let mut directory = source_root
        .try_clone()
        .map_err(|error| io_error("clone source root", diagnostic_path, error))?;
    let mut followed = BTreeSet::new();
    let mut resolving_link_target = false;
    loop {
        let name = pending
            .pop_front()
            .ok_or_else(|| PresetError::UnsafeEntry {
                path: diagnostic_path.to_path_buf(),
                reason: "symlink resolved to the preset root instead of a file or directory"
                    .to_owned(),
            })?;
        if resolving_link_target {
            account_entry(budget)?;
        }
        let stat = rustix::fs::statat(&directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| io_error("inspect source entry", diagnostic_path, error))?;
        let file_type = FileType::from_raw_mode(stat.st_mode);
        if file_type.is_symlink() {
            if !followed.insert((stat.st_dev, stat.st_ino)) {
                return Err(PresetError::UnsafeEntry {
                    path: diagnostic_path.to_path_buf(),
                    reason: "symlink cycle cannot be materialized".to_owned(),
                });
            }
            let target = read_link_unix(&directory, &name, diagnostic_path)?;
            let mut replacement = resolved.clone();
            apply_relative_target(&mut replacement, &target, diagnostic_path)?;
            replacement.extend(pending);
            if replacement.len() > MAX_COPY_DEPTH {
                return Err(depth_error());
            }
            pending = replacement.into();
            resolved.clear();
            resolving_link_target = true;
            directory = source_root
                .try_clone()
                .map_err(|error| io_error("clone source root", diagnostic_path, error))?;
            continue;
        }
        if !file_type.is_dir() && !file_type.is_file() {
            return Err(PresetError::UnsafeEntry {
                path: diagnostic_path.to_path_buf(),
                reason: "source entry resolves to a special file".to_owned(),
            });
        }
        let flags = if file_type.is_dir() {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        } else {
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        let opened = rustix::fs::openat(&directory, &name, flags, Mode::empty())
            .map(File::from)
            .map_err(|error| {
                unsafe_entry(diagnostic_path, "source entry changed or is unsafe", error)
            })?;
        let opened_stat = rustix::fs::fstat(&opened)
            .map_err(|error| io_error("inspect opened source entry", diagnostic_path, error))?;
        if opened_stat.st_dev != stat.st_dev || opened_stat.st_ino != stat.st_ino {
            return Err(PresetError::UnsafeEntry {
                path: diagnostic_path.to_path_buf(),
                reason: "source entry changed while it was resolved".to_owned(),
            });
        }
        resolved.push(name);
        if pending.is_empty() {
            return Ok(ResolvedSource {
                file: opened,
                stat: opened_stat,
                components: resolved,
            });
        }
        if !file_type.is_dir() {
            return Err(PresetError::UnsafeEntry {
                path: diagnostic_path.to_path_buf(),
                reason: "a symlink path component does not resolve to a directory".to_owned(),
            });
        }
        directory = opened;
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
fn read_link_unix(
    directory: &File,
    name: &std::ffi::OsStr,
    diagnostic_path: &Path,
) -> Result<std::ffi::OsString> {
    use std::os::unix::ffi::OsStrExt as _;

    rustix::fs::readlinkat(directory, name, Vec::new())
        .map(|target| std::ffi::OsStr::from_bytes(target.to_bytes()).to_os_string())
        .map_err(|error| io_error("read source symlink", diagnostic_path, error))
}

#[cfg(all(unix, target_os = "redox"))]
fn read_link_unix(
    _directory: &File,
    _name: &std::ffi::OsStr,
    diagnostic_path: &Path,
) -> Result<std::ffi::OsString> {
    Err(PresetError::UnsafeEntry {
        path: diagnostic_path.to_path_buf(),
        reason: "safe symlink materialization is unsupported on this platform".to_owned(),
    })
}

#[cfg(unix)]
fn apply_relative_target(
    base: &mut Vec<std::ffi::OsString>,
    target: &std::ffi::OsStr,
    diagnostic_path: &Path,
) -> Result<()> {
    use std::path::Component;

    let target = Path::new(target);
    if target.is_absolute() {
        return Err(PresetError::UnsafeEntry {
            path: diagnostic_path.to_path_buf(),
            reason: "absolute symlinks cannot be materialized inside a preset".to_owned(),
        });
    }
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => base.push(name.to_os_string()),
            Component::ParentDir => {
                if base.pop().is_none() {
                    return Err(PresetError::UnsafeEntry {
                        path: diagnostic_path.to_path_buf(),
                        reason: "symlink escapes the source preset root".to_owned(),
                    });
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PresetError::UnsafeEntry {
                    path: diagnostic_path.to_path_buf(),
                    reason: "absolute symlinks cannot be materialized inside a preset".to_owned(),
                });
            }
        }
        if base.len() > MAX_COPY_DEPTH {
            return Err(depth_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_file_unix(
    input: &mut File,
    destination: &File,
    name: &std::ffi::OsStr,
    path: &Path,
    source_mode: rustix::fs::RawMode,
    source_size: i64,
    budget: &mut CopyBudget,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags};

    let expected = u64::try_from(source_size).map_err(|_| PresetError::CopyLimit {
        resource: "aggregate bytes",
        maximum: MAX_COPY_BYTES,
    })?;
    account_bytes(budget, expected)?;
    let owner_executable = source_mode & Mode::XUSR.bits() != 0;
    let mode = if owner_executable {
        Mode::RUSR | Mode::WUSR | Mode::XUSR
    } else {
        Mode::RUSR | Mode::WUSR
    };
    let output = rustix::fs::openat(
        destination,
        name,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        mode,
    )
    .map(File::from)
    .map_err(|error| io_error("create staged file", path, error))?;
    copy_exact_bounded(input, &output, expected, path)?;
    rustix::fs::fchmod(&output, mode)
        .map_err(|error| io_error("set staged file mode", path, error))?;
    output
        .sync_all()
        .map_err(|error| io_error("sync staged file", path, error))
}

#[cfg(unix)]
fn write_file_unix(
    destination: &File,
    name: &std::ffi::OsStr,
    content: &[u8],
    executable: bool,
    diagnostic_path: &Path,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags};

    let mode = if executable {
        Mode::RUSR | Mode::WUSR | Mode::XUSR
    } else {
        Mode::RUSR | Mode::WUSR
    };
    let mut output = rustix::fs::openat(
        destination,
        name,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        mode,
    )
    .map(File::from)
    .map_err(|error| io_error("create staged metadata", diagnostic_path, error))?;
    output
        .write_all(content)
        .map_err(|error| io_error("write staged metadata", diagnostic_path, error))?;
    rustix::fs::fchmod(&output, mode)
        .map_err(|error| io_error("set staged metadata mode", diagnostic_path, error))?;
    output
        .sync_all()
        .map_err(|error| io_error("sync staged metadata", diagnostic_path, error))
}

fn copy_exact_bounded(
    input: &mut File,
    mut output: &File,
    expected: u64,
    path: &Path,
) -> Result<()> {
    let mut remaining = expected.saturating_add(1);
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = input
            .read(&mut buffer[..limit])
            .map_err(|error| io_error("read source file", path, error))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|error| io_error("write staged file", path, error))?;
        let read = u64::try_from(read).expect("buffer length fits u64");
        copied = copied.saturating_add(read);
        remaining = remaining.saturating_sub(read);
    }
    if copied != expected {
        return Err(PresetError::UnsafeEntry {
            path: path.to_path_buf(),
            reason: "source file changed while it was copied".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Debug)]
struct UnixStage {
    parent: File,
    directory: File,
    parent_path: PathBuf,
    name: String,
    published: bool,
}

#[cfg(unix)]
impl UnixStage {
    fn create(parent: File, parent_path: &Path) -> Result<Self> {
        use rustix::fs::{Mode, OFlags};
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                ".rsi-agent-preset-{}-{sequence:016x}.tmp",
                std::process::id()
            );
            match rustix::fs::mkdirat(&parent, &name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => {
                    let opened = rustix::fs::openat(
                        &parent,
                        &name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    );
                    let directory = match opened {
                        Ok(directory) => File::from(directory),
                        Err(error) => {
                            let _ = rustix::fs::unlinkat(
                                &parent,
                                &name,
                                rustix::fs::AtFlags::REMOVEDIR,
                            );
                            return Err(io_error("open staging directory", parent_path, error));
                        }
                    };
                    let stage = Self {
                        parent,
                        directory,
                        parent_path: parent_path.to_path_buf(),
                        name,
                        published: false,
                    };
                    rustix::fs::fchmod(&stage.directory, Mode::RUSR | Mode::WUSR | Mode::XUSR)
                        .map_err(|error| {
                            io_error("set staging directory mode", parent_path, error)
                        })?;
                    return Ok(stage);
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(io_error("create staging directory", parent_path, error));
                }
            }
        }
        Err(PresetError::Io {
            operation: "create staging directory",
            path: parent_path.to_path_buf(),
            message: "could not allocate a private staging name".to_owned(),
        })
    }

    fn directory(&self) -> &File {
        &self.directory
    }

    fn sync(&self) -> Result<()> {
        self.directory
            .sync_all()
            .map_err(|error| io_error("sync staging directory", &self.parent_path, error))
    }

    fn publish(&mut self, target: &AgentPresetId) -> Result<()> {
        use rustix::fs::RenameFlags;

        match rustix::fs::renameat_with(
            &self.parent,
            &self.name,
            &self.parent,
            target.as_str(),
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                self.published = true;
                Ok(())
            }
            Err(rustix::io::Errno::EXIST) => Err(PresetError::PresetExists {
                id: target.as_str().to_owned(),
            }),
            Err(error) => Err(io_error(
                "publish preset",
                &self.parent_path.join(target.as_str()),
                error,
            )),
        }
    }
}

#[cfg(unix)]
impl Drop for UnixStage {
    fn drop(&mut self) {
        if !self.published {
            let _ = remove_tree_at(&self.parent, self.name.as_ref());
        }
    }
}

#[cfg(unix)]
fn remove_tree_at(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if FileType::from_raw_mode(stat.st_mode).is_dir() {
        let directory = rustix::fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(std::io::Error::from)?;
        for entry in rustix::fs::Dir::read_from(&directory).map_err(std::io::Error::from)? {
            let entry = entry.map_err(std::io::Error::from)?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            remove_tree_at(&directory, OsStr::from_bytes(bytes))?;
        }
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(Into::into)
    } else {
        rustix::fs::unlinkat(parent, name, AtFlags::empty()).map_err(Into::into)
    }
}

#[cfg(not(unix))]
fn copy_preset_platform(
    source: &Path,
    user_root: &Path,
    target: &AgentPresetId,
    name_override: Option<&str>,
) -> Result<()> {
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|error| io_error("inspect source preset", source, error))?;
    if !source_metadata.file_type().is_dir() {
        return Err(PresetError::UnsafeEntry {
            path: source.to_path_buf(),
            reason: "source is not a no-follow directory".to_owned(),
        });
    }
    let user_metadata = match fs::symlink_metadata(user_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(user_root)
                .map_err(|error| io_error("create user root", user_root, error))?;
            fs::symlink_metadata(user_root)
                .map_err(|error| io_error("inspect user root", user_root, error))?
        }
        Err(error) => return Err(io_error("inspect user root", user_root, error)),
    };
    if !user_metadata.file_type().is_dir() {
        return Err(PresetError::UnsafeEntry {
            path: user_root.to_path_buf(),
            reason: "user root is not a no-follow directory".to_owned(),
        });
    }
    let temporary = tempfile::Builder::new()
        .prefix(".rsi-agent-preset-")
        .tempdir_in(user_root)
        .map_err(|error| io_error("create staging directory", user_root, error))?;
    let mut budget = CopyBudget::default();
    copy_directory_portable(
        source,
        temporary.path(),
        0,
        true,
        &mut budget,
        name_override,
    )?;
    if let Some(name) = name_override
        && !budget.source_metadata
    {
        let metadata = render_metadata_with_name(MetadataWire::default(), name)?;
        account_entry(&mut budget)?;
        account_bytes(
            &mut budget,
            u64::try_from(metadata.len()).unwrap_or(u64::MAX),
        )?;
        fs::write(temporary.path().join(METADATA_FILE), &metadata).map_err(|error| {
            io_error(
                "write staged metadata",
                &temporary.path().join(METADATA_FILE),
                error,
            )
        })?;
    }
    let target_path = user_root.join(target.as_str());
    fs::rename(temporary.path(), &target_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            PresetError::PresetExists {
                id: target.as_str().to_owned(),
            }
        } else {
            io_error("publish preset", &target_path, error)
        }
    })
}

#[cfg(not(unix))]
fn delete_preset_platform(user_root: &Path, target: &AgentPresetId) -> Result<()> {
    let target_path = user_root.join(target.as_str());
    let metadata = fs::symlink_metadata(&target_path)
        .map_err(|error| io_error("inspect delete target", &target_path, error))?;
    if !metadata.file_type().is_dir() {
        return Err(PresetError::UnsafeEntry {
            path: target_path,
            reason: "delete target is not a no-follow directory".to_owned(),
        });
    }
    let quarantine = tempfile::Builder::new()
        .prefix(".rsi-agent-preset-deleted-")
        .tempdir_in(user_root)
        .map_err(|error| io_error("create delete quarantine", user_root, error))?;
    let moved = quarantine.path().join("preset");
    fs::rename(&target_path, &moved)
        .map_err(|error| io_error("quarantine preset", &target_path, error))?;
    fs::remove_dir_all(&moved).map_err(|error| io_error("clean quarantined preset", &moved, error))
}

#[cfg(not(unix))]
fn copy_directory_portable(
    source: &Path,
    destination: &Path,
    depth: usize,
    root: bool,
    budget: &mut CopyBudget,
    name_override: Option<&str>,
) -> Result<()> {
    if depth > MAX_COPY_DEPTH {
        return Err(depth_error());
    }
    let entries =
        fs::read_dir(source).map_err(|error| io_error("read source directory", source, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error("read source directory", source, error))?;
        account_entry(budget)?;
        let source_path = entry.path();
        let name = entry.file_name();
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| io_error("inspect source entry", &source_path, error))?;
        if root && name == METADATA_FILE {
            if !metadata.file_type().is_file() {
                return Err(PresetError::UnsafeEntry {
                    path: source_path,
                    reason: "metadata is not a no-follow regular file".to_owned(),
                });
            }
            budget.source_metadata = true;
            let mut input = File::open(&source_path)
                .map_err(|error| io_error("open source metadata", &source_path, error))?;
            let metadata = read_copy_metadata(&mut input, &source_path, name_override)?;
            account_bytes(budget, u64::try_from(metadata.len()).unwrap_or(u64::MAX))?;
            fs::write(destination.join(&name), metadata).map_err(|error| {
                io_error("write staged metadata", &destination.join(&name), error)
            })?;
            continue;
        }
        let destination_path = destination.join(&name);
        if metadata.file_type().is_dir() {
            let next_depth = depth.checked_add(1).ok_or_else(depth_error)?;
            if next_depth > MAX_COPY_DEPTH {
                return Err(depth_error());
            }
            fs::create_dir(&destination_path)
                .map_err(|error| io_error("create staged directory", &destination_path, error))?;
            copy_directory_portable(
                &source_path,
                &destination_path,
                next_depth,
                false,
                budget,
                name_override,
            )?;
        } else if metadata.file_type().is_file() {
            account_bytes(budget, metadata.len())?;
            let mut input = File::open(&source_path)
                .map_err(|error| io_error("open source file", &source_path, error))?;
            let output = File::create(&destination_path)
                .map_err(|error| io_error("create staged file", &destination_path, error))?;
            copy_exact_bounded(&mut input, &output, metadata.len(), &source_path)?;
        } else {
            return Err(PresetError::UnsafeEntry {
                path: source_path,
                reason: "symlinks and special files cannot be copied".to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unsafe_entry(path: &Path, reason: &str, error: rustix::io::Errno) -> PresetError {
    PresetError::UnsafeEntry {
        path: path.to_path_buf(),
        reason: format!("{reason}: {error}"),
    }
}

#[cfg(unix)]
fn unix_identity(stat: &rustix::fs::Stat, path: &Path) -> Result<(u64, u64)> {
    let device = stat
        .st_dev
        .identity_u64()
        .ok_or_else(|| PresetError::UnsafeEntry {
            path: path.to_path_buf(),
            reason: "source device identity cannot be represented".to_owned(),
        })?;
    let inode = stat
        .st_ino
        .identity_u64()
        .ok_or_else(|| PresetError::UnsafeEntry {
            path: path.to_path_buf(),
            reason: "source inode identity cannot be represented".to_owned(),
        })?;
    Ok((device, inode))
}

#[cfg(unix)]
trait UnixIdentityPart {
    fn identity_u64(self) -> Option<u64>;
}

#[cfg(unix)]
impl UnixIdentityPart for u64 {
    fn identity_u64(self) -> Option<u64> {
        Some(self)
    }
}

#[cfg(unix)]
impl UnixIdentityPart for u32 {
    fn identity_u64(self) -> Option<u64> {
        Some(u64::from(self))
    }
}

#[cfg(unix)]
impl UnixIdentityPart for i64 {
    fn identity_u64(self) -> Option<u64> {
        u64::try_from(self).ok()
    }
}

#[cfg(unix)]
impl UnixIdentityPart for i32 {
    fn identity_u64(self) -> Option<u64> {
        u64::try_from(self).ok()
    }
}

fn io_error(operation: &'static str, path: &Path, error: impl std::fmt::Display) -> PresetError {
    PresetError::Io {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
