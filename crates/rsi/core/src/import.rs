//! Explicit home transfer. Providers continue to own the imported formats.
use crate::maintenance::STATE_ENTRIES as STATE;
use crate::{LaunchConfiguration, Result, RsiError, maintenance::Home};
use rsi_files_native_fs::{self as fs, Directory, EntryKind};
use rsi_host::HostPaths;
use rsi_service_host::{HostOwnerLease, HostStartupLease, ServiceHostPaths};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

const PENDING: &str = "import.pending.json";
const STAGING: &str = "import.staging.json";
const CONFIG: &[&str] = &[
    "rsi.toml",
    "settings.json",
    "application-profiles",
    "host-profiles",
    "agent-presets",
    "credentials",
    "AGENTS.md",
    "agents",
    "skills",
];
const MAX_ENTRIES: usize = 16_384;
const MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Entry names and recoverable archives; imported contents are never printed.
#[derive(Debug, Serialize)]
pub struct ImportReceipt {
    /// None for a dry run.
    pub transaction: Option<String>,
    /// Recovery discarded an incomplete preparation; no payload was published.
    pub discarded_staging: bool,
    /// Final admitted paths.
    pub entries: Vec<PathBuf>,
    /// Retained previous destination payloads.
    pub backups: Vec<PathBuf>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format: u32,
    id: String,
    config: PathBuf,
    state: PathBuf,
    entries: Vec<Entry>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    config: bool,
    name: String,
    old: Option<String>,
    new: String,
}

/// Ordinary commands must not activate a partially imported home pair.
pub fn reject_pending_import(paths: &HostPaths) -> Result<()> {
    for home in [paths.config(), paths.state()] {
        for name in [PENDING, STAGING] {
            match std::fs::symlink_metadata(home.join("session-host").join(name)) {
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                _ => {
                    return Err(error(
                        "an import is pending; run rsi import recover with the original homes",
                    ));
                }
            }
        }
    }
    Ok(())
}

struct Homes {
    config: Home,
    state: Home,
    authorities: Vec<Home>,
    _owner: HostOwnerLease,
    _startup: HostStartupLease,
}
impl Homes {
    fn open(paths: &HostPaths) -> Result<Self> {
        let host = ServiceHostPaths::from_host_paths(paths).map_err(error)?;
        let startup = HostStartupLease::try_acquire(&host).map_err(error)?;
        let owner = HostOwnerLease::try_acquire(host).map_err(error)?;
        let config = Home::open(paths.config())?;
        let state = Home::open(paths.state())?;
        let mut authorities = vec![Home::open(&paths.config().join("session-host"))?];
        if canonical(paths.config())? != canonical(paths.state())? {
            authorities.push(Home::open(&paths.state().join("session-host"))?);
        }
        Ok(Self {
            config,
            state,
            authorities,
            _owner: owner,
            _startup: startup,
        })
    }
    fn selected(&self, config: bool) -> &Home {
        if config { &self.config } else { &self.state }
    }
}

