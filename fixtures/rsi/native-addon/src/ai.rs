use rsi_ai_protocol::{
    ContentDelta, ContentStart, FinishReason, ImageToolResultCapability, LanguageEvent,
    LanguageProfile, ToolDialect,
    portable::{
        self, ControlRequest, ControlResponse, Decoder, Description, ImageHeader, ImageModel, Kind,
        LanguageModel,
    },
};
use rsi_api_protocol::ByteBudget;
use rsi_meta_native::{Message, ProviderChannel};
use serde_json::json;

#[allow(clippy::too_many_lines)] // Complete deterministic peer script for both native AI facets.
pub fn serve(channel: &mut ProviderChannel<'_>, label: &str) -> Result<(), String> {
    let budget = ByteBudget::default();
    let packet = receive(channel, &budget)?;
    if packet.kind != Kind::Json {
        return Err("expected control".into());
    }
    let request: ControlRequest =
        portable::decode_control(packet.bytes.as_bytes()).map_err(|_| "invalid control")?;
    drop(packet);
    match request {
        ControlRequest::Describe {} => {
            let description = Description {
                language: vec![LanguageModel {
                    model: "native-text".into(),
                    profile: LanguageProfile::new(
                        8192,
                        512,
                        1024,
                        ToolDialect::Responses,
                        false,
                        ImageToolResultCapability::No,
                        vec![],
                    )
                    .map_err(|e| e.to_string())?,
                    features: vec![],
                    request_extensions: vec![],
                }],
                image: vec![ImageModel {
                    model: "native-image".into(),
                    maximum_count: 1,
                    features: vec![],
                }],
            };
            send(
                channel,
                &budget,
                &ControlResponse::Description {
                    description: Box::new(description),
                },
            )
        }
        ControlRequest::PrepareLanguage { input } => send(
            channel,
            &budget,
            &ControlResponse::Prepared {
                snapshot: Box::new(input.snapshot),
                state: json!({"label":label}),
            },
        ),
        ControlRequest::PrepareImage { input } => send(
            channel,
            &budget,
            &ControlResponse::Prepared {
                snapshot: Box::new(input.snapshot),
                state: json!({"label":label}),
            },
        ),
        ControlRequest::StartLanguage { input, state } => {
            if state != json!({"label":label}) {
                return Err("state mismatch".into());
            }
            if input.snapshot.credential_source.is_some() {
                credential(channel, &budget)?;
            }
            if input.request.messages().iter().flat_map(rsi_ai_protocol::Message::content)
                .any(|content| matches!(content, rsi_ai_protocol::MessageContent::Text { text } if text == "native-cancel")) {
                send(channel, &budget, &ControlResponse::Language { event: Box::new(LanguageEvent::ContentStarted { index: 0, content: ContentStart::Text }) })?;
                channel.receive().map_err(|_| "cancelled native fixture")?;
                return Err("native stream ended without completion".into());
            }
            for event in [
                LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
                LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text(format!("{label}: native Language")),
                },
                LanguageEvent::ContentFinished { index: 0 },
                LanguageEvent::Finished {
                    reason: FinishReason::Stop,
                    replay: None,
                },
            ] {
                send(
                    channel,
                    &budget,
                    &ControlResponse::Language {
                        event: Box::new(event),
                    },
                )?;
            }
            Ok(())
        }
        ControlRequest::StartImage { input, state } => {
            if state != json!({"label":label}) {
                return Err("state mismatch".into());
            }
            if input.snapshot.credential_source.is_some() {
                credential(channel, &budget)?;
            }
            send(
                channel,
                &budget,
                &ControlResponse::Image {
                    header: ImageHeader::OutputStarted {
                        index: 0,
                        mime_type: "image/png".into(),
                    },
                },
            )?;
            send(
                channel,
                &budget,
                &ControlResponse::Image {
                    header: ImageHeader::OutputChunk {
                        index: 0,
                        sequence: 1,
                    },
                },
            )?;
            // ImageAssembler validates framing and descriptor hashes, not decoding pixels.
            send_bytes(channel, Kind::Binary, b"native-image-body")?;
            send(
                channel,
                &budget,
                &ControlResponse::Image {
                    header: ImageHeader::OutputFinished { index: 0 },
                },
            )?;
            send(
                channel,
                &budget,
                &ControlResponse::Image {
                    header: ImageHeader::Finished {},
                },
            )
        }
    }
}
fn credential(channel: &mut ProviderChannel<'_>, budget: &ByteBudget) -> Result<(), String> {
    send(channel, budget, &ControlResponse::Credential {})?;
    let packet = receive(channel, budget)?;
    if packet.kind != Kind::Binary || packet.bytes.as_bytes() != b"native-test-credential" {
        return Err("invalid fixture credential".into());
    }
    Ok(())
}
fn receive(
    channel: &mut ProviderChannel<'_>,
    budget: &ByteBudget,
) -> Result<portable::Packet, String> {
    let mut decoder = Decoder::new(budget.clone());
    loop {
        let message = channel
            .receive()
            .map_err(|_| "receive failed")?
            .ok_or("missing packet")?;
        if !message.capabilities.is_empty() {
            return Err("unexpected capabilities".into());
        }
        if let Some(packet) = decoder
            .push(&message.bytes)
            .map_err(|_| "invalid framing")?
        {
            return Ok(packet);
        }
    }
}
fn send(
    channel: &mut ProviderChannel<'_>,
    budget: &ByteBudget,
    response: &ControlResponse,
) -> Result<(), String> {
    let body = budget
        .encode(response, portable::MAXIMUM_CONTROL_BYTES)
        .map_err(|_| "encode failed")?;
    send_bytes(channel, Kind::Json, body.as_bytes())
}
fn send_bytes(channel: &mut ProviderChannel<'_>, kind: Kind, bytes: &[u8]) -> Result<(), String> {
    for frame in portable::frames(kind, bytes).map_err(|_| "invalid packet")? {
        channel
            .send(&Message {
                bytes: frame,
                capabilities: vec![],
            })
            .map_err(|_| "send failed")?;
    }
    Ok(())
}
