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
    validate_sha256("CAS identity", sha256)?;
    let path = cas_dir.join(sha256);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StoreError::NotFound(sha256.into())
        } else {
            io_error(error)
        }
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(StoreError::Corrupt(
            "CAS entry is not a regular file".into(),
        ));
    }
    let bytes = read_regular_file_bounded(&path, MAXIMUM_STORE_CAS_BYTES).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            StoreError::Corrupt(error.to_string())
        } else {
            io_error(error)
        }
    })?;
    validate_digest(sha256, &bytes)?;
    Ok(bytes)
}

pub(super) fn read_regular_file_bounded(
    path: &Path,
    maximum_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CAS entry is not a regular file",
        ));
    }
    let file = File::open(path)?;
    if file.metadata()?.len() > maximum_bytes as u64 {
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

pub(super) type ContextCheckpointProjection = (
    String,
    i64,
    String,
    i64,
    Option<Vec<u8>>,
    i64,
    Option<String>,
    i64,
);

pub(super) fn decode_context_checkpoint(
    projection: ContextCheckpointProjection,
) -> Result<StoredContextCheckpoint> {
    let (
        header_fingerprint,
        through_seq,
        fact_prefix_sha256,
        encoded_len,
        bytes,
        header_encoded_len,
        header_json,
        durable_seq,
    ) = projection;
    let encoded_len = usize::try_from(encoded_len)
        .map_err(|_| StoreError::Corrupt("checkpoint length is invalid".into()))?;
    if encoded_len == 0 || encoded_len > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
        return Err(StoreError::Corrupt(
            "checkpoint bytes exceed their durable bound".into(),
        ));
    }
    let bytes = bytes.ok_or_else(|| {
        StoreError::Corrupt("bounded checkpoint projection returned no bytes".into())
    })?;
    if bytes.len() != encoded_len {
        return Err(StoreError::Corrupt(
            "checkpoint byte length changed during read".into(),
        ));
    }
    let checkpoint = StoredContextCheckpoint {
        header_fingerprint,
        through_seq: decode_u64("checkpoint sequence", through_seq)?,
        fact_prefix_sha256,
        bytes: Arc::from(bytes),
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
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
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
    let message = error.to_string();
    drop(error);
    StoreError::Io(format!("SQLite: {message}"))
}

pub(super) fn io_error(error: std::io::Error) -> StoreError {
    let message = error.to_string();
    drop(error);
    StoreError::Io(format!("filesystem: {message}"))
}
