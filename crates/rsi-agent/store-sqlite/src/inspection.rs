//! Streaming offline CAS inspection; ownership metadata is never a deletion policy.
use super::*;
use std::ops::ControlFlow;
use tokio_util::sync::CancellationToken;

/// Explicit amount of work performed by offline inspection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum CasInspectionMode {
    /// Check registry metadata and physical entries without hashing bodies or replaying history.
    #[default]
    Metadata,
    /// Hash registered bodies and verify native CAS references during canonical replay.
    Full,
}
/// Whether the requested inspection reached the end of every pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum CasInspectionCompletion {
    /// All requested passes completed; issues may still have been reported.
    #[default]
    Complete,
    /// The caller's cooperative cancellation token stopped work.
    Cancelled,
    /// The visitor requested a stop.
    VisitorStopped,
    /// A fatal error interrupted inspection; the returned error owns its cause.
    Failed,
}
/// Fixed-size progress and issue counts; no object inventory is retained.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CasInspectionSummary {
    /// Completion of the requested passes, distinct from presence of issues.
    pub completion: CasInspectionCompletion,
    /// Registry rows examined, including invalid rows.
    pub metadata_rows: u64,
    /// Bytes declared by valid registry rows.
    pub registered_bytes: u64,
    /// CAS-root entries examined; staging is reported without traversing it.
    pub directory_entries: u64,
    /// Validly named, regular files without registry metadata.
    pub unregistered_files: u64,
    /// Physical bytes in those unregistered files, without declaring them reclaimable.
    pub unregistered_bytes: u64,
    /// Registered files successfully checked against length, EOF and digest.
    pub verified_objects: u64,
    /// Bytes actually read by hashing, including partially checked files.
    pub hashed_bytes: u64,
    /// Native references checked in canonical Facts and controls.
    pub references: u64,
    /// Issues emitted, independent of whether the inspection completed.
    pub issues: u64,
}
/// Canonical stream containing one native CAS reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CasInspectionStream {
    /// Agent control stream.
    Control,
    /// Session Fact stream.
    Fact,
}
/// Bounded identity of an observation, never an unbounded durable key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum CasInspectionLocation {
    /// Registry row; invalid or oversized digest keys are omitted.
    Metadata {
        /// `SQLite` rowid, also supporting negative values.
        rowid: i64,
        /// Valid lowercase digest, when present.
        sha256: Option<String>,
        /// Stored key length, without materializing an oversized key.
        key_bytes: Option<u64>,
    },
    /// One physical entry, with its name preview bounded to 128 encoded bytes.
    File {
        /// Bounded name preview; a preview is never used as a filesystem path.
        name: String,
    },
    /// A typed reference from a decoded canonical record.
    Reference {
        /// Exact owning Session.
        session_id: SessionId,
        /// Source canonical stream.
        stream: CasInspectionStream,
        /// Exact record sequence.
        sequence: u64,
        /// Valid referenced digest.
        sha256: String,
    },
}
/// An integrity or layout observation; reporting does not mutate ownership.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum CasInspectionIssue {
    /// Metadata has an invalid type, digest or object-length bound.
    InvalidMetadata,
    /// A registered physical file or referenced metadata row is absent.
    Missing,
    /// Actual file or registry length differs from the admitted reference.
    LengthMismatch {
        /// Expected exact length.
        expected: u64,
        /// Observed physical or registered length. If the body grows during
        /// hashing, the EOF probe reports only a lower bound.
        actual: u64,
    },
    /// The complete body does not match its digest name.
    DigestMismatch,
    /// A physical digest file is not registered; it is retained.
    Unregistered,
    /// An entry is a link, unexpected directory or unknown filename.
    UnexpectedEntry,
    /// A per-object read could not complete; other objects can still be inspected.
    /// Errors while reading an opened body remain unreadable regardless of I/O kind.
    Unreadable {
        /// Bounded filesystem diagnostic, containing no object body.
        message: String,
    },
}
/// One bounded streaming observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CasInspectionEvent {
    /// Source of this observation.
    pub location: CasInspectionLocation,
    /// Object/reference length, when a valid bounded value is available.
    pub byte_len: Option<u64>,
    /// Integrity issue, or a successful observation.
    pub issue: Option<CasInspectionIssue>,
}
/// Fatal inspection failure with all progress observed before it failed.
#[derive(Debug)]
pub struct CasInspectionError {
    /// Owning Store boundary's error.
    pub source: StoreError,
    /// Partial counts; the error means the requested inspection did not complete.
    pub partial: Box<CasInspectionSummary>,
}
impl std::fmt::Display for CasInspectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CAS inspection failed: {}", self.source)
    }
}
impl std::error::Error for CasInspectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
impl CasInspectionSummary {
    fn finish(mut self, result: Result<()>) -> std::result::Result<Self, CasInspectionError> {
        // Only the paired stop marker and private sentinel are a clean partial
        // result. A different failure must not be hidden by an earlier stop.
        if matches!(
            self.completion,
            CasInspectionCompletion::Cancelled | CasInspectionCompletion::VisitorStopped
        ) && matches!(result, Err(StoreError::ValidationBusy))
        {
            return Ok(self);
        }
        let source = match result {
            Ok(()) if self.completion == CasInspectionCompletion::Complete => return Ok(self),
            Ok(()) => StoreError::Corrupt("CAS inspection ended without its stop sentinel".into()),
            Err(source) => source,
        };
        self.completion = CasInspectionCompletion::Failed;
        Err(CasInspectionError {
            source,
            partial: Box::new(self),
        })
    }
}

