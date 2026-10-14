use super::bounded_text;
use super::*;

/// One filesystem operation in immutable CAS publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationStep {
    /// Writes the staging body.
    Write,
    /// Flushes the new staging file.
    FileSync,
    /// Publishes the immutable name without replacing it.
    Link,
    /// Removes the temporary name.
    TemporaryRemoval,
    /// Flushes the staging directory on Unix.
    StagingSync,
    /// Flushes the CAS directory on Unix.
    NamespaceSync,
    /// Flushes the same verified existing file.
    ExistingFileSync,
}

/// Whether a reported failure precedes or follows the real operation.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationPhase {
    /// The operation has not run.
    Before,
    /// The operation succeeded but its acknowledgement is lost.
    After,
}

#[derive(Debug, Default)]
pub(super) struct Publication {
    #[cfg(all(test, unix))]
    crash: Mutex<Option<(PublicationStep, PublicationPhase)>>,
    #[cfg(any(test, feature = "test-support"))]
    fault: Mutex<Option<(PublicationStep, PublicationPhase)>>,
}

impl Publication {
    fn run<T>(
        &self,
        step: PublicationStep,
        operation: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = (self, step);
        #[cfg(any(test, feature = "test-support"))]
        self.check(step, PublicationPhase::Before)?;
        let result = operation()?;
        #[cfg(any(test, feature = "test-support"))]
        self.check(step, PublicationPhase::After)?;
        Ok(result)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn check(&self, step: PublicationStep, phase: PublicationPhase) -> std::io::Result<()> {
        #[cfg(all(test, unix))]
        if *self.crash.lock().unwrap() == Some((step, phase)) {
            crash_child_exit(&format!("{step:?}/{phase:?}"));
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            let mut fault = self.fault.lock().unwrap();
            if *fault == Some((step, phase)) {
                fault.take();
                return Err(std::io::Error::other("injected CAS publication failure"));
            }
        }
        Ok(())
    }

    #[cfg(any(all(test, unix), feature = "test-support"))]
    pub(super) fn inject(&self, step: PublicationStep, phase: PublicationPhase) {
        assert!(self.fault.lock().unwrap().replace((step, phase)).is_none());
    }

    #[cfg(all(test, unix))]
    pub(super) fn crash_after(&self, step: PublicationStep) {
        require_crash_child();
        assert!(
            self.crash
                .lock()
                .unwrap()
                .replace((step, PublicationPhase::After))
                .is_none()
        );
    }
}

#[cfg(all(test, unix))]
pub(super) const CRASH_CHILD_EXIT_CODE: i32 = 86;

#[cfg(all(test, unix))]
pub(super) fn require_crash_child() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(
        args.iter().any(|arg| arg == "--exact")
            && args.iter().any(|arg| arg == "tests::cas_crash_child")
            && std::env::var_os("RSI_CAS_CRASH_TEST_ROOT").is_some()
            && std::env::var_os("RSI_CAS_CRASH_TEST_POINT").is_some(),
        "CAS crash injection requires --exact tests::cas_crash_child and its fixture environment"
    );
}

#[cfg(all(test, unix))]
pub(super) fn crash_child_exit(boundary: &str) -> ! {
    require_crash_child();
    eprintln!("intentional CAS crash child at {boundary}");
    std::process::exit(CRASH_CHILD_EXIT_CODE)
}

#[cfg(any(unix, windows))]
struct PublicationDirectory {
    directory: rsi_files_native_fs::Directory,
    #[cfg(windows)]
    namespace: rsi_files_native_fs::NamespaceLease,
}
#[cfg(any(unix, windows))]
impl PublicationDirectory {
    #[cfg_attr(unix, allow(clippy::unnecessary_wraps))] // Windows acquires a fallible namespace pin.
    fn new(directory: rsi_files_native_fs::Directory) -> std::io::Result<Self> {
        #[cfg(windows)]
        let namespace = directory.pin_namespace()?;
        Ok(Self {
            directory,
            #[cfg(windows)]
            namespace,
        })
    }
    fn open(&self, name: &Path) -> std::io::Result<(File, fs::Metadata)> {
        #[cfg(unix)]
        let file = rsi_files_native_fs::open_relative_file_no_follow(&self.directory, name)?;
        #[cfg(windows)]
        let file =
            rsi_files_native_fs::open_relative_private_file_no_follow(&self.directory, name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "CAS entry is not a regular file",
            ));
        }
        Ok((file, metadata))
    }
    fn sync(&self) -> std::io::Result<()> {
        self.directory.sync_all()
    }
    fn link(&self, source: &Self, name: &Path, destination: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        let target = &self.directory;
        #[cfg(windows)]
        let target = &self.namespace;
        target.link_child(&source.directory, name, destination)
    }
    fn remove(&self, name: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        let target = &self.directory;
        #[cfg(windows)]
        let target = &self.namespace;
        target.remove_child(name)
    }
}

