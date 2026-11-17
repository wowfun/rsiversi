use super::{
    Address, BTreeMap, Deserialize, EncodedValue, JSON_STORAGE_FORMAT, MAXIMUM_STORAGE_RECORDS,
    Mutation, Path, PathBuf, RecordObjectSize, RecordPolicy, Recovery, RecoveryCause, Serialize,
    StorageError, UnitOwner, UnitSnapshot, UnitState, atomic_write, check_size, corrupt,
    directory_exists, ensure_directory, fs, hash, io, io_error, read_optional,
    sync_parent_directory,
};
use serde_json::value::RawValue;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    format: u32,
    unit: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input<'a> {
    format: u32,
    unit: String,
    address: Address,
    version: u32,
    #[serde(borrow)]
    value: &'a RawValue,
}
#[derive(Serialize)]
struct Output<'a> {
    format: u32,
    unit: &'a str,
    address: &'a Address,
    version: u32,
    value: &'a EncodedValue,
}

pub(super) fn record_path(root: &Path, address: &Address) -> PathBuf {
    if address.is_global() {
        root.join("global.json")
    } else {
        root.join(hash(address.table().as_bytes()))
            .join(format!("{}.json", hash(address.key().as_bytes())))
    }
}
pub(super) fn load(
    owner: &UnitOwner,
    root: &Path,
) -> Result<(UnitSnapshot, UnitState), StorageError> {
    let mut snapshot = UnitSnapshot::default();
    let mut size = RecordObjectSize::default();
    let mut lengths = BTreeMap::new();
    if directory_exists(root)? {
        // No record is interpreted without an intact unit identity.
        let Some(bytes) = read_optional(&root.join("unit.json"), 4096)? else {
            // A crash before the first identity publication may leave only an
            // empty owned directory and temporary files, never admitted records.
            for entry in fs::read_dir(root).map_err(io_error)? {
                if !residue(&entry.map_err(io_error)?.file_name()) {
                    return Err(corrupt("missing per-record unit identity"));
                }
            }
            return Ok((
                snapshot,
                UnitState::PerRecord {
                    path: root.to_owned(),
                    lengths,
                    size,
                },
            ));
        };
        let identity: Identity = serde_json::from_slice(&bytes).map_err(corrupt)?;
        if identity.format != JSON_STORAGE_FORMAT {
            return Err(corrupt("unsupported JSON storage format"));
        }
        if identity.unit != owner.descriptor.name() {
            return Err(corrupt("JSON unit identity mismatch"));
        }
        let tables: BTreeMap<_, _> = owner
            .descriptor
            .tables()
            .keys()
            .map(|name| (hash(name.as_bytes()), name.as_str()))
            .collect();
        let mut paths = record_paths(root, &tables)?;
        // Ordered opens make recovery reports deterministic without concurrent
        // record reads or an unbounded filesystem task queue.
        paths.sort();
        for (path, table) in paths {
            match load_record(owner, root, &path, table) {
                Ok((address, value)) => {
                    size = size
                        .with_entry(None, address.entry_bytes(value.encoded_len())?)
                        .map_err(corrupt)?;
                    check_size(size).map_err(corrupt)?;
                    lengths.insert(address.clone(), value.encoded_len());
                    if snapshot.records.insert(address, value).is_some() {
                        return Err(corrupt("duplicate stored address"));
                    }
                }
                Err(RecordError::Invalid(cause))
                    if table.as_ref().is_some_and(|table| {
                        owner.descriptor.tables()[*table] == RecordPolicy::BackupAndSkip
                    }) =>
                {
                    let backup = backup(owner, &path)?;
                    snapshot.recoveries.push(Recovery {
                        table: table.expect("disposable table").to_owned(),
                        key: None,
                        backup,
                        cause,
                    });
                }
                Err(RecordError::Invalid(_)) => {
                    return Err(corrupt("malformed authoritative record"));
                }
                Err(RecordError::Fatal(error)) => return Err(error),
            }
        }
        // A recovered acknowledgement includes names in both directory levels.
        for table in tables.keys() {
            let path = root.join(table);
            if directory_exists(&path)? {
                sync_parent_directory(&path)?;
            }
        }
        sync_parent_directory(root)?;
    }
    Ok((
        snapshot,
        UnitState::PerRecord {
            path: root.to_owned(),
            lengths,
            size,
        },
    ))
}
enum RecordError {
    Invalid(RecoveryCause),
    Fatal(StorageError),
}
impl From<StorageError> for RecordError {
    fn from(error: StorageError) -> Self {
        Self::Fatal(error)
    }
}
fn load_record(
    owner: &UnitOwner,
    root: &Path,
    path: &Path,
    table: Option<&str>,
) -> Result<(Address, EncodedValue), RecordError> {
    let bytes = read_optional(path, owner.backend.config.maximum_document_bytes)?
        .ok_or_else(|| corrupt("record disappeared during open"))?;
    let input: Input<'_> = serde_json::from_slice(&bytes).map_err(|error| {
        if matches!(
            error.classify(),
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof
        ) {
            RecordError::Invalid(RecoveryCause::Envelope)
        } else {
            RecordError::Fatal(corrupt("invalid record envelope identity or version"))
        }
    })?;
    if input.format != JSON_STORAGE_FORMAT {
        return Err(corrupt("unsupported JSON storage format").into());
    }
    if input.unit != owner.descriptor.name()
        || table != (!input.address.is_global()).then_some(input.address.table())
        || record_path(root, &input.address) != path
    {
        return Err(corrupt("record identity mismatch").into());
    }
    owner
        .descriptor
        .admit_address(&input.address)
        .map_err(corrupt)?;
    owner.descriptor.admit_version(input.version)?;
    let value = EncodedValue::copy_raw(input.value)
        .map_err(|_| RecordError::Invalid(RecoveryCause::Value))?;
    Ok((input.address, value))
}
fn valid_hash(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn residue(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.starts_with('.') && Path::new(name).extension() == Some(std::ffi::OsStr::new("tmp"))
    })
}
fn materialize(owner: &UnitOwner, root: &Path) -> Result<(), StorageError> {
    directory_exists(&owner.backend.config.path)?;
    directory_exists(root)?;
    let identity = root.join("unit.json");
    if !identity.try_exists().map_err(io_error)? {
        atomic_write(
            &identity,
            4096,
            &Identity {
                format: JSON_STORAGE_FORMAT,
                unit: owner.descriptor.name().into(),
            },
        )?;
    }
    Ok(())
}
pub(super) fn write(
    owner: &UnitOwner,
    root: &Path,
    lengths: &mut BTreeMap<Address, usize>,
    size: &mut RecordObjectSize,
    mut changes: Vec<Mutation>,
) -> Result<(), StorageError> {
    changes.retain(|c| c.value.is_some() || lengths.contains_key(&c.address));
    if changes.is_empty() {
        return Ok(());
    }
    let mut next = *size;
    for change in &changes {
        let old = lengths
            .get(&change.address)
            .map(|length| change.address.entry_bytes(*length))
            .transpose()?;
        next = if let Some(value) = &change.value {
            next.with_entry(old, change.address.entry_bytes(value.encoded_len())?)?
        } else {
            next.without_entry(old.expect("existing deletion"))?
        };
    }
    check_size(next)?;
    materialize(owner, root)?;
    if changes.len() == 1 && changes[0].value.is_some() {
        let change = &changes[0];
        let path = record_path(root, &change.address);
        atomic_write(
            &path,
            owner.backend.config.maximum_document_bytes,
            &Output {
                format: JSON_STORAGE_FORMAT,
                unit: owner.descriptor.name(),
                address: &change.address,
                version: owner.descriptor.version(),
                value: change.value.as_ref().expect("put"),
            },
        )?;
    } else {
        delete_many(
            changes
                .iter()
                .map(|change| record_path(root, &change.address)),
        )?;
    }
    for change in changes {
        if let Some(value) = change.value {
            lengths.insert(change.address, value.encoded_len());
        } else {
            lengths.remove(&change.address);
        }
    }
    *size = next;
    Ok(())
}

