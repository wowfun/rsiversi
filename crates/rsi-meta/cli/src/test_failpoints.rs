use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::protocol::{CommandOutcome, CommandOutcomeEnvelope};

const ACK_GATE_ENV: &str = "RSI_META_TEST_ACK_GATE";
const READY_BYTE: u8 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckGate {
    command_id: String,
    gate_path: PathBuf,
}

/// Demo-only gate at the durable-terminal / UDS-ack boundary.
///
/// This module is absent from default builds. A feature build reads one exact
/// command id and Unix gate path, announces readiness after the host has
/// returned its durable terminal outcome, then blocks before any response
/// encoding or write. The release demo kills the daemon while it is blocked.
pub async fn gate_before_uds_ack(outcome: &CommandOutcomeEnvelope) -> Result<()> {
    let Some(encoded) = std::env::var_os(ACK_GATE_ENV) else {
        return Ok(());
    };
    let encoded = encoded
        .into_string()
        .map_err(|_| anyhow::anyhow!("{ACK_GATE_ENV} must be valid UTF-8 JSON"))?;
    let gate: AckGate =
        serde_json::from_str(&encoded).with_context(|| format!("decode {ACK_GATE_ENV} JSON"))?;
    if gate.command_id != outcome.command_id {
        return Ok(());
    }
    if matches!(&outcome.payload, CommandOutcome::Rejected { .. }) {
        bail!(
            "refusing {ACK_GATE_ENV} for rejected command {:?}",
            outcome.command_id
        );
    }

    let mut connection = UnixStream::connect(&gate.gate_path)
        .await
        .with_context(|| {
            format!(
                "connect test acknowledgement gate {}",
                gate.gate_path.display()
            )
        })?;
    connection
        .write_all(&[READY_BYTE])
        .await
        .context("notify durable-before-ack test gate")?;
    connection
        .flush()
        .await
        .context("flush durable-before-ack test gate")?;
    let mut release = [0_u8; 1];
    connection
        .read_exact(&mut release)
        .await
        .context("wait for durable-before-ack test gate release")?;
    Ok(())
}
