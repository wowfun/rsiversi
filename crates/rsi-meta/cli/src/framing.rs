use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio_util::codec::LinesCodec;

use crate::protocol::MAX_CONTROL_RESPONSE_BYTES;

#[cfg(test)]
use crate::protocol::CommandEnvelope;

pub const MAX_WIRE_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_WIRE_RESPONSE_BYTES: usize = MAX_CONTROL_RESPONSE_BYTES;

pub fn ndjson_request_codec() -> LinesCodec {
    LinesCodec::new_with_max_length(MAX_WIRE_REQUEST_BYTES)
}

pub fn ndjson_response_codec() -> LinesCodec {
    LinesCodec::new_with_max_length(MAX_WIRE_RESPONSE_BYTES)
}

#[cfg(test)]
pub fn decode_envelope(line: &str) -> Result<CommandEnvelope> {
    serde_json::from_str(line).context("decode control envelope")
}

pub fn encode_request(envelope: &impl Serialize) -> Result<String> {
    encode_envelope(envelope, MAX_WIRE_REQUEST_BYTES, "request")
}

pub fn encode_response(envelope: &impl Serialize) -> Result<String> {
    encode_envelope(envelope, MAX_WIRE_RESPONSE_BYTES, "response")
}

fn encode_envelope(envelope: &impl Serialize, limit: usize, direction: &str) -> Result<String> {
    let encoded = serde_json::to_string(envelope).context("encode wire envelope")?;
    if encoded.len() > limit {
        bail!("wire {direction} exceeds the configured NDJSON frame limit");
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;

    use super::*;
    use crate::protocol::{CliRequest, Command};

    #[tokio::test]
    async fn ndjson_preserves_exact_frame_boundaries() {
        let (left, right) = tokio::io::duplex(4096);
        let mut writer = Framed::new(left, ndjson_request_codec());
        let mut reader = Framed::new(right, ndjson_request_codec());
        let first = CliRequest::QueryGraph.into_envelope();
        let second = CliRequest::QueryEvents {
            after: 9,
            limit: 100,
        }
        .into_envelope();

        writer
            .send(serde_json::to_string(&first).unwrap())
            .await
            .unwrap();
        writer
            .send(serde_json::to_string(&second).unwrap())
            .await
            .unwrap();

        let decoded_first = decode_envelope(&reader.next().await.unwrap().unwrap()).unwrap();
        let decoded_second = decode_envelope(&reader.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(decoded_first.payload, Command::QueryGraph);
        assert_eq!(
            decoded_second.payload,
            Command::QueryEvents {
                after_cursor: 9,
                limit: 100,
            }
        );
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected() {
        let (left, right) = tokio::io::duplex(MAX_WIRE_REQUEST_BYTES + 16);
        let mut writer = Framed::new(left, LinesCodec::new());
        let mut reader = Framed::new(right, ndjson_request_codec());
        let oversized = "x".repeat(MAX_WIRE_REQUEST_BYTES + 1);

        let write = tokio::spawn(async move { writer.send(oversized).await });
        assert!(reader.next().await.unwrap().is_err());
        drop(reader);
        let _ = write.await;
    }

    #[test]
    fn oversized_outgoing_request_is_rejected() {
        let value = serde_json::json!({
            "payload": "x".repeat(MAX_WIRE_REQUEST_BYTES + 1),
        });
        assert!(encode_request(&value).is_err());
    }

    #[test]
    fn legal_large_control_response_is_encodable() {
        let value = serde_json::json!({
            "payload": {
                "type": "plugin",
                "instance": {
                    "config_schema": {
                        "description": "x".repeat(2 * 1024 * 1024),
                    }
                }
            }
        });
        assert!(
            encode_response(&value).is_ok(),
            "a legal 2 MiB plugin schema must fit in a control response"
        );
    }

    #[tokio::test]
    async fn ndjson_client_decodes_a_legal_large_control_response() {
        let (left, right) = tokio::io::duplex(64 * 1024);
        let mut writer = Framed::new(left, LinesCodec::new());
        let mut reader = Framed::new(right, ndjson_response_codec());
        let value = serde_json::json!({
            "payload": {
                "type": "plugin",
                "instance": {
                    "config_schema": {
                        "description": "x".repeat(2 * 1024 * 1024),
                    }
                }
            }
        });
        let encoded = encode_response(&value).unwrap();
        let expected = encoded.len();
        let write = tokio::spawn(async move { writer.send(encoded).await });

        let decoded = reader.next().await.unwrap().unwrap();
        assert_eq!(decoded.len(), expected);
        write.await.unwrap().unwrap();
    }

    #[test]
    fn oversized_outgoing_response_is_rejected() {
        let value = serde_json::json!({
            "payload": "x".repeat(MAX_WIRE_RESPONSE_BYTES + 1),
        });
        assert!(encode_response(&value).is_err());
    }

    #[test]
    fn query_events_wire_defaults_match_the_published_contract() {
        let decoded = decode_envelope(
            r#"{"protocol":"rsi-meta.control","version":0,"kind":"command","command_id":"defaults","payload":{"type":"query_events"}}"#,
        )
        .unwrap();
        assert_eq!(
            decoded.payload,
            Command::QueryEvents {
                after_cursor: 0,
                limit: 1_000,
            }
        );
    }
}
