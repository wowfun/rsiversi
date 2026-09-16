//! Reject corrupt indexed text while it is still borrowed from `SQLite`.

use rusqlite::{Row, types::ValueRef};

fn borrowed_text<'a>(row: &'a Row<'_>, index: usize, limit: usize) -> rusqlite::Result<&'a str> {
    let value = row.get_ref(index)?;
    let ValueRef::Text(bytes) = value else {
        return Err(rusqlite::Error::InvalidColumnType(
            index,
            row.as_ref().column_name(index)?.to_owned(),
            value.data_type(),
        ));
    };
    if bytes.len() > limit {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            value.data_type(),
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "indexed text exceeds its byte bound",
            )
            .into(),
        ));
    }
    std::str::from_utf8(bytes).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, value.data_type(), error.into())
    })
}

pub(super) fn bounded_text(row: &Row<'_>, index: usize, limit: usize) -> rusqlite::Result<String> {
    borrowed_text(row, index, limit).map(str::to_owned)
}

pub(super) fn optional_text(
    row: &Row<'_>,
    index: usize,
    limit: usize,
) -> rusqlite::Result<Option<String>> {
    if matches!(row.get_ref(index)?, ValueRef::Null) {
        Ok(None)
    } else {
        bounded_text(row, index, limit).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_boundary_rejects_bytes_types_and_invalid_utf8() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        for sql in [
            "SELECT 42",
            "SELECT x'6162'",
            "SELECT CAST(x'ff' AS TEXT)",
            "SELECT 'éé'",
        ] {
            assert!(
                connection
                    .query_row(sql, [], |row| bounded_text(row, 0, 3))
                    .is_err()
            );
        }
        let oversized = "x".repeat(4 * 1024 * 1024);
        assert!(
            connection
                .query_row("SELECT ?1", [&oversized], |row| {
                    // The check fails in the borrowed layer, before the owning conversion.
                    borrowed_text(row, 0, 256).map(|_| ())
                })
                .is_err()
        );
        assert_eq!(
            connection
                .query_row("SELECT 'é'", [], |row| bounded_text(row, 0, 2))
                .unwrap(),
            "é"
        );
        assert_eq!(
            connection
                .query_row("SELECT NULL", [], |row| optional_text(row, 0, 256))
                .unwrap(),
            None
        );
    }
}
