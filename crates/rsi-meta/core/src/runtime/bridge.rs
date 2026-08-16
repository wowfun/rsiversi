use super::{
    DataCommand, EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, HostError, InstanceId,
    OP_CANCEL, OP_CREDIT, OP_DATA, OP_HALF_CLOSE, OutboundBridgeCommand, PluginFrame,
    PluginFrameBody, Result, STREAM_BYTE_BUDGET, StreamEnvelope, StreamKind, StreamPort, Value,
    json, mpsc,
};

pub(super) fn bounded_byte_array_payload(
    bytes: &[u8],
    maximum_frame_bytes: usize,
    stream_id: &str,
) -> Result<(Value, u64)> {
    let encoded_bytes = encoded_byte_array_len(bytes).ok_or_else(|| {
        HostError::InvalidEnvelope("service stream payload size overflowed usize".to_owned())
    })?;
    let encoded_u64 = u64::try_from(encoded_bytes).unwrap_or(u64::MAX);
    if encoded_bytes > maximum_frame_bytes || encoded_u64 > STREAM_BYTE_BUDGET {
        return Err(HostError::StreamByteBudgetExceeded {
            stream_id: stream_id.to_owned(),
            requested: encoded_u64,
            available: STREAM_BYTE_BUDGET,
        });
    }
    Ok((
        Value::Array(bytes.iter().copied().map(Value::from).collect()),
        encoded_u64,
    ))
}

#[cfg(test)]
pub(super) fn json_byte_array_encoded_len(bytes: &[u8]) -> Option<usize> {
    encoded_byte_array_len(bytes)
}

fn encoded_byte_array_len(bytes: &[u8]) -> Option<usize> {
    let digits = bytes.iter().try_fold(0_usize, |total, byte| {
        total.checked_add(match byte {
            0..=9 => 1,
            10..=99 => 2,
            100..=u8::MAX => 3,
        })
    })?;
    digits
        .checked_add(2)?
        .checked_add(bytes.len().saturating_sub(1))
}

pub(super) async fn run_outbound_bridge(
    request_id: String,
    service: String,
    provider: InstanceId,
    mut port: StreamPort,
    mut commands: mpsc::Receiver<OutboundBridgeCommand>,
    data: mpsc::Sender<DataCommand>,
) {
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => {
                let Some(command) = command else {
                    let _ = port.cancel("consumer_runtime_closed".to_owned()).await;
                    break;
                };
                if let Err(error) = dispatch_outbound_command(&mut port, command).await {
                    let frame = PluginFrame::service_event(
                        Some(request_id.clone()),
                        &service,
                        EVENT_CANCEL,
                        json!({"reason": error.to_string(), "provider": provider.0}),
                    );
                    let _ = data.send(DataCommand::OutboundEvent(frame)).await;
                    break;
                }
            }
            event = port.recv() => {
                let Some(event) = event else {
                    let frame = PluginFrame::service_event(
                        Some(request_id.clone()),
                        &service,
                        EVENT_CANCEL,
                        json!({"reason": "provider_stream_closed", "provider": provider.0}),
                    );
                    let _ = data.send(DataCommand::OutboundEvent(frame)).await;
                    break;
                };
                let frame = match event {
                    Ok(envelope) => plugin_event_from_stream(&request_id, &service, envelope),
                    Err(error) => PluginFrame::service_event(
                        Some(request_id.clone()),
                        &service,
                        EVENT_CANCEL,
                        json!({"reason": error.to_string(), "provider": provider.0}),
                    ),
                };
                let terminal = matches!(
                    frame.body,
                    PluginFrameBody::ServiceEvent { ref event, .. }
                        if matches!(event.as_str(), EVENT_END | EVENT_CANCEL)
                );
                if data.send(DataCommand::OutboundEvent(frame)).await.is_err() || terminal {
                    break;
                }
            }
        }
    }
    let _ = data.send(DataCommand::OutboundClosed { request_id }).await;
}

async fn dispatch_outbound_command(
    port: &mut StreamPort,
    command: OutboundBridgeCommand,
) -> Result<()> {
    match command.operation.as_str() {
        OP_DATA => {
            let values = command.payload.as_array().ok_or_else(|| {
                HostError::InvalidEnvelope("plugin DATA payload is not a byte array".to_owned())
            })?;
            let bytes = values
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|byte| u8::try_from(byte).ok())
                        .ok_or_else(|| {
                            HostError::InvalidEnvelope(
                                "plugin DATA payload contains a non-byte value".to_owned(),
                            )
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            port.send(&bytes).await
        }
        OP_CREDIT => {
            let bytes = command
                .payload
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    HostError::InvalidEnvelope("plugin credit bytes are missing".to_owned())
                })?;
            port.grant_credit(bytes).await
        }
        OP_HALF_CLOSE => port.half_close().await,
        OP_CANCEL => {
            let reason = command
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("consumer_cancelled")
                .to_owned();
            port.cancel(reason).await
        }
        operation => Err(HostError::InvalidEnvelope(format!(
            "unknown plugin stream operation {operation:?}"
        ))),
    }
}

fn plugin_event_from_stream(
    request_id: &str,
    service: &str,
    envelope: StreamEnvelope,
) -> PluginFrame {
    let (event, payload) = match envelope.kind {
        StreamKind::Data => (EVENT_DATA, envelope.payload.unwrap_or(Value::Null)),
        StreamKind::Credit => (
            EVENT_CREDIT,
            json!({"bytes": envelope.credit_bytes.unwrap_or(0)}),
        ),
        StreamKind::End => (EVENT_END, envelope.payload.unwrap_or_else(|| json!({}))),
        StreamKind::Cancel => (
            EVENT_CANCEL,
            envelope
                .payload
                .unwrap_or_else(|| json!({"reason": "provider_cancelled"})),
        ),
        StreamKind::Open | StreamKind::HalfClose => (
            EVENT_CANCEL,
            json!({"reason": "invalid_provider_stream_event"}),
        ),
    };
    PluginFrame::service_event(Some(request_id.to_owned()), service, event, payload)
}
