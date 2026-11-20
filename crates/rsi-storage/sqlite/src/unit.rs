use super::{
    BTreeMap, Connection, MAXIMUM_SQLITE_DOMAINS, MAXIMUM_STORAGE_DOMAIN_BYTES,
    MAXIMUM_STORAGE_IDENTIFIER_BYTES, MAXIMUM_STORAGE_RECORDS, MAXIMUM_STORAGE_VALUE_BYTES,
    OptionalExtension, RecordObjectSize, StorageError, Transaction, bounded_domain_count,
    checked_stored_size, insert_domain, params, sqlite_io, validate_identifier,
};
use rsi_storage::{Address, EncodedValue, Mutation, UnitDescriptor, UnitSnapshot};

pub(super) fn load(
    connection: &mut Connection,
    unit: &UnitDescriptor,
    after_header: impl FnOnce(),
) -> Result<UnitSnapshot, StorageError> {
    let snapshot = connection
        .transaction()
        .map_err(|error| sqlite_io(&error))?;
    let Some((version, expected)) = header(&snapshot, unit.name())? else {
        return Ok(UnitSnapshot::default());
    };
    unit.admit_version(version)?;
    after_header();
    let mut measured = RecordObjectSize::default();
    let mut records = BTreeMap::new();
    let mut statement = snapshot
        .prepare_cached(
            "SELECT CASE WHEN length(CAST(table_name AS BLOB)) <= ?2 THEN table_name END,
                CASE WHEN length(CAST(key AS BLOB)) <= ?2 THEN key END, length(value), value
         FROM rsi_storage_records WHERE domain=?1 ORDER BY table_name,key LIMIT ?3",
        )
        .map_err(|error| sqlite_io(&error))?;
    let mut rows = statement
        .query(params![
            unit.name(),
            i64::try_from(MAXIMUM_STORAGE_IDENTIFIER_BYTES)
                .expect("identifier bound fits SQLite integer"),
            i64::try_from(MAXIMUM_STORAGE_RECORDS + 1).expect("record bound fits SQLite integer")
        ])
        .map_err(|error| sqlite_io(&error))?;
    while let Some(row) = rows.next().map_err(|error| sqlite_io(&error))? {
        let address = address(row)?;
        unit.admit_address(&address).map_err(corrupt)?;
        let length = value_length(row.get(2).map_err(|error| sqlite_io(&error))?)?;
        measured = measured
            .with_entry(None, address.entry_bytes(length)?)
            .map_err(corrupt)?;
        check_size(unit.name(), measured, true)?;
        // Length admission precedes allocation of the owned BLOB.
        let bytes: Vec<u8> = row.get(3).map_err(|error| sqlite_io(&error))?;
        let value = EncodedValue::from_bytes(bytes).map_err(corrupt)?;
        records.insert(address, value);
    }
    if measured != expected {
        return Err(inconsistent(unit.name()));
    }
    drop(rows);
    drop(statement);
    snapshot.commit().map_err(|error| sqlite_io(&error))?;
    Ok(UnitSnapshot {
        version: Some(version),
        records,
        recoveries: Vec::new(),
    })
}

