use super::{
    MAXIMUM_SESSION_FACT_BYTES, OptionalExtension, Result, SessionFact, SessionId, SqliteStore,
    StoreError, TransactionBehavior, decode_u64, params, sql_error, sqlite_u64,
};
use rsi_agent_store_protocol::{StoreFactOmission, StoreFactWindow, validate_window_limits};

impl SqliteStore {
    pub(super) async fn fact_window(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
        maximum_bytes: usize,
    ) -> Result<StoreFactWindow> {
        validate_window_limits(limit, maximum_bytes)?;
        self.ensure_session_validated(session_id).await?;
        let session_id = session_id.clone();
        #[cfg(any(test, feature = "test-support"))]
        let materializations = self.inner.fact_materializations.clone();
        self.with_reader(move |connection| {
            let transaction=connection.transaction_with_behavior(TransactionBehavior::Deferred).map_err(sql_error)?;
            let durable_seq=transaction.query_row("SELECT durable_seq FROM sessions WHERE session_id=?1",[session_id.as_str()],|row|row.get::<_,i64>(0)).optional().map_err(sql_error)?.ok_or_else(||StoreError::NotFound(session_id.to_string())).and_then(|value|decode_u64("Fact horizon",value))?;
            if after_seq>durable_seq {return Err(StoreError::Invalid("Fact window cursor exceeds durable horizon".into()));}
            let mut window=StoreFactWindow{after_seq,through_seq:after_seq,durable_seq,facts:vec![],omitted:vec![],encoded_bytes:0};
            let mut admitted=Vec::new();
            {
                let mut statement=transaction.prepare("SELECT seq,length(CAST(fact_json AS BLOB)) FROM facts WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3").map_err(sql_error)?;
                let mut rows=statement.query(params![session_id.as_str(),sqlite_u64("Fact cursor",after_seq)?,i64::try_from(limit).expect("bounded limit")]).map_err(sql_error)?;
                while let Some(row)=rows.next().map_err(sql_error)? {
                    let seq=decode_u64("Fact coordinate",row.get(0).map_err(sql_error)?)?;
                    let length=usize::try_from(row.get::<_,i64>(1).map_err(sql_error)?).map_err(|_|StoreError::Corrupt("negative Fact length".into()))?;
                    if length>MAXIMUM_SESSION_FACT_BYTES {return Err(StoreError::Corrupt("Fact exceeds durable byte bound".into()));}
                    if length>maximum_bytes {window.omitted.push(StoreFactOmission{seq,encoded_bytes:length});}
                    else {
                        if length>maximum_bytes-window.encoded_bytes {break;}
                        window.encoded_bytes+=length;
                        admitted.push((seq,length));
                    }
                    window.through_seq=seq;
                }
            }
            if !admitted.is_empty() {
                // Select only coordinates with prior byte admission. Do not rescan
                // oversized TEXT payloads merely to apply another length predicate.
                let placeholders = std::iter::repeat_n("?", admitted.len()).collect::<Vec<_>>().join(",");
                let sql = format!("SELECT seq,fact_json FROM facts WHERE session_id=? AND seq IN ({placeholders}) ORDER BY seq");
                let mut parameters = Vec::with_capacity(admitted.len() + 1);
                parameters.push(rusqlite::types::Value::Text(session_id.to_string()));
                for (seq, _) in &admitted {
                    parameters.push(rusqlite::types::Value::Integer(sqlite_u64("Fact coordinate", *seq)?));
                }
                let mut statement = transaction.prepare(&sql).map_err(sql_error)?;
                let mut rows = statement.query(rusqlite::params_from_iter(parameters)).map_err(sql_error)?;
                for (seq, length) in admitted {
                    let row = rows.next().map_err(sql_error)?.ok_or_else(|| StoreError::Corrupt("missing admitted window Fact".into()))?;
                    let coordinate = decode_u64("Fact coordinate", row.get(0).map_err(sql_error)?)?;
                    let text: String = row.get(1).map_err(sql_error)?;
                    #[cfg(any(test,feature="test-support"))]
                    materializations.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                    let fact: SessionFact = serde_json::from_str(&text).map_err(|error| StoreError::Corrupt(error.to_string()))?;
                    if coordinate != seq || fact.seq() != seq || fact.encoded_len() != length { return Err(StoreError::Corrupt("noncanonical window Fact".into())); }
                    window.facts.push(fact);
                }
                if rows.next().map_err(sql_error)?.is_some() {
                    return Err(StoreError::Corrupt("unadmitted window Fact".into()));
                }
            }
            window.validate(limit,maximum_bytes)?;
            transaction.commit().map_err(sql_error)?;
            Ok(window)
        }).await
    }
}