/// Copies only explicit owned entries, without starting providers or rewriting paths.
/// Actual import leases both stopped home pairs; dry runs only observe their contents.
pub fn import_product_homes(
    destination: &HostPaths,
    source: &HostPaths,
    dry_run: bool,
) -> Result<ImportReceipt> {
    import_with_checkpoint(destination, source, dry_run, |_| Ok(()))
}
fn import_with_checkpoint(
    destination: &HostPaths,
    source: &HostPaths,
    dry_run: bool,
    checkpoint: impl Fn(usize) -> Result<()>,
) -> Result<ImportReceipt> {
    // Resolve before creating authorities so overlap cannot change source payloads.
    for from in [source.config(), source.state()] {
        fs::open_absolute_directory_no_follow(from).map_err(error)?;
        for to in [destination.config(), destination.state()] {
            if overlaps(from, to)? {
                return Err(error("source and destination homes overlap"));
            }
        }
    }
    reject_pending_import(source)?;
    reject_pending_import(destination)?;
    if dry_run {
        return preview(destination, source);
    }
    let source_homes = Homes::open(source)?;
    let homes = Homes::open(destination)?;
    // Durable intent may have appeared between preflight and acquiring both leases.
    reject_pending_import(source)?;
    reject_pending_import(destination)?;
    // A reset needs its own recovery, never a second maintenance transaction.
    for home in source_homes.authorities.iter().chain(&homes.authorities) {
        if home.kind("reset.pending.json")?.is_some() {
            return Err(error("recover the pending reset before importing"));
        }
    }
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(error)?;
    let mut journal = Journal {
        format: 1,
        id: hex::encode(random),
        config: canonical(destination.config())?,
        state: canonical(destination.state())?,
        entries: Vec::new(),
    };
    let mut budget = Budget::default();
    // Complete preflight before copying or archiving any payload.
    for (config, names) in [(true, CONFIG), (false, STATE)] {
        let from = source_homes.selected(config);
        for name in names {
            if from.kind(name)?.is_none() {
                continue;
            }
            require_kind(from, name)?;
            let new = tree(&from.directory, name, None, &mut budget, 0)?;
            let target = homes.selected(config);
            let old = fingerprint(target, name)?;
            journal.entries.push(Entry {
                config,
                name: (*name).into(),
                old,
                new,
            });
        }
    }
    validate_configuration(&source_homes.config, None)?;
    let staging = encode_journal(&journal)?;
    for authority in &homes.authorities {
        authority.create(STAGING, &staging)?;
    }
    if let Err(failure) = prepare(&mut journal, &source_homes, &homes) {
        discard_staging(&journal, &homes)?;
        return Err(failure);
    }
    let bytes = encode_journal(&journal)?;
    let result = settle(&journal, &bytes, &homes, checkpoint)?;
    clear_staging(&homes)?;
    Ok(result)
}

fn preview(destination: &HostPaths, source: &HostPaths) -> Result<ImportReceipt> {
    let mut budget = Budget::default();
    let mut entries = Vec::new();
    for paths in [source, destination] {
        for path in [paths.config(), paths.state()] {
            let root = match fs::open_absolute_directory_no_follow(path) {
                Ok(root) => root,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => continue,
                Err(failure) => return Err(error(failure)),
            };
            let authority =
                match fs::open_relative_directory_no_follow(&root, Path::new("session-host")) {
                    Ok(authority) => authority,
                    Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(failure) => return Err(error(failure)),
                };
            match authority.child_kind(Path::new("reset.pending.json")) {
                Ok(_) => return Err(error("recover the pending reset before importing")),
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                Err(failure) => return Err(error(failure)),
            }
        }
    }
    for (from, to, names) in [
        (source.config(), destination.config(), CONFIG),
        (source.state(), destination.state(), STATE),
    ] {
        let home = Home::observe(from)?;
        let target = match fs::open_absolute_directory_no_follow(to) {
            Ok(directory) => Some(directory),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => None,
            Err(failure) => return Err(error(failure)),
        };
        for name in names {
            if home.kind(name)?.is_none() {
                continue;
            }
            require_kind(&home, name)?;
            tree(&home.directory, name, None, &mut budget, 0)?;
            if let Some(directory) = &target {
                match directory.child_kind(Path::new(name)) {
                    Ok(_) => {
                        tree(directory, name, None, &mut Budget::default(), 0)?;
                    }
                    Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {}
                    Err(failure) => return Err(error(failure)),
                }
            }
            entries.push(to.join(name));
        }
    }
    validate_configuration(&Home::observe(source.config())?, None)?;
    Ok(ImportReceipt {
        transaction: None,
        discarded_staging: false,
        entries,
        backups: Vec::new(),
    })
}

fn encode_journal(journal: &Journal) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(journal).map_err(error)?;
    if bytes.len() > 64 * 1024 {
        return Err(error("import journal exceeds its bound"));
    }
    Ok(bytes)
}