// Moving to an exclusive owned name preserves source bytes without a second
// full-body copy. Both source and quarantine names are durable before omission.
pub(super) fn backup(owner: &UnitOwner, path: &Path) -> Result<PathBuf, StorageError> {
    let directory = owner.backend.config.path.join(".quarantine");
    ensure_directory(&directory)?;
    let (backup, file) = io::create_temporary_file(&directory, "record")?;
    drop(file);
    if let Err(error) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&backup);
        return Err(io_error(error));
    }
    sync_parent_directory(&directory)
        .and_then(|()| sync_parent_directory(path.parent().expect("source parent")))
        .map_err(|error| StorageError::OutcomeUnknown(error.to_string()))?;
    Ok(backup)
}
#[cfg(not(unix))]
pub(super) fn delete_many(_paths: impl Iterator<Item = PathBuf>) -> Result<(), StorageError> {
    super::require_native_platform()
}
#[cfg(unix)]
pub(super) fn delete_many(paths: impl Iterator<Item = PathBuf>) -> Result<(), StorageError> {
    use rsi_files_native_fs as native;
    let paths: Vec<_> = paths.collect();
    let mut parents = BTreeMap::new();
    for path in &paths {
        let parent = path.parent().expect("record parent");
        if !parents.contains_key(parent) {
            parents.insert(
                parent.to_owned(),
                native::open_absolute_directory_no_follow(parent).map_err(io_error)?,
            );
        }
    }
    let mut moved = Vec::new();
    let staged = (|| {
        for path in &paths {
            let parent = &parents[path.parent().expect("record parent")];
            let source = Path::new(path.file_name().expect("record name"));
            let temporary = deletion_name(parent)?;
            if let Err(error) = rustix::fs::renameat(parent, source, parent, &temporary) {
                let _ = parent.remove_child(&temporary);
                return Err(io_error(error));
            }
            moved.push((parent, source, temporary));
        }
        for parent in parents.values() {
            parent.sync_all().map_err(io_error)?;
        }
        Ok(())
    })();
    if let Err(error) = staged {
        for (parent, source, temporary) in moved.iter().rev() {
            rustix::fs::renameat(*parent, temporary, *parent, *source).map_err(|restore| {
                StorageError::OutcomeUnknown(format!("{error}; restore: {restore}"))
            })?;
        }
        for parent in parents.values() {
            parent.sync_all().map_err(|restore| {
                StorageError::OutcomeUnknown(format!("{error}; restore sync: {restore}"))
            })?;
        }
        return Err(error);
    }
    // Logical deletion is already durable. Residue is outside the readable set.
    for (parent, _, temporary) in moved {
        let _ = parent.remove_child(&temporary);
    }
    Ok(())
}
#[cfg(unix)]
fn deletion_name(parent: &rsi_files_native_fs::Directory) -> Result<PathBuf, StorageError> {
    for _ in 0..super::MAXIMUM_TEMPORARY_NAME_ATTEMPTS {
        let sequence = super::NEXT_TEMPORARY_SEQUENCE.fetch_add(1, super::Ordering::Relaxed);
        let name = PathBuf::from(format!(".deleted-{}-{sequence}.tmp", std::process::id()));
        match rsi_files_native_fs::create_relative_private_file_no_follow(parent, &name) {
            Ok(file) => {
                drop(file);
                return Ok(name);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(StorageError::Io(
        "temporary deletion names exhausted".into(),
    ))
}

fn record_paths<'a>(
    root: &Path,
    tables: &BTreeMap<String, &'a str>,
) -> Result<Vec<(PathBuf, Option<&'a str>)>, StorageError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        if name == "unit.json" {
            continue;
        }
        if name == "global.json" {
            if paths.len() >= MAXIMUM_STORAGE_RECORDS {
                return Err(corrupt("record count exceeds bound"));
            }
            paths.push((entry.path(), None));
            continue;
        }
        if residue(&name) {
            continue;
        }
        let table = name
            .to_str()
            .and_then(|name| tables.get(name))
            .ok_or_else(|| corrupt("undeclared per-record table"))?;
        directory_exists(&entry.path())?;
        for entry in fs::read_dir(entry.path()).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            if residue(&entry.file_name()) {
                continue;
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| corrupt("invalid record filename"))?;
            name.strip_suffix(".json")
                .filter(|stem| valid_hash(stem))
                .ok_or_else(|| corrupt("invalid record filename"))?;
            if paths.len() >= MAXIMUM_STORAGE_RECORDS {
                return Err(corrupt("record count exceeds bound"));
            }
            paths.push((entry.path(), Some(*table)));
        }
    }
    Ok(paths)
}
