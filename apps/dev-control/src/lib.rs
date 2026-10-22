//! Bounded application-owned child control protocol for source development.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use serde::{Deserialize, Serialize};
use std::{io, net::SocketAddr};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Maximum encoded line size, including LF.
pub const MAXIMUM_FRAME_BYTES: usize = 32 * 1024;
/// Fixed startup stage deadline, excluding compilation.
pub const STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);
/// Deadline for configuring the already-bound proxy.
pub const PROXY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Exact framing/admission interface shared with the source frontend.
pub fn protocol_descriptor() -> rsi_api_protocol::ProtocolDescriptor {
    rsi_api_protocol::wire_contract!()
}

/// Closed, versioned session envelope. Debug deliberately excludes message data.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "FrameInput")]
pub struct Frame {
    version: u8,
    interface: rsi_api_protocol::ProtocolDescriptor,
    run_id: String,
    message: Message,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameInput {
    version: u8,
    interface: rsi_api_protocol::ProtocolDescriptor,
    run_id: String,
    message: Message,
}
impl TryFrom<FrameInput> for Frame {
    type Error = io::Error;
    fn try_from(input: FrameInput) -> io::Result<Self> {
        let frame = Self {
            version: input.version,
            interface: input.interface,
            run_id: input.run_id,
            message: input.message,
        };
        frame.validate()?;
        Ok(frame)
    }
}
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevelopmentFrame").finish_non_exhaustive()
    }
}

