//! Explicit offline operations retain the namespace and exclusive writer lease.
use super::{
    Connection, OpenFlags, Path, Result, Serialize, SqliteStore, StoreError, TransactionBehavior,
    filesystem, fs, io_error, reference_index, sql_error, validate_database, validate_schema_shape,
};
/// Physical database size before and after an explicit exclusive compact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompactReceipt {
    /// Original database file size, excluding WAL.
    pub before_bytes: u64,
    /// Final checkpointed database file size.
    pub after_bytes: u64,
}
pub(super) fn require_settled(connection: &Connection) -> Result<()> {
    let pending=connection.query_row("SELECT EXISTS(SELECT 1 FROM purge_operations WHERE complete=0) OR EXISTS(SELECT 1 FROM cas_deletions WHERE state='deleting')",[],|row| row.get::<_,bool>(0)).map_err(sql_error)?;
    if pending {
        return Err(StoreError::Unsupported(
            "finish pending retirements before offline maintenance".into(),
        ));
    }
    Ok(())
}
fn writable(offline: &filesystem::OfflineStore) -> Result<Connection> {
    require_settled(&offline.connection)?;
    validate_database(&offline.connection)?;
    let connection = Connection::open_with_flags(
        offline.root.join("sessions.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql_error)?;
    filesystem::configure_writer(&connection)?;
    Ok(connection)
}
impl SqliteStore {
    /// Rebuilds derived reference coverage from valid canonical history offline.
    pub fn rebuild_reference_index(
        root: impl AsRef<Path>,
        codecs: &rsi_agent_store_protocol::ReferenceCodecs,
    ) -> Result<u64> {
        let offline = filesystem::open_offline_store(root.as_ref())?;
        let mut connection = writable(&offline)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let count = reference_index::rebuild(&transaction, codecs)?;
        reference_index::validate_all(&transaction, codecs)?;
        transaction.commit().map_err(sql_error)?;
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(sql_error)?;
        Ok(count)
    }
    /// Compacts an existing Store under its exclusive offline writer lease.
    pub fn compact(root: impl AsRef<Path>) -> Result<CompactReceipt> {
        let offline = filesystem::open_offline_store(root.as_ref())?;
        let connection = writable(&offline)?;
        let path = offline.root.join("sessions.sqlite3");
        let before_bytes = fs::metadata(&path).map_err(io_error)?.len();
        connection
            .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(sql_error)?;
        validate_schema_shape(&connection)?;
        validate_database(&connection)?;
        let after_bytes = fs::metadata(path).map_err(io_error)?.len();
        Ok(CompactReceipt {
            before_bytes,
            after_bytes,
        })
    }
}
