use super::{
    Connection, Result, SessionId, SqliteStore, StoreError, TransactionBehavior, bounded_text,
    decode_json, decode_u64, encode_json, params, sql_error, sqlite_u64,
};
use rsi_agent_session_protocol::ExecutionCoordinates;
use rsi_agent_store_protocol::{
    ExecutionLocations, MAXIMUM_ORDER_MEMBERS, StoreActivityCursor, StoreActivityPage,
    StoreActivityRow, StoreOrderSeed,
};

pub(super) fn coordinates_key(coordinates: &ExecutionCoordinates) -> Result<String> {
    encode_json("execution coordinates", coordinates)
}
pub(super) fn advance_activity(
    connection: &Connection,
    session: &SessionId,
    timestamp: u64,
) -> Result<()> {
    if timestamp != 0 {
        connection.execute("UPDATE sessions SET last_activity_ms = max(last_activity_ms, ?2) WHERE session_id = ?1",
            params![session.as_str(), sqlite_u64("activity timestamp", timestamp)?]).map_err(sql_error)?;
    }
    Ok(())
}
impl SqliteStore {
    pub(super) async fn activity_summaries(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<Option<StoreActivityRow>>> {
        rsi_agent_store_protocol::validate_activity_summaries(sessions)?;
        if sessions.is_empty() {
            return Ok(Vec::new());
        }
        let sessions = sessions.to_vec();
        self.with_reader(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred).map_err(sql_error)?;
            let result = {
                let values = (0..sessions.len()).map(|index| format!("({index},?{})", index + 1)).collect::<Vec<_>>().join(",");
                let sql = format!("WITH requested(ordinal, id) AS (VALUES {values})
                    SELECT sessions.session_id, sessions.coordinates_key, sessions.created_at_ms, sessions.last_activity_ms
                    FROM requested LEFT JOIN sessions ON sessions.session_id = requested.id ORDER BY requested.ordinal");
                let mut statement = transaction.prepare(&sql).map_err(sql_error)?;
                let mut rows = statement.query(rusqlite::params_from_iter(sessions.iter().map(SessionId::as_str))).map_err(sql_error)?;
                let mut result = Vec::with_capacity(sessions.len());
                for session in &sessions {
                    let raw = rows.next().map_err(sql_error)?.ok_or_else(|| StoreError::Corrupt("missing activity summary slot".into()))?;
                    if matches!(raw.get_ref(0).map_err(sql_error)?, rusqlite::types::ValueRef::Null) {
                        result.push(None);
                        continue;
                    }
                    let id = bounded_text(raw, 0, 256).map_err(sql_error)?;
                    let key = bounded_text(raw, 1, 128 * 1024).map_err(sql_error)?;
                    let row = StoreActivityRow {
                        session_id: SessionId::new(id).map_err(|error| StoreError::Corrupt(error.to_string()))?,
                        coordinates: decode_json("indexed execution coordinates", &key)?,
                        created_at_ms: decode_u64("creation timestamp", raw.get(2).map_err(sql_error)?)?,
                        last_activity_ms: decode_u64("activity timestamp", raw.get(3).map_err(sql_error)?)?,
                    };
                    if row.session_id != *session || row.created_at_ms == 0 || row.last_activity_ms < row.created_at_ms {
                        return Err(StoreError::Corrupt("invalid activity summary".into()));
                    }
                    result.push(Some(row));
                }
                result
            };
            transaction.commit().map_err(sql_error)?;
            Ok(result)
        }).await
    }
    pub(super) async fn activity_page(
        &self,
        locations: &ExecutionLocations,
        coordinates: Option<&ExecutionCoordinates>,
        after: Option<&StoreActivityCursor>,
        limit: usize,
    ) -> Result<StoreActivityPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)?;
        let locations = locations.clone();
        let coordinates = coordinates.cloned();
        let after = after.cloned();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let key = coordinates.as_ref().map(coordinates_key).transpose()?;
            let mut sessions = read_rows(
                &transaction,
                &locations,
                key.as_deref(),
                after.as_ref(),
                limit + 1,
            )?;
            let newest = if after.is_none() {
                sessions.first().map(StoreActivityRow::cursor)
            } else {
                read_rows(&transaction, &locations, key.as_deref(), None, 1)?
                    .first()
                    .map(StoreActivityRow::cursor)
            };
            let has_more = sessions.len() > limit;
            sessions.truncate(limit);
            let page = StoreActivityPage {
                after,
                sessions,
                has_more,
                newest,
            };
            page.validate(coordinates.as_ref(), limit)?;
            transaction.commit().map_err(sql_error)?;
            Ok(page)
        })
        .await
    }
    pub(super) async fn order_seed(
        &self,
        locations: &ExecutionLocations,
        coordinates: Option<&ExecutionCoordinates>,
    ) -> Result<StoreOrderSeed> {
        let key = coordinates.map(coordinates_key).transpose()?;
        let locations = locations.clone();
        self.with_reader(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(sql_error)?;
            let seed = {
                let (sql, parameters) = ordered_query(
                    &locations,
                    key.as_deref(),
                    Ordering::Identity,
                    None,
                    MAXIMUM_ORDER_MEMBERS + 1,
                )?;
                let mut statement = transaction.prepare_cached(&sql).map_err(sql_error)?;
                let mut rows = statement
                    .query(rusqlite::params_from_iter(parameters))
                    .map_err(sql_error)?;
                let mut members = Vec::new();
                while let Some(row) = rows.next().map_err(sql_error)? {
                    members.push((
                        SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
                            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                        decode_json(
                            "order coordinates",
                            &bounded_text(row, 1, 128 * 1024).map_err(sql_error)?,
                        )?,
                        decode_u64("member activity", row.get(3).map_err(sql_error)?)?,
                    ));
                }
                StoreOrderSeed::from_members(members)?
            };
            transaction.commit().map_err(sql_error)?;
            Ok(seed)
        })
        .await
    }
}
fn read_rows(
    connection: &Connection,
    locations: &ExecutionLocations,
    key: Option<&str>,
    after: Option<&StoreActivityCursor>,
    limit: usize,
) -> Result<Vec<StoreActivityRow>> {
    let (sql, parameters) = ordered_query(
        locations,
        key,
        Ordering::Activity,
        after.map(|cursor| (cursor.last_activity_ms, &cursor.session_id)),
        limit,
    )?;
    let mut statement = connection.prepare_cached(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(parameters))
        .map_err(sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        result.push(StoreActivityRow {
            session_id: SessionId::new(bounded_text(row, 0, 256).map_err(sql_error)?)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            coordinates: decode_json(
                "indexed execution coordinates",
                &bounded_text(row, 1, 128 * 1024).map_err(sql_error)?,
            )?,
            created_at_ms: decode_u64(
                "indexed creation timestamp",
                row.get(2).map_err(sql_error)?,
            )?,
            last_activity_ms: decode_u64(
                "indexed activity timestamp",
                row.get(3).map_err(sql_error)?,
            )?,
        });
    }
    Ok(result)
}

