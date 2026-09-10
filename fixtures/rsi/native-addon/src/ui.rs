use rsi_api_protocol::portable::{
    FRAGMENT_TAG, HEADER_TAG, Header, MAXIMUM_FRAGMENT_BYTES, MAXIMUM_HEADER_BYTES,
};
use rsi_meta_native::{Message, ProviderChannel};
use rsi_ui_protocol::{
    ExportScope, ModelAction, ModelSchema, ModelSource, TargetKind, UiModel,
    portable::{self, Request},
};
use serde_json::json;

fn send_json(
    channel: &mut ProviderChannel<'_>,
    value: &impl serde::Serialize,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > portable::MAXIMUM_PACKET_BYTES {
        return Err("UI reply exceeds limit".into());
    }
    channel
        .send(&Message::new(bytes))
        .map_err(|error| error.to_string())
}
pub fn serve(channel: &mut ProviderChannel<'_>, counter: &mut u32) -> Result<(), String> {
    let message = channel
        .receive()
        .map_err(|error| error.to_string())?
        .ok_or("missing UI request")?;
    if message.bytes.len() > portable::MAXIMUM_PACKET_BYTES || message.capabilities.len() > 1 {
        return Err("invalid UI request bound".into());
    }
    let request: Request =
        serde_json::from_slice(&message.bytes).map_err(|error| error.to_string())?;
    if channel
        .receive()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Err("expected UI request EOF".into());
    }
    if matches!(request, Request::Describe {}) {
        if !message.capabilities.is_empty() {
            return Err("Describe must not acquire business authority".into());
        }
        return send_json(
            channel,
            &portable::Description {
                name: "fixture.native.ui".into(),
                surfaces: vec![portable::Surface {
                    name: "counter".into(),
                    title: "Native Session model".into(),
                    target: TargetKind::Surface,
                }],
                actions: vec![portable::Action {
                    name: "refresh".into(),
                    target: TargetKind::Surface,
                }],
            },
        );
    }
    let scope = match &request {
        Request::Snapshot { scope, .. }
        | Request::Invoke { scope, .. }
        | Request::Source { scope, .. } => scope.as_ref().ok_or("missing business scope")?,
        Request::Describe {} => unreachable!(),
    };
    if message.capabilities.len() != 1 {
        return Err("expected one bound Session API grant".into());
    }
    // Opaque presentation addresses do not provide authority. This read uses the
    // independently granted target client and verifies its actual domain result.
    let session = session_header(channel, &message, scope)?;
    if let Request::Source {
        name,
        offset,
        maximum,
        ..
    } = request
    {
        if name != "raw" || maximum == 0 || maximum > 65536 {
            return Err("invalid UI source".into());
        }
        let bytes = b"\0\xffABC"
            .get(usize::try_from(offset).unwrap_or(usize::MAX)..)
            .unwrap_or_default()
            .iter()
            .copied()
            .take(maximum)
            .collect::<Vec<_>>();
        return channel
            .send(&Message::new(bytes))
            .map_err(|error| error.to_string());
    }
    if let Request::Invoke { action, input, .. } = request
        && (action != "refresh" || !input.fields.is_empty() || !input.value.is_null())
    {
        return Err("invalid refresh input".into());
    }
    *counter = counter
        .checked_add(1)
        .filter(|value| *value <= 1_000_000)
        .ok_or("counter exhausted")?;
    let model = UiModel {
        renderer: "fixture.rust".into(),
        schema: ModelSchema {
            name: "fixture.counter".into(),
            version: 1,
        },
        data: json!({"label":format!("Native Session {session}"),"count":counter}),
        actions: vec![ModelAction {
            name: "refresh".into(),
            title: "Refresh native model".into(),
        }],
        sources: vec![ModelSource {
            name: "raw".into(),
            title: "Native bytes".into(),
            media_type: "application/octet-stream".into(),
        }],
        standard_view: None,
    };
    model.validate().map_err(|error| error.to_string())?;
    send_json(channel, &model)
}

fn session_header(
    channel: &mut ProviderChannel<'_>,
    message: &Message,
    scope: &ExportScope,
) -> Result<String, String> {
    scope.validate().map_err(|error| error.to_string())?;
    if scope.kind != "session" {
        return Err("unsupported business scope".into());
    }
    let host = channel.host();
    let mut call = host
        .open(&message.capabilities[0])
        .map_err(|error| error.to_string())?;
    let input =
        serde_json::to_vec(&json!({"session_id":scope.key})).map_err(|error| error.to_string())?;
    let header = Header::Call {
        operation: rsi_api_protocol::OperationId::new("session", "attach", 1)
            .map_err(|error| error.to_string())?,
        bytes: input.len(),
    };
    let mut encoded = vec![HEADER_TAG];
    encoded.extend(serde_json::to_vec(&header).map_err(|error| error.to_string())?);
    call.send(&Message::new(encoded))
        .map_err(|error| error.to_string())?;
    let mut fragment = vec![FRAGMENT_TAG];
    fragment.extend(0_u32.to_le_bytes());
    fragment.extend(input);
    call.send(&Message::new(fragment))
        .map_err(|error| error.to_string())?;
    call.finish_requests().map_err(|error| error.to_string())?;
    let response = call
        .receive()
        .map_err(|error| error.to_string())?
        .ok_or("missing Session reply")?;
    if !response.capabilities.is_empty()
        || response.bytes.first() != Some(&HEADER_TAG)
        || response.bytes.len() > MAXIMUM_HEADER_BYTES + 1
    {
        return Err("invalid Session reply header".into());
    }
    let Header::Reply { json, binary: None } =
        serde_json::from_slice::<Header>(&response.bytes[1..])
            .map_err(|error| error.to_string())?
    else {
        return Err("Session read was not successful".into());
    };
    if json > 65536 {
        return Err("Session Header exceeds fixture bound".into());
    }
    let mut bytes = Vec::with_capacity(json);
    while bytes.len() < json {
        let message = call
            .receive()
            .map_err(|error| error.to_string())?
            .ok_or("truncated Session Header")?;
        if !message.capabilities.is_empty()
            || message.bytes.len() < 6
            || message.bytes.len() > MAXIMUM_FRAGMENT_BYTES + 5
            || message.bytes[0] != FRAGMENT_TAG
        {
            return Err("invalid Session fragment".into());
        }
        let offset = u32::from_le_bytes(message.bytes[1..5].try_into().expect("checked fragment"));
        if usize::try_from(offset).ok() != Some(bytes.len())
            || bytes.len() + message.bytes.len() - 5 > json
        {
            return Err("invalid Session fragment offset".into());
        }
        bytes.extend(&message.bytes[5..]);
    }
    if call.receive().map_err(|error| error.to_string())?.is_some() {
        return Err("extra Session response".into());
    }
    call.terminal().map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let session = value["session_id"]
        .as_str()
        .ok_or("missing successful Session Header")?;
    if session != scope.key {
        return Err("Session grant retargeted".into());
    }
    Ok(session.into())
}
