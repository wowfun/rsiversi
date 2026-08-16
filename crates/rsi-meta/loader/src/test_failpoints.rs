use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{ContentHash, LoaderError};

const STAGE_GATE_ENV: &str = "RSI_META_LOADER_TEST_STAGE_GATE";
const READY_BYTE: u8 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageGate {
    artifact_hash: String,
    cache_path: PathBuf,
    gate_path: PathBuf,
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> LoaderError {
    LoaderError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

/// Test-only crash gate between a durable temporary file and CAS publication.
pub fn gate_before_cache_publish(
    cache_path: &Path,
    artifact_hash: ContentHash,
) -> Result<(), LoaderError> {
    let Some(encoded) = std::env::var_os(STAGE_GATE_ENV) else {
        return Ok(());
    };
    let encoded = encoded.into_string().map_err(|_| {
        io_error(
            "decode test staging gate",
            cache_path,
            io::Error::new(io::ErrorKind::InvalidInput, "gate JSON is not UTF-8"),
        )
    })?;
    let gate: StageGate = serde_json::from_str(&encoded).map_err(|source| {
        io_error(
            "decode test staging gate",
            cache_path,
            io::Error::new(io::ErrorKind::InvalidInput, source),
        )
    })?;
    if gate.cache_path != cache_path || gate.artifact_hash != artifact_hash.to_hex() {
        return Ok(());
    }

    let mut connection = UnixStream::connect(&gate.gate_path)
        .map_err(|source| io_error("connect test staging gate", &gate.gate_path, source))?;
    connection
        .write_all(&[READY_BYTE])
        .and_then(|()| connection.flush())
        .map_err(|source| io_error("notify test staging gate", &gate.gate_path, source))?;
    let mut release = [0_u8; 1];
    connection.read_exact(&mut release).map_err(|source| {
        io_error(
            "wait for test staging gate release",
            &gate.gate_path,
            source,
        )
    })?;
    Ok(())
}