#[derive(Clone, Copy)]
pub(super) enum Ordering {
    Activity,
    Created,
    Identity,
}

// Each selected index contributes at most `limit` candidates to the global merge.
pub(super) fn ordered_query(
    locations: &ExecutionLocations,
    key: Option<&str>,
    ordering: Ordering,
    after: Option<(u64, &SessionId)>,
    limit: usize,
) -> Result<(String, Vec<rusqlite::types::Value>)> {
    let column = match ordering {
        Ordering::Activity => "last_activity_ms",
        Ordering::Created => "created_at_ms",
        Ordering::Identity => "session_id",
    };
    let order = if matches!(ordering, Ordering::Identity) {
        "session_id ASC".to_owned()
    } else {
        format!("{column} DESC, session_id DESC")
    };
    let mut parameters = Vec::new();
    let mut query = |scope: &ExecutionLocations| -> Result<String> {
        let (mut conditions, values) = selection(scope, key)?;
        parameters.extend(values);
        if let Some((timestamp, id)) = after {
            conditions.push(format!("({column}, session_id) < (?, ?)"));
            parameters.push(sqlite_u64("page cursor", timestamp)?.into());
            parameters.push(id.as_str().to_owned().into());
        }
        parameters.push(
            i64::try_from(limit)
                .map_err(|_| StoreError::Invalid("page limit".into()))?
                .into(),
        );
        Ok(format!(
            "SELECT session_id, coordinates_key, created_at_ms, last_activity_ms FROM sessions {} ORDER BY {order} LIMIT ?",
            predicate(&conditions)
        ))
    };
    let sql = if let Some(selected) = locations
        .selected()
        .filter(|selected| selected.len() > 1 && key.is_none())
    {
        let queries = selected
            .iter()
            .map(|location| {
                let scope =
                    ExecutionLocations::only(std::collections::BTreeSet::from([location.clone()]))
                        .expect("one location");
                query(&scope).map(|query| format!("SELECT * FROM ({query})"))
            })
            .collect::<Result<Vec<_>>>()?;
        parameters.push(
            i64::try_from(limit)
                .map_err(|_| StoreError::Invalid("page limit".into()))?
                .into(),
        );
        format!(
            "SELECT * FROM ({}) ORDER BY {order} LIMIT ?",
            queries.join(" UNION ALL ")
        )
    } else {
        query(locations)?
    };
    Ok((sql, parameters))
}

