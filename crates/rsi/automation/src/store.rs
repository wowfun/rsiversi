use crate::now;
use crate::{Attempt, AttemptState, AutomationRule, Deployment, Receipt};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const DAY: u64 = 86_400_000;
const MAX_ROWS: i64 = 16_384;
const MAX_METADATA: i64 = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    #[error("invalid automation input: {0}")]
    Invalid(String),
    #[error("automation permission denied")]
    Unauthorized,
    #[error("automation capacity reached")]
    Capacity,
    #[error("automation identity conflict")]
    Conflict,
    #[error("automation storage unavailable; execution is fenced")]
    Unavailable,
    #[error("automation commit outcome unknown; execution is fenced")]
    OutcomeUnknown,
    #[error("corrupt durable automation record")]
    Corrupt,
    #[error("automation record not found")]
    NotFound,
}
type Result<T> = std::result::Result<T, AdmissionError>;
fn read_source(c: &Connection, task: u64) -> Result<String> {
    let source: String = c
        .query_row(
            "SELECT substr(source,1,129) FROM tasks WHERE id=?1",
            [SqlU64(task)],
            |r| r.get(0),
        )
        .optional()
        .map_err(invalid)?
        .ok_or(AdmissionError::NotFound)?;
    crate::protocol::identity(&source).map_err(|_| AdmissionError::Corrupt)?;
    Ok(source)
}
fn invalid(error: impl std::fmt::Display) -> AdmissionError {
    AdmissionError::Invalid(error.to_string())
}
fn encoded<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(invalid)
}
fn decoded<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    if value.len() > 1024 * 1024 {
        return Err(AdmissionError::Corrupt);
    }
    serde_json::from_str(value).map_err(|_| AdmissionError::Corrupt)
}

