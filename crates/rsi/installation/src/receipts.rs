//! Reset/import archive formats shared with their producers; filenames confer no authority.
use crate::{Result, error, filesystem as fs, record::hex};
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

pub(crate) const EXTRA_STATE: &[&str] = &[
    "desktop-webview",
    "trust",
    "session-host/artifacts",
    "session-host/owner.log",
    "session-host/owner.json",
    "session-host/runtime",
];
pub(crate) const CACHE: &[&str] = &[
    "history",
    "spill",
    "workspace-review",
    "native-applications",
    "native-addons",
    "agent-presets/system",
];

pub const STATE: &[&str] = &[
    "agent",
    "acp",
    "automation",
    "base.sqlite3",
    "base.sqlite3-wal",
    "base.sqlite3-shm",
];
pub const RESET_CONFIG: &[&str] = &[
    "rsi.toml",
    "settings.json",
    "application-profiles",
    "host-profiles",
];
pub const IMPORT_CONFIG: &[&str] = &[
    "rsi.toml",
    "settings.json",
    "application-profiles",
    "host-profiles",
    "agent-presets",
    "AGENTS.md",
    "agents",
    "skills",
];
pub const RESET_DATA: &[&str] = &["media"];
pub const IMPORT_DATA: &[&str] = &["credentials", "media"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeKind {
    Config,
    Data,
    State,
}
impl HomeKind {
    pub fn path(self, paths: &HostPaths) -> &std::path::Path {
        match self {
            Self::Config => paths.config(),
            Self::Data => paths.data(),
            Self::State => paths.state(),
        }
    }
    pub fn reset_entries(self) -> &'static [&'static str] {
        match self {
            Self::Config => RESET_CONFIG,
            Self::Data => RESET_DATA,
            Self::State => STATE,
        }
    }
    pub fn import_entries(self) -> &'static [&'static str] {
        match self {
            Self::Config => IMPORT_CONFIG,
            Self::Data => IMPORT_DATA,
            Self::State => STATE,
        }
    }
    pub fn selected(self, selection: ResetSelection) -> bool {
        match self {
            Self::Config => selection.config,
            Self::Data | Self::State => selection.state,
        }
    }
}

