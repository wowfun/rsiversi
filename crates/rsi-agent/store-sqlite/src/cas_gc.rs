//! Explicit one-hash GC. The operation identity never authorizes a later generation.
use super::{
    CasObjectRef, Connection, OptionalExtension, Result, SqliteStore, StoreError,
    TransactionBehavior, bounded_text, cas, cas_ownership, decode_u64, params, sql_error,
    sqlite_u64,
};
use rsi_agent_store_protocol::{CasGcReceipt, CasGcTarget, MaintenanceOperationId};
fn status(connection: &Connection, operation: &str) -> Result<Option<CasGcReceipt>> {
    connection
        .query_row(
            "SELECT sha256,byte_len,generation,state FROM cas_deletions WHERE operation_id=?1",
            [operation],
            |row| {
                Ok((
                    bounded_text(row, 0, 64)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    bounded_text(row, 3, 8)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .map(|(sha256, bytes, generation, state)| {
            let object = CasObjectRef {
                sha256,
                byte_len: decode_u64("CAS deletion bytes", bytes)?,
            };
            object.validate()?;
            if !matches!(state.as_str(), "deleting" | "deleted") {
                return Err(StoreError::Corrupt("invalid CAS deletion state".into()));
            }
            Ok(CasGcReceipt {
                operation_id: MaintenanceOperationId::new(operation)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                target: CasGcTarget {
                    object,
                    generation: decode_u64("CAS generation", generation)?,
                },
                complete: state == "deleted",
            })
        })
        .transpose()
}
impl SqliteStore {
    pub(super) async fn gc_status(&self, operation: &str) -> Result<Option<CasGcReceipt>> {
        MaintenanceOperationId::new(operation).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let operation = operation.to_owned();
        self.with_reader(move |connection| status(connection, &operation))
            .await
    }
    pub(super) async fn gc_preview(&self, limit: usize) -> Result<Vec<CasGcTarget>> {
        if !(1..=256).contains(&limit) {
            return Err(StoreError::Invalid(
                "CAS preview limit must be 1..=256".into(),
            ));
        }
        self.validate_references().await?;
        self.with_reader(move |connection| {
            let mut statement = connection.prepare("SELECT sha256,byte_len,generation FROM cas_objects WHERE NOT EXISTS(SELECT 1 FROM reference_edges WHERE target_kind='cas' AND target_id=sha256) AND NOT EXISTS(SELECT 1 FROM cas_deletions d WHERE d.sha256=cas_objects.sha256 AND d.state='deleting') ORDER BY sha256 LIMIT ?1").map_err(sql_error)?;
            let mut rows = statement.query([i64::try_from(limit).expect("bounded CAS preview limit")]).map_err(sql_error)?; let mut targets=Vec::new();
            while let Some(row)=rows.next().map_err(sql_error)? {
                let object=CasObjectRef { sha256:bounded_text(row,0,64).map_err(sql_error)?,byte_len:decode_u64("CAS length",row.get(1).map_err(sql_error)?)? }; object.validate()?;
                targets.push(CasGcTarget { object,generation:decode_u64("CAS generation",row.get(2).map_err(sql_error)?)? });
            }
            Ok(targets)
        }).await
    }
    pub(super) async fn gc_collect(
        &self,
        operation: &str,
        target: CasGcTarget,
    ) -> Result<CasGcReceipt> {
        MaintenanceOperationId::new(operation).map_err(|e| StoreError::Invalid(e.to_string()))?;
        target.validate()?;
        if let Some(receipt) = self.gc_status(operation).await? {
            if receipt.target != target {
                return Err(StoreError::Invalid("CAS collection identity reused".into()));
            }
            if receipt.complete {
                return Ok(receipt);
            }
        }
        let ownership = self
            .inner
            .cas_ownership
            .acquire(&target.object.sha256, true)?;
        let operation = operation.to_owned();
        let store = self.clone();
        // One task owns the receipt, namespace and per-hash guard, even after waiter loss.
        tokio::spawn(async move {
            let _ownership = ownership;
            store.gc_owned(operation, target).await
        })
        .await
        .map_err(|e| StoreError::Io(format!("CAS collection completion unknown: {e}")))?
    }
    async fn gc_owned(&self, operation: String, target: CasGcTarget) -> Result<CasGcReceipt> {
        if let Some(receipt) = self.gc_status(&operation).await? {
            if receipt.target != target {
                return Err(StoreError::Invalid("CAS collection identity reused".into()));
            }
            if receipt.complete {
                return Ok(receipt);
            }
        }
        let id = operation.clone();
        let selected = target.clone();
        let owner = self.inner.clone();
        self.with_writer(move |connection| {
            let transaction=connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
            if let Some(receipt)=status(&transaction,&id)? {
                if receipt.target!=selected { return Err(StoreError::Invalid("CAS collection identity reused".into())); }
                return Ok(());
            }
            super::reference_validation::ensure(&owner, &transaction)?;
            cas_ownership::available(&transaction,&selected.object.sha256)?;
            let current=transaction.query_row("SELECT byte_len,generation FROM cas_objects WHERE sha256=?1",[&selected.object.sha256],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))).optional().map_err(sql_error)?.ok_or_else(|| StoreError::NotFound(selected.object.sha256.clone()))?;
            if decode_u64("CAS bytes",current.0)?!=selected.object.byte_len || decode_u64("CAS generation",current.1)?!=selected.generation { return Err(StoreError::Invalid("CAS publication changed".into())); }
            let referenced=transaction.query_row("SELECT EXISTS(SELECT 1 FROM reference_edges WHERE target_kind='cas' AND target_id=?1)",[&selected.object.sha256],|row| row.get::<_,bool>(0)).map_err(sql_error)?;
            if referenced { return Err(StoreError::Invalid("CAS object is referenced".into())); }
            super::maintenance::enable_strict(&transaction, &owner)?;
            transaction.execute("INSERT INTO cas_deletions(operation_id,sha256,byte_len,generation,state) VALUES(?1,?2,?3,?4,'deleting')",params![id,selected.object.sha256,sqlite_u64("CAS bytes",selected.object.byte_len)?,sqlite_u64("CAS generation",selected.generation)?]).map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
            super::reference_validation::strict(&owner);
            Ok(())
        }).await?;
        let directory = self.inner.cas_dir.clone();
        let object = target.object.clone();
        self.with_cas(move || cas::remove_cas(&directory, &object))
            .await?;
        self.with_writer(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let deleted = transaction
                .execute(
                    "DELETE FROM cas_objects WHERE sha256=?1 AND byte_len=?2 AND generation=?3",
                    params![
                        target.object.sha256,
                        sqlite_u64("CAS bytes", target.object.byte_len)?,
                        sqlite_u64("CAS generation", target.generation)?
                    ],
                )
                .map_err(sql_error)?;
            if deleted != 1 {
                return Err(StoreError::Corrupt(
                    "CAS deletion publication changed".into(),
                ));
            }
            transaction
                .execute(
                    "UPDATE cas_deletions SET state='deleted' WHERE operation_id=?1",
                    [&operation],
                )
                .map_err(sql_error)?;
            let receipt = status(&transaction, &operation)?
                .ok_or_else(|| StoreError::Corrupt("missing CAS deletion receipt".into()))?;
            transaction.commit().map_err(sql_error)?;
            Ok(receipt)
        })
        .await
    }
}
