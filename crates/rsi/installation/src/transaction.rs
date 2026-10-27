//! Recoverable fixed-role promotion across the bin and private control directories.
use crate::{
    Result, error, filesystem as fs,
    record::{Layout, PENDING, Phase, Role, hex},
};
use rsi_files_native_fs as native;
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Participant {
    role: Role,
    old: String,
    new: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format: u8,
    id: String,
    installation: String,
    config: PathBuf,
    state: PathBuf,
    was_running: bool,
    participants: Vec<Participant>,
}
#[derive(Debug)]
pub struct Transaction {
    layout: Layout,
    bin: fs::Lease,
    control: fs::Lease,
    journal: Journal,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Topology {
    Unstaged,
    Prepared,
    Archived,
    Installed,
    Restoring,
    Restored,
}
impl Transaction {
    fn directory(&self, role: Role) -> &native::Directory {
        if role == Role::Core {
            &self.bin.directory
        } else {
            &self.control.directory
        }
    }
    fn name(&self, role: Role, phase: &str) -> String {
        format!(".rsi-{phase}-{}-{}", self.journal.id, role.name())
    }
    #[allow(clippy::too_many_lines)] // Persist the complete fixed-role intent before any staged participant exists.
    pub fn prepare(
        layout: Layout,
        inputs: [&Path; 3],
        admitted: [&str; 3],
        paths: &HostPaths,
        was_running: bool,
    ) -> Result<Self> {
        let bin = fs::Lease::acquire(&layout.bin, false, true)?;
        let control = fs::Lease::acquire(&layout.control, false, true)?;
        layout.reject_pending()?;
        let mut record = layout
            .record()?
            .ok_or_else(|| error("managed upgrade requires an installation record"))?;
        if record.phase != Phase::Installed {
            return Err(error("finish pending installation/removal first"));
        }
        let journal = Journal {
            format: 2,
            id: fs::id()?,
            installation: record.id.clone(),
            config: std::fs::canonicalize(paths.config())?,
            state: std::fs::canonicalize(paths.state())?,
            was_running,
            participants: vec![],
        };
        let mut transaction = Self {
            layout,
            bin,
            control,
            journal,
        };
        for ((role, input), admitted) in [Role::Core, Role::Uninstaller, Role::Launcher]
            .into_iter()
            .zip(inputs)
            .zip(admitted)
        {
            let old = fs::digest(transaction.directory(role), role.name())?
                .ok_or_else(|| error("missing installed participant"))?;
            let expected = match role {
                Role::Core => &record.core,
                Role::Uninstaller => &record.uninstaller,
                _ => &record.launcher,
            };
            if &old != expected {
                return Err(error(format!(
                    "installed {} differs from its ownership receipt",
                    role.name()
                )));
            }
            let new = fs::digest_file(fs::source(input)?)?;
            if new != admitted {
                return Err(error(format!(
                    "{} differs from its admitted package digest",
                    role.name()
                )));
            }
            match role {
                Role::Core => record.core.clone_from(&new),
                Role::Uninstaller => record.uninstaller.clone_from(&new),
                _ => record.launcher.clone_from(&new),
            }
            transaction
                .journal
                .participants
                .push(Participant { role, old, new });
        }
        let record_bytes = serde_json::to_vec(&record).map_err(error)?;
        transaction.journal.participants.push(Participant {
            role: Role::Record,
            old: fs::digest(&transaction.control.directory, Role::Record.name())?
                .ok_or_else(|| error("missing record"))?,
            new: hex::encode(sha2::Sha256::digest(&record_bytes)),
        });
        // Persist the entire fixed-role intent before any candidate can survive a crash.
        fs::write(
            &transaction.bin.directory,
            PENDING,
            &serde_json::to_vec(&transaction.journal).map_err(error)?,
            false,
        )?;
        let result = (|| -> Result<()> {
            for ((role, input), expected) in [Role::Core, Role::Uninstaller, Role::Launcher]
                .into_iter()
                .zip(inputs)
                .zip(admitted)
            {
                let copied = fs::copy(
                    transaction.directory(role),
                    &transaction.name(role, "candidate"),
                    input,
                    role != Role::Launcher || cfg!(unix),
                )?;
                if copied != expected {
                    return Err(error("candidate source changed after journal admission"));
                }
            }
            fs::write(
                &transaction.control.directory,
                &transaction.name(Role::Record, "candidate"),
                &record_bytes,
                false,
            )
        })();
        if let Err(failure) = result {
            transaction
                .rollback()
                .map_err(|e| error(format!("{failure}; recovery required: {e}")))?;
            transaction
                .finish(false)
                .map_err(|e| error(format!("{failure}; recovery required: {e}")))?;
            return Err(failure);
        }
        Ok(transaction)
    }
    pub fn recover(layout: Layout, paths: &HostPaths) -> Result<Self> {
        let bin = fs::Lease::acquire(&layout.bin, false, true)?;
        let control = fs::Lease::acquire(&layout.control, false, true)?;
        let bytes = fs::read(&bin.directory, PENDING, fs::DOCUMENT_LIMIT)?
            .ok_or_else(|| error("no managed upgrade pending"))?;
        let journal: Journal = serde_json::from_slice(&bytes).map_err(error)?;
        validate(&journal)?;
        if journal.config != std::fs::canonicalize(paths.config())?
            || journal.state != std::fs::canonicalize(paths.state())?
        {
            return Err(error("recovery requires original config/state homes"));
        }
        let transaction = Self {
            layout,
            bin,
            control,
            journal,
        };
        transaction.topologies()?;
        Ok(transaction)
    }
    fn topology(&self, p: &Participant) -> Result<Topology> {
        let dir = self.directory(p.role);
        let current = fs::digest(dir, p.role.name())?;
        let old = fs::digest(dir, &self.name(p.role, "previous"))?;
        let new = fs::digest(dir, &self.name(p.role, "candidate"))?;
        let failed = fs::digest(dir, &self.name(p.role, "failed"))?;
        let a = Some(p.old.as_str());
        let b = Some(p.new.as_str());
        match (
            current.as_deref(),
            old.as_deref(),
            new.as_deref(),
            failed.as_deref(),
        ) {
            (value, None, None, None) if value == a => Ok(Topology::Unstaged),
            (value, None, stage, None) if value == a && stage == b => Ok(Topology::Prepared),
            (None, backup, stage, None) if backup == a && stage == b => Ok(Topology::Archived),
            (value, backup, None, None) if value == b && backup == a => Ok(Topology::Installed),
            (None, backup, None, failure) if backup == a && failure == b => Ok(Topology::Restoring),
            (value, None, None, failure) if value == a && failure == b => Ok(Topology::Restored),
            _ => Err(error(format!(
                "ambiguous upgrade participant {}; retain all files",
                p.role.name()
            ))),
        }
    }
    fn topologies(&self) -> Result<Vec<Topology>> {
        self.journal
            .participants
            .iter()
            .map(|p| self.topology(p))
            .collect()
    }
    pub fn candidate(&self) -> PathBuf {
        self.layout.bin.join(self.name(Role::Core, "candidate"))
    }
    pub fn candidate_uninstaller(&self) -> PathBuf {
        self.layout
            .control
            .join(self.name(Role::Uninstaller, "candidate"))
    }
    pub fn executable(&self) -> PathBuf {
        self.layout.target(Role::Core)
    }
    pub fn was_running(&self) -> bool {
        self.journal.was_running
    }
    #[allow(clippy::missing_panics_doc)] // Only validated constructors can create this fixed-role journal.
    pub fn candidate_digest(&self) -> &str {
        &self
            .journal
            .participants
            .iter()
            .find(|p| p.role == Role::Core)
            .expect("validated core participant")
            .new
    }
    pub fn promote(&self) -> Result<()> {
        self.promote_with(|| Ok(()))
    }
    pub(crate) fn promote_with(&self, mut checkpoint: impl FnMut() -> Result<()>) -> Result<()> {
        let states = self.topologies()?;
        if states.iter().any(|state| {
            matches!(
                state,
                Topology::Unstaged | Topology::Restoring | Topology::Restored
            )
        }) {
            return Err(error(
                "incomplete staging or restoring transaction cannot be promoted",
            ));
        }
        for (p, state) in self.journal.participants.iter().zip(states) {
            let dir = self.directory(p.role);
            if state == Topology::Prepared {
                fs::rename(dir, p.role.name(), &self.name(p.role, "previous"), false)?;
                checkpoint()?;
            }
            if state != Topology::Installed {
                fs::rename(dir, &self.name(p.role, "candidate"), p.role.name(), false)?;
                checkpoint()?;
            }
        }
        Ok(())
    }
    pub fn rollback(&self) -> Result<()> {
        let states = self.topologies()?;
        for (p, state) in self.journal.participants.iter().zip(states).rev() {
            let dir = self.directory(p.role);
            if state == Topology::Installed {
                fs::rename(dir, p.role.name(), &self.name(p.role, "failed"), false)?;
            }
            if matches!(
                state,
                Topology::Archived | Topology::Installed | Topology::Restoring
            ) {
                fs::rename(dir, &self.name(p.role, "previous"), p.role.name(), false)?;
            }
        }
        Ok(())
    }
    pub fn finish(self, committed: bool) -> Result<PathBuf> {
        self.topologies()?;
        for p in &self.journal.participants {
            if fs::digest(self.directory(p.role), p.role.name())?.as_ref()
                != Some(if committed { &p.new } else { &p.old })
            {
                return Err(error("cannot settle unverified participant"));
            }
        }
        let name = format!(
            ".rsi-core-upgrade-{}.{}.json",
            self.journal.id,
            if committed { "committed" } else { "recovered" }
        );
        fs::rename(&self.bin.directory, PENDING, &name, false)?;
        if let Err(e) = collect(&self.layout, &self.bin.directory, &self.control.directory) {
            eprintln!("Upgrade settled; completed artifacts retained for inspection: {e}");
        }
        Ok(self.layout.bin.join(name))
    }
}
fn validate(journal: &Journal) -> Result<()> {
    if journal.format != 2
        || !hex(&journal.id, 32)
        || !hex(&journal.installation, 32)
        || journal.participants.len() != 4
        || !journal.config.is_absolute()
        || !journal.state.is_absolute()
    {
        return Err(error("invalid v2 upgrade journal"));
    }
    for (p, role) in journal.participants.iter().zip(Role::ALL) {
        if p.role != role || !hex(&p.old, 64) || !hex(&p.new, 64) {
            return Err(error("invalid upgrade role or digest"));
        }
    }
    Ok(())
}
fn collection_plan(
    layout: &Layout,
    bin: &native::Directory,
    control: Option<&native::Directory>,
) -> Result<Vec<(Role, String, bool)>> {
    let mut plan = Vec::new();
    layout.reject_pending()?;
    for (count, entry) in bin.entries()?.enumerate() {
        if count >= 8192 {
            return Err(error("upgrade receipt enumeration exceeds 8192 entries"));
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(".rsi-core-upgrade-")
            || !(name.ends_with(".committed.json") || name.ends_with(".recovered.json"))
        {
            continue;
        }
        let bytes =
            fs::read(bin, name, fs::DOCUMENT_LIMIT)?.ok_or_else(|| error("receipt disappeared"))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(error)?;
        if value["format"] == 1 {
            plan.extend(
                crate::legacy::receipt_files(bin, name, &bytes)?
                    .into_iter()
                    .map(|name| (Role::Core, name, false)),
            );
            plan.push((Role::Core, name.to_owned(), true));
            continue;
        }
        let journal: Journal = serde_json::from_slice(&bytes).map_err(error)?;
        validate(&journal)?;
        if name != format!(".rsi-core-upgrade-{}.committed.json", journal.id)
            && name != format!(".rsi-core-upgrade-{}.recovered.json", journal.id)
        {
            return Err(error("upgrade receipt identity mismatch"));
        }
        for p in &journal.participants {
            let Some(dir) = (if p.role == Role::Core {
                Some(bin)
            } else {
                control
            }) else {
                continue;
            };
            for (phase, digest) in [
                ("previous", &p.old),
                ("candidate", &p.new),
                ("failed", &p.new),
            ] {
                let file = format!(".rsi-{phase}-{}-{}", journal.id, p.role.name());
                if let Some(actual) = fs::digest(dir, &file)? {
                    if actual != *digest {
                        return Err(error("upgrade artifact digest changed; retained"));
                    }
                    plan.push((p.role, file, false));
                }
            }
        }
        plan.push((Role::Core, name.to_owned(), true));
    }
    Ok(plan)
}

pub fn collect(
    layout: &Layout,
    bin: &native::Directory,
    control: &native::Directory,
) -> Result<()> {
    for (role, name, receipt) in collection_plan(layout, bin, Some(control))? {
        if !receipt {
            (if role == Role::Core { bin } else { control }).remove_child(Path::new(&name))?;
        }
    }
    Ok(())
}
pub(crate) fn preview_collection(layout: &Layout) -> Result<Vec<PathBuf>> {
    let open = |path: &Path| match native::open_absolute_directory_no_follow(path) {
        Ok(dir) => Ok(Some(dir)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    let Some(bin) = open(&layout.bin)? else {
        return Ok(vec![]);
    };
    let control = open(&layout.control)?;
    collection_plan(layout, &bin, control.as_ref()).map(|plan| {
        plan.into_iter()
            .map(|(role, name, _)| layout.directory(role).join(name))
            .collect()
    })
}