/// Rejects layouts where a managed entry contains another product home.
/// Independent children under a common parent are valid; disposable subtrees cannot overlap authored state.
pub fn validate_home_layout(paths: &HostPaths) -> Result<()> {
    let roots = [
        paths.config(),
        paths.data(),
        paths.state(),
        paths.cache(),
        paths.tmp(),
    ]
    .map(|path| rsi_files_native_fs::resolve_absolute_root_alias(path, true));
    let [config, data, state, cache, tmp] = roots;
    let roots = [config?, data?, state?, cache?, tmp?];
    #[cfg(windows)]
    let roots = roots.map(|path| PathBuf::from(path.to_string_lossy().to_lowercase()));
    for (root, names) in [
        (&roots[0], IMPORT_CONFIG),
        (&roots[1], &["credentials", "media", "packages"][..]),
        (&roots[2], STATE),
        (&roots[2], EXTRA_STATE),
        (&roots[3], CACHE),
    ] {
        for name in names.iter().copied().chain(std::iter::once("session-host")) {
            let target = root.join(name);
            #[cfg(windows)]
            let target = PathBuf::from(target.to_string_lossy().to_lowercase());
            if roots.iter().any(|other| other.starts_with(&target)) {
                return Err(error(format!(
                    "managed entry contains a product home: {}",
                    target.display()
                )));
            }
        }
    }
    for (root, names) in [
        (&roots[1], &["credentials", "media", "packages"][..]),
        (&roots[2], STATE),
        (&roots[2], EXTRA_STATE),
        (&roots[3], CACHE),
    ] {
        for name in names {
            let disposable = root.join(name);
            for authored in IMPORT_CONFIG
                .iter()
                .copied()
                .chain(std::iter::once("native-addons"))
            {
                let protected = roots[0].join(authored);
                #[cfg(windows)]
                let protected = PathBuf::from(protected.to_string_lossy().to_lowercase());
                if disposable.starts_with(&protected) || protected.starts_with(&disposable) {
                    return Err(error(format!(
                        "disposable entry overlaps authored configuration: {}",
                        disposable.display()
                    )));
                }
            }
        }
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Independent reset selections; neither includes credentials or packages.
pub struct ResetSelection {
    /// Archive managed launch/settings/Profile configuration.
    pub config: bool,
    /// Archive managed runtime data.
    pub state: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResetJournal {
    pub format: u8,
    pub id: String,
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub selection: ResetSelection,
    pub entries: Vec<ResetEntry>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResetEntry {
    pub home: HomeKind,
    pub name: String,
    pub directory: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug)]
pub struct ImportJournal {
    pub format: u32,
    pub id: String,
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub entries: Vec<ImportEntry>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug)]
pub struct ImportEntry {
    pub home: HomeKind,
    pub name: String,
    pub old: Option<String>,
    pub new: String,
}

#[allow(clippy::too_many_lines)]
// Both archive formats are checked against one complete home authority before authorizing deletion.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Receipt filenames are exact durable protocol identities, including case.
pub fn backups(
    paths: &HostPaths,
) -> Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>, BTreeSet<PathBuf>)> {
    let mut remove = BTreeSet::new();
    let mut known = BTreeSet::new();
    let mut receipts = BTreeSet::new();
    for root in [paths.config(), paths.data(), paths.state()] {
        let directory = match rsi_files_native_fs::open_absolute_directory_no_follow(
            &root.join("session-host"),
        ) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for (count, entry) in directory.entries()?.enumerate() {
            if count >= 8192 {
                return Err(error("receipt enumeration exceeds bound"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let reset = name.starts_with("reset-");
            let completion = reset && name.ends_with(".complete");
            if !(reset || name.starts_with("import-")) || !(name.ends_with(".json") || completion) {
                continue;
            }
            let bytes = fs::read(&directory, name, fs::DOCUMENT_LIMIT)?
                .ok_or_else(|| error("receipt disappeared"))?;
            let (id, config, data, state, entries) = if reset {
                let j: ResetJournal = serde_json::from_slice(&bytes).map_err(error)?;
                if j.format != 2 || !(j.selection.config || j.selection.state) {
                    return Err(error("invalid reset receipt"));
                }
                for e in &j.entries {
                    if !e.home.selected(j.selection)
                        || !e.home.reset_entries().contains(&e.name.as_str())
                    {
                        return Err(error("invalid reset selection"));
                    }
                }
                (
                    j.id,
                    j.config,
                    j.data,
                    j.state,
                    j.entries
                        .into_iter()
                        .map(|e| (e.home, e.name, true))
                        .collect::<Vec<_>>(),
                )
            } else {
                let j: ImportJournal = serde_json::from_slice(&bytes).map_err(error)?;
                if j.format != 2
                    || j.entries
                        .iter()
                        .any(|e| !hex(&e.new, 64) || e.old.as_ref().is_some_and(|v| !hex(v, 64)))
                {
                    return Err(error("invalid import receipt"));
                }
                (
                    j.id,
                    j.config,
                    j.data,
                    j.state,
                    j.entries
                        .into_iter()
                        .map(|e| (e.home, e.name, e.old.is_some()))
                        .collect(),
                )
            };
            if !hex(&id, 32)
                || name
                    != format!(
                        "{}-{id}.{}",
                        if reset { "reset" } else { "import" },
                        if completion { "complete" } else { "json" }
                    )
                || std::fs::canonicalize(&config)? != std::fs::canonicalize(paths.config())?
                || std::fs::canonicalize(&data)? != std::fs::canonicalize(paths.data())?
                || std::fs::canonicalize(&state)? != std::fs::canonicalize(paths.state())?
                || entries.len() > 16
            {
                return Err(error("receipt does not describe selected homes"));
            }
            let mut seen = BTreeSet::new();
            let mut preserve_receipt = false;
            for (home, entry, has_backup) in entries {
                if !seen.insert((home, entry.clone()))
                    || !(if reset {
                        home.reset_entries()
                    } else {
                        home.import_entries()
                    })
                    .contains(&entry.as_str())
                {
                    return Err(error("unauthorized backup entry"));
                }
                if !has_backup {
                    continue;
                }
                let backup = home.path(paths).join(if reset {
                    format!(".reset-{id}-{entry}")
                } else {
                    format!(".import-{id}-backup-{entry}")
                });
                known.insert(backup.clone());
                if home == HomeKind::Config {
                    preserve_receipt = true;
                } else {
                    remove.insert(backup);
                }
            }
            if !preserve_receipt {
                receipts.insert(root.join("session-host").join(name));
            }
        }
    }
    // Unknown backup-looking files may contain secrets; never report a complete purge.
    for root in [paths.config(), paths.data(), paths.state()] {
        let directory = match rsi_files_native_fs::open_absolute_directory_no_follow(root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for (count, entry) in directory.entries()?.enumerate() {
            if count >= 16384 {
                return Err(error("home enumeration exceeds bound"));
            }
            let name = entry?.file_name();
            let text = name.to_string_lossy();
            let path = root.join(&name);
            if (text.starts_with(".reset-") || text.starts_with(".import-"))
                && !known.contains(&path)
            {
                return Err(error(format!(
                    "unverified backup retained: {}",
                    path.display()
                )));
            }
        }
    }
    Ok((remove, known, receipts))
}
