//! Product management and observation leases over an authenticated native stream.
#![deny(unsafe_code)]
#![warn(missing_docs)]
use rsi_api_protocol::{HostEpoch, ProtocolDescriptor};
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Maximum complete JSON frame, excluding its four-byte big-endian length.
pub const MAX_FRAME: usize = 256 * 1024;
/// Generation-fenced product control request.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Active Host incarnation.
    pub epoch: HostEpoch,
    /// Required management interface.
    pub protocol: ProtocolDescriptor,
    /// Explicit management operation.
    pub command: Command,
}
/// Operations share the already running Service, without creating another Store.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Reload the current Service source through its existing Profile controller.
    Reload {},
    /// Hold automatic configuration observation until this connection closes.
    Watch {},
    /// Retain one explicitly authorized source package build.
    Build {
        /// Frozen command, filesystem authority and caller environment.
        recipe: BuildRecipe,
    },
    /// Compare frozen process inputs without transmitting their values.
    Environment {
        /// Lowercase SHA-256, salted with this request's active Host epoch.
        fingerprint: String,
    },
}
/// Closed public result without exposing configs or unbounded source diagnostics.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReloadResult {
    /// New graph was accepted.
    Applied,
    /// Source produced the current graph.
    Unchanged,
    /// Static code or another retained prefix requires a Host restart.
    RestartRequired,
    /// Candidate failed and the previous graph was restored.
    RolledBack,
    /// Candidate and restoration could not establish a healthy graph.
    Degraded,
}
/// One bounded management reply.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// One configuration observer is now owned by the connection.
    Watching {},
    /// Latest state of the connection's shared source build.
    Build {
        /// Coalesced build state; no command output or environment is exposed.
        update: BuildUpdate,
    },
    /// Whether the caller supplied the same frozen environment inputs.
    Environment {
        /// False calls for a categorical client diagnostic, not an implicit restart.
        matches: bool,
    },
    /// Actual reload outcome and accepted source identity.
    Reloaded {
        /// Convergence result.
        outcome: ReloadResult,
        /// Observed graph revision.
        revision: u64,
        /// Accepted source digest.
        source_revision: String,
    },
    /// A conflicting source recipe is still retaining its output; callers may retry.
    BuildBusy {},
    /// Categorical non-secret failure.
    Error {
        /// Bounded explanation.
        message: String,
    },
}

/// Explicit source authority. Debug deliberately omits command and environment values.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildRecipe {
    /// Expected package identity.
    pub package: String,
    /// Absolute source root.
    pub directory: std::path::PathBuf,
    /// Resolved absolute native executable and literal arguments.
    pub command: Vec<String>,
    /// Confined relative package.json output.
    pub output: std::path::PathBuf,
    /// Explicit confined relative source inputs.
    pub inputs: Vec<std::path::PathBuf>,
    /// Exact scrubbed caller environment.
    pub environment: std::collections::BTreeMap<String, String>,
}
impl std::fmt::Debug for BuildRecipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildRecipe").finish_non_exhaustive()
    }
}
/// Bounded coalesced progress for a source lease.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildUpdate {
    /// Command is running or being admitted.
    Building {},
    /// A complete package was published atomically.
    Published {
        /// Exact immutable manifest digest.
        digest: String,
        /// Linked code needs an explicit process restart.
        restart_required: bool,
    },
    /// Previous package selection remains available.
    Failed {
        /// Categorical failure, never command output.
        reason: String,
    },
}
/// Description of this interface, independent of Lifecycle v1.
pub fn protocol_descriptor() -> ProtocolDescriptor {
    rsi_api_protocol::wire_contract!()
}
/// Bounds encoded output before writing the length prefix.
///
/// # Errors
/// Returns an error for oversized or unserializable frames, or failed stream writes.
pub async fn encode_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let bytes = rsi_api_protocol::ByteBudget::default()
        .encode(value, MAX_FRAME)
        .map_err(io::Error::other)?;
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    stream.write_u32(length).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}
/// Bounds allocation before parsing strict operation DTOs.
///
/// # Errors
/// Returns an error for invalid lengths or payloads, or failed stream reads.
pub async fn decode_frame<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::other("invalid Host control length"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| io::Error::other("invalid Host control frame"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_oversized_frames_and_unknown_operation_fields() {
        for size in [0, u32::try_from(MAX_FRAME).unwrap() + 1] {
            assert!(
                decode_frame::<Request>(&mut size.to_be_bytes().as_slice())
                    .await
                    .is_err()
            );
        }
        assert!(serde_json::from_str::<Command>(r#"{"operation":"watch","extra":true}"#).is_err());
        assert!(serde_json::from_str::<Reply>(r#"{"status":"watching","extra":true}"#).is_err());
    }
}
