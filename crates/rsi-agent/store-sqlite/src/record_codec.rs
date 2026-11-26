//! Canonical records stored as UTF-8 JSON or a smaller checksummed frame.

use super::{AgentControlRecord, AgentControlRecordBody, AgentMessageSource, Result, StoreError};
use rusqlite::{Row, types::ValueRef};
use std::{borrow::Cow, io};

pub(super) fn compress(json: &str) -> Result<Vec<u8>> {
    zstd::bulk::Compressor::new(3)
        .and_then(|mut encoder| {
            encoder.include_checksum(true)?;
            encoder.compress(json.as_bytes())
        })
        .map_err(|error| StoreError::Io(format!("compressing session record: {error}")))
}

pub(super) fn direct_acceptance(record: &AgentControlRecord) -> bool {
    matches!(record.body(), AgentControlRecordBody::MessageAccepted { message, .. }
        if matches!(message.source, AgentMessageSource::Human { .. } | AgentMessageSource::Application { .. }))
}

pub(super) fn expand(frame: &[u8], length: usize) -> io::Result<String> {
    let bytes = zstd::bulk::decompress(frame, length)?;
    if bytes.len() != length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session frame length differs",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Read the bounded SQL pair (logical byte length, text or compressed bytes).
pub(super) fn payload<'a>(
    row: &'a Row<'_>,
    index: usize,
    maximum: usize,
) -> rusqlite::Result<Cow<'a, str>> {
    let ValueRef::Blob(frame) = row.get_ref(index + 1)? else {
        return super::text::borrowed_text(row, index + 1, maximum).map(Cow::Borrowed);
    };
    let stored = row.get::<_, i64>(index)?;
    let length = usize::try_from(stored)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, stored))?;
    let result = if length <= maximum && !frame.is_empty() && frame.len() < length {
        expand(frame, length)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session frame exceeds its byte bound",
        ))
    };
    result.map(Cow::Owned).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index + 1,
            rusqlite::types::Type::Blob,
            error.into(),
        )
    })
}

/// A bounded SQL projection omits the payload when its logical size exceeds the limit.
pub(super) fn projected<'a>(
    row: &'a Row<'_>,
    index: usize,
    maximum: usize,
) -> rusqlite::Result<Option<Cow<'a, str>>> {
    if matches!(row.get_ref(index + 1)?, ValueRef::Null) {
        return Ok(None);
    }
    payload(row, index, maximum).map(Some)
}