fn prepare(journal: &mut Journal, source_homes: &Homes, homes: &Homes) -> Result<()> {
    let mut copy_budget = Budget::default();
    for entry in &mut journal.entries {
        let from = source_homes.selected(entry.config);
        let target = homes.selected(entry.config);
        let stage = format!(".import-{}-stage-{}", journal.id, entry.name);
        let copied = tree(
            &from.directory,
            &entry.name,
            Some((&target.directory, &stage)),
            &mut copy_budget,
            0,
        )?;
        if copied != entry.new || fingerprint(from, &entry.name)?.as_ref() != Some(&entry.new) {
            return Err(error(
                "source changed while staging; original and destination payloads are retained",
            ));
        }
        if !entry.config && entry.name == "agent" {
            rsi_agent_store_sqlite::SqliteStore::verify(target.path.join(&stage)).map_err(error)?;
            // Offline verification may settle SQLite sidecars in the private copy.
            entry.new = fingerprint(target, &stage)?.ok_or_else(|| error("staging disappeared"))?;
        }
    }
    validate_configuration(&homes.config, Some(&journal.id))?;
    Ok(())
}

/// Completes staged import intent without opening the original source.
pub fn recover_product_import(paths: &HostPaths) -> Result<ImportReceipt> {
    let homes = Homes::open(paths)?;
    let staging = read_intent(&homes, STAGING)?;
    let staging = staging
        .map(|bytes| serde_json::from_slice::<Journal>(&bytes).map_err(error))
        .transpose()?;
    if let Some(journal) = &staging {
        validate(journal, paths)?;
    }
    let bytes = read_intent(&homes, PENDING)?;
    let bytes = match (bytes, &staging) {
        (Some(bytes), _) => bytes,
        (None, Some(journal)) => {
            if let Some(bytes) = read_intent(&homes, &format!("import-{}.json", journal.id))? {
                bytes
            } else {
                discard_staging(journal, &homes)?;
                return Ok(ImportReceipt {
                    transaction: Some(journal.id.clone()),
                    discarded_staging: true,
                    entries: Vec::new(),
                    backups: Vec::new(),
                });
            }
        }
        (None, None) => return Err(error("no import is pending")),
    };
    let journal: Journal = serde_json::from_slice(&bytes).map_err(error)?;
    validate(&journal, paths)?;
    if staging.as_ref().is_some_and(|prepared| {
        prepared.id != journal.id
            || prepared
                .entries
                .iter()
                .map(|e| (e.config, &e.name))
                .collect::<Vec<_>>()
                != journal
                    .entries
                    .iter()
                    .map(|e| (e.config, &e.name))
                    .collect::<Vec<_>>()
    }) {
        return Err(error("staging and final import identities differ"));
    }
    let result = settle(&journal, &bytes, &homes, |_| Ok(()))?;
    clear_staging(&homes)?;
    Ok(result)
}

fn read_intent(homes: &Homes, name: &str) -> Result<Option<Vec<u8>>> {
    let mut bytes = None;
    for home in &homes.authorities {
        if let Some(value) = home.read(name)? {
            if bytes.as_ref().is_some_and(|other| *other != value) {
                return Err(error("home authorities contain different imports"));
            }
            bytes = Some(value);
        }
    }
    Ok(bytes)
}

fn clear_staging(homes: &Homes) -> Result<()> {
    for authority in &homes.authorities {
        if authority.kind(STAGING)?.is_some() {
            authority
                .directory
                .remove_child(Path::new(STAGING))
                .map_err(error)?;
            authority.directory.sync_all().map_err(error)?;
        }
    }
    Ok(())
}

fn discard_staging(journal: &Journal, homes: &Homes) -> Result<()> {
    let mut remaining = MAX_ENTRIES;
    for entry in &journal.entries {
        let home = homes.selected(entry.config);
        let stage = format!(".import-{}-stage-{}", journal.id, entry.name);
        if home.kind(&stage)?.is_some() {
            remove_staging(&home.directory, Path::new(&stage), 0, &mut remaining)?;
            home.directory.sync_all().map_err(error)?;
        }
    }
    clear_staging(homes)
}