#[cfg(any(unix, windows))]
pub(super) fn install_cas(
    cas_dir: &Path,
    bytes: &[u8],
    publication: &Publication,
) -> Result<CasObjectRef> {
    let cas = PublicationDirectory::new(
        rsi_files_native_fs::open_absolute_directory_no_follow(cas_dir).map_err(io_error)?,
    )
    .map_err(io_error)?;
    let staging = PublicationDirectory::new(
        rsi_files_native_fs::open_relative_directory_no_follow(
            &cas.directory,
            Path::new("staging"),
        )
        .map_err(io_error)?,
    )
    .map_err(io_error)?;
    install_in_namespaces(&cas, &staging, bytes, publication)
}

#[cfg(any(unix, windows))]
fn install_in_namespaces(
    cas: &PublicationDirectory,
    staging: &PublicationDirectory,
    bytes: &[u8],
    publication: &Publication,
) -> Result<CasObjectRef> {
    let reference = CasObjectRef {
        sha256: hex::encode(Sha256::digest(bytes)),
        byte_len: u64::try_from(bytes.len())
            .map_err(|_| StoreError::Invalid("CAS length exceeds u64".into()))?,
    };
    reference.validate()?;
    let sha256 = &reference.sha256;
    let target = Path::new(sha256);
    match cas.open(target) {
        Ok((file, metadata)) => {
            sync_verified_existing(file, &metadata, bytes, publication)
                .map_err(|error| cas_read_error(sha256, error))?;
            publication
                .run(PublicationStep::NamespaceSync, || cas.sync())
                .map_err(io_error)?;
            return Ok(reference);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(cas_read_error(sha256, error)),
    }
    let name = format!(
        ".{sha256}.{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let temporary = Path::new(&name);
    let mut file =
        rsi_files_native_fs::create_relative_private_file_no_follow(&staging.directory, temporary)
            .map_err(io_error)?;
    let publish = (|| -> std::io::Result<()> {
        publication.run(PublicationStep::Write, || file.write_all(bytes))?;
        publication.run(PublicationStep::FileSync, || file.sync_all())?;
        drop(file);
        match publication.run(PublicationStep::Link, || {
            cas.link(staging, temporary, target)
        }) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let (file, metadata) = cas.open(target)?;
                sync_verified_existing(file, &metadata, bytes, publication)?;
            }
            Err(error) => return Err(error),
        }
        publication.run(PublicationStep::TemporaryRemoval, || {
            staging.remove(temporary)
        })?;
        publication.run(PublicationStep::StagingSync, || staging.sync())?;
        publication.run(PublicationStep::NamespaceSync, || cas.sync())
    })();
    if let Err(error) = publish {
        let _ignored = staging.remove(temporary);
        return Err(io_error(error));
    }
    Ok(reference)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn install_cas(
    _cas_dir: &Path,
    _bytes: &[u8],
    _publication: &Publication,
) -> Result<CasObjectRef> {
    filesystem::require_native_platform()?;
    unreachable!("unsupported platforms refuse native authority")
}

#[cfg(all(test, unix))]
mod publication_authority_tests {
    use super::*;
    #[test]
    fn publication_keeps_both_opened_directories_after_parent_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("cas");
        fs::create_dir_all(original.join("staging")).unwrap();
        let cas = PublicationDirectory::new(
            rsi_files_native_fs::open_absolute_directory_no_follow(&original).unwrap(),
        )
        .unwrap();
        let staging = PublicationDirectory::new(
            rsi_files_native_fs::open_relative_directory_no_follow(
                &cas.directory,
                Path::new("staging"),
            )
            .unwrap(),
        )
        .unwrap();
        let moved = temp.path().join("moved");
        fs::rename(&original, &moved).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &original).unwrap();
        let result =
            install_in_namespaces(&cas, &staging, b"retained body", &Publication::default())
                .unwrap();
        assert_eq!(
            fs::read(moved.join(result.sha256)).unwrap(),
            b"retained body"
        );
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
        assert_eq!(fs::read_dir(moved.join("staging")).unwrap().count(), 0);
    }
}

