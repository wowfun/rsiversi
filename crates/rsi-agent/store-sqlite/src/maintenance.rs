//! Bounded explicit retirement transactions. Live-owner policy belongs to Session.
use super::{
    BTreeSet, Connection, Digest, OptionalExtension, Result, SessionId, Sha256, SqliteStore,
    StoreError, Transaction, TransactionBehavior, async_trait, bounded_text, decode_u64, params,
    read_session_header_row, reference_index, sql_error,
};
use rsi_agent_store_protocol::{
    MaintenanceOperationId, PurgeBlocker, PurgePreview, PurgeReceipt, PurgeRequest,
    StoreMaintenance,
};

use rsi_agent_session_protocol::ExecutionLocation;

impl SqliteStore {
    async fn pending_operations(
        &self,
        after: Option<&str>,
        limit: usize,
        table: &'static str,
        predicate: &'static str,
    ) -> Result<Vec<String>> {
        if !(1..=256).contains(&limit) {
            return Err(StoreError::Invalid(
                "pending operation limit must be 1..=256".into(),
            ));
        }
        if let Some(after) = after {
            MaintenanceOperationId::new(after).map_err(|e| StoreError::Invalid(e.to_string()))?;
        }
        let after = after.unwrap_or("").to_owned();
        self.with_reader(move |connection| {
            let mut statement = connection.prepare(&format!("SELECT operation_id FROM {table} WHERE {predicate} AND operation_id>?1 ORDER BY operation_id LIMIT ?2")).map_err(sql_error)?;
            let mut rows = statement.query(params![after, i64::try_from(limit).expect("bounded pending operation page")]).map_err(sql_error)?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next().map_err(sql_error)? {
                let id = bounded_text(row, 0, 256).map_err(sql_error)?;
                MaintenanceOperationId::new(&id).map_err(|e| StoreError::Corrupt(e.to_string()))?;
                ids.push(id);
            }
            Ok(ids)
        }).await
    }
    pub(super) async fn ensure_not_retired(&self, id: &SessionId) -> Result<()> {
        if self
            .inner
            .retirement_started
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let id = id.clone();
            self.with_reader(move |connection| reference_index::available(connection, &id))
                .await?;
        }
        Ok(())
    }
}