pub(super) fn write(
    transaction: &Transaction<'_>,
    unit: &UnitDescriptor,
    changes: Vec<Mutation>,
) -> Result<(), StorageError> {
    let original = header(transaction, unit.name())?;
    if let Some((version, _)) = original {
        unit.admit_version(version)?;
    }
    let mut size = original.map_or_else(RecordObjectSize::default, |(_, size)| size);
    let mut admitted = Vec::with_capacity(changes.len());
    let mut select = transaction.prepare_cached("SELECT length(value) FROM rsi_storage_records WHERE domain=?1 AND table_name=?2 AND key=?3")
        .map_err(|error| sqlite_io(&error))?;
    for change in changes {
        let address = &change.address;
        let previous: Option<i64> = select
            .query_row(
                params![unit.name(), address.table(), address.key()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| sqlite_io(&error))?;
        let previous = previous
            .map(value_length)
            .transpose()?
            .map(|length| address.entry_bytes(length))
            .transpose()?;
        if let Some(value) = &change.value {
            size = size.with_entry(previous, address.entry_bytes(value.encoded_len())?)?;
        } else if let Some(previous) = previous {
            size = size.without_entry(previous)?;
        } else {
            continue;
        }
        admitted.push(change);
    }
    if admitted.is_empty() {
        return Ok(());
    }
    // Check final geometry before modifying rows. Inserts ordered before deletes
    // neither bypass bounds nor cause a spurious intermediate-count rejection.
    check_size(unit.name(), size, false)?;
    if original.is_none() {
        insert_domain(transaction, unit.name(), unit.version())?;
    }
    let mut upsert = transaction.prepare_cached("INSERT INTO rsi_storage_records(domain,table_name,key,value) VALUES (?1,?2,?3,?4) ON CONFLICT(domain,table_name,key) DO UPDATE SET value=excluded.value")
        .map_err(|error| sqlite_io(&error))?;
    let mut remove = transaction
        .prepare_cached(
            "DELETE FROM rsi_storage_records WHERE domain=?1 AND table_name=?2 AND key=?3",
        )
        .map_err(|error| sqlite_io(&error))?;
    for change in admitted {
        let address = change.address;
        if let Some(value) = change.value {
            upsert
                .execute(params![
                    unit.name(),
                    address.table(),
                    address.key(),
                    value.as_bytes()
                ])
                .map_err(|error| sqlite_io(&error))?;
        } else {
            remove
                .execute(params![unit.name(), address.table(), address.key()])
                .map_err(|error| sqlite_io(&error))?;
        }
    }
    transaction.execute("UPDATE rsi_storage_domains SET version=?2,record_count=?3,record_bytes=?4 WHERE domain=?1",
        params![unit.name(), unit.version(), i64::try_from(size.records()).expect("admitted record count"), i64::try_from(size.bytes()).expect("admitted byte count")]).map_err(|error| sqlite_io(&error))?;
    Ok(())
}

pub(super) fn validate_accounting(connection: &Connection) -> Result<(), StorageError> {
    let snapshot = connection
        .unchecked_transaction()
        .map_err(|error| sqlite_io(&error))?;
    if bounded_domain_count(&snapshot)? > MAXIMUM_SQLITE_DOMAINS {
        return Err(StorageError::Corrupt(format!(
            "SQLite domain count exceeds {MAXIMUM_SQLITE_DOMAINS}"
        )));
    }
    validate_child_domains(&snapshot)?;
    let mut domains = snapshot.prepare("SELECT CASE WHEN length(CAST(domain AS BLOB)) <= ?1 THEN domain END,version,record_count,record_bytes FROM rsi_storage_domains ORDER BY domain LIMIT ?2")
        .map_err(|error| sqlite_io(&error))?;
    let mut rows = domains
        .query(params![
            i64::try_from(MAXIMUM_STORAGE_IDENTIFIER_BYTES)
                .expect("identifier bound fits SQLite integer"),
            MAXIMUM_SQLITE_DOMAINS
        ])
        .map_err(|error| sqlite_io(&error))?;
    let mut records = snapshot.prepare("SELECT CASE WHEN length(CAST(table_name AS BLOB)) <= ?2 THEN table_name END, CASE WHEN length(CAST(key AS BLOB)) <= ?2 THEN key END,length(value) FROM rsi_storage_records WHERE domain=?1 ORDER BY table_name,key LIMIT ?3")
        .map_err(|error| sqlite_io(&error))?;
    while let Some(row) = rows.next().map_err(|error| sqlite_io(&error))? {
        let domain: String = row
            .get::<_, Option<String>>(0)
            .map_err(|error| sqlite_io(&error))?
            .ok_or_else(|| corrupt("oversized stored domain"))?;
        validate_identifier("domain", &domain).map_err(corrupt)?;
        stored_version(row.get(1).map_err(|error| sqlite_io(&error))?)?;
        let expected = checked_stored_size(
            &domain,
            row.get(2).map_err(|error| sqlite_io(&error))?,
            row.get(3).map_err(|error| sqlite_io(&error))?,
        )?;
        let mut measured = RecordObjectSize::default();
        let mut entries = records
            .query(params![
                domain,
                i64::try_from(MAXIMUM_STORAGE_IDENTIFIER_BYTES)
                    .expect("identifier bound fits SQLite integer"),
                i64::try_from(MAXIMUM_STORAGE_RECORDS + 1)
                    .expect("record bound fits SQLite integer")
            ])
            .map_err(|error| sqlite_io(&error))?;
        while let Some(row) = entries.next().map_err(|error| sqlite_io(&error))? {
            let address = address(row)?;
            let length = value_length(row.get(2).map_err(|error| sqlite_io(&error))?)?;
            measured = measured
                .with_entry(None, address.entry_bytes(length)?)
                .map_err(corrupt)?;
            check_size(&domain, measured, true)?;
        }
        if measured != expected {
            return Err(inconsistent(&domain));
        }
    }
    Ok(())
}

fn header(
    connection: &Connection,
    domain: &str,
) -> Result<Option<(u32, RecordObjectSize)>, StorageError> {
    let stored = connection
        .query_row(
            "SELECT version,record_count,record_bytes FROM rsi_storage_domains WHERE domain=?1",
            [domain],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_io(&error))?
        .map(|(version, count, bytes)| {
            Ok((
                stored_version(version)?,
                checked_stored_size(domain, count, bytes)?,
            ))
        })
        .transpose()?;
    if stored.is_none()
        && connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM rsi_storage_records WHERE domain=?1)",
                [domain],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| sqlite_io(&error))?
    {
        return Err(corrupt("stored records have no domain header"));
    }
    Ok(stored)
}

