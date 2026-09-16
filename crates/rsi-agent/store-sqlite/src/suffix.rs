use super::{
    MAXIMUM_SESSION_FACT_BYTES, OptionalExtension, Result, SessionFact, SessionId, SqliteStore,
    StoreError, TransactionBehavior, decode_u64, params, sql_error,
};
use rsi_agent_store_protocol::{StoreFactSuffix, validate_suffix_limits};

impl SqliteStore {
    pub(super) async fn fact_suffix(
        &self,
        session_id: &SessionId,
        limit: usize,
        maximum_bytes: usize,
    ) -> Result<StoreFactSuffix> {
        validate_suffix_limits(limit, maximum_bytes)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        #[cfg(any(test, feature = "test-support"))]
        let materializations = self.inner.fact_materializations.clone();
        self.with_reader(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred).map_err(sql_error)?;
            let (through, digest) = transaction.query_row(
                "SELECT durable_seq, CASE WHEN length(CAST(fact_prefix_sha256 AS BLOB)) = 64 THEN fact_prefix_sha256 ELSE '' END FROM sessions WHERE session_id = ?1",
                [session_id.as_str()], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,String>(1)?)))
                .optional().map_err(sql_error)?.ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
            let through_seq = decode_u64("suffix horizon", through)?;
            let mut suffix = StoreFactSuffix { through_seq, fact_prefix_sha256:digest, facts:Vec::new(), encoded_bytes:0, byte_limited:false };
            let mut admitted = Vec::new();
            {
                // This query projects lengths only. No body reaches SQLite's result
                // column or Rust allocation until the remaining budget admits it.
                let mut lengths = transaction.prepare("SELECT seq, length(CAST(fact_json AS BLOB)) FROM facts WHERE session_id = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT ?3").map_err(sql_error)?;
                let mut rows = lengths.query(params![session_id.as_str(), through, i64::try_from(limit).expect("bounded limit")]).map_err(sql_error)?;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    let seq = row.get::<_,i64>(0).map_err(sql_error)?;
                    let length = usize::try_from(row.get::<_,i64>(1).map_err(sql_error)?).map_err(|_| StoreError::Corrupt("invalid Fact length".into()))?;
                    if length > MAXIMUM_SESSION_FACT_BYTES { return Err(StoreError::Corrupt("Fact exceeds durable byte bound".into())); }
                    if length > maximum_bytes - suffix.encoded_bytes { suffix.byte_limited = true; break; }
                    suffix.encoded_bytes += length;
                    admitted.push((seq, length));
                }
            }
            if let Some(&(first, _)) = admitted.last() {
                // The read transaction keeps the admitted lengths and body range atomic.
                let mut statement = transaction.prepare("SELECT seq, fact_json FROM facts WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3 ORDER BY seq ASC").map_err(sql_error)?;
                let mut rows = statement.query(params![session_id.as_str(), first, through]).map_err(sql_error)?;
                for (seq, length) in admitted.into_iter().rev() {
                    let row = rows.next().map_err(sql_error)?.ok_or_else(|| StoreError::Corrupt("missing admitted suffix Fact".into()))?;
                    if row.get::<_, i64>(0).map_err(sql_error)? != seq {
                        return Err(StoreError::Corrupt("suffix Fact sequence changed".into()));
                    }
                    let text: String = row.get(1).map_err(sql_error)?;
                    #[cfg(any(test, feature = "test-support"))]
                    materializations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let fact: SessionFact = serde_json::from_str(&text).map_err(|error| StoreError::Corrupt(error.to_string()))?;
                    if fact.encoded_len() != length || fact.seq() != decode_u64("suffix Fact", seq)? {
                        return Err(StoreError::Corrupt("noncanonical suffix Fact".into()));
                    }
                    suffix.facts.push(fact);
                }
                if rows.next().map_err(sql_error)?.is_some() {
                    return Err(StoreError::Corrupt("unexpected suffix Fact".into()));
                }
            }
            suffix.validate(limit, maximum_bytes)?;
            transaction.commit().map_err(sql_error)?;
            Ok(suffix)
        }).await
    }
}