pub(super) fn enable_strict(
    transaction: &Transaction<'_>,
    owner: &super::StoreInner,
) -> Result<()> {
    let proof = owner.reference_proof.lock().expect("reference proof");
    match proof.as_ref() {
        None => return Err(StoreError::ValidationBusy),
        Some(proof) if proof.strict => return Ok(()),
        Some(_) => {}
    }
    transaction.execute("INSERT INTO reference_policy(id,strict) VALUES(1,1) ON CONFLICT(id) DO UPDATE SET strict=1", []).map_err(sql_error)?;
    Ok(())
}
fn preview(
    connection: &Connection,
    request: &PurgeRequest,
) -> Result<(PurgePreview, Vec<ExecutionLocation>)> {
    let selected: BTreeSet<_> = request.targets.iter().map(|t| &t.session_id).collect();
    let mut blockers = Vec::new();
    let mut locations = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        let id = &target.session_id;
        let found = match read_session_header_row(connection, id) {
            Ok(value) => Some(value),
            Err(StoreError::NotFound(_)) => None,
            Err(error) => return Err(error),
        };
        let Some((header, fact_seq)) = found else {
            blockers.push(PurgeBlocker::Changed {
                session_id: id.clone(),
            });
            continue;
        };
        locations.push(header.coordinates().location().clone());
        let (control, retired) = connection.query_row("SELECT control_seq, EXISTS(SELECT 1 FROM session_tombstones WHERE session_id=?1) FROM sessions WHERE session_id=?1",[id.as_str()],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,bool>(1)?))).map_err(sql_error)?;
        if fact_seq != target.fact_seq
            || decode_u64("control tail", control)? != target.control_seq
            || header
                .fingerprint()
                .map_err(|e| StoreError::Corrupt(e.to_string()))?
                != target.header_sha256
            || retired
        {
            blockers.push(PurgeBlocker::Changed {
                session_id: id.clone(),
            });
            continue;
        }
        if header.protection().is_some() {
            blockers.push(PurgeBlocker::ProductOwned {
                session_id: id.clone(),
            });
            continue;
        }
        let busy = connection.query_row("SELECT EXISTS(SELECT 1 FROM active_activations WHERE session_id=?1) OR EXISTS(SELECT 1 FROM turns WHERE session_id=?1 AND terminal_seq IS NULL) OR EXISTS(SELECT 1 FROM agent_messages WHERE session_id=?1 AND state='pending') OR EXISTS(SELECT 1 FROM program_runs WHERE session_id=?1 AND terminal=0)",[id.as_str()],|row| row.get::<_,bool>(0)).map_err(sql_error)?;
        if busy {
            blockers.push(PurgeBlocker::Busy {
                session_id: id.clone(),
            });
            continue;
        }
        let mut statement = connection.prepare("SELECT DISTINCT session_id FROM reference_edges WHERE target_kind='session' AND target_id=?1 ORDER BY session_id").map_err(sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let source = SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;
            if !selected.contains(&source) {
                blockers.push(PurgeBlocker::Referenced {
                    source: Some(source),
                    target: id.clone(),
                });
                break;
            }
        }
    }
    Ok((PurgePreview { blockers }, locations))
}
fn location_digest(request: &PurgeRequest, locations: &str) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(request).map_err(|error| StoreError::Corrupt(error.to_string()))?,
    );
    digest.update(locations.as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn operation_locations(
    connection: &Connection,
    request: &PurgeRequest,
) -> Result<Vec<ExecutionLocation>> {
    let (encoded, expected) = connection
        .query_row(
            "SELECT locations_json,locations_sha256 FROM purge_operations WHERE operation_id=?1",
            [request.operation_id.as_str()],
            |row| Ok((bounded_text(row, 0, 256 * 1024)?, bounded_text(row, 1, 64)?)),
        )
        .map_err(sql_error)?;
    if location_digest(request, &encoded)? != expected {
        return Err(StoreError::Corrupt(
            "purge location binding digest differs".into(),
        ));
    }
    let locations: Vec<ExecutionLocation> =
        serde_json::from_str(&encoded).map_err(|error| StoreError::Corrupt(error.to_string()))?;
    if locations.len() != request.targets.len() {
        return Err(StoreError::Corrupt(
            "purge location binding count differs".into(),
        ));
    }
    Ok(locations)
}

fn receipt(connection: &Connection, operation: &str) -> Result<Option<PurgeReceipt>> {
    let row = connection.query_row("SELECT complete,deleted_records,deleted_payload_bytes FROM purge_operations WHERE operation_id=?1",[operation], |row| Ok((row.get::<_,bool>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?))).optional().map_err(sql_error)?;
    row.map(|(complete, records, bytes)| {
        Ok(PurgeReceipt {
            operation_id: MaintenanceOperationId::new(operation)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            complete,
            deleted_records: decode_u64("purge records", records)?,
            deleted_payload_bytes: decode_u64("purge bytes", bytes)?,
            free_pages: decode_u64(
                "SQLite free pages",
                connection
                    .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                    .map_err(sql_error)?,
            )?,
            retained_cas_objects: decode_u64(
                "CAS object count",
                connection
                    .query_row("SELECT COUNT(*) FROM cas_objects", [], |row| row.get(0))
                    .map_err(sql_error)?,
            )?,
        })
    })
    .transpose()
}
pub(super) fn operation_request(
    connection: &Connection,
    operation: &str,
) -> Result<Option<PurgeRequest>> {
    let encoded = connection
        .query_row(
            "SELECT request_json FROM purge_operations WHERE operation_id=?1",
            [operation],
            |row| bounded_text(row, 0, 256 * 1024),
        )
        .optional()
        .map_err(sql_error)?;
    encoded
        .map(|encoded| {
            let request: PurgeRequest =
                serde_json::from_str(&encoded).map_err(|e| StoreError::Corrupt(e.to_string()))?;
            request
                .validate()
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;
            if request.operation_id.as_str() != operation {
                return Err(StoreError::Corrupt("purge request identity differs".into()));
            }
            Ok(request)
        })
        .transpose()
}
#[async_trait]
impl StoreMaintenance for SqliteStore {
    async fn pending_purges(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
        self.pending_operations(after, limit, "purge_operations", "complete=0")
            .await
    }
    async fn pending_cas_collections(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        self.pending_operations(after, limit, "cas_deletions", "state='deleting'")
            .await
    }
    async fn preview_cas_gc(
        &self,
        limit: usize,
    ) -> Result<Vec<rsi_agent_store_protocol::CasGcTarget>> {
        self.gc_preview(limit).await
    }
    async fn collect_cas(
        &self,
        operation_id: &str,
        target: rsi_agent_store_protocol::CasGcTarget,
    ) -> Result<rsi_agent_store_protocol::CasGcReceipt> {
        self.gc_collect(operation_id, target).await
    }
    async fn cas_gc_status(
        &self,
        operation_id: &str,
    ) -> Result<Option<rsi_agent_store_protocol::CasGcReceipt>> {
        self.gc_status(operation_id).await
    }

    async fn preview_purge(&self, request: &PurgeRequest) -> Result<PurgePreview> {
        request.validate()?;
        self.validate_references().await?;
        let request = request.clone();
        self.with_reader(move |connection| {
            let transaction = connection.transaction().map_err(sql_error)?;
            preview(&transaction, &request).map(|(preview, _)| preview)
        })
        .await
    }
    async fn begin_purge(
        &self,
        request: PurgeRequest,
    ) -> Result<std::result::Result<PurgeReceipt, PurgePreview>> {
        request.validate()?;
        // Idempotent recovery must work after part of the selected history is gone.
        let original = request.clone();
        let existing = self
            .with_reader(move |connection| {
                if let Some(previous) =
                    operation_request(connection, original.operation_id.as_str())?
                {
                    if previous != original {
                        return Err(StoreError::Invalid(
                            "purge operation identity reused with different selection".into(),
                        ));
                    }
                    receipt(connection, original.operation_id.as_str())
                } else {
                    Ok(None)
                }
            })
            .await?;
        if let Some(existing) = existing {
            return Ok(Ok(existing));
        }
        let owner = self.inner.clone();
        self.with_writer(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
            if let Some(previous) = operation_request(&transaction,request.operation_id.as_str())? {
                if previous != request { return Err(StoreError::Invalid("purge operation identity reused".into())); }
                return receipt(&transaction,request.operation_id.as_str())?.map(Ok).ok_or_else(|| StoreError::Corrupt("missing purge receipt".into()));
            }
            super::reference_validation::ensure(&owner, &transaction)?;
            let (admission, locations) = preview(&transaction,&request)?;
            if !admission.blockers.is_empty() { return Ok(Err(admission)); }
            #[cfg(feature = "test-support")]
            if let Some((started, wait)) = owner.purge_admission_pause.lock().unwrap().take() {
                let _ = started.send(());
                let _ = wait.recv();
            }
            #[cfg(feature = "test-support")]
            let fault = owner.purge_admission_fault.lock().unwrap().take();
            #[cfg(feature = "test-support")]
            if fault == Some(false) {
                owner.purge_binding_fault.store(true, std::sync::atomic::Ordering::Release);
                return Err(StoreError::Io("injected pre-commit admission failure".into()));
            }
            enable_strict(&transaction, &owner)?;
            let json = serde_json::to_string(&request).map_err(|e| StoreError::Invalid(e.to_string()))?;
            let locations = serde_json::to_string(&locations).map_err(|e| StoreError::Invalid(e.to_string()))?;
            let digest = location_digest(&request, &locations)?;
            transaction.execute("INSERT INTO purge_operations(operation_id,request_json,locations_json,locations_sha256,complete,deleted_records,deleted_payload_bytes) VALUES(?1,?2,?3,?4,0,0,0)",params![request.operation_id.as_str(),json,locations,digest]).map_err(sql_error)?;
            for target in &request.targets {
                transaction.execute("INSERT INTO session_tombstones(session_id,operation_id,state) VALUES(?1,?2,'retiring')",params![target.session_id.as_str(),request.operation_id.as_str()]).map_err(sql_error)?;
            }
            let result = receipt(&transaction,request.operation_id.as_str())?.ok_or_else(|| StoreError::Corrupt("missing new purge receipt".into()))?;
            // Fence cached proofs before commit becomes visible. A failed commit
            // may be uncertain; only an admitted retirement latches this flag.
            owner.retirement_started.store(true, std::sync::atomic::Ordering::Release);
            transaction.commit().map_err(sql_error)?;
            super::reference_validation::strict(&owner);
            #[cfg(feature = "test-support")]
            if fault == Some(true) {
                owner.purge_binding_fault.store(true, std::sync::atomic::Ordering::Release);
                return Err(StoreError::Io("injected post-commit acknowledgement failure".into()));
            }
            Ok(Ok(result))
        }).await
    }
    async fn purge_binding(
        &self,
        operation_id: &str,
    ) -> Result<Option<(PurgeRequest, Vec<ExecutionLocation>)>> {
        #[cfg(feature = "test-support")]
        if self
            .inner
            .purge_binding_fault
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(StoreError::Io(
                "injected unavailable admission status".into(),
            ));
        }
        MaintenanceOperationId::new(operation_id)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let operation = operation_id.to_owned();
        self.with_reader(move |connection| {
            let Some(request) = operation_request(connection, &operation)? else {
                return Ok(None);
            };
            let locations = operation_locations(connection, &request)?;
            Ok(Some((request, locations)))
        })
        .await
    }
    async fn purge_status(&self, operation_id: &str) -> Result<Option<PurgeReceipt>> {
        MaintenanceOperationId::new(operation_id)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let operation = operation_id.to_owned();
        self.with_reader(move |connection| receipt(connection, &operation))
            .await
    }
    async fn purge_batch(&self, operation_id: &str) -> Result<PurgeReceipt> {
        MaintenanceOperationId::new(operation_id)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let operation = operation_id.to_owned();
        let codecs = self.inner.reference_codecs.clone();
        self.with_writer(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
            let complete = transaction.query_row("SELECT complete FROM purge_operations WHERE operation_id=?1", [&operation], |row| row.get::<_, bool>(0)).optional().map_err(sql_error)?.ok_or_else(|| StoreError::NotFound(operation.clone()))?;
            reference_index::validate_retirement_bindings(&transaction, Some(&operation))?;
            if complete { return Ok(()); }
            let strict: bool = transaction.query_row("SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)", [], |row| row.get(0)).map_err(sql_error)?;
            if !strict { return Err(StoreError::Corrupt("retirement requires strict reference admission".into())); }
            let (records,bytes) = delete_batch(&transaction,&operation,&codecs)?;
            let remaining = transaction.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id IN (SELECT session_id FROM session_tombstones WHERE operation_id=?1))",[&operation],|row| row.get::<_,bool>(0)).map_err(sql_error)?;
            transaction.execute("UPDATE purge_operations SET complete=?2,deleted_records=deleted_records+?3,deleted_payload_bytes=deleted_payload_bytes+?4 WHERE operation_id=?1",params![operation,!remaining,records,bytes]).map_err(sql_error)?;
            if !remaining { transaction.execute("UPDATE session_tombstones SET state='purged' WHERE operation_id=?1",[&operation]).map_err(sql_error)?; }
            transaction.commit().map_err(sql_error)?;
            Ok(())
        }).await?;
        self.purge_status(operation_id)
            .await?
            .ok_or_else(|| StoreError::Corrupt("purge receipt disappeared".into()))
    }
}