fn validate_child_domains(connection: &Connection) -> Result<(), StorageError> {
    let mut parent = connection
        .prepare("SELECT EXISTS(SELECT 1 FROM rsi_storage_domains WHERE domain=?1)")
        .map_err(|error| sqlite_io(&error))?;
    let mut after: Option<String> = None;
    for _ in 0..=MAXIMUM_SQLITE_DOMAINS {
        // Separate first/next queries let SQLite seek past a whole domain in the
        // primary-key index instead of scanning its records to find an orphan.
        let sql = if after.is_some() {
            "SELECT CASE WHEN length(CAST(domain AS BLOB)) <= ?1 THEN domain END
             FROM rsi_storage_records WHERE domain > ?2 ORDER BY domain LIMIT 1"
        } else {
            "SELECT CASE WHEN length(CAST(domain AS BLOB)) <= ?1 THEN domain END
             FROM rsi_storage_records ORDER BY domain LIMIT 1"
        };
        let mut statement = connection
            .prepare_cached(sql)
            .map_err(|error| sqlite_io(&error))?;
        let bound = i64::try_from(MAXIMUM_STORAGE_IDENTIFIER_BYTES).expect("identifier bound");
        let next = if let Some(previous) = &after {
            statement.query_row(params![bound, previous], |row| {
                row.get::<_, Option<String>>(0)
            })
        } else {
            statement.query_row([bound], |row| row.get::<_, Option<String>>(0))
        }
        .optional()
        .map_err(|error| sqlite_io(&error))?;
        let Some(domain) = next else { return Ok(()) };
        let domain = domain.ok_or_else(|| corrupt("oversized stored domain"))?;
        validate_identifier("domain", &domain).map_err(corrupt)?;
        // The parent scan validates accounting once; this walk only checks membership.
        if !parent
            .query_row([&domain], |row| row.get::<_, bool>(0))
            .map_err(|error| sqlite_io(&error))?
        {
            return Err(corrupt("stored records have no domain header"));
        }
        after = Some(domain);
    }
    Err(corrupt("stored child domains exceed the domain bound"))
}

fn address(row: &rusqlite::Row<'_>) -> Result<Address, StorageError> {
    let part = |index| {
        row.get::<_, Option<String>>(index)
            .map_err(|error| sqlite_io(&error))?
            .ok_or_else(|| corrupt("oversized stored address"))
    };
    Address::from_parts(part(0)?, part(1)?).map_err(corrupt)
}

fn value_length(length: i64) -> Result<usize, StorageError> {
    let length = usize::try_from(length).map_err(corrupt)?;
    if length == 0 || length > MAXIMUM_STORAGE_VALUE_BYTES {
        return Err(corrupt("stored value exceeds its byte bound"));
    }
    Ok(length)
}

fn check_size(domain: &str, size: RecordObjectSize, durable: bool) -> Result<(), StorageError> {
    let exceeded = if size.records() > MAXIMUM_STORAGE_RECORDS {
        Some("record count")
    } else if size.bytes() > MAXIMUM_STORAGE_DOMAIN_BYTES {
        Some("aggregate bytes")
    } else {
        None
    };
    if let Some(bound) = exceeded {
        let message = format!("domain `{domain}` exceeds its {bound} bound");
        return Err(if durable {
            StorageError::Corrupt(message)
        } else {
            StorageError::InvalidInput(message)
        });
    }
    Ok(())
}
pub(super) fn inconsistent(domain: &str) -> StorageError {
    corrupt(format!(
        "domain `{domain}` has inconsistent record-accounting metadata"
    ))
}
fn corrupt(error: impl std::fmt::Display) -> StorageError {
    StorageError::Corrupt(error.to_string())
}

fn stored_version(version: i64) -> Result<u32, StorageError> {
    u32::try_from(version)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| corrupt("invalid stored domain version"))
}