fn remove_staging(
    parent: &Directory,
    name: &Path,
    depth: usize,
    remaining: &mut usize,
) -> Result<()> {
    if depth > 64 || *remaining == 0 {
        return Err(error("staging cleanup exceeds traversal bound"));
    }
    *remaining -= 1;
    match parent.child_kind(name).map_err(error)? {
        EntryKind::File => parent.remove_child(name).map_err(error)?,
        EntryKind::Directory => {
            let directory = fs::open_relative_directory_no_follow(parent, name).map_err(error)?;
            for child in directory.entries().map_err(error)? {
                let child = child.map_err(error)?;
                remove_staging(
                    &directory,
                    Path::new(&child.file_name()),
                    depth + 1,
                    remaining,
                )?;
            }
            drop(directory);
            parent.remove_empty_directory(name).map_err(error)?;
        }
        _ => return Err(error("staging cleanup refuses links and special objects")),
    }
    Ok(())
}

fn validate(journal: &Journal, paths: &HostPaths) -> Result<()> {
    if journal.format != 1
        || !hexadecimal(&journal.id, 32)
        || journal.config != canonical(paths.config())?
        || journal.state != canonical(paths.state())?
        || journal.entries.len() > CONFIG.len() + STATE.len()
    {
        return Err(error(
            "invalid import journal or different home authorities",
        ));
    }
    let mut names = BTreeSet::new();
    for entry in &journal.entries {
        if !(if entry.config { CONFIG } else { STATE }).contains(&entry.name.as_str())
            || !names.insert((entry.config, &entry.name))
            || !hexadecimal(&entry.new, 64)
            || entry.old.as_ref().is_some_and(|v| !hexadecimal(v, 64))
        {
            return Err(error("invalid or repeated import participant"));
        }
    }
    Ok(())
}

enum Progress {
    Prepared,
    Archived,
    Published,
}
fn progress(journal: &Journal, entry: &Entry, home: &Home) -> Result<Progress> {
    let current = fingerprint(home, &entry.name)?;
    let stage = fingerprint(
        home,
        &format!(".import-{}-stage-{}", journal.id, entry.name),
    )?;
    let backup = fingerprint(
        home,
        &format!(".import-{}-backup-{}", journal.id, entry.name),
    )?;
    if stage.as_ref() == Some(&entry.new) && backup.is_none() && current == entry.old {
        Ok(Progress::Prepared)
    } else if stage.as_ref() == Some(&entry.new) && current.is_none() && backup == entry.old {
        Ok(Progress::Archived)
    } else if stage.is_none() && current.as_ref() == Some(&entry.new) && backup == entry.old {
        Ok(Progress::Published)
    } else {
        Err(error(format!(
            "unexpected import participant {}; retain journal and backups",
            entry.name
        )))
    }
}
fn settle(
    journal: &Journal,
    bytes: &[u8],
    homes: &Homes,
    checkpoint: impl Fn(usize) -> Result<()>,
) -> Result<ImportReceipt> {
    // Validate the complete topology before recovering any one participant.
    let progress = journal
        .entries
        .iter()
        .map(|entry| progress(journal, entry, homes.selected(entry.config)))
        .collect::<Result<Vec<_>>>()?;
    let archive = format!("import-{}.json", journal.id);
    for authority in &homes.authorities {
        if let Some(previous) = authority.read(PENDING)? {
            if previous != bytes {
                return Err(error("different import intent"));
            }
        } else if let Some(previous) = authority.read(&archive)? {
            if previous != bytes {
                return Err(error("different import receipt"));
            }
        } else {
            authority.create(PENDING, bytes)?;
        }
    }
    let mut step = 0;
    for (entry, progress) in journal.entries.iter().zip(progress) {
        let home = homes.selected(entry.config);
        let stage = format!(".import-{}-stage-{}", journal.id, entry.name);
        let backup = format!(".import-{}-backup-{}", journal.id, entry.name);
        match progress {
            Progress::Published => continue,
            Progress::Prepared if entry.old.is_some() => {
                home.rename(
                    &entry.name,
                    &backup,
                    home.kind(&entry.name)?
                        .ok_or_else(|| error("participant disappeared"))?,
                )?;
                step += 1;
                checkpoint(step)?;
            }
            _ => {}
        }
        home.rename(
            &stage,
            &entry.name,
            home.kind(&stage)?
                .ok_or_else(|| error("stage disappeared"))?,
        )?;
        step += 1;
        checkpoint(step)?;
    }
    if homes.config.kind(crate::LAUNCH_FILE)?.is_none() {
        LaunchConfiguration::initialize(&homes.config.path)?;
    }
    for authority in &homes.authorities {
        if authority.read(PENDING)?.is_some() {
            authority.rename(PENDING, &archive, EntryKind::File)?;
        }
    }
    Ok(receipt(journal, homes))
}
fn receipt(journal: &Journal, homes: &Homes) -> ImportReceipt {
    ImportReceipt {
        transaction: Some(journal.id.clone()),
        discarded_staging: false,
        entries: journal
            .entries
            .iter()
            .map(|e| homes.selected(e.config).path.join(&e.name))
            .collect(),
        backups: journal
            .entries
            .iter()
            .filter(|e| e.old.is_some())
            .map(|e| {
                homes
                    .selected(e.config)
                    .path
                    .join(format!(".import-{}-backup-{}", journal.id, e.name))
            })
            .collect(),
    }
}

