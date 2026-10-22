//! Recoverable executable promotion. Process stop/start stays with the launcher.
use crate::{Result, RsiError, maintenance::Home};
use rsi_files_native_fs::EntryKind;
use rsi_host::HostPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

const PENDING: &str = ".rsi-core-upgrade.pending.json";
const LOCK: &str = ".rsi-core-upgrade.lock";
const MAXIMUM_EXECUTABLE: u64 = rsi_packages::MAX_FILE_BYTES;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format: u32,
    id: String,
    executable: String,
    previous: String,
    candidate: String,
    failed: String,
    previous_digest: String,
    candidate_digest: String,
    config: PathBuf,
    state: PathBuf,
    was_running: bool,
}

/// Same-volume executable transaction under an exclusive native installation lease.
///
/// The caller separately holds the Host startup coordinator across process
/// shutdown, promotion, readiness and settlement. Dropping this value releases
/// the installation lease but keeps recovery intent and all previous bytes.
pub struct CoreUpgrade {
    home: Home,
    _lock: File,
    journal: Journal,
}
impl std::fmt::Debug for CoreUpgrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreUpgrade").finish_non_exhaustive()
    }
}
impl CoreUpgrade {
    /// Detects recovery intent without creating installation or product files.
    pub fn has_pending(installation: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(installation.join(PENDING)) {
            Ok(_) => Ok(true),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(failure) => Err(error(failure)),
        }
    }
    /// Stages a previously admitted core export before any process is stopped.
    /// The installed executable must use the ordinary rsi filename.
    pub fn prepare(
        executable: &Path,
        candidate: &Path,
        expected_digest: &str,
        paths: &HostPaths,
        was_running: bool,
    ) -> Result<Self> {
        let name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| matches!(*name, "rsi" | "rsi.exe"))
            .ok_or_else(|| error("upgrade requires the installed rsi executable"))?;
        let parent = executable
            .parent()
            .ok_or_else(|| error("missing installation directory"))?;
        let (home, lock) = open(parent)?;
        if home.kind(PENDING)?.is_some() {
            return Err(error(
                "an upgrade is pending; use rsi core recover with its original homes",
            ));
        }
        if !hex(expected_digest) {
            return Err(error("invalid candidate digest"));
        }
        let previous_digest =
            digest(&home, name)?.ok_or_else(|| error("installed executable is missing"))?;
        if let Err(error) = collect_completed(&home) {
            eprintln!("warning: completed core upgrade cleanup: {error}");
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(error)?;
        let id = hex::encode(random);
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let journal = Journal {
            format: 1,
            previous: format!(".rsi-previous-{id}{suffix}"),
            candidate: format!(".rsi-candidate-{id}{suffix}"),
            failed: format!(".rsi-failed-{id}{suffix}"),
            id,
            executable: name.into(),
            previous_digest,
            candidate_digest: expected_digest.into(),
            config: std::fs::canonicalize(paths.config()).map_err(error)?,
            state: std::fs::canonicalize(paths.state()).map_err(error)?,
            was_running,
        };
        let source_parent = rsi_files_native_fs::open_absolute_directory_no_follow(
            candidate
                .parent()
                .ok_or_else(|| error("missing candidate parent"))?,
        )
        .map_err(error)?;
        let source = rsi_files_native_fs::open_relative_file_no_follow(
            &source_parent,
            Path::new(
                candidate
                    .file_name()
                    .ok_or_else(|| error("missing candidate name"))?,
            ),
        )
        .map_err(error)?;
        if let Err(failure) = copy(&home, &journal.candidate, source, expected_digest) {
            let _ = home.directory.remove_child(Path::new(&journal.candidate));
            return Err(failure);
        }
        home.create(PENDING, &serde_json::to_vec(&journal).map_err(error)?)?;
        Ok(Self {
            home,
            _lock: lock,
            journal,
        })
    }

    /// Reads bounded recovery intent and verifies the caller's exact home pair.
    pub fn recover(installation: &Path, paths: &HostPaths) -> Result<Self> {
        let (home, lock) = open(installation)?;
        let bytes = home
            .read(PENDING)?
            .ok_or_else(|| error("no core upgrade is pending"))?;
        let journal: Journal = serde_json::from_slice(&bytes).map_err(error)?;
        validate(&journal, paths)?;
        let transaction = Self {
            home,
            _lock: lock,
            journal,
        };
        // Every known participant is checked before any recovery mutation.
        transaction.topology()?;
        Ok(transaction)
    }

