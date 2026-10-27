//! Exclusive product maintenance with same-volume backups and recoverable intent.

use crate::{LaunchConfiguration, Result, RsiError};
use rsi_files_native_fs::{Directory, EntryKind};
use rsi_host::HostPaths;
use rsi_service_host::{HostOwnerLease, HostStartupLease, ServiceHostPaths};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

const PENDING: &str = "reset.pending.json";
const MAX_JOURNAL: usize = 64 * 1024;
pub use rsi_installation::receipts::ResetSelection;
use rsi_installation::receipts::{
    RESET_CONFIG as CONFIG_ENTRIES, ResetEntry as Entry, ResetJournal as Journal,
    STATE as STATE_ENTRIES,
};

/// Durable completed reset evidence; backup paths contain the original bytes.
#[derive(Clone, Debug, Serialize)]
pub struct ResetReceipt {
    /// Transaction identity retained in both authority directories.
    pub transaction: String,
    /// Operations completed, including when resuming after interruption.
    pub selection: ResetSelection,
    /// Existing data archived without interpreting its contents.
    pub backups: Vec<PathBuf>,
}

pub(super) struct Home {
    pub(super) path: PathBuf,
    pub(super) directory: Directory,
    #[cfg(windows)]
    namespace: Option<rsi_files_native_fs::NamespaceLease>,
}
impl Home {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let directory =
            rsi_files_native_fs::create_absolute_directory_no_follow(path).map_err(error)?;
        #[cfg(windows)]
        let directory = {
            drop(directory);
            rsi_files_native_fs::set_private_directory_permissions_no_follow(path).map_err(error)?
        };
        #[cfg(windows)]
        let namespace = directory.pin_namespace().map_err(error)?;
        Ok(Self {
            path: path.to_owned(),
            directory,
            #[cfg(windows)]
            namespace: Some(namespace),
        })
    }

    pub(super) fn observe(path: &Path) -> Result<Self> {
        let directory =
            rsi_files_native_fs::open_absolute_directory_no_follow(path).map_err(error)?;
        Ok(Self {
            path: path.to_owned(),
            directory,
            #[cfg(windows)]
            namespace: None,
        })
    }
    pub(super) fn kind(&self, name: &str) -> Result<Option<EntryKind>> {
        match self.directory.child_kind(Path::new(name)) {
            Ok(kind) => Ok(Some(kind)),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(failure) => Err(error(failure)),
        }
    }

    pub(super) fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.kind(name)? {
            None => return Ok(None),
            Some(EntryKind::File) => {}
            _ => return Err(error("maintenance journal is not a regular file")),
        }
        let mut file =
            rsi_files_native_fs::open_relative_file_no_follow(&self.directory, Path::new(name))
                .map_err(error)?;
        if !file.metadata().map_err(error)?.is_file() {
            return Err(error("maintenance journal changed type"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_JOURNAL as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > MAX_JOURNAL {
            return Err(error("maintenance journal exceeds 64 KiB"));
        }
        Ok(Some(bytes))
    }

    pub(super) fn create(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(error)?;
        let staging = format!(".{name}.{}.tmp", hex::encode(random));
        let mut file = rsi_files_native_fs::create_relative_private_file_no_follow(
            &self.directory,
            Path::new(&staging),
        )
        .map_err(error)?;
        file.write_all(bytes).map_err(error)?;
        file.sync_all().map_err(error)?;
        drop(file);
        self.rename(&staging, name, EntryKind::File)
    }

    pub(super) fn rename(&self, source: &str, destination: &str, kind: EntryKind) -> Result<()> {
        #[cfg(windows)]
        {
            // User-edited launch files can inherit an ACL. Admit their existing
            // TokenUser ownership before giving the archive the private policy.
            match kind {
                EntryKind::File => {
                    rsi_files_native_fs::set_relative_private_file_permissions_no_follow(
                        &self.directory,
                        Path::new(source),
                    )
                    .map_err(error)?;
                }
                EntryKind::Directory => {
                    rsi_files_native_fs::set_relative_private_directory_permissions_no_follow(
                        &self.directory,
                        Path::new(source),
                    )
                    .map_err(error)?;
                }
                _ => return Err(error("cannot archive a link or special object")),
            }
            self.namespace
                .as_ref()
                .ok_or_else(|| error("read-only home cannot publish"))?
                .publish_child(Path::new(source), Path::new(destination), kind, false)
                .map_err(|failure| error(format!("archive {source}: {failure}")))?;
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let _ = kind;
            rustix::fs::renameat_with(
                self.directory.as_fd(),
                source,
                self.directory.as_fd(),
                destination,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(error)?;
        }
        self.directory.sync_all().map_err(error)
    }
}

/// Performs an explicit reset or recovers an interrupted one before product configuration is read.
///
/// Passing no selection performs no I/O mutation when no pending journal exists.
/// Any live Host or startup coordinator prevents maintenance; callers must stop it explicitly.
pub fn maintain_product_homes(
    paths: &HostPaths,
    selected: Option<ResetSelection>,
) -> Result<Vec<ResetReceipt>> {
    crate::reject_pending_import(paths)?;
    let pending = [paths.config(), paths.state()].into_iter().any(|home| {
        !matches!(std::fs::symlink_metadata(home.join("session-host").join(PENDING)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    });
    let selected = selected.filter(|selection| selection.config || selection.state);
    if !pending && selected.is_none() {
        return Ok(Vec::new());
    }
    let host_paths = ServiceHostPaths::from_host_paths(paths).map_err(error)?;
    let _startup = HostStartupLease::try_acquire(&host_paths).map_err(error)?;
    let _owner = HostOwnerLease::try_acquire(host_paths).map_err(error)?;
    crate::reject_pending_import(paths)?;
    let config = Home::open(paths.config())?;
    let state = Home::open(paths.state())?;
    let mut authorities = vec![Home::open(&paths.config().join("session-host"))?];
    if std::fs::canonicalize(paths.config()).map_err(error)?
        != std::fs::canonicalize(paths.state()).map_err(error)?
    {
        authorities.push(Home::open(&paths.state().join("session-host"))?);
    }
    let mut existing = None;
    for home in &authorities {
        if let Some(bytes) = home.read(PENDING)? {
            if existing.as_ref().is_some_and(|previous| previous != &bytes) {
                return Err(error("home authorities contain different pending resets"));
            }
            existing = Some(bytes);
        }
    }
    let mut receipts = Vec::new();
    let mut recovered = None;
    if let Some(bytes) = existing {
        let journal: Journal = serde_json::from_slice(&bytes).map_err(error)?;
        validate(&journal, paths)?;
        recovered = Some(journal.selection);
        receipts.push(complete(
            &journal,
            &bytes,
            &config,
            &state,
            &authorities,
            |_| Ok(()),
        )?);
    }
    if let Some(selection) = selected.filter(|selection| Some(*selection) != recovered) {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(error)?;
        let mut journal = Journal {
            format: 1,
            id: hex::encode(random),
            config: paths.config().to_owned(),
            state: paths.state().to_owned(),
            selection,
            entries: Vec::new(),
        };
        for (is_config, names, home) in [
            (true, CONFIG_ENTRIES, &config),
            (false, STATE_ENTRIES, &state),
        ] {
            if !(if is_config {
                selection.config
            } else {
                selection.state
            }) {
                continue;
            }
            for name in names {
                match home.kind(name)? {
                    None => {}
                    Some(kind @ (EntryKind::File | EntryKind::Directory)) => {
                        journal.entries.push(Entry {
                            config: is_config,
                            name: (*name).into(),
                            directory: kind == EntryKind::Directory,
                        });
                    }
                    _ => return Err(error(format!("refusing reset of non-regular entry {name}"))),
                }
            }
        }
        let bytes = serde_json::to_vec(&journal).map_err(error)?;
        receipts.push(complete(
            &journal,
            &bytes,
            &config,
            &state,
            &authorities,
            |_| Ok(()),
        )?);
    }
    Ok(receipts)
}

fn validate(journal: &Journal, paths: &HostPaths) -> Result<()> {
    if journal.format != 1
        || journal.config != paths.config()
        || journal.state != paths.state()
        || journal.id.len() != 32
        || !journal
            .id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !(journal.selection.config || journal.selection.state)
        || journal.entries.len() > CONFIG_ENTRIES.len() + STATE_ENTRIES.len()
    {
        return Err(error("pending reset does not describe this home pair"));
    }
    let mut seen = BTreeSet::new();
    for entry in &journal.entries {
        let (enabled, names) = if entry.config {
            (journal.selection.config, CONFIG_ENTRIES)
        } else {
            (journal.selection.state, STATE_ENTRIES)
        };
        if !enabled
            || !names.contains(&entry.name.as_str())
            || !seen.insert((entry.config, &entry.name))
        {
            return Err(error(
                "pending reset contains an unauthorized or repeated entry",
            ));
        }
    }
    Ok(())
}

fn complete(
    journal: &Journal,
    bytes: &[u8],
    config: &Home,
    state: &Home,
    authorities: &[Home],
    progress: impl Fn(usize) -> Result<()>,
) -> Result<ResetReceipt> {
    let archive = format!("reset-{}.json", journal.id);
    let committed = format!("reset-{}.complete", journal.id);
    let mut completed = false;
    for home in authorities {
        if let Some(record) = home.read(&committed)? {
            if record != bytes {
                return Err(error("reset completion receipt differs from intent"));
            }
            completed = true;
        }
        if let Some(record) = home.read(&archive)? {
            if record != bytes {
                return Err(error("archived reset intent differs from pending intent"));
            }
        } else if home.read(PENDING)?.is_none() {
            home.create(PENDING, bytes)?;
        }
    }
    let mut backups = Vec::new();
    let mut renames = Vec::new();
    for entry in &journal.entries {
        let home = if entry.config { config } else { state };
        let backup = format!(".reset-{}-{}", journal.id, entry.name);
        let expected = if entry.directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        backups.push(home.path.join(&backup));
        let rename = if completed {
            false
        } else {
            match (home.kind(&entry.name)?, home.kind(&backup)?) {
                (Some(kind), None) if kind == expected => true,
                (None, Some(kind)) if kind == expected => false,
                _ => {
                    return Err(error(format!(
                        "ambiguous reset progress for {}; retained journal {}",
                        entry.name, journal.id
                    )));
                }
            }
        };
        renames.push((home, entry, backup, expected, rename));
    }
    for (index, (home, entry, backup, kind, rename)) in renames.into_iter().enumerate() {
        if rename {
            home.rename(&entry.name, &backup, kind)?;
        }
        if !completed {
            progress(index + 1)?;
        }
    }
    for home in authorities {
        if home.read(&committed)?.is_none() {
            home.create(&committed, bytes)?;
        }
    }
    if journal.selection.config && config.kind(crate::LAUNCH_FILE)?.is_none() {
        LaunchConfiguration::initialize(&config.path)?;
    }
    for home in authorities {
        if home.read(&archive)?.is_none() {
            home.rename(PENDING, &archive, EntryKind::File)?;
        }
    }
    Ok(ResetReceipt {
        transaction: journal.id.clone(),
        selection: journal.selection,
        backups,
    })
}

fn error(value: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("product maintenance: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_siblings_do_not_exhaust_exact_maintenance_lookups() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::open(root.path()).unwrap();
        for index in 0..8193 {
            std::fs::write(
                root.path().join(format!(".reset-{index}-settings.json")),
                b"{}",
            )
            .unwrap();
        }
        assert_eq!(home.kind("missing").unwrap(), None);
        home.create("rsi.toml", b"format = 1").unwrap();
        assert_eq!(home.kind("rsi.toml").unwrap(), Some(EntryKind::File));
    }

    fn paths(root: &Path) -> HostPaths {
        HostPaths::new(root.join("config"), root.join("state"), root.join("cache")).unwrap()
    }

    #[test]
    fn both_resets_preserve_credentials_packages_and_unmanaged_inputs() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        for (home, files) in [
            (
                paths.config(),
                vec![
                    "rsi.toml",
                    "settings.json",
                    "credentials/key",
                    "AGENTS.md",
                    "skills/one",
                ],
            ),
            (
                paths.state(),
                vec![
                    "agent/corrupt.sqlite",
                    "base.sqlite3",
                    "packages/lock",
                    "trust/highwater",
                ],
            ),
        ] {
            for name in files {
                let path = home.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, name).unwrap();
            }
        }
        let receipts = maintain_product_homes(
            &paths,
            Some(ResetSelection {
                config: true,
                state: true,
            }),
        )
        .unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].backups.len(), 4);
        assert_eq!(
            std::fs::read_to_string(paths.config().join("credentials/key")).unwrap(),
            "credentials/key"
        );
        assert_eq!(
            std::fs::read_to_string(paths.config().join("AGENTS.md")).unwrap(),
            "AGENTS.md"
        );
        assert!(paths.config().join("skills/one").exists());
        assert!(paths.state().join("packages/lock").exists());
        assert!(paths.state().join("trust/highwater").exists());
        assert!(!paths.state().join("agent").exists());
        assert!(!paths.state().join("base.sqlite3").exists());
        LaunchConfiguration::load(paths.config(), true).unwrap();
        assert!(maintain_product_homes(&paths, None).unwrap().is_empty());
    }

    #[test]
    fn reset_refuses_active_owner_before_mutating_configuration() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let _owner =
            HostOwnerLease::try_acquire(ServiceHostPaths::from_host_paths(&paths).unwrap())
                .unwrap();
        std::fs::write(paths.config().join("rsi.toml"), "broken").unwrap();
        assert!(
            maintain_product_homes(
                &paths,
                Some(ResetSelection {
                    config: true,
                    state: true
                })
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(paths.config().join("rsi.toml")).unwrap(),
            "broken"
        );
        assert!(!paths.state().join("session-host").join(PENDING).exists());
    }

    #[test]
    fn interrupted_reset_recovers_each_home_without_rearchiving() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let config = Home::open(paths.config()).unwrap();
        let state = Home::open(paths.state()).unwrap();
        config.create("settings.json", b"old-settings").unwrap();
        state.create("base.sqlite3", b"old-state").unwrap();
        let authorities = [
            Home::open(&paths.config().join("session-host")).unwrap(),
            Home::open(&paths.state().join("session-host")).unwrap(),
        ];
        let journal = Journal {
            format: 1,
            id: "a".repeat(32),
            config: paths.config().into(),
            state: paths.state().into(),
            selection: ResetSelection {
                config: true,
                state: true,
            },
            entries: vec![
                Entry {
                    config: true,
                    name: "settings.json".into(),
                    directory: false,
                },
                Entry {
                    config: false,
                    name: "base.sqlite3".into(),
                    directory: false,
                },
            ],
        };
        let bytes = serde_json::to_vec(&journal).unwrap();
        let collision = format!(".reset-{}-base.sqlite3", journal.id);
        state.create(&collision, b"unexpected").unwrap();
        assert!(complete(&journal, &bytes, &config, &state, &authorities, |_| Ok(())).is_err());
        assert!(
            config.path.join("settings.json").exists(),
            "all participants must be admitted before the first rename"
        );
        state.directory.remove_child(Path::new(&collision)).unwrap();
        assert!(
            complete(&journal, &bytes, &config, &state, &authorities, |_| Err(
                error("injected interruption")
            ))
            .is_err()
        );
        assert!(!config.path.join("settings.json").exists());
        assert!(state.path.join("base.sqlite3").exists());
        drop(authorities);
        drop(config);
        drop(state);
        let receipt = maintain_product_homes(&paths, Some(journal.selection)).unwrap();
        assert_eq!(receipt.len(), 1);
        assert_eq!(receipt[0].transaction, journal.id);
        assert_eq!(
            std::fs::read(&receipt[0].backups[0]).unwrap(),
            b"old-settings"
        );
        assert_eq!(std::fs::read(&receipt[0].backups[1]).unwrap(), b"old-state");
    }

    #[test]
    fn journals_cannot_select_external_or_preserved_entries() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let mut journal = Journal {
            format: 1,
            id: "b".repeat(32),
            config: paths.config().into(),
            state: paths.state().into(),
            selection: ResetSelection {
                config: true,
                state: true,
            },
            entries: Vec::new(),
        };
        for name in ["../outside", "credentials", "packages", "session-host"] {
            journal.entries = vec![Entry {
                config: true,
                name: name.into(),
                directory: true,
            }];
            assert!(validate(&journal, &paths).is_err());
        }
    }
}