fn sync_verified_existing(
    mut file: File,
    metadata: &fs::Metadata,
    bytes: &[u8],
    publication: &Publication,
) -> std::io::Result<()> {
    if metadata.len() != bytes.len() as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "existing CAS body conflicts with its digest name",
        ));
    }
    // Keep the verified handle through sync: reopening would validate one inode
    // and acknowledge durability of a possibly replaced one.
    let mut remaining = bytes;
    let mut buffer = [0; 16 * 1024];
    while !remaining.is_empty() {
        let length = remaining.len().min(buffer.len());
        file.read_exact(&mut buffer[..length]).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "existing CAS body shrank during verification",
                )
            } else {
                error
            }
        })?;
        if buffer[..length] != remaining[..length] {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "existing CAS body conflicts with its digest name",
            ));
        }
        remaining = &remaining[length..];
    }
    if super::filesystem::read_retry(&mut file, &mut buffer[..1])? != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "existing CAS body grew during verification",
        ));
    }
    publication.run(PublicationStep::ExistingFileSync, || file.sync_all())
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(windows)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    rsi_files_native_fs::sync_absolute_directory_no_follow(path).map_err(io_error)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn sync_directory(_path: &Path) -> Result<()> {
    super::filesystem::require_native_platform()
}

pub(super) struct ContextCheckpointProjection {
    pub(super) header_fingerprint: String,
    pub(super) through_seq: i64,
    pub(super) fact_prefix_sha256: String,
    pub(super) encoded_len: usize,
    pub(super) header_encoded_len: i64,
    pub(super) header_json: Option<String>,
    pub(super) durable_seq: i64,
}

pub(super) fn decode_context_checkpoint(
    projection: ContextCheckpointProjection,
    bytes: rsi_api_protocol::RetainedBytes,
) -> Result<StoredContextCheckpoint> {
    let ContextCheckpointProjection {
        header_fingerprint,
        through_seq,
        fact_prefix_sha256,
        encoded_len,
        header_encoded_len,
        header_json,
        durable_seq,
    } = projection;
    if bytes.len() != encoded_len {
        return Err(StoreError::Corrupt(
            "checkpoint byte length changed during read".into(),
        ));
    }
    let checkpoint = StoredContextCheckpoint {
        header_fingerprint,
        through_seq: decode_u64("checkpoint sequence", through_seq)?,
        fact_prefix_sha256,
        bytes,
    };
    checkpoint
        .validate()
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    let header: SessionHeader = decode_projected_json(
        "session header",
        (header_encoded_len, header_json),
        MAXIMUM_SESSION_HEADER_BYTES,
    )?;
    let expected_fingerprint = header.fingerprint().map_err(|error| {
        StoreError::Corrupt(format!("stored session header is invalid: {error}"))
    })?;
    if checkpoint.header_fingerprint != expected_fingerprint {
        return Err(StoreError::Corrupt(
            "checkpoint header fingerprint differs from the durable session".into(),
        ));
    }
    if checkpoint.through_seq > decode_u64("durable sequence", durable_seq)? {
        return Err(StoreError::Corrupt(
            "checkpoint cursor exceeds the durable tail".into(),
        ));
    }
    Ok(checkpoint)
}

pub(super) fn validate_sha256(label: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(StoreError::Corrupt(format!(
            "{label} is not lowercase SHA-256"
        )));
    }
    Ok(())
}

