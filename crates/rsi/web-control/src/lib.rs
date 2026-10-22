//! Bounded local control of resident Web and per-attachment HTTP listeners.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use rsi_api_protocol::{HostEpoch, ProtocolDescriptor};
use serde::{Deserialize, Serialize};
use std::{io, net::SocketAddr, path::PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Maximum encoded control request or reply.
pub const MAX_FRAME: usize = 16 * 1024;

/// Explicit source authority for one listener generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assets {
    /// Immutable installed package export.
    Package {
        /// Installed name.
        package: String,
        /// Named Web-assets export.
        export: String,
    },
    /// Explicit operator-supplied local bundle; never claims distributor authenticity.
    Directory {
        /// Absolute source directory.
        path: PathBuf,
    },
}
impl Default for Assets {
    fn default() -> Self {
        Self::Package {
            package: "web".into(),
            export: "web".into(),
        }
    }
}

/// A local operation, admitted only after native identity and protocol checks.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Get a fresh browser launch ticket, starting the persistent listener if needed.
    Launch {
        /// Omitted means reuse its actual port, or 8787 for the first launch.
        port: Option<u16>,
        /// Omitted means reuse its selected source, or the default Web package.
        assets: Option<Assets>,
    },
    /// Stop only the persistent browser listener.
    Stop {},
    /// Query the persistent browser listener without starting it.
    Status {},
    /// Re-resolve an installed package for active listeners and publish renderer-only changes.
    Refresh {
        /// Installed package whose source build was admitted.
        package: String,
    },
    /// Start an authenticated API listener owned by this control connection.
    Serve {
        /// Existing HTTP listener policy and TLS files.
        config: rsi_api_http::HttpConfig,
        /// Optional Web bundle; absence selects an API-only listener.
        assets: Option<Assets>,
    },
    /// Start a loopback Web listener owned by one Desktop process connection.
    Desktop {
        /// Selected immutable Web bundle.
        assets: Assets,
    },
    /// Start a connection-owned Web listener behind a supervised frontend.
    Watch {
        /// Selected immutable Web bundle.
        assets: Assets,
        /// Exact canonical loopback Origin reported by that frontend.
        frontend_origin: String,
    },
}

/// One generation-fenced request.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Exact active generation.
    pub epoch: HostEpoch,
    /// Required Web control wire contract.
    pub protocol: ProtocolDescriptor,
    /// Requested listener action.
    pub command: Command,
}

/// One bounded reply. Tickets intentionally have no Debug representation.
#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// Active renderer generations were refreshed, or need application restart.
    Refreshed {
        /// Bootstrap or Worker changes require a new application attachment.
        restart_required: bool,
    },
    /// Listener is ready; ticket is present only for a browser handoff.
    Ready {
        /// Actual listening address.
        address: SocketAddr,
        /// One-use ticket with a ten-minute maximum lifetime.
        ticket: Option<String>,
    },
    /// No persistent browser listener remains.
    Stopped {},
    /// Bounded categorical action failure.
    Error {
        /// Non-secret diagnostic.
        message: String,
    },
}
impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refreshed { restart_required } => {
                f.debug_tuple("Refreshed").field(restart_required).finish()
            }
            Self::Ready { address, .. } => f.debug_tuple("Ready").field(address).finish(),
            Self::Stopped {} => f.write_str("Stopped"),
            Self::Error { message } => f.debug_tuple("Error").field(message).finish(),
        }
    }
}

/// One private launcher-to-Desktop handoff; Debug must not expose the ticket.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopLaunch {
    /// Exact frontend URL including the one-use launch fragment.
    pub url: String,
}
impl std::fmt::Debug for DesktopLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopLaunch").finish_non_exhaustive()
    }
}
impl DesktopLaunch {
    /// Validates a canonical loopback Origin and bounded hexadecimal ticket.
    ///
    /// # Errors
    /// Returns an error for an invalid Origin or launch ticket.
    pub fn validate(&self) -> io::Result<()> {
        let (origin, ticket) = self
            .url
            .split_once("#rsi-launch=")
            .ok_or_else(|| io::Error::other("invalid Desktop launch"))?;
        if ticket.len() != 64
            || !ticket
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || rsi_api_http::LoopbackBrowserOrigin::new(origin.into()).is_err()
        {
            return Err(io::Error::other("invalid Desktop launch"));
        }
        Ok(())
    }
    /// Reads exactly one bounded frame before the remaining pipe becomes a lifetime lease.
    ///
    /// # Errors
    /// Returns an error for failed reads, invalid lengths or invalid launch data.
    pub fn read(reader: &mut impl io::Read) -> io::Result<Self> {
        let mut length = [0; 4];
        reader.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > 4096 {
            return Err(io::Error::other("invalid Desktop launch length"));
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes)?;
        let launch: Self = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("invalid Desktop launch"))?;
        launch.validate()?;
        Ok(launch)
    }
}

/// Descriptor for this protocol only.
pub fn protocol_descriptor() -> ProtocolDescriptor {
    rsi_api_protocol::wire_contract!()
}

/// Encodes and flushes one bounded control frame.
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
/// Bounds allocation before decoding a complete frame.
///
/// # Errors
/// Returns an error for invalid lengths or payloads, or failed stream reads.
pub async fn decode_frame<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::other("invalid Web control length"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| io::Error::other("invalid Web control frame"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_handoff_bounds_frame_and_rejects_remote_origin() {
        for url in [
            "https://example.test#rsi-launch=",
            "http://127.0.0.1:0#rsi-launch=",
        ] {
            assert!(
                DesktopLaunch {
                    url: format!("{url}{}", "b".repeat(64))
                }
                .validate()
                .is_err()
            );
        }
        let launch = DesktopLaunch {
            url: format!("http://127.0.0.1:42117#rsi-launch={}", "b".repeat(64)),
        };
        launch.validate().unwrap();
        let payload = serde_json::to_vec(&launch).unwrap();
        let bytes: Vec<_> = (u32::try_from(payload.len()).unwrap())
            .to_be_bytes()
            .into_iter()
            .chain(payload)
            .collect();
        assert_eq!(
            DesktopLaunch::read(&mut bytes.as_slice()).unwrap().url,
            launch.url
        );
        assert!(DesktopLaunch::read(&mut 4097_u32.to_be_bytes().as_slice()).is_err());
    }
    #[tokio::test]
    async fn invalid_length_and_unknown_fields_fail_before_action_admission() {
        let mut bytes = (u32::try_from(MAX_FRAME).unwrap() + 1)
            .to_be_bytes()
            .as_slice()
            .to_vec();
        assert!(
            decode_frame::<Request>(&mut bytes.as_slice())
                .await
                .is_err()
        );
        let payload = br#"{"status":"stopped","unexpected":1}"#;
        bytes = (u32::try_from(payload.len()).unwrap())
            .to_be_bytes()
            .into_iter()
            .chain(payload.iter().copied())
            .collect();
        assert!(decode_frame::<Reply>(&mut bytes.as_slice()).await.is_err());
        assert!(serde_json::from_str::<Command>(r#"{"operation":"stop","unexpected":1}"#).is_err());
        assert!(
            serde_json::from_str::<Command>(r#"{"operation":"status","unexpected":1}"#).is_err()
        );
    }
    #[test]
    fn debug_does_not_disclose_launch_tickets() {
        let reply = Reply::Ready {
            address: "127.0.0.1:8787".parse().unwrap(),
            ticket: Some("private-ticket".into()),
        };
        assert!(!format!("{reply:?}").contains("private-ticket"));
    }
}