    /// Exact staged executable used for a bounded loadability probe.
    pub fn candidate(&self) -> PathBuf {
        self.home.path.join(&self.journal.candidate)
    }
    /// Ordinary installed executable path after promotion or rollback.
    pub fn executable(&self) -> PathBuf {
        self.home.path.join(&self.journal.executable)
    }
    /// Whether recovery should restart the previous Host.
    pub fn was_running(&self) -> bool {
        self.journal.was_running
    }
    /// Exact expected installed executable digest.
    pub fn candidate_digest(&self) -> &str {
        &self.journal.candidate_digest
    }

    /// Promotes only known bytes, retaining the previous executable on the same volume.
    pub fn promote(&self) -> Result<()> {
        match self.topology()? {
            Topology::Prepared => {
                self.home.rename(
                    &self.journal.executable,
                    &self.journal.previous,
                    EntryKind::File,
                )?;
                self.home.rename(
                    &self.journal.candidate,
                    &self.journal.executable,
                    EntryKind::File,
                )?;
            }
            Topology::Archived => {
                self.home.rename(
                    &self.journal.candidate,
                    &self.journal.executable,
                    EntryKind::File,
                )?;
            }
            Topology::Installed => {}
            Topology::Restoring | Topology::Restored => {
                return Err(error("a restored upgrade cannot be promoted again"));
            }
        }
        if digest(&self.home, &self.journal.executable)?.as_deref()
            != Some(&self.journal.candidate_digest)
        {
            return Err(error("installed executable failed verification"));
        }
        Ok(())
    }

    /// Restores only the recorded previous bytes; never overwrites an unexpected file.
    pub fn rollback(&self) -> Result<()> {
        match self.topology()? {
            Topology::Prepared | Topology::Restored => {}
            Topology::Archived | Topology::Restoring => {
                self.home.rename(
                    &self.journal.previous,
                    &self.journal.executable,
                    EntryKind::File,
                )?;
            }
            Topology::Installed => {
                self.home.rename(
                    &self.journal.executable,
                    &self.journal.failed,
                    EntryKind::File,
                )?;
                self.home.rename(
                    &self.journal.previous,
                    &self.journal.executable,
                    EntryKind::File,
                )?;
            }
        }
        Ok(())
    }

    /// Archives immutable recovery intent only after process readiness or rollback settles.
    pub fn finish(self, committed: bool) -> Result<PathBuf> {
        let expected = if committed {
            &self.journal.candidate_digest
        } else {
            &self.journal.previous_digest
        };
        if digest(&self.home, &self.journal.executable)?.as_ref() != Some(expected) {
            return Err(error("cannot finish an unverified installation"));
        }
        let receipt = format!(
            ".rsi-core-upgrade-{}.{}.json",
            self.journal.id,
            if committed { "committed" } else { "recovered" }
        );
        self.home.rename(PENDING, &receipt, EntryKind::File)?;
        if let Err(error) = collect_completed(&self.home) {
            eprintln!("warning: completed core upgrade cleanup: {error}");
        }
        Ok(self.home.path.join(receipt))
    }

    fn topology(&self) -> Result<Topology> {
        let current = digest(&self.home, &self.journal.executable)?;
        let previous = digest(&self.home, &self.journal.previous)?;
        let candidate = digest(&self.home, &self.journal.candidate)?;
        let failed = digest(&self.home, &self.journal.failed)?;
        let old = Some(self.journal.previous_digest.as_str());
        let new = Some(self.journal.candidate_digest.as_str());
        if previous.as_deref().is_some_and(|value| Some(value) != old)
            || candidate.as_deref().is_some_and(|value| Some(value) != new)
            || failed.as_deref().is_some_and(|value| Some(value) != new)
        {
            return Err(error("upgrade participants contain unexpected bytes"));
        }
        match (
            current.as_deref(),
            previous.as_deref(),
            candidate.as_deref(),
            failed.as_deref(),
        ) {
            (value, None, stage, None) if value == old && stage == new => Ok(Topology::Prepared),
            (None, backup, stage, None) if backup == old && stage == new => Ok(Topology::Archived),
            (value, backup, None, None) if value == new && backup == old => Ok(Topology::Installed),
            (None, backup, None, failure) if backup == old && failure == new => {
                Ok(Topology::Restoring)
            }
            (value, None, None, failure) if value == old && failure == new => {
                Ok(Topology::Restored)
            }
            _ => Err(error(
                "ambiguous core upgrade state; retain all files for recovery",
            )),
        }
    }
}
enum Topology {
    Prepared,
    Archived,
    Installed,
    Restoring,
    Restored,
}

