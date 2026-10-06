//! Diagnostic-only bounded intake journal. It never authorizes admission.
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Malformed,
    Oversized,
    Timeout,
    Busy,
    Unauthorized,
    InvalidEvent,
    Conflict,
    Capacity,
    StorageUnavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rejection {
    pub timestamp: String,
    pub source: Option<String>,
    pub delivery: Option<String>,
    pub reason: Reason,
}
pub(crate) fn load(directory: &Path) -> (Vec<Rejection>, Option<String>) {
    let path = directory.join("intake-log.json");
    if !path.exists() {
        return (vec![], None);
    }
    let result = (|| {
        let mut data = vec![];
        crate::private_file(&path)
            .map_err(|_| "log_unavailable")?
            .take(128 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "log_unavailable")?;
        if data.len() > 128 * 1024 {
            return Err("log_invalid");
        }
        let rows: Vec<Rejection> = serde_json::from_slice(&data).map_err(|_| "log_invalid")?;
        if rows.len() > 256
            || rows.iter().any(|r| {
                r.timestamp.len() > 20
                    || r.timestamp.parse::<u64>().is_err()
                    || [&r.source, &r.delivery].iter().any(|s| {
                        s.as_ref()
                            .is_some_and(|s| crate::protocol::identity(s).is_err())
                    })
            })
        {
            return Err("log_invalid");
        }
        Ok(rows)
    })();
    match result {
        Ok(rows) => (rows, None),
        Err(e) => (vec![], Some(e.into())),
    }
}
pub(crate) fn save(directory: &Path, rows: &[Rejection]) -> Result<(), String> {
    let write = (|| {
        let bytes = serde_json::to_vec(rows).map_err(|_| "log_write_failed")?;
        if bytes.len() > 128 * 1024 {
            return Err("log_write_failed");
        }
        let pending = directory.join(".intake-log.pending");
        let mut file = crate::private_file(&pending).map_err(|_| "log_write_failed")?;
        file.set_len(0).map_err(|_| "log_write_failed")?;
        file.write_all(&bytes).map_err(|_| "log_write_failed")?;
        file.sync_all().map_err(|_| "log_write_failed")?;
        std::fs::rename(pending, directory.join("intake-log.json"))
            .map_err(|_| "log_write_failed")?;
        std::fs::File::open(directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| "log_write_failed")?;
        Ok(())
    })();
    write.map_err(str::to_owned)
}
