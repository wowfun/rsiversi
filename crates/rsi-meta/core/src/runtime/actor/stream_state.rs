use super::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_END, HostError, Lane, OP_CREDIT, Ordering, PluginFrame,
    Result, RuntimeActor, RuntimePhase, STREAM_BYTE_BUDGET, StreamEnvelope, StreamId, StreamKind,
    Value, json,
};

impl RuntimeActor {
    pub(super) fn handle_service_event(
        &mut self,
        stream_id: &StreamId,
        service: &str,
        event: &str,
        payload: Value,
    ) {
        let Some(stream) = self.streams.get_mut(stream_id) else {
            // A terminal frame may race a host-owned cancellation. Once the
            // host has removed the stream, a late terminal is harmless.
            return;
        };
        if stream.terminal {
            return;
        }
        if stream.service.as_str() != service {
            let expected = stream.service.to_string();
            let _ = stream;
            self.fail_stream_reason(
                stream_id,
                &format!(
                    "plugin_protocol_fault: service mismatch, expected {expected:?}, got {service:?}"
                ),
            );
            return;
        }
        match event {
            EVENT_CREDIT => {
                let Some(bytes) = payload.get("bytes").and_then(Value::as_u64) else {
                    self.fail_stream(
                        stream_id,
                        HostError::InvalidEnvelope("plugin credit has no byte count".to_owned()),
                    );
                    return;
                };
                if let Err(error) = stream.send_credit.add(bytes) {
                    self.fail_stream(stream_id, error);
                    return;
                }
                let mut envelope = StreamEnvelope::new(stream_id.clone(), StreamKind::Credit);
                envelope.credit_bytes = Some(bytes);
                let delivery = stream.events.try_send(Ok(envelope));
                let _ = stream;
                if delivery.is_err() {
                    self.finish_stream(
                        stream_id,
                        StreamKind::Cancel,
                        Some(json!({"reason": "slow_receiver"})),
                    );
                }
            }
            EVENT_END => self.finish_stream(stream_id, StreamKind::End, Some(payload)),
            EVENT_CANCEL
                if payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| !reason.is_empty()) =>
            {
                self.finish_stream(stream_id, StreamKind::Cancel, Some(payload));
            }
            EVENT_CANCEL => self.fail_stream_reason(
                stream_id,
                "plugin_protocol_fault: cancel event requires a non-empty reason",
            ),
            _ => self.fail_stream_reason(
                stream_id,
                &format!("plugin_protocol_fault: unknown service event {event:?}"),
            ),
        }
    }

    pub(super) fn handle_service_data_event(
        &mut self,
        stream_id: &StreamId,
        service: &str,
        payload: Vec<u8>,
    ) {
        let Some(stream) = self.streams.get_mut(stream_id) else {
            return;
        };
        if stream.terminal {
            return;
        }
        if stream.service.as_str() != service {
            let expected = stream.service.to_string();
            let _ = stream;
            self.fail_stream_reason(
                stream_id,
                &format!(
                    "plugin_protocol_fault: service mismatch, expected {expected:?}, got {service:?}",
                ),
            );
            return;
        }
        let raw_bytes = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        if raw_bytes > stream.receive_credit {
            let available = stream.receive_credit;
            let _ = stream;
            self.fail_stream(
                stream_id,
                HostError::StreamByteBudgetExceeded {
                    stream_id: stream_id.to_string(),
                    requested: raw_bytes,
                    available,
                },
            );
            return;
        }
        stream.receive_credit -= raw_bytes;
        stream.next_output_sequence = stream.next_output_sequence.saturating_add(1);
        let mut envelope = StreamEnvelope::new(stream_id.clone(), StreamKind::Data);
        envelope.sequence = Some(stream.next_output_sequence);
        envelope.data = Some(payload);
        let delivery = stream.events.try_send(Ok(envelope));
        let _ = stream;
        if delivery.is_err() {
            self.finish_stream(
                stream_id,
                StreamKind::Cancel,
                Some(json!({"reason": "slow_receiver"})),
            );
        }
    }

    pub(super) fn grant_credit(&mut self, stream_id: &StreamId, bytes: u64) -> Result<()> {
        let stream = self
            .streams
            .get_mut(stream_id)
            .ok_or_else(|| HostError::StreamClosed {
                stream_id: stream_id.to_string(),
            })?;
        stream.receive_credit = stream
            .receive_credit
            .checked_add(bytes)
            .filter(|credit| *credit <= STREAM_BYTE_BUDGET)
            .ok_or_else(|| HostError::StreamByteBudgetExceeded {
                stream_id: stream_id.to_string(),
                requested: bytes,
                available: STREAM_BYTE_BUDGET.saturating_sub(stream.receive_credit),
            })?;
        self.dispatch_stream_request(stream_id, OP_CREDIT, json!({"bytes": bytes}))
    }

    pub(super) fn dispatch_stream_request(
        &mut self,
        stream_id: &StreamId,
        operation: &'static str,
        payload: Value,
    ) -> Result<()> {
        let service = self
            .streams
            .get(stream_id)
            .ok_or_else(|| HostError::StreamClosed {
                stream_id: stream_id.to_string(),
            })?
            .service
            .clone();
        self.dispatch(
            Lane::Data,
            &PluginFrame::service_request(
                stream_id.to_string(),
                service.as_str(),
                operation,
                payload,
            ),
        )
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn fail_stream(&mut self, stream_id: &StreamId, error: HostError) {
        self.fail_stream_reason(stream_id, &error.to_string());
    }

    pub(super) fn fail_stream_reason(&mut self, stream_id: &StreamId, reason: &str) {
        if let Some(mut stream) = self.streams.remove(stream_id)
            && !stream.terminal
        {
            stream.terminal = true;
            stream.runtime_terminal.store(true, Ordering::Release);
            stream.send_credit.close();
            let mut terminal = StreamEnvelope::new(stream_id.clone(), StreamKind::Cancel);
            terminal.payload = Some(json!({"reason": reason}));
            if stream.events.try_send(Ok(terminal.clone())).is_err() {
                stream.terminal_fallback.store(terminal);
            }
        }
    }

    pub(super) fn finish_stream(
        &mut self,
        stream_id: &StreamId,
        kind: StreamKind,
        payload: Option<Value>,
    ) {
        if let Some(mut stream) = self.streams.remove(stream_id)
            && !stream.terminal
        {
            stream.terminal = true;
            stream.send_credit.close();
            let mut envelope = StreamEnvelope::new(stream_id.clone(), kind);
            envelope.payload = payload;
            if stream.events.try_send(Ok(envelope.clone())).is_err() {
                stream.terminal_fallback.store(envelope);
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn close_all_streams(&mut self, kind: StreamKind, payload: Option<Value>) {
        let stream_ids: Vec<_> = self.streams.keys().cloned().collect();
        for stream_id in stream_ids {
            self.finish_stream(&stream_id, kind, payload.clone());
        }
    }

    pub(super) fn protocol_fault(&mut self, reason: &str) {
        if self.phase == RuntimePhase::Faulted {
            return;
        }
        let reason = bounded_fault_reason(reason);
        self.phase = RuntimePhase::Faulted;
        self.control_output_open = false;
        self.data_output_open = false;
        self.healthy.store(false, Ordering::Release);
        *self
            .fault_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.clone());
        self.pending_runtime_fault = Some(super::super::RuntimeFault {
            instance: self.instance.clone(),
            generation: self.generation,
            reason: reason.clone(),
        });
        self.fail_pending_prepare(&reason);
        self.close_all_streams(
            StreamKind::Cancel,
            Some(json!({"reason": "plugin_protocol_fault", "message": reason})),
        );
        self.outbound_streams.clear();
    }
}

fn bounded_fault_reason(reason: &str) -> String {
    const MAX_FAULT_REASON_BYTES: usize = 512;
    if reason.len() <= MAX_FAULT_REASON_BYTES {
        return reason.to_owned();
    }
    let mut end = MAX_FAULT_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}