fn validate(journal: &Journal, paths: &HostPaths) -> Result<()> {
    validate_shape(journal)?;
    if journal.config != std::fs::canonicalize(paths.config()).map_err(error)?
        || journal.state != std::fs::canonicalize(paths.state()).map_err(error)?
    {
        return Err(error("different config/state homes"));
    }
    Ok(())
}

fn validate_shape(journal: &Journal) -> Result<()> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    if journal.format != 1
        || journal.id.len() != 32
        || !journal
            .id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !matches!(journal.executable.as_str(), "rsi" | "rsi.exe")
        || journal.previous != format!(".rsi-previous-{}{suffix}", journal.id)
        || journal.candidate != format!(".rsi-candidate-{}{suffix}", journal.id)
        || journal.failed != format!(".rsi-failed-{}{suffix}", journal.id)
        || !hex(&journal.previous_digest)
        || !hex(&journal.candidate_digest)
    {
        return Err(error(
            "invalid upgrade journal or different config/state homes",
        ));
    }
    Ok(())
}

fn collect_completed(home: &Home) -> Result<()> {
    if home.kind(PENDING)?.is_some() {
        return Err(error("pending core transaction cannot be collected"));
    }
    for (index, entry) in home.directory.entries().map_err(error)?.enumerate() {
        if index >= 8192 {
            return Err(error("core receipt collection exceeds 8192 entries"));
        }
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        if let Some(name) = name.to_str() {
            let suffix = if cfg!(windows) { ".exe" } else { "" };
            if let Some(id) = name
                .strip_prefix(".rsi-candidate-")
                .and_then(|id| id.strip_suffix(suffix))
                && id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && home
                    .kind(&format!(".rsi-core-upgrade-{id}.committed.json"))?
                    .is_none()
                && home
                    .kind(&format!(".rsi-core-upgrade-{id}.recovered.json"))?
                    .is_none()
            {
                if entry.file_type().map_err(error)? != EntryKind::File {
                    return Err(error("abandoned core candidate is not a regular file"));
                }
                home.directory
                    .remove_child(Path::new(name))
                    .map_err(error)?;
                continue;
            }
        }
        let Some(name) = name.to_str().filter(|name| {
            name.starts_with(".rsi-core-upgrade-")
                && (name.ends_with(".committed.json") || name.ends_with(".recovered.json"))
        }) else {
            continue;
        };
        let journal: Journal = serde_json::from_slice(
            &home
                .read(name)?
                .ok_or_else(|| error("core receipt disappeared"))?,
        )
        .map_err(error)?;
        validate_shape(&journal)?;
        if name != format!(".rsi-core-upgrade-{}.committed.json", journal.id)
            && name != format!(".rsi-core-upgrade-{}.recovered.json", journal.id)
        {
            return Err(error("core receipt identity differs"));
        }
        for name in [&journal.previous, &journal.candidate, &journal.failed] {
            match home.kind(name)? {
                None => {}
                Some(EntryKind::File) => home
                    .directory
                    .remove_child(Path::new(name))
                    .map_err(error)?,
                _ => {
                    return Err(error(
                        "completed core participant is not an ordinary file; retained",
                    ));
                }
            }
        }
    }
    home.directory.sync_all().map_err(error)
}
fn open(path: &Path) -> Result<(Home, File)> {
    let home = Home::installation(path)?;
    let file = match rsi_files_native_fs::create_relative_private_file_no_follow(
        &home.directory,
        Path::new(LOCK),
    ) {
        Ok(file) => file,
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(windows)]
            let file = rsi_files_native_fs::open_relative_private_file_no_follow(
                &home.directory,
                Path::new(LOCK),
            );
            #[cfg(not(windows))]
            let file =
                rsi_files_native_fs::open_relative_file_no_follow(&home.directory, Path::new(LOCK));
            file.map_err(error)?
        }
        Err(failure) => return Err(error(failure)),
    };
    file.try_lock()
        .map_err(|_| error("another core upgrade owns this installation"))?;
    Ok((home, file))
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(home: &Home, name: &str) -> Result<Option<String>> {
    match home.kind(name)? {
        None => Ok(None),
        Some(EntryKind::File) => {
            let mut file =
                rsi_files_native_fs::open_relative_file_no_follow(&home.directory, Path::new(name))
                    .map_err(error)?;
            hash(&mut file, None).map(Some)
        }
        _ => Err(error("upgrade participant is not an ordinary file")),
    }
}
fn hash(source: &mut File, mut destination: Option<&mut File>) -> Result<String> {
    let before = source.metadata().map_err(error)?;
    if !before.is_file() || before.len() > MAXIMUM_EXECUTABLE {
        return Err(error("executable exceeds its file bound"));
    }
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let length = source.read(&mut buffer).map_err(error)?;
        if length == 0 {
            break;
        }
        total += length as u64;
        if total > MAXIMUM_EXECUTABLE {
            return Err(error("executable grew beyond its bound"));
        }
        digest.update(&buffer[..length]);
        if let Some(destination) = destination.as_mut() {
            destination.write_all(&buffer[..length]).map_err(error)?;
        }
    }
    if total != before.len() {
        return Err(error("executable changed while copying"));
    }
    Ok(hex::encode(digest.finalize()))
}
fn copy(home: &Home, name: &str, mut source: File, expected: &str) -> Result<()> {
    let mut destination = rsi_files_native_fs::create_relative_private_file_no_follow(
        &home.directory,
        Path::new(name),
    )
    .map_err(error)?;
    if hash(&mut source, Some(&mut destination))? != expected {
        return Err(error("candidate digest differs from its admitted package"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        destination
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(error)?;
    }
    destination.sync_all().map_err(error)?;
    drop(destination);
    home.directory.sync_all().map_err(error)
}
fn error(value: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("core upgrade: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        temporary: tempfile::TempDir,
        paths: HostPaths,
        executable: PathBuf,
        candidate: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            let paths = HostPaths::new(root.join("config"), root.join("state"), root.join("cache"))
                .unwrap();
            for path in [paths.config(), paths.state(), &root.join("bin")] {
                rsi_files_native_fs::create_absolute_directory_no_follow(path).unwrap();
            }
            let executable = root
                .join("bin")
                .join(if cfg!(windows) { "rsi.exe" } else { "rsi" });
            let candidate = root.join("update");
            std::fs::write(&executable, b"previous").unwrap();
            std::fs::write(&candidate, b"candidate").unwrap();
            std::fs::write(root.join("bin/unrelated"), b"keep").unwrap();
            Self {
                temporary,
                paths,
                executable,
                candidate,
            }
        }
        fn begin(&self) -> CoreUpgrade {
            CoreUpgrade::prepare(
                &self.executable,
                &self.candidate,
                &hex::encode(Sha256::digest(b"candidate")),
                &self.paths,
                true,
            )
            .unwrap()
        }
        fn recover(&self) -> CoreUpgrade {
            CoreUpgrade::recover(self.executable.parent().unwrap(), &self.paths).unwrap()
        }
        fn preserved(&self) {
            assert_eq!(
                std::fs::read(self.temporary.path().join("bin/unrelated")).unwrap(),
                b"keep"
            );
        }
    }
    #[cfg(windows)]
    #[test]
    fn shared_installation_is_rejected_without_acl_repair_or_guard_creation() {
        let fixture = Fixture::new();
        let shared = fixture.temporary.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        let selected = rsi_files_native_fs::open_absolute_directory_no_follow(&shared).unwrap();
        assert!(selected.verify_private().is_err());
        std::fs::write(shared.join("rsi.exe"), b"previous").unwrap();
        let result = CoreUpgrade::prepare(
            &shared.join("rsi.exe"),
            &fixture.candidate,
            &hex::encode(Sha256::digest(b"candidate")),
            &fixture.paths,
            false,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("private current-user installation")
        );
        assert!(selected.verify_private().is_err());
        assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
    }
    #[test]
    fn preparation_reclaims_unjournaled_candidates_and_refuses_failed_copy() {
        let fixture = Fixture::new();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let abandoned = fixture
            .executable
            .parent()
            .unwrap()
            .join(format!(".rsi-candidate-{}{suffix}", "a".repeat(32)));
        std::fs::write(&abandoned, b"incomplete copy").unwrap();
        assert!(
            CoreUpgrade::prepare(
                &fixture.executable,
                &fixture.candidate,
                &"b".repeat(64),
                &fixture.paths,
                false
            )
            .is_err()
        );
        assert!(!abandoned.exists());
        assert!(
            !std::fs::read_dir(fixture.executable.parent().unwrap())
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".rsi-candidate-"))
        );
        assert_eq!(std::fs::read(&fixture.executable).unwrap(), b"previous");
        fixture.preserved();
    }

    #[test]
    fn promotion_has_one_writer_and_keeps_previous_bytes_and_receipt() {
        let fixture = Fixture::new();
        let transaction = fixture.begin();
        assert!(
            CoreUpgrade::recover(fixture.executable.parent().unwrap(), &fixture.paths).is_err()
        );
        transaction.promote().unwrap();
        assert_eq!(std::fs::read(&fixture.executable).unwrap(), b"candidate");
        assert_eq!(
            std::fs::read(transaction.home.path.join(&transaction.journal.previous)).unwrap(),
            b"previous"
        );
        assert!(transaction.finish(true).unwrap().is_file());
        assert!(
            CoreUpgrade::recover(fixture.executable.parent().unwrap(), &fixture.paths).is_err()
        );
        fixture.preserved();
    }
    #[test]
    fn recovery_restores_each_interrupted_rename_boundary() {
        for step in 0..=3 {
            let fixture = Fixture::new();
            let transaction = fixture.begin();
            if step > 0 {
                transaction
                    .home
                    .rename(
                        &transaction.journal.executable,
                        &transaction.journal.previous,
                        EntryKind::File,
                    )
                    .unwrap();
            }
            if step > 1 {
                transaction
                    .home
                    .rename(
                        &transaction.journal.candidate,
                        &transaction.journal.executable,
                        EntryKind::File,
                    )
                    .unwrap();
            }
            if step > 2 {
                transaction
                    .home
                    .rename(
                        &transaction.journal.executable,
                        &transaction.journal.failed,
                        EntryKind::File,
                    )
                    .unwrap();
            }
            drop(transaction);
            let transaction = fixture.recover();
            assert!(transaction.was_running());
            transaction.rollback().unwrap();
            drop(transaction);
            let transaction = fixture.recover();
            transaction.rollback().unwrap();
            assert_eq!(std::fs::read(&fixture.executable).unwrap(), b"previous");
            transaction.finish(false).unwrap();
            fixture.preserved();
        }
    }
    #[test]
    fn foreign_homes_and_changed_participants_are_refused_before_recovery() {
        let fixture = Fixture::new();
        let transaction = fixture.begin();
        transaction.promote().unwrap();
        drop(transaction);
        let foreign = fixture.temporary.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        let paths = HostPaths::new(fixture.paths.config(), foreign, fixture.paths.cache()).unwrap();
        assert!(CoreUpgrade::recover(fixture.executable.parent().unwrap(), &paths).is_err());
        std::fs::write(&fixture.executable, b"unexpected").unwrap();
        assert!(
            CoreUpgrade::recover(fixture.executable.parent().unwrap(), &fixture.paths).is_err()
        );
        assert_eq!(std::fs::read(&fixture.executable).unwrap(), b"unexpected");
        fixture.preserved();
    }

    #[test]
    fn settled_transactions_reclaim_executables_but_keep_receipts() {
        let fixture = Fixture::new();
        let transaction = fixture.begin();
        let previous = transaction.home.path.join(&transaction.journal.previous);
        transaction.promote().unwrap();
        // Settled payloads are disposable; receipt identity and file kind own cleanup.
        std::fs::write(&previous, b"obsolete modified payload").unwrap();
        let receipt = transaction.finish(true).unwrap();
        assert!(receipt.is_file());
        assert!(!previous.exists());
        let transaction = fixture.begin();
        let candidate = transaction.candidate();
        transaction.finish(false).unwrap();
        assert!(!candidate.exists());
        assert_eq!(std::fs::read(&fixture.executable).unwrap(), b"candidate");
        fixture.preserved();
    }
}