pub(super) fn decode_sha256(label: &str, value: &str) -> Result<[u8; 32]> {
    validate_sha256(label, value)?;
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(value, &mut digest)
        .map_err(|error| StoreError::Corrupt(format!("cannot decode {label}: {error}")))?;
    Ok(digest)
}

pub(super) fn validate_digest(sha256: &str, bytes: &[u8]) -> Result<()> {
    validate_sha256("CAS identity", sha256)?;
    if hex::encode(Sha256::digest(bytes)) != sha256 {
        return Err(StoreError::Corrupt(
            "CAS body does not match its digest".into(),
        ));
    }
    Ok(())
}

pub(super) fn encode_json(label: &str, value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::Invalid(format!("cannot encode {label}: {error}")))
}

pub(super) fn decode_json<T: serde::de::DeserializeOwned>(label: &str, json: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|error| StoreError::Corrupt(format!("invalid {label}: {error}")))
}

pub(super) fn decode_projected_json<T: serde::de::DeserializeOwned>(
    label: &str,
    (encoded_len, json): (i64, Option<String>),
    maximum_bytes: usize,
) -> Result<T> {
    super::cold_validation::check()?;
    #[cfg(feature = "test-support")]
    let _decode = super::test_support::DecodeProbe::start();
    let encoded_len = usize::try_from(encoded_len)
        .map_err(|_| StoreError::Corrupt(format!("{label} has a negative byte length")))?;
    if encoded_len > maximum_bytes {
        return Err(StoreError::Corrupt(format!(
            "{label} exceeds {maximum_bytes} encoded bytes"
        )));
    }
    let json = json.ok_or_else(|| {
        StoreError::Corrupt(format!(
            "{label} is absent from its bounded SQLite projection"
        ))
    })?;
    if json.len() != encoded_len {
        return Err(StoreError::Corrupt(format!(
            "{label} byte length disagrees with its SQLite projection"
        )));
    }
    decode_json(label, &json)
}

pub(super) fn read_indexed_fact(
    connection: &Connection,
    session_id: &SessionId,
    sequence: i64,
) -> Result<SessionFact> {
    let projection = connection
        .query_row(
            "SELECT seq, turn_id, fact_kind,
                    length(CAST(fact_json AS BLOB)),
                    CASE WHEN length(CAST(fact_json AS BLOB)) <= ?3
                         THEN fact_json END
             FROM facts WHERE session_id = ?1 AND seq = ?2",
            params![
                session_id.as_str(),
                sequence,
                i64::try_from(MAXIMUM_SESSION_FACT_BYTES)
                    .expect("session Fact bound fits SQLite INTEGER"),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    bounded_text(row, 1, 256)?,
                    bounded_text(row, 2, 8)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            StoreError::Corrupt("turn index references an absent canonical Fact".into())
        })?;
    let fact: SessionFact = decode_projected_json(
        "session Fact",
        (projection.3, projection.4),
        MAXIMUM_SESSION_FACT_BYTES,
    )?;
    let indexed_sequence = decode_u64("indexed Fact sequence", projection.0)?;
    if fact.seq() != indexed_sequence
        || fact.body().turn_id().as_str() != projection.1
        || fact_index_kind(fact.body()) != projection.2
    {
        return Err(StoreError::Corrupt(
            "indexed Fact JSON differs from its relational row".into(),
        ));
    }
    Ok(fact)
}

pub(super) fn sqlite_u64(label: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::Invalid(format!("{label} exceeds SQLite INTEGER")))
}

pub(super) fn decode_u64(label: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt(format!("{label} is negative")))
}

pub(super) const fn fact_index_kind(body: &SessionFactBody) -> &'static str {
    match rsi_agent_store_protocol::store_fact_turn_role(body) {
        StoreFactTurnRole::Acceptance => "accepted",
        StoreFactTurnRole::Terminal => "terminal",
        StoreFactTurnRole::Event => "event",
    }
}