fn validate_configuration(home: &Home, stage: Option<&str>) -> Result<()> {
    for name in ["rsi.toml", "settings.json"] {
        let selected = stage.map_or_else(
            || name.to_owned(),
            |id| format!(".import-{id}-stage-{name}"),
        );
        if home.kind(&selected)?.is_none() {
            continue;
        }
        let mut file = fs::open_relative_file_no_follow(&home.directory, Path::new(&selected))
            .map_err(error)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(error("configuration exceeds its owner's bound"));
        }
        if name == "rsi.toml" {
            LaunchConfiguration::parse(&bytes)?;
        } else {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
            let object = value
                .as_object()
                .ok_or_else(|| error("Settings root is not an object"))?;
            for (namespace, section) in object {
                rsi_settings_protocol::validate_namespace(namespace).map_err(error)?;
                rsi_settings_protocol::validate_section(section).map_err(error)?;
            }
        }
    }
    Ok(())
}
fn require_kind(home: &Home, name: &str) -> Result<()> {
    let file = matches!(
        name,
        "rsi.toml"
            | "settings.json"
            | "AGENTS.md"
            | "base.sqlite3"
            | "base.sqlite3-wal"
            | "base.sqlite3-shm"
    );
    if home.kind(name)?
        != Some(if file {
            EntryKind::File
        } else {
            EntryKind::Directory
        })
    {
        return Err(error(format!("unexpected type for {name}")));
    }
    Ok(())
}
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
}
fn fingerprint(home: &Home, name: &str) -> Result<Option<String>> {
    if home.kind(name)?.is_none() {
        return Ok(None);
    }
    tree(&home.directory, name, None, &mut Budget::default(), 0).map(Some)
}
fn tree(
    parent: &Directory,
    name: &str,
    target: Option<(&Directory, &str)>,
    budget: &mut Budget,
    depth: usize,
) -> Result<String> {
    // A native no-follow open decides the actual type; links and special entries
    // never reach a byte read or a destination write.
    let kind = Some(parent.child_kind(Path::new(name)).map_err(error)?);
    tree_kind(parent, name, kind, target, budget, depth)
}
#[allow(clippy::too_many_lines)] // Keep the shared tree budget, no-follow walk and fingerprint publication together.
fn tree_kind(
    parent: &Directory,
    name: &str,
    kind: Option<EntryKind>,
    target: Option<(&Directory, &str)>,
    budget: &mut Budget,
    depth: usize,
) -> Result<String> {
    budget.entries += 1;
    if budget.entries > MAX_ENTRIES || depth > 64 {
        return Err(error("import tree exceeds entry or depth bound"));
    }
    let mut digest = Sha256::new();
    match kind {
        Some(EntryKind::File) => {
            digest.update(b"file");
            let mut source =
                fs::open_relative_file_no_follow(parent, Path::new(name)).map_err(error)?;
            let metadata = source.metadata().map_err(error)?;
            if !metadata.is_file() || metadata.len() > rsi_packages::MAX_FILE_BYTES {
                return Err(error("import file exceeds bound"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                digest.update((metadata.permissions().mode() & 0o100).to_le_bytes());
            }
            let mut output = target
                .map(|(directory, name)| {
                    fs::create_relative_private_file_no_follow(directory, Path::new(name))
                })
                .transpose()
                .map_err(error)?;
            let mut size = 0;
            let mut buffer = vec![0; 64 * 1024].into_boxed_slice();
            loop {
                let count = source.read(&mut buffer).map_err(error)?;
                if count == 0 {
                    break;
                }
                size += count as u64;
                budget.bytes += count as u64;
                if size > rsi_packages::MAX_FILE_BYTES || budget.bytes > MAX_BYTES {
                    return Err(error("import byte budget exceeded"));
                }
                digest.update(&buffer[..count]);
                if let Some(output) = output.as_mut() {
                    output.write_all(&buffer[..count]).map_err(error)?;
                }
            }
            if size != metadata.len() {
                return Err(error("import source changed length"));
            }
            if let Some(output) = output {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    output
                        .set_permissions(std::fs::Permissions::from_mode(
                            0o600 | (metadata.permissions().mode() & 0o100),
                        ))
                        .map_err(error)?;
                }
                output.sync_all().map_err(error)?;
            }
        }
        Some(EntryKind::Directory) => {
            digest.update(b"directory");
            let source =
                fs::open_relative_directory_no_follow(parent, Path::new(name)).map_err(error)?;
            let output = target
                .map(|(directory, name)| {
                    fs::create_directory_no_follow(directory, std::ffi::OsStr::new(name))
                })
                .transpose()
                .map_err(error)?;
            let mut children = Vec::new();
            for child in source.entries().map_err(error)? {
                if children.len() + budget.entries >= MAX_ENTRIES {
                    return Err(error("import entry budget exceeded"));
                }
                let child = child.map_err(error)?;
                let kind = child.file_type().map_err(error)?;
                if !matches!(kind, EntryKind::File | EntryKind::Directory) {
                    return Err(error("import refuses links and special files"));
                }
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| error("import names must be UTF-8"))?;
                children.push((name, kind));
            }
            children.sort_by(|left, right| left.0.cmp(&right.0));
            for (child, kind) in children {
                digest.update((child.len() as u64).to_le_bytes());
                digest.update(child.as_bytes());
                digest.update(
                    tree_kind(
                        &source,
                        &child,
                        Some(kind),
                        output.as_ref().map(|dir| (dir, child.as_str())),
                        budget,
                        depth + 1,
                    )?
                    .as_bytes(),
                );
            }
            if let Some(output) = output {
                output.sync_all().map_err(error)?;
            }
        }
        _ => {
            return Err(error(
                "import refuses missing entries, links and special files",
            ));
        }
    }
    if depth == 0
        && let Some((parent, _)) = target
    {
        parent.sync_all().map_err(error)?;
    }
    Ok(hex::encode(digest.finalize()))
}
fn hexadecimal(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(error)
}
fn overlaps(first: &Path, second: &Path) -> Result<bool> {
    let normalize = |path: &Path| -> Result<PathBuf> {
        let path = crate::normalize_launch_path(path, Path::new("."))?;
        let path = fs::resolve_absolute_root_alias(&path, true).map_err(error)?;
        #[cfg(windows)]
        {
            Ok(PathBuf::from(path.to_string_lossy().to_lowercase()))
        }
        #[cfg(not(windows))]
        {
            Ok(path)
        }
    };
    let first = normalize(first)?;
    let second = normalize(second)?;
    Ok(first.starts_with(&second) || second.starts_with(&first))
}
fn error(value: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("import: {value}"))
}

#[cfg(test)]
mod tests;
