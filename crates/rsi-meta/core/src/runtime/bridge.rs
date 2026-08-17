use super::{
    DataCommand, EVENT_CANCEL, EVENT_CREDIT, EVENT_END, HostError, InstanceId, OP_CANCEL,
    OP_CREDIT, OP_HALF_CLOSE, OutboundBridgeCommand, PluginFrame, PluginFrameBody, Result,
    StreamEnvelope, StreamKind, StreamPort, Value, json, mpsc,
};

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
                    Ok(envelope) => match plugin_event_from_stream(&request_id, &service, envelope) {
                        Ok(frame) => frame,
                        Err(error) => PluginFrame::service_event(
                            Some(request_id.clone()),
                            &service,
                            EVENT_CANCEL,
                            json!({"reason": error.to_string(), "provider": provider.0}),
                        ),
                    },
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
    match command {
        OutboundBridgeCommand::Data(bytes) => port.send(&bytes).await,
        OutboundBridgeCommand::Control { operation, payload } if operation == OP_CREDIT => {
            let bytes = payload
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    HostError::InvalidEnvelope("plugin credit bytes are missing".to_owned())
                })?;
            port.grant_credit(bytes).await
        }
        OutboundBridgeCommand::Control { operation, .. } if operation == OP_HALF_CLOSE => {
            port.half_close().await
        }
        OutboundBridgeCommand::Control { operation, payload } if operation == OP_CANCEL => {
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("consumer_cancelled")
                .to_owned();
            port.cancel(reason).await
        }
        OutboundBridgeCommand::Control { operation, .. } => Err(HostError::InvalidEnvelope(
            format!("unknown plugin stream operation {operation:?}"),
        )),
    }
}

fn plugin_event_from_stream(
    request_id: &str,
    service: &str,
    envelope: StreamEnvelope,
) -> Result<PluginFrame> {
    if envelope.kind == StreamKind::Data {
        let payload = envelope.data.ok_or_else(|| {
            HostError::InvalidEnvelope("service DATA frame has no raw bytes".to_owned())
        })?;
        return Ok(PluginFrame::service_data_event(
            request_id, service, payload,
        ));
    }
    let (event, payload) = match envelope.kind {
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
        StreamKind::Open | StreamKind::HalfClose | StreamKind::Data => (
            EVENT_CANCEL,
            json!({"reason": "invalid_provider_stream_event"}),
        ),
    };
    Ok(PluginFrame::service_event(
        Some(request_id.to_owned()),
        service,
        event,
        payload,
    ))
}