pub(super) fn sql_error(error: rusqlite::Error) -> StoreError {
    if super::cold_validation::in_read_scope()
        && matches!(&error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::OperationInterrupted))
    {
        return StoreError::ValidationBusy;
    }
    let message = error.to_string();
    let mapped = match &error {
        rusqlite::Error::InvalidColumnType(..) | rusqlite::Error::FromSqlConversionFailure(..) => {
            StoreError::Corrupt(format!("SQLite row: {message}"))
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            StoreError::Corrupt(format!("SQLite: {message}"))
        }
        _ => StoreError::Io(format!("SQLite: {message}")),
    };
    drop(error);
    mapped
}

pub(super) fn io_error(error: std::io::Error) -> StoreError {
    let message = error.to_string();
    drop(error);
    StoreError::Io(format!("filesystem: {message}"))
}

/// The caller reserved the exact validated object length before this allocation.
pub(super) fn read_cas_file_admitted(
    cas_dir: &Path,
    digest: &str,
    length: usize,
) -> Result<Vec<u8>> {
    validate_sha256("CAS identity", digest)?;
    let path = cas_dir.join(digest);
    let (mut file, actual) = open_cas_file(&path).map_err(|error| cas_read_error(digest, error))?;
    if actual.len() != length as u64 {
        return Err(StoreError::Corrupt(format!(
            "CAS entry exceeds {length} bytes, is not regular, or has changed length"
        )));
    }
    let bytes = read_admitted_cas_body(&mut file, length)?;
    validate_digest(digest, &bytes)?;
    Ok(bytes)
}

fn read_admitted_cas_body(file: &mut impl std::io::Read, length: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            StoreError::Corrupt("CAS entry shrank below admitted length".into())
        } else {
            io_error(error)
        }
    })?;
    let mut extra = [0];
    if super::filesystem::read_retry(file, &mut extra).map_err(io_error)? != 0 {
        return Err(StoreError::Corrupt(
            "CAS entry grew beyond admitted length".into(),
        ));
    }
    Ok(bytes)
}

// Bounded verification, payload reads and fsync share the same Unix file boundary.
#[cfg(unix)]
pub(super) fn open_cas_file(path: &Path) -> std::io::Result<(File, fs::Metadata)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CAS entry is not a regular file",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("CAS path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("CAS path has no file name"))?;
    let directory = rsi_files_native_fs::open_absolute_directory_no_follow(parent)?;
    let file = rsi_files_native_fs::open_relative_file_no_follow(&directory, Path::new(name))?;
    let actual = file.metadata()?;
    if !actual.file_type().is_file() || actual.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CAS entry is not a regular file",
        ));
    }
    Ok((file, actual))
}

#[cfg(windows)]
pub(super) fn open_cas_file(path: &Path) -> std::io::Result<(File, fs::Metadata)> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("CAS path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("CAS path has no name"))?;
    let directory = rsi_files_native_fs::open_absolute_directory_no_follow(parent)?;
    let file =
        rsi_files_native_fs::open_relative_private_file_no_follow(&directory, Path::new(name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("CAS entry is not a regular file"));
    }
    Ok((file, metadata))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn open_cas_file(_path: &Path) -> std::io::Result<(File, fs::Metadata)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "CAS requires native Unix Store authority",
    ))
}

fn cas_read_error(digest: &str, error: std::io::Error) -> StoreError {
    #[cfg(unix)]
    if rsi_files_native_fs::is_link_rejection(&error) {
        return StoreError::Corrupt("CAS path contains a symlink or non-directory parent".into());
    }
    if error.kind() == std::io::ErrorKind::NotFound {
        StoreError::NotFound(digest.into())
    } else if error.kind() == std::io::ErrorKind::InvalidData {
        StoreError::Corrupt(error.to_string())
    } else {
        io_error(error)
    }
}

#[cfg(test)]
mod admitted_tests {
    use super::*;
    #[test]
    fn admitted_cas_body_rejects_truncation_and_growth_as_corruption() {
        for body in [b"short".as_slice(), b"too long".as_slice()] {
            assert!(matches!(
                read_admitted_cas_body(&mut std::io::Cursor::new(body), 6),
                Err(StoreError::Corrupt(_))
            ));
        }
        assert_eq!(
            read_admitted_cas_body(&mut std::io::Cursor::new(b"exact!"), 6).unwrap(),
            b"exact!"
        );
    }
}