// Dependency stages are schema-owned. Each metadata item or canonical record with
// its reference projection is one logical deletion; payload lengths are read first.
const STAGES: &[(&str, Option<&str>)] = &[
    ("domain_heads", None),
    ("domain_versions", None),
    ("domain_requests", None),
    ("queue_mutations", None),
    ("ready_messages", None),
    ("agent_messages", None),
    ("program_records", None),
    ("program_runs", None),
    ("active_activations", None),
    ("turns", None),
    ("context_checkpoints", None),
    ("agent_nodes", None),
    ("facts", Some("fact")),
    ("agent_controls", Some("control")),
    ("sessions", Some("header")),
];
// Largest valid checkpoint plus its Session identity and two hexadecimal digests.
const MAXIMUM_BATCH_PAYLOAD: i64 = 64 * 1024 * 1024 + 256 + 2 * 64;
fn delete_batch(
    transaction: &Transaction<'_>,
    operation: &str,
    codecs: &rsi_agent_store_protocol::ReferenceCodecs,
) -> Result<(i64, i64)> {
    let mut removed = 0_i64;
    let mut bytes = 0_i64;
    for &(table, stream) in STAGES {
        let length = payload_expression(transaction, table)?;
        let length = if let Some(stream) = stream {
            let seq = if stream == "header" {
                "0"
            } else {
                &format!("{table}.seq")
            };
            format!(
                "({length}) + COALESCE((SELECT SUM(length(CAST(session_id AS BLOB))+length(stream)+length(target_kind)+length(CAST(target_id AS BLOB))) FROM reference_edges e WHERE e.session_id={table}.session_id AND e.stream='{stream}' AND e.seq={seq}),0) + COALESCE((SELECT length(CAST(session_id AS BLOB))+length(stream)+length(sha256) FROM reference_records r WHERE r.session_id={table}.session_id AND r.stream='{stream}' AND r.seq={seq}),0)"
            )
        } else {
            length
        };
        let sequence = if matches!(stream, Some("fact" | "control")) {
            "seq"
        } else {
            "0"
        };
        let sql = deletion_query(table, sequence, &length);
        let mut statement = transaction.prepare(&sql).map_err(sql_error)?;
        let mut rows = statement
            .query(params![operation, 512 - removed])
            .map_err(sql_error)?;
        let mut keys = Vec::new();
        let mut full = false;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let length = row.get::<_, i64>(3).map_err(sql_error)?;
            if !(0..=MAXIMUM_BATCH_PAYLOAD).contains(&length) {
                return Err(StoreError::Corrupt(
                    "purge record exceeds stored payload bound".into(),
                ));
            }
            if bytes + length > MAXIMUM_BATCH_PAYLOAD {
                full = true;
                break;
            }
            keys.push((
                row.get::<_, i64>(0).map_err(sql_error)?,
                bounded_text(row, 1, 256).map_err(sql_error)?,
                row.get::<_, i64>(2).map_err(sql_error)?,
            ));
            bytes += length;
            removed += 1;
        }
        drop(rows);
        drop(statement);
        for (rowid, session, seq) in keys {
            if let Some(stream) = stream {
                verify_selected(transaction, codecs, table, stream, rowid, &session, seq)?;
                reference_index::remove(transaction, &session, stream, seq)?;
            }
            transaction
                .execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [rowid])
                .map_err(sql_error)?;
        }
        if full || removed == 512 {
            break;
        }
    }
    if removed == 0 {
        return Err(StoreError::Corrupt(
            "unfinished purge made no progress".into(),
        ));
    }
    Ok((removed, bytes))
}