/// Closed development messages; launch links are secrets and must not be logged.
#[allow(missing_docs)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    StartFrontend { port: u16, family: String },
    FrontendBound { origin: String },
    ConfigureProxy { upstream: String },
    ProxyReady {},
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevelopmentMessage").finish_non_exhaustive()
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid development control frame",
    )
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Frame {
    /// Creates a validated frame without exposing its payload in diagnostics.
    pub fn new(run_id: &str, message: Message) -> io::Result<Self> {
        let frame = Self {
            version: 1,
            interface: protocol_descriptor(),
            run_id: run_id.into(),
            message,
        };
        frame.validate()?;
        Ok(frame)
    }
    /// Returns this session's opaque identifier.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    /// Consumes the frame only after verifying its session.
    pub fn into_message(self, run_id: &str) -> io::Result<Message> {
        if self.run_id != run_id {
            return Err(invalid());
        }
        Ok(self.message)
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.interface != protocol_descriptor() || !hex(&self.run_id, 32) {
            return Err(invalid());
        }
        match &self.message {
            Message::StartFrontend { family, .. } if !hex(family, 64) => return Err(invalid()),
            Message::FrontendBound { origin } | Message::ConfigureProxy { upstream: origin } => {
                loopback_address(origin)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Parses the canonical IPv4 loopback HTTP origin used by owned dev listeners.
pub fn loopback_address(origin: &str) -> io::Result<SocketAddr> {
    let authority = origin.strip_prefix("http://").ok_or_else(invalid)?;
    let address: SocketAddr = if authority == "127.0.0.1" {
        SocketAddr::from(([127, 0, 0, 1], 80))
    } else {
        authority.parse().map_err(|_| invalid())?
    };
    if address.ip() != std::net::Ipv4Addr::LOCALHOST
        || address.port() == 0
        || loopback_origin(address) != origin
    {
        return Err(invalid());
    }
    Ok(address)
}
/// Formats the canonical origin of an already-owned listener.
pub fn loopback_origin(address: SocketAddr) -> String {
    if address.port() == 80 {
        "http://127.0.0.1".into()
    } else {
        format!("http://{address}")
    }
}

/// Reads one bounded complete frame; EOF, partial lines and oversized data fail.
pub async fn read(reader: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Frame> {
    let mut bytes = Vec::with_capacity(MAXIMUM_FRAME_BYTES);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !bytes.is_empty() {
                return Err(invalid());
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "development control closed",
            ));
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1);
        if bytes.len() + count > MAXIMUM_FRAME_BYTES {
            return Err(invalid());
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            break;
        }
    }
    let frame: Frame = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    Ok(frame)
}
struct BoundedFrame(Vec<u8>);
impl io::Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() >= MAXIMUM_FRAME_BYTES {
            return Err(invalid());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Writes exactly one bounded frame, without logging its data.
pub async fn write(writer: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> io::Result<()> {
    let mut encoded = BoundedFrame(Vec::with_capacity(MAXIMUM_FRAME_BYTES));
    serde_json::to_writer(&mut encoded, frame).map_err(|_| invalid())?;
    let mut bytes = encoded.0;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    const RUN: &str = "0123456789abcdef0123456789abcdef";
    #[tokio::test]
    async fn only_empty_eof_is_a_clean_owner_stop() {
        assert_eq!(
            read(&mut &b""[..]).await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            read(&mut &b"{\"version\":1"[..]).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    #[test]
    fn public_deserialization_preserves_validated_frame_invariants() {
        let json = r#"{"version":1,"run_id":"invalid","message":{"event":"proxy_ready"}}"#;
        assert!(serde_json::from_str::<Frame>(json).is_err());
    }
    #[tokio::test]
    async fn shared_frontend_corpus_and_deadlines_match_the_rust_contract() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../conformance.json")).unwrap();
        assert_eq!(corpus["maximum_frame_bytes"], MAXIMUM_FRAME_BYTES);
        assert_eq!(
            corpus["startup_timeout_ms"],
            u64::try_from(STARTUP_TIMEOUT.as_millis()).unwrap()
        );
        assert_eq!(
            corpus["proxy_timeout_ms"],
            u64::try_from(PROXY_TIMEOUT.as_millis()).unwrap()
        );
        for sample in corpus["cases"].as_array().unwrap() {
            let json = sample["json"].as_str().unwrap().replace(
                "@protocol@",
                &serde_json::to_string(&protocol_descriptor()).unwrap(),
            );
            let valid = sample["valid"].as_bool().unwrap();
            assert_eq!(
                serde_json::from_str::<Frame>(&json).is_ok(),
                valid,
                "{}",
                sample["name"]
            );
            assert_eq!(
                read(&mut format!("{json}\n").as_bytes()).await.is_ok(),
                valid,
                "{}",
                sample["name"]
            );
        }
    }
    #[tokio::test]
    async fn closed_frames_reject_unbounded_unknown_duplicate_partial_and_wrong_session() {
        for text in [
            "x".repeat(MAXIMUM_FRAME_BYTES + 1),
            format!(
                "{{\"version\":1,\"run_id\":\"{RUN}\",\"message\":{{\"event\":\"proxy_ready\",\"extra\":1}}}}\n"
            ),
            format!(
                "{{\"version\":1,\"version\":1,\"run_id\":\"{RUN}\",\"message\":{{\"event\":\"proxy_ready\"}}}}\n"
            ),
            format!(
                "{{\"version\":1,\"run_id\":\"{RUN}\",\"message\":{{\"event\":\"proxy_ready\"}}}}"
            ),
        ] {
            assert!(
                read(&mut text.as_bytes()).await.is_err(),
                "accepted test frame: {text}"
            );
        }
        let frame = Frame::new(RUN, Message::ProxyReady {}).unwrap();
        let mut bytes = Vec::new();
        write(&mut bytes, &frame).await.unwrap();
        assert!(matches!(
            read(&mut bytes.as_slice())
                .await
                .unwrap()
                .into_message(RUN)
                .unwrap(),
            Message::ProxyReady {}
        ));
        assert!(
            frame
                .into_message("ffffffffffffffffffffffffffffffff")
                .is_err()
        );
    }
    #[test]
    fn origins_are_closed_and_debug_is_redacted() {
        for origin in [
            "http://localhost:123",
            "http://127.0.0.1:0",
            "http://127.0.0.1:80",
            "http://127.0.0.1:123/",
            "https://127.0.0.1:123",
            "http://127.0.0.1:123?x",
        ] {
            assert!(loopback_address(origin).is_err(), "{origin}");
        }
        assert!(loopback_address("http://127.0.0.1:123").is_ok());
        let secret = "a".repeat(64);
        let frame = Frame::new(
            RUN,
            Message::StartFrontend {
                port: 0,
                family: secret.clone(),
            },
        )
        .unwrap();
        assert!(!format!("{frame:?}").contains(&secret));
    }
}