/// Independently fenced authoritative `SQLite` owner. Blocking work retains its lease.
#[derive(Debug)]
pub struct Ledger {
    connection: Mutex<Connection>,
    work: tokio_util::task::TaskTracker,
    work_admission: Mutex<()>,
    work_lane: Arc<tokio::sync::Semaphore>,
    _lease: File,
    fenced: AtomicBool,
    #[cfg(test)]
    lose_commit_reply: AtomicBool,
}
type SourcedAttemptPage = (Vec<(Attempt, String)>, u64, bool);
impl Ledger {
    /// # Errors
    /// Fails when the selected resources cannot be validated, exclusively owned or made ready.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep one complete ownership operation or acceptance scenario together"
    )]
    pub fn open(directory: &Path, now_ms: u64) -> Result<Arc<Self>> {
        crate::protocol::timestamp(now_ms).map_err(invalid)?;
        let existed = directory.exists();
        let root =
            rsi_files_native_fs::create_absolute_directory_no_follow(directory).map_err(invalid)?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| AdmissionError::Invalid(e.to_string()))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(directory).map_err(invalid)?;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0
            {
                return Err(invalid("automation directory must be private"));
            }
        }
        let _root = root;
        for entry in std::fs::read_dir(directory).map_err(invalid)? {
            let entry = entry.map_err(invalid)?;
            if ![
                ".writer.lock",
                "automation.sqlite3",
                "automation.sqlite3-wal",
                "automation.sqlite3-shm",
            ]
            .iter()
            .any(|name| entry.file_name() == *name)
                || !entry.file_type().map_err(invalid)?.is_file()
            {
                return Err(invalid("automation directory contains unrelated files"));
            }
        }
        let lease = crate::private_file(&directory.join(".writer.lock"))?;
        lease.try_lock().map_err(|_| AdmissionError::Capacity)?;
        let database = crate::private_file(&directory.join("automation.sqlite3"))?;
        let connection = Connection::open(directory.join("automation.sqlite3")).map_err(invalid)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = database.metadata().map_err(invalid)?;
            let named = std::fs::metadata(directory.join("automation.sqlite3")).map_err(invalid)?;
            if opened.ino() != named.ino() || opened.dev() != named.dev() {
                return Err(invalid("database identity changed during open"));
            }
        }
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(invalid)?;
        let tables:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0)).map_err(invalid)?;
        if (version == 0 && tables != 0) || !matches!(version, 0 | 2) {
            return Err(invalid("unsupported Automation database format"));
        }
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=1000; PRAGMA max_page_count=270336; PRAGMA journal_size_limit=8388608;
          CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS tasks(id INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,repo INTEGER NOT NULL,deployment INTEGER NOT NULL,rule TEXT NOT NULL,environment TEXT NOT NULL,url TEXT NOT NULL,created INTEGER NOT NULL,UNIQUE(source,repo,deployment,rule));
          CREATE TABLE IF NOT EXISTS attempts(id INTEGER PRIMARY KEY AUTOINCREMENT,task INTEGER NOT NULL REFERENCES tasks(id),created INTEGER NOT NULL,state TEXT NOT NULL,data TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS attempts_queue ON attempts(state,id);
          CREATE INDEX IF NOT EXISTS attempts_task ON attempts(task,id);
          CREATE TABLE IF NOT EXISTS receipts(source TEXT NOT NULL,delivery TEXT NOT NULL,digest TEXT NOT NULL,task INTEGER NOT NULL,attempt INTEGER NOT NULL,created INTEGER NOT NULL,rule TEXT NOT NULL,PRIMARY KEY(source,delivery,rule));
          CREATE INDEX IF NOT EXISTS receipts_body ON receipts(source,digest,rule);
          CREATE TABLE IF NOT EXISTS mutations(request TEXT PRIMARY KEY,digest TEXT NOT NULL,attempt INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS artifacts(attempt INTEGER NOT NULL REFERENCES attempts(id),ordinal INTEGER NOT NULL,created INTEGER NOT NULL,png BLOB NOT NULL,PRIMARY KEY(attempt,ordinal));
          INSERT OR IGNORE INTO settings VALUES('floor',0); PRAGMA user_version=2;").map_err(invalid)?;
        let ledger = Arc::new(Self {
            connection: Mutex::new(connection),
            work: tokio_util::task::TaskTracker::new(),
            work_admission: Mutex::new(()),
            work_lane: Arc::new(tokio::sync::Semaphore::new(1)),
            _lease: lease,
            fenced: AtomicBool::new(false),
            #[cfg(test)]
            lose_commit_reply: AtomicBool::new(false),
        });
        ledger.transact(|tx| {
            let existing_floor: u64 = tx
                .query_row("SELECT value FROM settings WHERE key='floor'", [], |row| {
                    row_number(row, 0)
                })
                .map_err(invalid)?;
            crate::protocol::timestamp(existing_floor.max(1))
                .map_err(|_| AdmissionError::Corrupt)?;
            initialize_accounting(tx)?;
            let floor = now_ms.saturating_sub(7 * DAY);
            tx.execute(
                "UPDATE settings SET value=max(value,?1) WHERE key='floor'",
                [SqlU64(floor)],
            )
            .map_err(invalid)?;
            let mut cursor = 0;
            loop {
                let mut statement = tx.prepare_cached("SELECT id,substr(data,1,1048577) FROM attempts WHERE id>?1 ORDER BY id LIMIT 16").map_err(invalid)?;
                let rows = statement.query_map([SqlU64(cursor)], |row| Ok((row_number(row, 0)?, row.get::<_, String>(1)?))).map_err(invalid)?.collect::<std::result::Result<Vec<_>, _>>().map_err(invalid)?;
                if rows.is_empty() { break; }
                drop(statement);
                for (id, data) in rows {
                cursor = id;
                let mut a: Attempt = decoded(&data)?;
                if a.id != id { return Err(AdmissionError::Corrupt); }
                validate_attempt(&a).map_err(|_| AdmissionError::Corrupt)?;
                let mut changed = false;
                if matches!(a.state, AttemptState::Queued | AttemptState::Running) {
                    a.state = AttemptState::Interrupted;
                    changed = true;
                }
                if matches!(
                    a.exploration,
                    crate::ExplorationState::Starting | crate::ExplorationState::Running
                ) {
                    a.exploration = crate::ExplorationState::Interrupted;
                    changed = true;
                }
                if changed {
                    save(tx, &a)?;
                }
                }
            }
            Ok(())
        })?;
        Ok(ledger)
    }
    pub(crate) fn fence(&self) {
        self.fenced.store(true, Ordering::Release);
    }
    pub fn available(&self) -> bool {
        !self.fenced.load(Ordering::Acquire)
    }
    pub(crate) async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self
            .work_lane
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AdmissionError::Unavailable)?;
        let job = {
            let _admission = self
                .work_admission
                .lock()
                .map_err(|_| AdmissionError::Unavailable)?;
            if self.work.is_closed() {
                return Err(AdmissionError::Unavailable);
            }
            let owner = self.clone();
            self.work.spawn_blocking(move || {
                let _permit = permit;
                operation(&owner)
            })
        };
        job.await.map_err(|_| {
            self.fenced.store(true, Ordering::Release);
            AdmissionError::OutcomeUnknown
        })?
    }
    pub(crate) async fn drain(&self) {
        {
            let _admission = self
                .work_admission
                .lock()
                .expect("Automation work admission");
            self.work_lane.close();
            self.work.close();
        }
        self.work.wait().await;
    }
    fn transact<T>(&self, work: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T> {
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        self.transact_locked(&mut connection, work)
    }
    fn transact_locked<T>(
        &self,
        connection: &mut Connection,
        work: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(invalid)?;
        let value = match work(&tx) {
            Ok(value) => value,
            Err(error) => {
                if matches!(error, AdmissionError::Corrupt) {
                    self.fenced.store(true, Ordering::Release);
                }
                if tx.rollback().is_err() {
                    self.fenced.store(true, Ordering::Release);
                    return Err(AdmissionError::OutcomeUnknown);
                }
                return Err(error);
            }
        };
        if tx.commit().is_err() {
            self.fenced.store(true, Ordering::Release);
            return Err(AdmissionError::OutcomeUnknown);
        }
        #[cfg(test)]
        if self.lose_commit_reply.swap(false, Ordering::AcqRel) {
            self.fenced.store(true, Ordering::Release);
            return Err(AdmissionError::OutcomeUnknown);
        }
        let checkpoint = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            r.get::<_, i64>(0)
        });
        if !matches!(checkpoint, Ok(0)) {
            self.fenced.store(true, Ordering::Release);
            return Err(AdmissionError::OutcomeUnknown);
        }
        Ok(value)
    }
    /// Admission runs to settlement even when its asynchronous waiter is dropped.
    /// # Errors
    /// Rejects invalid metadata, identity conflicts, capacity or failed and uncertain durable admission.
    pub async fn admit(
        self: &Arc<Self>,
        source: String,
        delivery: String,
        body_digest: String,
        rule: AutomationRule,
        deployment: Deployment,
        now_ms: u64,
    ) -> Result<Receipt> {
        self.admit_rules(
            source,
            delivery,
            body_digest,
            vec![rule],
            deployment,
            now_ms,
        )
        .await?
        .pop()
        .ok_or(AdmissionError::Corrupt)
    }
    /// Atomically admits all matching frozen rules, preserving the acceptance floor on rejection.
    /// # Errors
    /// Rejects invalid metadata, capacity, identity conflicts or unavailable durable storage.
    pub async fn admit_rules(
        self: &Arc<Self>,
        source: String,
        delivery: String,
        body_digest: String,
        rules: Vec<AutomationRule>,
        deployment: Deployment,
        now_ms: u64,
    ) -> Result<Vec<Receipt>> {
        crate::protocol::identity(&source).map_err(invalid)?;
        crate::protocol::identity(&delivery).map_err(invalid)?;
        crate::protocol::digest(&body_digest).map_err(invalid)?;
        crate::protocol::timestamp(now_ms).map_err(invalid)?;
        deployment.validate().map_err(invalid)?;
        if rules.len() > crate::protocol::MAXIMUM_RULES {
            return Err(AdmissionError::Capacity);
        }
        let mut identities = std::collections::BTreeSet::new();
        for rule in &rules {
            rule.validate().map_err(invalid)?;
            rule.policy(&deployment.url).map_err(invalid)?;
            if !rule.enabled
                || rule.repository_id != deployment.repository_id
                || rule.environment != deployment.environment
                || !identities.insert(&rule.id)
            {
                return Err(invalid("event does not match distinct live rules"));
            }
        }
        self.run(move |owner| {
            let outcome = owner.transact(|tx| {
                tx.execute(
                    "UPDATE settings SET value=max(value,?1) WHERE key='floor'",
                    [SqlU64(now_ms.saturating_sub(7 * DAY))],
                )
                .map_err(invalid)?;
                tx.execute_batch("SAVEPOINT admission").map_err(invalid)?;
                let admitted = rules
                    .into_iter()
                    .map(|rule| {
                        admit_candidate(
                            tx,
                            &source,
                            delivery.clone(),
                            &body_digest,
                            rule,
                            deployment.clone(),
                            now_ms,
                        )
                    })
                    .collect::<Result<Vec<_>>>();
                if admitted.is_err() {
                    tx.execute_batch("ROLLBACK TO admission").map_err(invalid)?;
                }
                tx.execute_batch("RELEASE admission").map_err(invalid)?;
                Ok(admitted)
            })?;
            if matches!(outcome, Err(AdmissionError::Corrupt)) {
                owner.fence();
            }
            outcome
        })
        .await
    }
    /// # Errors
    /// Fails when the task is absent or authoritative storage cannot be read.
    pub fn source(&self, task: u64) -> Result<String> {
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let c = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        self.durable(read_source(&c, task))
    }
    /// # Errors
    /// Fails when the attempt is absent or its durable state cannot be validated.
    pub fn get(&self, id: u64) -> Result<Attempt> {
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let c = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        self.durable(read(&c, id))
    }
    pub(crate) fn get_with_source(&self, id: u64) -> Result<(Attempt, String)> {
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let c = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        self.durable((|| {
            let attempt = read(&c, id)?;
            let source = read_source(&c, attempt.task_id)?;
            Ok((attempt, source))
        })())
    }
    fn durable<T>(&self, result: Result<T>) -> Result<T> {
        if matches!(&result, Err(AdmissionError::Corrupt)) {
            self.fenced.store(true, Ordering::Release);
            return Err(AdmissionError::Unavailable);
        }
        result
    }
    /// # Errors
    /// Rejects invalid page limits or unreadable and corrupt durable attempts.
    pub fn list(
        &self,
        after: u64,
        watermark: Option<u64>,
        limit: usize,
    ) -> Result<(Vec<Attempt>, u64, bool)> {
        let (rows, watermark, more) = self.list_with_sources(after, watermark, limit)?;
        Ok((
            rows.into_iter().map(|(attempt, _)| attempt).collect(),
            watermark,
            more,
        ))
    }
    pub(crate) fn list_with_sources(
        &self,
        after: u64,
        watermark: Option<u64>,
        limit: usize,
    ) -> Result<SourcedAttemptPage> {
        if limit == 0 || limit > 50 {
            return Err(invalid("page limit must be 1..50"));
        }
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let c = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        let watermark = match watermark {
            Some(value) => value,
            None => c
                .query_row("SELECT coalesce(max(id),0) FROM attempts", [], |r| {
                    row_number(r, 0)
                })
                .map_err(invalid)?,
        };
        let mut statement = c.prepare("SELECT substr(a.data,1,1048577),substr(t.source,1,129) FROM attempts a JOIN tasks t ON t.id=a.task WHERE a.id>?1 AND a.id<=?2 ORDER BY a.id LIMIT ?3").map_err(invalid)?;
        let rows = statement
            .query_map(
                params![SqlU64(after), SqlU64(watermark), SqlU64((limit + 1) as u64)],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(invalid)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(invalid)?;
        let more = rows.len() > limit;
        let attempts = self.durable(
            rows.into_iter()
                .take(limit)
                .map(|(data, source)| {
                    let attempt = decoded(&data)?;
                    validate_attempt(&attempt).map_err(|_| AdmissionError::Corrupt)?;
                    crate::protocol::identity(&source).map_err(|_| AdmissionError::Corrupt)?;
                    Ok((attempt, source))
                })
                .collect::<Result<Vec<_>>>(),
        )?;
        Ok((attempts, watermark, more))
    }
    /// # Errors
    /// Rejects invalid time or failed and uncertain durable claim transitions.
    pub fn claim(&self, now_ms: u64) -> Result<Option<Attempt>> {
        crate::protocol::timestamp(now_ms).map_err(invalid)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let ready: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE state='queued') AND (SELECT count(*) FROM attempts WHERE state='running') < 2",[],|r|r.get(0)).map_err(invalid)?;
        if !ready {
            return Ok(None);
        }
        self.transact_locked(&mut connection, |tx| {
            loop {
                let data: Option<String> = tx.query_row("SELECT substr(data,1,1048577) FROM attempts WHERE state='queued' ORDER BY id LIMIT 1", [], |r| r.get(0)).optional().map_err(invalid)?;
                let Some(data) = data else { return Ok(None); };
                let mut a: Attempt = decoded(&data)?;
                if a.created_ms.saturating_add(30 * 60 * 1000) < now_ms {
                    a.state = AttemptState::Interrupted;
                    save(tx, &a)?;
                    continue;
                }
                capacity(tx)?;
                a.state = AttemptState::Running;
                save(tx, &a)?;
                return Ok(Some(a));
            }
        })
    }
    /// # Errors
    /// Rejects invalid verdicts and PNGs, capacity, stale state or failed durable settlement.
    pub fn settle(
        &self,
        id: u64,
        result: rsi_browser::CheckResult,
        artifacts: Vec<Vec<u8>>,
        now_ms: u64,
    ) -> Result<Attempt> {
        crate::protocol::timestamp(now_ms).map_err(invalid)?;
        result.validate().map_err(invalid)?;
        if artifacts.len() > 4
            || (result.outcome == rsi_browser::CheckOutcome::PolicyBlocked && !artifacts.is_empty())
        {
            return Err(invalid("invalid bounded screenshot evidence"));
        }
        let artifacts = artifacts
            .into_iter()
            .map(|png| {
                if png.len() > 512 * 1024 {
                    return Err(invalid("screenshot exceeds byte bound"));
                }
                rsi_media::normalize_artifact_png(png.into(), 1280 * 720, 512 * 1024)
                    .map(|canonical| canonical.to_vec())
                    .map_err(invalid)
            })
            .collect::<Result<Vec<_>>>()?;
        self.transact(|tx| {
            let mut a = read(tx, id)?;
            if a.state != AttemptState::Running {
                return Err(AdmissionError::Conflict);
            }
            let total: i64 = tx
                .query_row(
                    "SELECT coalesce(sum(length(png)),0) FROM artifacts",
                    [],
                    |r| r.get(0),
                )
                .map_err(invalid)?;
            if total
                + artifacts
                    .iter()
                    .map(|a| i64::try_from(a.len()).unwrap_or(i64::MAX))
                    .sum::<i64>()
                > 1024 * 1024 * 1024
            {
                return Err(AdmissionError::Capacity);
            }
            a.state = match result.outcome {
                rsi_browser::CheckOutcome::Pass => AttemptState::Passed,
                rsi_browser::CheckOutcome::AssertionFailed => AttemptState::Failed,
                rsi_browser::CheckOutcome::Cancelled => AttemptState::Cancelled,
                _ => AttemptState::Unavailable,
            };
            a.result = Some(result);
            save(tx, &a)?;
            for (ordinal, png) in artifacts.iter().enumerate() {
                tx.execute(
                    "INSERT INTO artifacts VALUES(?1,?2,?3,?4)",
                    params![SqlU64(id), SqlU64(ordinal as u64), SqlU64(now_ms), png],
                )
                .map_err(invalid)?;
            }
            Ok(a)
        })
    }
    /// Settles execution failure once, preserving an already recorded verdict.
    /// # Errors
    /// Fails if authoritative state cannot be read or the terminal transition cannot commit.
    pub fn fail_execution(&self, id: u64, cancelled: bool, error: &str) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let mut a = self.durable(read(&connection, id))?;
        let mut changed = false;
        if a.state == AttemptState::Running {
            a.state = if cancelled {
                AttemptState::Cancelled
            } else {
                AttemptState::Unavailable
            };
            a.result = Some(rsi_browser::CheckResult {
                outcome: if cancelled {
                    rsi_browser::CheckOutcome::Cancelled
                } else {
                    rsi_browser::CheckOutcome::InfrastructureFailed
                },
                final_url: a.deployment.url.clone(),
                assertions: vec![],
                snapshot: String::new(),
                dialogs_dismissed: 0,
                evidence_error: Some(error.chars().take(256).collect()),
            });
            changed = true;
        }
        if matches!(a.state, AttemptState::Failed | AttemptState::Cancelled)
            && !matches!(
                a.exploration,
                crate::ExplorationState::Complete
                    | crate::ExplorationState::Failed
                    | crate::ExplorationState::Cancelled
            )
        {
            a.exploration = if cancelled {
                crate::ExplorationState::Cancelled
            } else {
                crate::ExplorationState::Failed
            };
            if a.report.is_none() {
                a.report = Some(error.chars().take(1024).collect());
            }
            changed = true;
        }
        if !changed {
            return Ok(());
        }
        self.transact_locked(&mut connection, |tx| save(tx, &a))
    }
    /// # Errors
    /// Rejects invalid identities, conflicting state or failed durable report publication.
    pub fn record_exploration(
        &self,
        id: u64,
        session_id: String,
        report: Option<String>,
    ) -> Result<()> {
        crate::protocol::identity(&session_id).map_err(invalid)?;
        if report.as_ref().is_some_and(|s| s.len() > 64 * 1024) {
            return Err(invalid("exploration report too large"));
        }
        self.transact(|tx| {
            let mut a = read(tx, id)?;
            if a.state != AttemptState::Failed
                || a.exploration == crate::ExplorationState::Cancelled
            {
                return Err(AdmissionError::Conflict);
            }
            if a.session_id.as_ref().is_some_and(|old| old != &session_id) {
                return Err(AdmissionError::Conflict);
            }
            a.session_id = Some(session_id);
            a.exploration = if report.is_some() {
                crate::ExplorationState::Complete
            } else {
                crate::ExplorationState::Starting
            };
            a.report = report;
            save(tx, &a)
        })
    }
    /// # Errors
    /// Rejects oversized reports, conflicting state or failed durable transitions.
    pub fn exploration_state(
        &self,
        id: u64,
        state: crate::ExplorationState,
        report: Option<String>,
    ) -> Result<()> {
        if report.as_ref().is_some_and(|s| s.len() > 65536) {
            return Err(AdmissionError::Capacity);
        }
        self.transact(|tx| {
            let mut a = read(tx, id)?;
            if !matches!(a.state, AttemptState::Failed | AttemptState::Cancelled) {
                return Err(AdmissionError::Conflict);
            }
            if a.exploration == crate::ExplorationState::Cancelled
                && state != crate::ExplorationState::Cancelled
            {
                return Err(AdmissionError::Conflict);
            }
            a.exploration = state;
            if report.is_some() && a.report.is_none() {
                a.report = report;
            }
            save(tx, &a)
        })
    }
    /// # Errors
    /// Fails if the authoritative attempt and newer-deployment status cannot be read.
    pub fn eligible(&self, id: u64) -> Result<bool> {
        let a = self.get(id)?;
        if a.state != AttemptState::Failed
            || !a.rule.explore_on_failure
            || a.session_id.is_some()
            || a.exploration != crate::ExplorationState::NotStarted
        {
            return Ok(false);
        }
        let c = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?;
        let newer:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM tasks other JOIN tasks own ON own.id=?1 WHERE other.source=own.source AND other.rule=own.rule AND other.environment=own.environment AND other.created>own.created)",[SqlU64(a.task_id)],|r|r.get(0)).map_err(invalid)?;
        Ok(!newer)
    }
    /// # Errors
    /// Rejects identity conflicts, capacity or failed and uncertain durable cancellation.
    pub fn cancel(&self, id: u64, request: &str) -> Result<Attempt> {
        crate::protocol::identity(request).map_err(invalid)?;
        self.transact(|tx| {
            let mut a = read(tx, id)?;
            let fingerprint = hex::encode(Sha256::digest(format!("cancel:{id}")));
            if let Some((digest, prior)) = mutation(tx, request)? {
                if digest != fingerprint {
                    return Err(AdmissionError::Conflict);
                }
                return read(tx, prior);
            }
            if matches!(a.state, AttemptState::Queued | AttemptState::Running) {
                a.state = AttemptState::Cancelled;
                save(tx, &a)?;
            } else if a.work_pending() {
                a.exploration = crate::ExplorationState::Cancelled;
                save(tx, &a)?;
            }
            tx.execute(
                "INSERT INTO mutations VALUES(?1,?2,?3)",
                params![request, fingerprint, SqlU64(id)],
            )
            .map_err(invalid)?;
            if accounting_total(tx, "metadata_rows")? > MAX_ROWS
                || accounting_total(tx, "metadata_bytes")? > MAX_METADATA
            {
                return Err(AdmissionError::Capacity);
            }
            Ok(a)
        })
    }
    /// # Errors
    /// Rejects invalid or stale rules, conflicting identities, capacity or failed durable admission.
    pub fn resume(&self, id: u64, request: &str, rule: AutomationRule) -> Result<Attempt> {
        crate::protocol::identity(request).map_err(invalid)?;
        rule.validate().map_err(invalid)?;
        if !rule.enabled {
            return Err(invalid("rule disabled"));
        }
        self.transact(|tx| {
            let old = read(tx, id)?;
            if old.rule.id != rule.id {
                return Err(AdmissionError::Conflict);
            }
            let fingerprint =
                hex::encode(Sha256::digest(format!("resume:{id}:{}", encoded(&rule)?)));
            if let Some((digest, prior)) = mutation(tx, request)? {
                if digest != fingerprint {
                    return Err(AdmissionError::Conflict);
                }
                return read(tx, prior);
            }
            if old.work_pending() {
                return Err(AdmissionError::Conflict);
            }
            rule.policy(&old.deployment.url).map_err(invalid)?;
            capacity(tx)?;
            let queued: u64 = tx
                .query_row(
                    "SELECT count(*) FROM attempts WHERE state='queued'",
                    [],
                    |r| row_number(r, 0),
                )
                .map_err(invalid)?;
            if queued >= 32 {
                return Err(AdmissionError::Capacity);
            }
            let mut a = Attempt {
                id: 0,
                task_id: old.task_id,
                rule,
                deployment: old.deployment,
                state: AttemptState::Queued,
                created_ms: now(),
                result: None,
                session_id: None,
                report: None,
                exploration: crate::ExplorationState::NotStarted,
            };
            tx.execute(
                "INSERT INTO attempts(task,created,state,data) VALUES(?1,?2,'queued','{}')",
                params![SqlU64(a.task_id), SqlU64(a.created_ms)],
            )
            .map_err(invalid)?;
            a.id = u64::try_from(tx.last_insert_rowid()).map_err(invalid)?;
            save(tx, &a)?;
            tx.execute(
                "INSERT INTO mutations VALUES(?1,?2,?3)",
                params![request, fingerprint, SqlU64(a.id)],
            )
            .map_err(invalid)?;
            Ok(a)
        })
    }
    /// # Errors
    /// Rejects absent artifacts and corrupt or oversized durable PNG evidence.
    pub fn artifact(&self, id: u64, ordinal: u8) -> Result<Vec<u8>> {
        if !self.available() {
            return Err(AdmissionError::Unavailable);
        }
        let png: Vec<u8> = self
            .connection
            .lock()
            .map_err(|_| AdmissionError::Unavailable)?
            .query_row(
                "SELECT substr(png,1,524289) FROM artifacts WHERE attempt=?1 AND ordinal=?2",
                params![SqlU64(id), SqlU64(u64::from(ordinal))],
                |r| r.get(0),
            )
            .optional()
            .map_err(invalid)?
            .ok_or(AdmissionError::NotFound)?;
        let valid = png.len() <= 512 * 1024
            && rsi_media::normalize_artifact_png(png.clone().into(), 1280 * 720, 512 * 1024)
                .is_ok_and(|canonical| canonical.as_ref() == png.as_slice());
        if !valid {
            self.fenced.store(true, Ordering::Release);
            return Err(AdmissionError::Corrupt);
        }
        Ok(png)
    }
    /// # Errors
    /// Rejects invalid time or failed and uncertain durable retention.
    pub fn retain(&self, now_ms: u64) -> Result<()> {
        crate::protocol::timestamp(now_ms).map_err(invalid)?;
        self.transact(|tx| {
            expire_queued(tx, now_ms)?;
            tx.execute("UPDATE settings SET value=max(value,?1) WHERE key='floor'",[SqlU64(now_ms.saturating_sub(7*DAY))]).map_err(invalid)?;
            tx.execute("DELETE FROM artifacts WHERE created<?1",[SqlU64(now_ms.saturating_sub(7*DAY))]).map_err(invalid)?;
            tx.execute("DELETE FROM receipts WHERE created<?1 AND task IN (SELECT id FROM tasks WHERE created<(SELECT value FROM settings WHERE key='floor'))",[SqlU64(now_ms.saturating_sub(30*DAY))]).map_err(invalid)?;
            tx.execute("DELETE FROM mutations WHERE attempt IN (SELECT id FROM attempts WHERE created<?1 AND state NOT IN ('queued','running'))",[SqlU64(now_ms.saturating_sub(30*DAY))]).map_err(invalid)?;
            tx.execute("DELETE FROM artifacts WHERE attempt IN (SELECT id FROM attempts WHERE created<?1 AND state NOT IN ('queued','running'))",[SqlU64(now_ms.saturating_sub(30*DAY))]).map_err(invalid)?;
            tx.execute("DELETE FROM attempts WHERE created<?1 AND state NOT IN ('queued','running') AND task NOT IN (SELECT task FROM receipts)",[SqlU64(now_ms.saturating_sub(30*DAY))]).map_err(invalid)?;
            tx.execute("DELETE FROM tasks WHERE id NOT IN (SELECT task FROM attempts)",[]).map_err(invalid)?;
            Ok(())
        })
    }
}
fn expire_queued(tx: &Connection, now_ms: u64) -> Result<()> {
    let rows = collect(
        tx,
        "SELECT substr(data,1,1048577) FROM attempts WHERE state='queued' AND created<?1",
        [SqlU64(now_ms.saturating_sub(30 * 60 * 1000))],
    )?;
    for data in rows {
        let mut attempt: Attempt = decoded(&data)?;
        validate_attempt(&attempt).map_err(|_| AdmissionError::Corrupt)?;
        attempt.state = AttemptState::Interrupted;
        save(tx, &attempt)?;
    }
    Ok(())
}
#[expect(
    clippy::too_many_lines,
    reason = "Keep one complete ownership operation or acceptance scenario together"
)]
fn admit_candidate(
    tx: &Connection,
    source: &str,
    delivery: String,
    body_digest: &str,
    rule: AutomationRule,
    deployment: Deployment,
    now_ms: u64,
) -> Result<Receipt> {
    let floor: u64 = tx
        .query_row("SELECT value FROM settings WHERE key='floor'", [], |r| {
            row_number(r, 0)
        })
        .map_err(invalid)?;
    if deployment.deployment_created_ms < floor
        || deployment.status_created_ms < floor
        || deployment.deployment_created_ms > now_ms.saturating_add(300_000)
        || deployment.status_created_ms > now_ms.saturating_add(300_000)
    {
        return Err(invalid("event outside acceptance window"));
    }
    let previous_digest: Option<String> = tx
        .query_row(
            "SELECT digest FROM receipts WHERE source=?1 AND delivery=?2 LIMIT 1",
            params![source, delivery],
            |r| r.get(0),
        )
        .optional()
        .map_err(invalid)?;
    if previous_digest.is_some_and(|digest| digest != body_digest) {
        return Err(AdmissionError::Conflict);
    }
    let duplicate: Option<(String, u64, u64)> = tx
        .query_row(
            "SELECT digest,task,attempt FROM receipts WHERE source=?1 AND delivery=?2 AND rule=?3",
            params![source, delivery, rule.id],
            |r| Ok((r.get(0)?, row_number(r, 1)?, row_number(r, 2)?)),
        )
        .optional()
        .map_err(invalid)?;
    if let Some((digest, task_id, attempt_id)) = duplicate {
        if digest != body_digest {
            return Err(AdmissionError::Conflict);
        }
        return Ok(Receipt {
            delivery,
            task_id,
            attempt_id,
            duplicate: true,
        });
    }
    let same_body: Option<(u64, u64)> = tx.query_row(
        "SELECT r.task,r.attempt FROM receipts r JOIN tasks t ON t.id=r.task WHERE r.source=?1 AND r.digest=?2 AND r.rule=?3 AND t.repo=?4 AND t.deployment=?5 LIMIT 1",
        params![source, body_digest, rule.id, SqlU64(deployment.repository_id), SqlU64(deployment.deployment_id)],
        |r| Ok((row_number(r, 0)?, row_number(r, 1)?)),
    ).optional().map_err(invalid)?;
    if let Some((task_id, attempt_id)) = same_body {
        let original = read(tx, attempt_id)?;
        if original.deployment != deployment {
            return Err(AdmissionError::Conflict);
        }
        return Ok(Receipt {
            delivery,
            task_id,
            attempt_id,
            duplicate: true,
        });
    }
    capacity(tx)?;
    let existing: Option<(u64,u64)> = tx.query_row("SELECT t.id,(SELECT id FROM attempts a WHERE a.task=t.id ORDER BY id LIMIT 1) FROM tasks t WHERE source=?1 AND repo=?2 AND deployment=?3 AND rule=?4",params![source,SqlU64(deployment.repository_id),SqlU64(deployment.deployment_id),rule.id], |r|Ok((row_number(r,0)?,row_number(r,1)?))).optional().map_err(invalid)?;
    let rule_id = rule.id.clone();
    let (task_id, attempt_id) = if let Some(ids) = existing {
        let original = read(tx, ids.1)?;
        if original.deployment.sha != deployment.sha
            || original.deployment.url != deployment.url
            || original.deployment.deployment_created_ms != deployment.deployment_created_ms
            || original.deployment.environment != deployment.environment
        {
            return Err(AdmissionError::Conflict);
        }
        ids
    } else {
        let reused: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE source=?1 AND repo=?2 AND url=?3 AND deployment<>?4)",params![source,SqlU64(deployment.repository_id),deployment.url,SqlU64(deployment.deployment_id)], |r|r.get(0)).map_err(invalid)?;
        if reused {
            return Err(invalid("preview URL reused by a different deployment"));
        }
        let newest: Option<u64> = tx
            .query_row(
                "SELECT max(created) FROM tasks WHERE source=?1 AND rule=?2 AND environment=?3",
                params![source, rule.id, rule.environment],
                |r| row_optional_number(r, 0),
            )
            .map_err(invalid)?;
        let old = newest.is_some_and(|value| deployment.deployment_created_ms < value);
        if !old {
            let mut statement = tx.prepare("SELECT substr(a.data,1,1048577) FROM attempts a JOIN tasks t ON a.task=t.id WHERE a.state='queued' AND t.source=?1 AND t.rule=?2 AND t.environment=?3 AND t.created<?4").map_err(invalid)?;
            let rows = statement
                .query_map(
                    params![
                        source,
                        rule.id,
                        rule.environment,
                        SqlU64(deployment.deployment_created_ms)
                    ],
                    |r| r.get::<_, String>(0),
                )
                .map_err(invalid)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(invalid)?;
            drop(statement);
            for data in rows {
                let mut a: Attempt = decoded(&data)?;
                a.state = AttemptState::Superseded;
                save(tx, &a)?;
            }
        }
        tx.execute("INSERT INTO tasks(source,repo,deployment,rule,environment,url,created) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![source,SqlU64(deployment.repository_id),SqlU64(deployment.deployment_id),rule.id,rule.environment,deployment.url,SqlU64(deployment.deployment_created_ms)]).map_err(invalid)?;
        let task_id = u64::try_from(tx.last_insert_rowid()).map_err(invalid)?;
        let queued: u64 = tx
            .query_row(
                "SELECT count(*) FROM attempts WHERE state='queued'",
                [],
                |r| row_number(r, 0),
            )
            .map_err(invalid)?;
        let state = if old {
            AttemptState::Superseded
        } else if queued >= 32 {
            AttemptState::CapacityRejected
        } else {
            AttemptState::Queued
        };
        let mut a = Attempt {
            id: 0,
            task_id,
            rule,
            deployment,
            state,
            created_ms: now_ms,
            result: None,
            session_id: None,
            report: None,
            exploration: crate::ExplorationState::NotStarted,
        };
        tx.execute(
            "INSERT INTO attempts(task,created,state,data) VALUES(?1,?2,?3,'{}')",
            params![SqlU64(task_id), SqlU64(now_ms), state_name(&a.state)],
        )
        .map_err(invalid)?;
        a.id = u64::try_from(tx.last_insert_rowid()).map_err(invalid)?;
        save(tx, &a)?;
        (task_id, a.id)
    };
    tx.execute(
        "INSERT INTO receipts VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            source,
            delivery,
            body_digest,
            SqlU64(task_id),
            SqlU64(attempt_id),
            SqlU64(now_ms),
            rule_id
        ],
    )
    .map_err(invalid)?;
    Ok(Receipt {
        delivery,
        task_id,
        attempt_id,
        duplicate: existing.is_some(),
    })
}