impl SqliteStore {
    /// Inspects an existing offline Store without creating, cleaning or deleting paths.
    ///
    /// Holds the existing writer lease through every pass. Cancellation is checked
    /// per row, directory entry and hash chunk, not during a blocking syscall.
    /// Visitor stop and cancellation return partial summaries; fatal errors carry
    /// their partial summary even if a stop was requested. A stopped directory
    /// pass starts from the beginning on a new explicit invocation.
    /// This synchronous call blocks on database and filesystem I/O and the visitor.
    /// Async callers must dispatch it with `spawn_blocking`, cancel cooperatively
    /// on timeout, and await the worker before assuming its lease is released.
    pub fn inspect_cas(
        root: impl AsRef<Path>,
        mode: CasInspectionMode,
        cancellation: &CancellationToken,
        mut visitor: impl FnMut(CasInspectionEvent) -> ControlFlow<()>,
    ) -> std::result::Result<CasInspectionSummary, CasInspectionError> {
        let mut summary = CasInspectionSummary::default();
        if cancellation.is_cancelled() {
            summary.completion = CasInspectionCompletion::Cancelled;
            return Ok(summary);
        }
        let offline = filesystem::open_offline_store(root.as_ref()).map_err(|source| {
            summary.completion = CasInspectionCompletion::Failed;
            CasInspectionError {
                source,
                partial: Box::new(summary.clone()),
            }
        })?;
        let cas = offline.root.join("cas");
        let mut inspection = Inspection {
            connection: &offline.connection,
            cas: &cas,
            mode,
            cancellation,
            visitor: &mut visitor,
            summary,
            hash_buffer: Vec::new(),
        };
        let result = inspection.run();
        inspection.summary.finish(result)
    }
}

enum MetadataLength {
    Missing,
    Invalid,
    Valid(u64),
}