fn selection(
    locations: &ExecutionLocations,
    key: Option<&str>,
) -> Result<(Vec<String>, Vec<rusqlite::types::Value>)> {
    let mut conditions = Vec::new();
    let mut parameters = Vec::new();
    if let Some(key) = key {
        conditions.push("coordinates_key = ?".into());
        parameters.push(key.to_owned().into());
    }
    if let Some(selected) = locations.selected() {
        if selected.is_empty() {
            conditions.push("0".into());
        } else {
            let placeholders = vec!["?"; selected.len()].join(",");
            conditions.push(format!(
                "json_extract(coordinates_key, '$.location') IN ({placeholders})"
            ));
            for location in selected {
                parameters.push(encode_json("execution location filter", location)?.into());
            }
        }
    }
    Ok((conditions, parameters))
}
fn predicate(conditions: &[String]) -> String {
    if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maximum_location_union_preserves_all_rows_across_continuation_pages() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY, coordinates_key TEXT, created_at_ms INTEGER, last_activity_ms INTEGER);").unwrap();
        let mut locations = std::collections::BTreeSet::from([
            rsi_agent_session_protocol::ExecutionLocation::Local,
        ]);
        for index in 0..256 {
            locations.insert(rsi_agent_session_protocol::ExecutionLocation::Ssh {
                target: rsi_agent_session_protocol::ExecutionTargetId::parse(format!(
                    "{index:032x}"
                ))
                .unwrap(),
            });
        }
        for (index, location) in locations.iter().enumerate() {
            let key = coordinates_key(
                &ExecutionCoordinates::new(location.clone(), "/workspace").unwrap(),
            )
            .unwrap();
            connection
                .execute(
                    "INSERT INTO sessions VALUES(?1,?2,?3,?3)",
                    params![
                        format!("session-{index:03}"),
                        key,
                        i64::try_from(index).unwrap() + 1
                    ],
                )
                .unwrap();
        }
        let scope = ExecutionLocations::only(locations).unwrap();
        for ordering in [Ordering::Activity, Ordering::Created] {
            let mut cursor: Option<(u64, SessionId)> = None;
            let mut seen = vec![];
            loop {
                let (sql, parameters) = ordered_query(
                    &scope,
                    None,
                    ordering,
                    cursor.as_ref().map(|(time, id)| (*time, id)),
                    64,
                )
                .unwrap();
                let page = connection
                    .prepare(&sql)
                    .unwrap()
                    .query_map(rusqlite::params_from_iter(parameters), |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(2)?))
                    })
                    .unwrap()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .unwrap();
                let Some((id, time)) = page.last() else {
                    break;
                };
                cursor = Some((u64::try_from(*time).unwrap(), SessionId::new(id).unwrap()));
                seen.extend(page.into_iter().map(|(id, _)| id));
            }
            assert_eq!(
                seen,
                (0..257)
                    .rev()
                    .map(|index| format!("session-{index:03}"))
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn selected_location_pages_bound_physical_sort_work_before_merging() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY, coordinates_key TEXT, created_at_ms INTEGER, last_activity_ms INTEGER);").unwrap();
        for (name, sql) in crate::EXPECTED_INDEXES {
            if matches!(
                name,
                "sessions_by_location_activity"
                    | "sessions_by_location_created"
                    | "sessions_by_location_id"
            ) {
                connection.execute_batch(sql).unwrap();
            }
        }
        let locations = [
            rsi_agent_session_protocol::ExecutionLocation::Local,
            rsi_agent_session_protocol::ExecutionLocation::Ssh {
                target: rsi_agent_session_protocol::ExecutionTargetId::parse("a".repeat(32))
                    .unwrap(),
            },
        ];
        let transaction = connection.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO sessions VALUES(?1,?2,?3,?3)")
                .unwrap();
            for (index, location) in locations.iter().enumerate() {
                let key = coordinates_key(
                    &ExecutionCoordinates::new(location.clone(), "/workspace").unwrap(),
                )
                .unwrap();
                for time in 1..=10_000 {
                    insert
                        .execute(params![format!("s-{index}-{time:05}"), key, time])
                        .unwrap();
                }
            }
        }
        transaction.commit().unwrap();
        let scope = ExecutionLocations::only(locations.into_iter().collect()).unwrap();
        let cursor = SessionId::new("s-1-05000").unwrap();
        let (sql, parameters) = ordered_query(&scope, None, Ordering::Identity, None, 17).unwrap();
        let mut statement = connection.prepare_cached(&sql).unwrap();
        let ids = statement
            .query_map(rusqlite::params_from_iter(parameters), |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(ids.len(), 17);
        assert_eq!(ids[0], "s-0-00001");
        assert_eq!(ids[16], "s-0-00017");
        let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
        assert!(
            steps < 3000,
            "bounded membership seed took {steps} VM steps"
        );

        for ordering in [Ordering::Activity, Ordering::Created] {
            for after in [None, Some((5000, &cursor))] {
                let (sql, parameters) = ordered_query(&scope, None, ordering, after, 17).unwrap();
                let mut statement = connection.prepare_cached(&sql).unwrap();
                let ids = statement
                    .query_map(rusqlite::params_from_iter(parameters), |row| {
                        row.get::<_, String>(0)
                    })
                    .unwrap()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(ids.len(), 17);
                assert_eq!(
                    ids[0],
                    if after.is_some() {
                        "s-0-05000"
                    } else {
                        "s-1-10000"
                    }
                );
                let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
                assert!(
                    steps < 3000,
                    "bounded two-location page took {steps} VM steps"
                );
            }
        }
    }
}
