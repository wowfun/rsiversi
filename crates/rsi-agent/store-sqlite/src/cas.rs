use super::bounded_text;
use super::*;

pub(super) fn install_cas(
    cas_dir: &Path,
    cas_staging_dir: &Path,
    sha256: &str,
    bytes: &[u8],
) -> Result<()> {
    validate_digest(sha256, bytes)?;
    let target = cas_dir.join(sha256);
    if target.exists() {
        let existing = read_cas_file(cas_dir, sha256)?;
        if existing != bytes {
            return Err(StoreError::Corrupt(
                "existing CAS body conflicts with its digest name".into(),
            ));
        }
        return Ok(());
    }
    let temporary = cas_staging_dir.join(format!(
        ".{sha256}.{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(io_error)?;
    let publish = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_regular_file_bounded(&target, MAXIMUM_STORE_CAS_BYTES)? != bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "existing CAS body differs from candidate",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        fs::remove_file(&temporary)?;
        sync_directory_io(cas_staging_dir)?;
        sync_directory_io(cas_dir)
    })();
    if let Err(error) = publish {
        let _ignored = fs::remove_file(&temporary);
        if target.exists() && read_cas_file(cas_dir, sha256)? == bytes {
            return Ok(());
        }
        return Err(io_error(error));
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_directory_io(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
pub(super) fn sync_directory_io(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub(super) fn read_cas_file(cas_dir: &Path, sha256: &str) -> Result<Vec<u8>> {
    read_cas_file_bounded(cas_dir, sha256, MAXIMUM_STORE_CAS_BYTES)
}

pub(super) fn read_cas_file_bounded(
    cas_dir: &Path,
    sha256: &str,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    validate_sha256("CAS identity", sha256)?;
    let path = cas_dir.join(sha256);
    let bytes = read_regular_file_bounded(&path, maximum_bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            StoreError::Corrupt(error.to_string())
        } else {
            cas_read_error(sha256, error)
        }
    })?;
    validate_digest(sha256, &bytes)?;
    Ok(bytes)
}

pub(super) fn read_regular_file_bounded(
    path: &Path,
    maximum_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let (file, metadata) = open_cas_file(path)?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("CAS entry exceeds {maximum_bytes} bytes"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("CAS entry exceeds {maximum_bytes} bytes"),
        ));
    }
    Ok(bytes)
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
    if file.read(&mut extra).map_err(io_error)? != 0 {
        return Err(StoreError::Corrupt(
            "CAS entry grew beyond admitted length".into(),
        ));
    }
    Ok(bytes)
}

// Both bounded verification and admitted payload reads open the same regular-file boundary.
fn open_cas_file(path: &Path) -> std::io::Result<(File, fs::Metadata)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CAS entry is not a regular file",
        ));
    }
    #[cfg(unix)]
    let file = {
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("CAS path has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| std::io::Error::other("CAS path has no file name"))?;
        let directory = rsi_files_native_fs::open_absolute_directory_no_follow(parent)?;
        rsi_files_native_fs::open_relative_file_no_follow(&directory, Path::new(name))?
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        // BACKUP_SEMANTICS permits directory handles; OPEN_REPARSE_POINT opens
        // each component itself. Excluding FILE_SHARE_DELETE pins every acquired
        // parent against rename/replacement until the leaf handle is acquired.
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("CAS path has no parent"))?;
        let mut prefix = PathBuf::new();
        let mut parents = Vec::new();
        for component in parent.components() {
            prefix.push(component);
            if matches!(component, std::path::Component::Prefix(_)) {
                continue;
            }
            if !matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            ) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "CAS parent is not normalized",
                ));
            }
            let directory = OpenOptions::new()
                .read(true)
                .share_mode(0x0000_0003)
                .custom_flags(0x0220_0000)
                .open(&prefix)?;
            let metadata = directory.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & 0x0000_0400 != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "CAS parent is a reparse point or not a directory",
                ));
            }
            parents.push(directory);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0x0020_0000)
            .open(path)?;
        if file.metadata()?.file_attributes() & 0x0000_0400 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "CAS leaf is a reparse point",
            ));
        }
        drop(parents);
        file
    };
    #[cfg(not(any(unix, windows)))]
    let file = File::open(path)?;
    let actual = file.metadata()?;
    if !actual.file_type().is_file() || actual.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CAS entry is not a regular file",
        ));
    }
    Ok((file, actual))
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