struct Inspection<'a, V> {
    connection: &'a Connection,
    cas: &'a Path,
    mode: CasInspectionMode,
    cancellation: &'a CancellationToken,
    visitor: &'a mut V,
    summary: CasInspectionSummary,
    hash_buffer: Vec<u8>,
}
impl<V: FnMut(CasInspectionEvent) -> ControlFlow<()>> Inspection<'_, V> {
    fn run(&mut self) -> Result<()> {
        self.registry()?;
        self.directory()?;
        if self.mode == CasInspectionMode::Full {
            validation::validate_database_observed(self.connection, self)?;
        }
        Ok(())
    }
    fn checkpoint(&mut self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            self.summary.completion = CasInspectionCompletion::Cancelled;
            // Private stop sentinel; inspect_cas translates it using completion,
            // so it never escapes as a Store admission refusal.
            return Err(StoreError::ValidationBusy);
        }
        Ok(())
    }
    fn emit(&mut self, event: CasInspectionEvent) -> Result<()> {
        self.checkpoint()?;
        if event.issue.is_some() {
            add(&mut self.summary.issues, 1)?;
        }
        if (self.visitor)(event).is_break() {
            self.summary.completion = CasInspectionCompletion::VisitorStopped;
            return Err(StoreError::ValidationBusy);
        }
        Ok(())
    }
    fn registry(&mut self) -> Result<()> {
        let columns = "SELECT rowid, typeof(sha256), octet_length(sha256),
                    CASE WHEN typeof(sha256) = 'text' AND octet_length(sha256) = 64 THEN sha256 END,
                    typeof(byte_len), CASE WHEN typeof(byte_len) = 'integer' THEN byte_len END
                 FROM cas_objects";
        // An unfiltered first page includes i64::MIN. Subsequent pages retain
        // the indexed rowid seek instead of adding an OR to the paging predicate.
        let first_page = format!("{columns} ORDER BY rowid LIMIT 256");
        let next_page = format!("{columns} WHERE rowid > ?1 ORDER BY rowid LIMIT 256");
        let mut cursor: Option<i64> = None;
        loop {
            self.checkpoint()?;
            let sql = if cursor.is_some() {
                &next_page
            } else {
                &first_page
            };
            let mut statement = self.connection.prepare_cached(sql).map_err(sql_error)?;
            let mut rows = if let Some(cursor) = cursor {
                statement.query([cursor])
            } else {
                statement.query([])
            }
            .map_err(sql_error)?;
            let mut seen = 0;
            while let Some(row) = rows.next().map_err(sql_error)? {
                self.checkpoint()?;
                let rowid: i64 = row.get(0).map_err(sql_error)?;
                let key_bytes = row
                    .get::<_, Option<i64>>(2)
                    .map_err(sql_error)?
                    .and_then(|value| u64::try_from(value).ok());
                let digest = match row.get_ref(3).map_err(sql_error)? {
                    rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                        .ok()
                        .filter(|value| validate_sha256("CAS identity", value).is_ok())
                        .map(str::to_owned),
                    _ => None,
                };
                let length = row
                    .get::<_, Option<i64>>(5)
                    .map_err(sql_error)?
                    .and_then(valid_length);
                add(&mut self.summary.metadata_rows, 1)?;
                let issue = if let (Some(digest), Some(length)) = (&digest, length) {
                    add(&mut self.summary.registered_bytes, length)?;
                    self.check_file(digest, length, self.mode == CasInspectionMode::Full)?
                } else {
                    Some(CasInspectionIssue::InvalidMetadata)
                };
                self.emit(CasInspectionEvent {
                    location: CasInspectionLocation::Metadata {
                        rowid,
                        sha256: digest,
                        key_bytes,
                    },
                    byte_len: length,
                    issue,
                })?;
                cursor = Some(rowid);
                seen += 1;
            }
            if seen == 0 {
                return Ok(());
            }
        }
    }
    fn check_file(
        &mut self,
        digest: &str,
        length: u64,
        hash: bool,
    ) -> Result<Option<CasInspectionIssue>> {
        let (mut file, metadata) = match cas::open_cas_file(&self.cas.join(digest)) {
            Ok(opened) => opened,
            Err(error) => return Ok(Some(file_issue(&error))),
        };
        if metadata.len() != length {
            return Ok(Some(CasInspectionIssue::LengthMismatch {
                expected: length,
                actual: metadata.len(),
            }));
        }
        if !hash {
            return Ok(None);
        }
        self.hash_reader(&mut file, digest, length)
    }
    fn hash_reader(
        &mut self,
        reader: &mut impl std::io::Read,
        digest: &str,
        length: u64,
    ) -> Result<Option<CasInspectionIssue>> {
        let mut remaining = length;
        let mut hasher = Sha256::new();
        // Allocate once on the first hashed object; Metadata mode needs no buffer.
        self.hash_buffer.resize(64 * 1024, 0);
        while remaining > 0 {
            self.checkpoint()?;
            let requested = usize::try_from(remaining.min(self.hash_buffer.len() as u64))
                .expect("bounded by buffer size");
            let read = match reader.read(&mut self.hash_buffer[..requested]) {
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Ok(Some(unreadable(&error))),
            };
            if read == 0 {
                return Ok(Some(CasInspectionIssue::LengthMismatch {
                    expected: length,
                    actual: length - remaining,
                }));
            }
            hasher.update(&self.hash_buffer[..read]);
            add(&mut self.summary.hashed_bytes, read as u64)?;
            remaining -= read as u64;
        }
        self.checkpoint()?;
        match super::filesystem::read_retry(reader, &mut self.hash_buffer[..1]) {
            Ok(0) => {}
            Ok(_) => {
                return Ok(Some(CasInspectionIssue::LengthMismatch {
                    expected: length,
                    actual: length + 1,
                }));
            }
            Err(error) => return Ok(Some(unreadable(&error))),
        }
        if hex::encode(hasher.finalize()) != digest {
            return Ok(Some(CasInspectionIssue::DigestMismatch));
        }
        add(&mut self.summary.verified_objects, 1)?;
        Ok(None)
    }
    fn directory(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let directory = rsi_files_native_fs::open_absolute_directory_no_follow(self.cas)
                .map_err(io_error)?;
            for entry in rsi_files_native_fs::directory_entries(&directory).map_err(io_error)? {
                self.checkpoint()?;
                let entry = entry.map_err(io_error)?;
                let kind = entry.file_type().map_err(io_error)?;
                self.entry(
                    &entry.file_name(),
                    kind.is_file(),
                    kind.is_dir(),
                    kind.is_symlink(),
                )?;
            }
        }
        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            let _parents = cas::pin_windows_directories(self.cas).map_err(io_error)?;
            let metadata = fs::symlink_metadata(self.cas).map_err(io_error)?;
            if !metadata.is_dir() || metadata.is_symlink() {
                return Err(StoreError::Corrupt(
                    "CAS root is not a real directory".into(),
                ));
            }
            for entry in fs::read_dir(self.cas).map_err(io_error)? {
                self.checkpoint()?;
                let entry = entry.map_err(io_error)?;
                let kind = entry.file_type().map_err(io_error)?;
                self.entry(
                    &entry.file_name(),
                    kind.is_file(),
                    kind.is_dir(),
                    kind.is_symlink(),
                )?;
            }
        }
        Ok(())
    }
    fn entry(
        &mut self,
        name: &std::ffi::OsStr,
        regular: bool,
        directory: bool,
        link: bool,
    ) -> Result<()> {
        add(&mut self.summary.directory_entries, 1)?;
        let raw = name.as_encoded_bytes();
        // Bound allocation before lossy conversion, then bound the encoded
        // preview again because replacement characters can expand invalid bytes.
        let mut preview = String::from_utf8_lossy(&raw[..raw.len().min(128)]).into_owned();
        if preview.len() > 128 {
            let mut end = 128;
            while !preview.is_char_boundary(end) {
                end -= 1;
            }
            preview.truncate(end);
        }
        let mut length = None;
        let issue = if directory && !link && name == "staging" {
            None
        } else if !regular || link {
            Some(CasInspectionIssue::UnexpectedEntry)
        } else if let Some(digest) = name
            .to_str()
            .filter(|digest| validate_sha256("CAS identity", digest).is_ok())
        {
            match self.metadata_length(digest)? {
                MetadataLength::Valid(_) | MetadataLength::Invalid => None,
                MetadataLength::Missing => match cas::open_cas_file(&self.cas.join(digest)) {
                    Ok((_, metadata)) => {
                        length = Some(metadata.len());
                        add(&mut self.summary.unregistered_files, 1)?;
                        add(&mut self.summary.unregistered_bytes, metadata.len())?;
                        Some(CasInspectionIssue::Unregistered)
                    }
                    Err(error) => Some(file_issue(&error)),
                },
            }
        } else {
            Some(CasInspectionIssue::UnexpectedEntry)
        };
        self.emit(CasInspectionEvent {
            location: CasInspectionLocation::File { name: preview },
            byte_len: length,
            issue,
        })
    }
    fn metadata_length(&self, digest: &str) -> Result<MetadataLength> {
        let mut statement = self.connection.prepare_cached(
            "SELECT CASE WHEN typeof(byte_len) = 'integer' THEN byte_len END FROM cas_objects WHERE sha256 = ?1",
        ).map_err(sql_error)?;
        let value = statement
            .query_row([digest], |row| row.get::<_, Option<i64>>(0))
            .optional()
            .map_err(sql_error)?;
        Ok(match value {
            None => MetadataLength::Missing,
            Some(value) => value
                .and_then(valid_length)
                .map_or(MetadataLength::Invalid, MetadataLength::Valid),
        })
    }
    fn reference(
        &mut self,
        session: &SessionId,
        stream: CasInspectionStream,
        sequence: u64,
        digest: &str,
        length: u64,
    ) -> Result<()> {
        self.checkpoint()?;
        add(&mut self.summary.references, 1)?;
        let issue = match self.metadata_length(digest)? {
            MetadataLength::Missing => Some(CasInspectionIssue::Missing),
            MetadataLength::Invalid => Some(CasInspectionIssue::InvalidMetadata),
            MetadataLength::Valid(actual) if actual != length => {
                Some(CasInspectionIssue::LengthMismatch {
                    expected: length,
                    actual,
                })
            }
            MetadataLength::Valid(_) => None,
        };
        self.emit(CasInspectionEvent {
            location: CasInspectionLocation::Reference {
                session_id: session.clone(),
                stream,
                sequence,
                sha256: digest.into(),
            },
            byte_len: Some(length),
            issue,
        })
    }
    fn content(
        &mut self,
        session: &SessionId,
        stream: CasInspectionStream,
        sequence: u64,
        content: &[rsi_agent_session_protocol::AgentMessageContent],
    ) -> Result<()> {
        for content in content {
            use rsi_agent_session_protocol::AgentMessageContent;
            match content {
                AgentMessageContent::Reference { reference } => self.reference(
                    session,
                    stream,
                    sequence,
                    &reference.snapshot.sha256,
                    reference.snapshot.byte_len,
                )?,
                AgentMessageContent::Text { .. } | AgentMessageContent::Image { .. } => {}
            }
        }
        Ok(())
    }
}
impl<V: FnMut(CasInspectionEvent) -> ControlFlow<()>> validation::CanonicalObserver
    for Inspection<'_, V>
{
    fn checkpoint(&mut self) -> Result<()> {
        self.checkpoint()
    }
    fn control(&mut self, session: &SessionId, record: &AgentControlRecord) -> Result<()> {
        use rsi_agent_session_protocol::ProgramRunEvent;
        match record.body() {
            AgentControlRecordBody::ProgramRun {
                event: ProgramRunEvent::Accepted { descriptor },
                ..
            } => self.reference(
                session,
                CasInspectionStream::Control,
                record.seq(),
                &descriptor.script.sha256,
                descriptor.script.bytes,
            ),
            AgentControlRecordBody::ProgramRun {
                event:
                    ProgramRunEvent::Terminal {
                        result: Some(result),
                        ..
                    },
                ..
            } => self.reference(
                session,
                CasInspectionStream::Control,
                record.seq(),
                &result.sha256,
                result.bytes,
            ),
            AgentControlRecordBody::MessageAccepted { message, .. } => self.content(
                session,
                CasInspectionStream::Control,
                record.seq(),
                &message.content,
            ),
            _ => Ok(()),
        }
    }
    fn fact(&mut self, session: &SessionId, fact: &SessionFact) -> Result<()> {
        if let SessionFactBody::InputMessageEntered { content, .. } = fact.body() {
            self.content(session, CasInspectionStream::Fact, fact.seq(), content)?;
        }
        Ok(())
    }
}
fn valid_length(value: i64) -> Option<u64> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0 && *value <= MAXIMUM_STORE_CAS_BYTES as u64)
}
fn add(count: &mut u64, value: u64) -> Result<()> {
    *count = count
        .checked_add(value)
        .ok_or_else(|| StoreError::Corrupt("CAS inspection count overflow".into()))?;
    Ok(())
}
fn file_issue(error: &std::io::Error) -> CasInspectionIssue {
    if error.kind() == std::io::ErrorKind::NotFound {
        CasInspectionIssue::Missing
    } else if error.kind() == std::io::ErrorKind::InvalidData {
        CasInspectionIssue::UnexpectedEntry
    } else {
        unreadable(error)
    }
}
fn unreadable(error: &std::io::Error) -> CasInspectionIssue {
    CasInspectionIssue::Unreadable {
        message: error.to_string().chars().take(256).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_inspection_does_not_hide_a_fatal_error() {
        for completion in [
            CasInspectionCompletion::Cancelled,
            CasInspectionCompletion::VisitorStopped,
        ] {
            let summary = CasInspectionSummary {
                completion,
                metadata_rows: 7,
                ..CasInspectionSummary::default()
            };
            let source = StoreError::Io("fatal after stop".into());
            let failure = summary.clone().finish(Err(source.clone())).unwrap_err();
            assert_eq!(failure.source, source);
            assert_eq!(failure.partial.completion, CasInspectionCompletion::Failed);
            assert_eq!(failure.partial.metadata_rows, 7);
            assert_eq!(
                summary
                    .clone()
                    .finish(Err(StoreError::ValidationBusy))
                    .unwrap(),
                summary
            );
            assert!(
                summary.finish(Ok(())).is_err(),
                "a stopped pass must return its sentinel"
            );
        }
        assert!(
            CasInspectionSummary::default()
                .finish(Err(StoreError::ValidationBusy))
                .is_err(),
            "a real refusal is fatal without a stop marker"
        );
        assert_eq!(
            CasInspectionSummary::default().finish(Ok(())).unwrap(),
            CasInspectionSummary::default()
        );
    }

    struct Cancelling {
        cancel: CancellationToken,
        calls: usize,
    }
    impl std::io::Read for Cancelling {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            assert_eq!(
                self.calls, 1,
                "cancelled inspection must not read another chunk"
            );
            buffer.fill(b'x');
            self.cancel.cancel();
            Ok(buffer.len())
        }
    }
    struct InvalidRead {
        remaining: usize,
    }
    impl std::io::Read for InvalidRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "body read failed",
                ));
            }
            let count = self.remaining.min(buffer.len());
            buffer[..count].fill(b'x');
            self.remaining -= count;
            Ok(count)
        }
    }
    #[test]
    fn hashing_enforces_exact_eof_and_cancellation_between_chunks() {
        let connection = Connection::open_in_memory().unwrap();
        let cancel = CancellationToken::new();
        let mut visitor = |_| ControlFlow::Continue(());
        let mut inspection = Inspection {
            connection: &connection,
            cas: Path::new("unused"),
            mode: CasInspectionMode::Full,
            cancellation: &cancel,
            visitor: &mut visitor,
            summary: CasInspectionSummary::default(),
            hash_buffer: Vec::new(),
        };
        let digest = hex::encode(Sha256::digest(b"exact"));
        for (bytes, actual) in [
            (b"exac".as_slice(), 4),
            (b"exact!".as_slice(), 6),
            (b"exact and much more".as_slice(), 6),
        ] {
            let issue = inspection
                .hash_reader(&mut std::io::Cursor::new(bytes), &digest, 5)
                .unwrap();
            assert_eq!(
                issue,
                Some(CasInspectionIssue::LengthMismatch {
                    expected: 5,
                    actual
                })
            );
        }
        for remaining in [0, 5] {
            assert_eq!(
                inspection
                    .hash_reader(&mut InvalidRead { remaining }, &digest, 5)
                    .unwrap(),
                Some(CasInspectionIssue::Unreadable {
                    message: "body read failed".into()
                }),
                "both body and EOF-probe errors are read failures"
            );
        }
        let mut source = Cancelling {
            cancel: cancel.clone(),
            calls: 0,
        };
        assert!(
            inspection
                .hash_reader(&mut source, &digest, 128 * 1024)
                .is_err()
        );
        assert_eq!(
            inspection.summary.completion,
            CasInspectionCompletion::Cancelled
        );
        assert_eq!(inspection.summary.hashed_bytes, 4 + 5 + 5 + 5 + 64 * 1024);
        assert_eq!(inspection.summary.verified_objects, 0);
    }
}