pub(super) fn deletion_query(table: &str, sequence: &str, length: &str) -> String {
    format!(
        "SELECT rowid,session_id,{sequence},{length} FROM {table} WHERE session_id IN (SELECT session_id FROM session_tombstones WHERE operation_id=?1 AND state='retiring') LIMIT ?2"
    )
}

// Payload means persisted TEXT/BLOB values, including reference projection rows;
// SQLite record headers, numeric storage, indexes, free pages and WAL are separate.
fn payload_expression(connection: &Connection, table: &str) -> Result<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sql_error)?;
    let columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(sql_error)?;
    let mut lengths = Vec::new();
    for column in columns {
        let (name, kind) = column.map_err(sql_error)?;
        if matches!(kind.as_str(), "TEXT" | "BLOB") {
            lengths.push(format!(
                "COALESCE(length(CAST({table}.\"{name}\" AS BLOB)),0)"
            ));
        }
    }
    Ok(if lengths.is_empty() {
        "0".into()
    } else {
        lengths.join("+")
    })
}

// Keys come only from the schema-owned stage query, never from external SQL.
fn verify_selected(
    transaction: &Transaction<'_>,
    codecs: &rsi_agent_store_protocol::ReferenceCodecs,
    table: &str,
    stream: &str,
    rowid: i64,
    session: &str,
    seq: i64,
) -> Result<()> {
    let (column, maximum) = match stream {
        "header" => (
            "header_json",
            rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES,
        ),
        "fact" => (
            "fact_json",
            rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES,
        ),
        _ => ("control_json", super::MAXIMUM_STORE_CONTROL_PAGE_BYTES),
    };
    let encoded = transaction
        .query_row(
            &format!("SELECT {column} FROM {table} WHERE rowid=?1"),
            [rowid],
            |row| bounded_text(row, 0, maximum),
        )
        .map_err(sql_error)?;
    let id = SessionId::new(session).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    reference_index::verify_encoded(
        transaction,
        codecs,
        &id,
        stream,
        decode_u64("reference sequence", seq)?,
        &encoded,
    )?;
    if stream == "header" {
        let extra: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM reference_records WHERE session_id=?1 AND (stream!='header' OR seq!=0)) OR EXISTS(SELECT 1 FROM reference_edges WHERE session_id=?1 AND (stream!='header' OR seq!=0))", [session], |row| row.get(0)).map_err(sql_error)?;
        if extra {
            return Err(StoreError::Corrupt(
                "extra retiring reference coverage".into(),
            ));
        }
    }
    Ok(())
}