fn mutation(tx: &Connection, request: &str) -> Result<Option<(String, u64)>> {
    tx.query_row(
        "SELECT digest,attempt FROM mutations WHERE request=?1",
        [request],
        |r| Ok((r.get(0)?, row_number(r, 1)?)),
    )
    .optional()
    .map_err(invalid)
}
fn capacity(tx: &Connection) -> Result<()> {
    let rows = accounting_total(tx, "metadata_rows")?;
    let bytes = accounting_total(tx, "metadata_bytes")?;
    if rows >= MAX_ROWS - 4 || bytes > MAX_METADATA - 1024 * 1024 {
        return Err(AdmissionError::Capacity);
    }
    Ok(())
}

fn accounting_total(c: &Connection, key: &str) -> Result<i64> {
    let total: i64 = c
        .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .map_err(invalid)?;
    let maximum = if key == "metadata_rows" {
        MAX_ROWS
    } else {
        MAX_METADATA
    };
    if !(0..=maximum).contains(&total) {
        return Err(AdmissionError::Corrupt);
    }
    Ok(total)
}

fn initialize_accounting(c: &Connection) -> Result<()> {
    // Only these owner-defined SQL identifiers/expressions enter generated SQL.
    const TABLES: [(&str, &str); 4] = [
        ("attempts", "length(CAST($data AS BLOB))"),
        (
            "receipts",
            "length($source)+length($delivery)+length($digest)+length($rule)+48",
        ),
        ("mutations", "length($request)+length($digest)+16"),
        (
            "tasks",
            "length($source)+length($rule)+length(CAST($environment AS BLOB))+length(CAST($url AS BLOB))+64",
        ),
    ];
    let rows = TABLES
        .iter()
        .map(|(table, _)| format!("(SELECT count(*) FROM {table})"))
        .collect::<Vec<_>>()
        .join("+");
    let bytes = TABLES
        .iter()
        .map(|(table, expression)| {
            format!(
                "(SELECT coalesce(sum({}),0) FROM {table})",
                expression.replace('$', "")
            )
        })
        .collect::<Vec<_>>()
        .join("+");
    c.execute(
        &format!("INSERT OR REPLACE INTO settings VALUES('metadata_rows',{rows})"),
        [],
    )
    .map_err(invalid)?;
    c.execute(
        &format!("INSERT OR REPLACE INTO settings VALUES('metadata_bytes',{bytes})"),
        [],
    )
    .map_err(invalid)?;
    accounting_total(c, "metadata_rows")?;
    accounting_total(c, "metadata_bytes")?;
    for (table, expression) in TABLES {
        let new = expression.replace('$', "NEW.");
        let old = expression.replace('$', "OLD.");
        for (event, delta, count) in [
            ("INSERT", new.clone(), 1),
            ("DELETE", format!("-({old})"), -1),
            ("UPDATE", format!("({new})-({old})"), 0),
        ] {
            c.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS account_{table}_{event} AFTER {event} ON {table} BEGIN UPDATE settings SET value=value+({delta}) WHERE key='metadata_bytes'; UPDATE settings SET value=value+({count}) WHERE key='metadata_rows'; END;")).map_err(invalid)?;
        }
    }
    Ok(())
}
fn collect<P: rusqlite::Params>(c: &Connection, sql: &str, p: P) -> Result<Vec<String>> {
    let mut s = c.prepare(sql).map_err(invalid)?;
    s.query_map(p, |r| r.get(0))
        .map_err(invalid)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(invalid)
}
fn read(c: &Connection, id: u64) -> Result<Attempt> {
    let s: Option<String> = c
        .query_row(
            "SELECT substr(data,1,1048577) FROM attempts WHERE id=?1",
            [SqlU64(id)],
            |r| r.get(0),
        )
        .optional()
        .map_err(invalid)?;
    let a = decoded(&s.ok_or(AdmissionError::NotFound)?)?;
    validate_attempt(&a).map_err(|_| AdmissionError::Corrupt)?;
    if a.id != id {
        return Err(AdmissionError::Corrupt);
    }
    Ok(a)
}
fn validate_attempt(a: &Attempt) -> Result<()> {
    a.rule.validate().map_err(invalid)?;
    a.deployment.validate().map_err(invalid)?;
    crate::protocol::timestamp(a.created_ms).map_err(invalid)?;
    if let Some(r) = &a.result {
        r.validate().map_err(invalid)?;
    }
    if let Some(id) = &a.session_id {
        rsi_agent_session_protocol::SessionId::new(id).map_err(invalid)?;
    }
    if a.rule.repository_id != a.deployment.repository_id
        || a.rule.environment != a.deployment.environment
        || a.created_ms == 0
    {
        return Err(invalid("inconsistent durable attempt"));
    }
    a.rule.policy(&a.deployment.url).map_err(invalid)?;
    if let Some(result) = &a.result
        && matches!(
            result.outcome,
            rsi_browser::CheckOutcome::Pass | rsi_browser::CheckOutcome::AssertionFailed
        )
        && (result.assertions.len() != a.rule.checks.assertions.len()
            || result
                .assertions
                .iter()
                .zip(&a.rule.checks.assertions)
                .any(|(actual, expected)| &actual.assertion != expected))
    {
        return Err(invalid("incomplete deterministic verdict"));
    }
    if a.id == 0 || a.task_id == 0 || a.report.as_ref().is_some_and(|s| s.len() > 64 * 1024) {
        return Err(invalid("invalid durable attempt"));
    }
    Ok(())
}
fn save(c: &Connection, a: &Attempt) -> Result<()> {
    validate_attempt(a)?;
    let data = encoded(a)?;
    if data.len() > 1024 * 1024 {
        return Err(AdmissionError::Capacity);
    }
    let current_bytes = accounting_total(c, "metadata_bytes")?;
    let old_bytes: i64 = c
        .query_row(
            "SELECT length(CAST(data AS BLOB)) FROM attempts WHERE id=?1",
            [SqlU64(a.id)],
            |r| r.get(0),
        )
        .map_err(invalid)?;
    if current_bytes - old_bytes + i64::try_from(data.len()).map_err(invalid)? > MAX_METADATA {
        return Err(AdmissionError::Capacity);
    }
    c.execute(
        "UPDATE attempts SET state=?1,data=?2 WHERE id=?3",
        params![state_name(&a.state), data, SqlU64(a.id)],
    )
    .map_err(invalid)?;
    Ok(())
}
fn state_name(s: &AttemptState) -> &'static str {
    match s {
        AttemptState::Queued => "queued",
        AttemptState::Running => "running",
        AttemptState::Passed => "passed",
        AttemptState::Failed => "failed",
        AttemptState::Unavailable => "unavailable",
        AttemptState::Cancelled => "cancelled",
        AttemptState::Interrupted => "interrupted",
        AttemptState::Superseded => "superseded",
        AttemptState::CapacityRejected => "capacity_rejected",
    }
}

#[derive(Debug)]
struct SqlU64(u64);
impl rusqlite::types::ToSql for SqlU64 {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let value = i64::try_from(self.0)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(value.into())
    }
}
fn row_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn row_optional_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    let value: Option<i64> = row.get(index)?;
    value
        .map(|value| {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
        })
        .transpose()
}

#[cfg(test)]
#[path = "store_tests.rs"]
pub(crate) mod tests;
