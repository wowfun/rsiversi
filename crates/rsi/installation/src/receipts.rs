//! Reset/import archive formats shared with their producers; filenames confer no authority.
use crate::{Result, error, filesystem as fs, record::hex};
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

pub const STATE: &[&str] = &[
    "agent",
    "media",
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
    "credentials",
    "AGENTS.md",
    "agents",
    "skills",
];
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
    pub state: PathBuf,
    pub selection: ResetSelection,
    pub entries: Vec<ResetEntry>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResetEntry {
    pub config: bool,
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
    pub state: PathBuf,
    pub entries: Vec<ImportEntry>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug)]
pub struct ImportEntry {
    pub config: bool,
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
    for root in [paths.config(), paths.state()] {
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
            if !(reset || name.starts_with("import-")) || !name.ends_with(".json") {
                continue;
            }
            let bytes = fs::read(&directory, name, fs::DOCUMENT_LIMIT)?
                .ok_or_else(|| error("receipt disappeared"))?;
            let (id, config, state, entries) = if reset {
                let j: ResetJournal = serde_json::from_slice(&bytes).map_err(error)?;
                if j.format != 1 || !(j.selection.config || j.selection.state) {
                    return Err(error("invalid reset receipt"));
                }
                for e in &j.entries {
                    if (e.config
                        && (!j.selection.config || !RESET_CONFIG.contains(&e.name.as_str())))
                        || (!e.config && !j.selection.state)
                    {
                        return Err(error("invalid reset selection"));
                    }
                }
                (
                    j.id,
                    j.config,
                    j.state,
                    j.entries
                        .into_iter()
                        .map(|e| (e.config, e.name, true))
                        .collect::<Vec<_>>(),
                )
            } else {
                let j: ImportJournal = serde_json::from_slice(&bytes).map_err(error)?;
                if j.format != 1
                    || j.entries
                        .iter()
                        .any(|e| !hex(&e.new, 64) || e.old.as_ref().is_some_and(|v| !hex(v, 64)))
                {
                    return Err(error("invalid import receipt"));
                }
                (
                    j.id,
                    j.config,
                    j.state,
                    j.entries
                        .into_iter()
                        .map(|e| (e.config, e.name, e.old.is_some()))
                        .collect(),
                )
            };
            if !hex(&id, 32)
                || name != format!("{}-{id}.json", if reset { "reset" } else { "import" })
                || std::fs::canonicalize(&config)? != std::fs::canonicalize(paths.config())?
                || std::fs::canonicalize(&state)? != std::fs::canonicalize(paths.state())?
                || entries.len() > 16
            {
                return Err(error("receipt does not describe selected homes"));
            }
            let mut seen = BTreeSet::new();
            let mut preserve_receipt = false;
            let config_entries = if reset { RESET_CONFIG } else { IMPORT_CONFIG };
            for (is_config, entry, has_backup) in entries {
                if !seen.insert((is_config, entry.clone()))
                    || !(if is_config { config_entries } else { STATE }).contains(&entry.as_str())
                {
                    return Err(error("unauthorized backup entry"));
                }
                if !has_backup {
                    continue;
                }
                let backup = (if is_config {
                    paths.config()
                } else {
                    paths.state()
                })
                .join(if reset {
                    format!(".reset-{id}-{entry}")
                } else {
                    format!(".import-{id}-backup-{entry}")
                });
                known.insert(backup.clone());
                if !is_config || entry == "credentials" {
                    remove.insert(backup);
                } else {
                    preserve_receipt = true;
                }
            }
            if !preserve_receipt {
                receipts.insert(root.join("session-host").join(name));
            }
        }
    }
    // Unknown backup-looking files may contain secrets; never report a complete purge.
    for root in [paths.config(), paths.state()] {
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
