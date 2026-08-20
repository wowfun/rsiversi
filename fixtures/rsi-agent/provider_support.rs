use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use rsi_meta_plugin::sdk::Host;
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_END, Frame, FrameBody, Lane, LifecyclePhase, OP_CANCEL,
    OP_CREDIT, OP_HALF_CLOSE, OP_OPEN, PostFrameOutcome, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
    STREAM_BYTE_BUDGET,
};
use serde_json::{Value, json};

const MAX_PRIMARY_STREAMS_PER_LIFETIME: u64 = 2;

#[derive(Debug)]
pub(crate) struct InboundData {
    pub(crate) stream_id: String,
    pub(crate) service: &'static str,
    pub(crate) payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServiceObservation {
    pub(crate) open_attempts: u64,
    pub(crate) accepted_opens: u64,
    pub(crate) data_frames: u64,
    pub(crate) max_concurrent_streams: u64,
}

#[derive(Clone, Debug)]
struct PendingPost {
    bytes: Vec<u8>,
    credit_charge: u64,
    terminal: bool,
}

#[derive(Debug)]
struct ProviderStream {
    service: &'static str,
    output_credit: u64,
    reserved_credit: u64,
    pending_output: VecDeque<PendingPost>,
    input_closed: bool,
}

pub(crate) struct ProviderIo {
    host: Host,
    primary_service: &'static str,
    observer_service: &'static str,
    prepared: Option<u64>,
    committed: Option<u64>,
    pending_retired: Option<u64>,
    primary_open_attempts: u64,
    primary_accepted_opens: u64,
    primary_data_frames: u64,
    primary_active_streams: u64,
    primary_max_concurrent_streams: u64,
    observer_opened: bool,
    streams: BTreeMap<String, ProviderStream>,
}

impl fmt::Debug for ProviderIo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderIo")
            .field("primary_service", &self.primary_service)
            .field("observer_service", &self.observer_service)
            .field("prepared", &self.prepared)
            .field("committed", &self.committed)
            .field("stream_count", &self.streams.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProviderIoError(&'static str);

impl ProviderIoError {
    pub(crate) const fn protocol(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for ProviderIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl ProviderIo {
    pub(crate) fn new(
        host: Host,
        primary_service: &'static str,
        observer_service: &'static str,
    ) -> Self {
        Self {
            host,
            primary_service,
            observer_service,
            prepared: None,
            committed: None,
            pending_retired: None,
            primary_open_attempts: 0,
            primary_accepted_opens: 0,
            primary_data_frames: 0,
            primary_active_streams: 0,
            primary_max_concurrent_streams: 0,
            observer_opened: false,
            streams: BTreeMap::new(),
        }
    }

    pub(crate) const fn observation(&self) -> ServiceObservation {
        ServiceObservation {
            open_attempts: self.primary_open_attempts,
            accepted_opens: self.primary_accepted_opens,
            data_frames: self.primary_data_frames,
            max_concurrent_streams: self.primary_max_concurrent_streams,
        }
    }

    pub(crate) fn receive(
        &mut self,
        lane: Lane,
        payload: &[u8],
    ) -> Result<Option<InboundData>, ProviderIoError> {
        let frame = Frame::decode(payload).map_err(|_| ProviderIoError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                ..
            } if lane == Lane::Control => {
                self.prepare(generation)?;
                Ok(None)
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => {
                if self.prepared == Some(generation) {
                    self.prepared = None;
                }
                Ok(None)
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control && self.prepared == Some(generation) => {
                self.prepared = None;
                self.committed = Some(generation);
                Ok(None)
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control && self.committed == Some(generation) => {
                self.streams.clear();
                self.primary_active_streams = 0;
                self.committed = None;
                self.pending_retired = Some(generation);
                self.flush_retired()?;
                Ok(None)
            }
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data => {
                self.receive_service_request(&request_id, &service, &operation, payload)?;
                Ok(None)
            }
            FrameBody::ServiceDataRequest {
                request_id,
                service,
                payload,
            } if lane == Lane::Data => self
                .receive_service_data(request_id, &service, payload)
                .map(Some),
            FrameBody::ServiceEvent {
                service,
                event,
                payload,
                ..
            } if matches!(lane, Lane::Control | Lane::Data)
                && service == RUNTIME_TICK_SERVICE
                && event == RUNTIME_TICK_EVENT =>
            {
                payload
                    .get("tick")
                    .and_then(Value::as_u64)
                    .ok_or(ProviderIoError("tick must be a u64"))?;
                self.flush_all()?;
                Ok(None)
            }
            _ => Err(ProviderIoError("frame rejected in current lifecycle state")),
        }
    }

    fn receive_service_request(
        &mut self,
        request_id: &str,
        service: &str,
        operation: &str,
        payload: Value,
    ) -> Result<(), ProviderIoError> {
        if self.committed.is_none() {
            return Err(ProviderIoError("service request before commit"));
        }
        let service = self
            .resolve_service(service)
            .ok_or(ProviderIoError("unknown service contract"))?;
        match operation {
            OP_OPEN => self.open(request_id, service, &payload),
            OP_CREDIT => self.grant_output_credit(request_id, service, &payload),
            OP_HALF_CLOSE => self.half_close(request_id, service, &payload),
            OP_CANCEL => self.cancel(request_id, service, payload),
            _ => Err(ProviderIoError("unknown stream operation")),
        }
    }

    fn receive_service_data(
        &mut self,
        request_id: String,
        service: &str,
        payload: Vec<u8>,
    ) -> Result<InboundData, ProviderIoError> {
        if self.committed.is_none() {
            return Err(ProviderIoError("service DATA before commit"));
        }
        let service = self
            .resolve_service(service)
            .ok_or(ProviderIoError("unknown service contract"))?;
        let stream = self
            .streams
            .get(&request_id)
            .ok_or(ProviderIoError("unknown stream"))?;
        if stream.input_closed {
            return Err(ProviderIoError("stream input is closed"));
        }
        if stream.service != service {
            return Err(ProviderIoError("stream service mismatch"));
        }
        if service == self.primary_service {
            self.primary_data_frames = self.primary_data_frames.saturating_add(1);
        }
        Ok(InboundData {
            stream_id: request_id,
            service,
            payload,
        })
    }

    pub(crate) fn send_data(
        &mut self,
        stream_id: &str,
        payload: Vec<u8>,
    ) -> Result<(), ProviderIoError> {
        let service = self
            .streams
            .get(stream_id)
            .ok_or(ProviderIoError("unknown stream"))?
            .service;
        let credit_charge =
            u64::try_from(payload.len()).map_err(|_| ProviderIoError("response too large"))?;
        self.enqueue(
            stream_id,
            &Frame::service_data_event(stream_id, service, payload),
            credit_charge,
            false,
        )
    }

    fn prepare(&mut self, generation: u64) -> Result<(), ProviderIoError> {
        if self.prepared.is_some() || self.committed.is_some() {
            return Err(ProviderIoError("invalid prepare state"));
        }
        self.prepared = Some(generation);
        if let Err(error) = self.post(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Prepared, generation, None),
        ) {
            self.prepared = None;
            return Err(error);
        }
        Ok(())
    }

    fn open(
        &mut self,
        request_id: &str,
        service: &'static str,
        payload: &Value,
    ) -> Result<(), ProviderIoError> {
        let primary = service == self.primary_service;
        if primary {
            self.primary_open_attempts = self.primary_open_attempts.saturating_add(1);
        }
        if payload.get("consumer").and_then(Value::as_str).is_none()
            || payload.get("sequence").and_then(Value::as_u64) != Some(0)
            || (primary && self.primary_accepted_opens >= MAX_PRIMARY_STREAMS_PER_LIFETIME)
            || (!primary && self.observer_opened)
            || self.streams.contains_key(request_id)
        {
            return Err(ProviderIoError("invalid stream open"));
        }
        self.streams.insert(
            request_id.to_owned(),
            ProviderStream {
                service,
                output_credit: 0,
                reserved_credit: 0,
                pending_output: VecDeque::new(),
                input_closed: false,
            },
        );
        let posted = self.post(
            Lane::Data,
            &Frame::service_event(
                Some(request_id.to_owned()),
                service,
                EVENT_CREDIT,
                json!({"bytes": STREAM_BYTE_BUDGET}),
            ),
        );
        if posted.is_err() {
            self.streams.remove(request_id);
        } else if primary {
            self.primary_accepted_opens = self.primary_accepted_opens.saturating_add(1);
            self.primary_active_streams = self.primary_active_streams.saturating_add(1);
            self.primary_max_concurrent_streams = self
                .primary_max_concurrent_streams
                .max(self.primary_active_streams);
        } else {
            self.observer_opened = true;
        }
        posted
    }

    fn grant_output_credit(
        &mut self,
        request_id: &str,
        service: &'static str,
        payload: &Value,
    ) -> Result<(), ProviderIoError> {
        let bytes = payload
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or(ProviderIoError("credit bytes missing"))?;
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProviderIoError("unknown stream"))?;
        if stream.service != service {
            return Err(ProviderIoError("stream service mismatch"));
        }
        stream.output_credit = stream
            .output_credit
            .checked_add(bytes)
            .filter(|total| *total <= STREAM_BYTE_BUDGET)
            .ok_or(ProviderIoError("credit limit exceeded"))?;
        self.flush_stream(request_id)
    }

    fn half_close(
        &mut self,
        request_id: &str,
        service: &'static str,
        payload: &Value,
    ) -> Result<(), ProviderIoError> {
        if payload.get("sequence").and_then(Value::as_u64).is_none() {
            return Err(ProviderIoError("half-close sequence missing"));
        }
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProviderIoError("unknown stream"))?;
        if stream.input_closed || stream.service != service {
            return Err(ProviderIoError("duplicate or mismatched half close"));
        }
        stream.input_closed = true;
        self.enqueue(
            request_id,
            &Frame::service_event(Some(request_id.to_owned()), service, EVENT_END, json!({})),
            0,
            true,
        )
    }

    fn cancel(
        &mut self,
        request_id: &str,
        service: &'static str,
        payload: Value,
    ) -> Result<(), ProviderIoError> {
        if payload
            .get("reason")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(ProviderIoError("cancel reason missing"));
        }
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProviderIoError("unknown stream"))?;
        if stream.service != service {
            return Err(ProviderIoError("stream service mismatch"));
        }
        stream.pending_output.clear();
        stream.reserved_credit = 0;
        stream.input_closed = true;
        self.enqueue(
            request_id,
            &Frame::service_event(Some(request_id.to_owned()), service, EVENT_CANCEL, payload),
            0,
            true,
        )
    }

    fn enqueue(
        &mut self,
        request_id: &str,
        frame: &Frame,
        credit_charge: u64,
        terminal: bool,
    ) -> Result<(), ProviderIoError> {
        let bytes = frame
            .encode()
            .map_err(|_| ProviderIoError("encode output frame"))?;
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProviderIoError("unknown stream"))?;
        stream.reserved_credit = stream
            .reserved_credit
            .checked_add(credit_charge)
            .filter(|reserved| *reserved <= stream.output_credit)
            .ok_or(ProviderIoError("output credit exceeded"))?;
        stream.pending_output.push_back(PendingPost {
            bytes,
            credit_charge,
            terminal,
        });
        self.flush_stream(request_id)
    }

    fn flush_stream(&mut self, request_id: &str) -> Result<(), ProviderIoError> {
        loop {
            let Some(pending) = self
                .streams
                .get(request_id)
                .and_then(|stream| stream.pending_output.front())
                .cloned()
            else {
                return Ok(());
            };
            match self.post_bytes(Lane::Data, &pending.bytes)? {
                PostFrameOutcome::Accepted => {
                    let stream = self
                        .streams
                        .get_mut(request_id)
                        .ok_or(ProviderIoError("stream disappeared"))?;
                    stream.pending_output.pop_front();
                    stream.output_credit -= pending.credit_charge;
                    stream.reserved_credit -= pending.credit_charge;
                    if pending.terminal {
                        let removed = self
                            .streams
                            .remove(request_id)
                            .ok_or(ProviderIoError("stream disappeared"))?;
                        if removed.service == self.primary_service {
                            self.primary_active_streams = self
                                .primary_active_streams
                                .checked_sub(1)
                                .ok_or(ProviderIoError("primary stream count underflow"))?;
                        }
                        return Ok(());
                    }
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(ProviderIoError("host closed"));
                }
            }
        }
    }

    fn flush_all(&mut self) -> Result<(), ProviderIoError> {
        let stream_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for stream_id in stream_ids {
            self.flush_stream(&stream_id)?;
        }
        self.flush_retired()
    }

    fn flush_retired(&mut self) -> Result<(), ProviderIoError> {
        let Some(generation) = self.pending_retired else {
            return Ok(());
        };
        match self.post_outcome(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Retired, generation, None),
        )? {
            PostFrameOutcome::Accepted => {
                self.pending_retired = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(ProviderIoError("host closed"))
            }
        }
    }

    fn post(&self, lane: Lane, frame: &Frame) -> Result<(), ProviderIoError> {
        match self.post_outcome(lane, frame)? {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(ProviderIoError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(ProviderIoError("host closed"))
            }
        }
    }

    fn post_outcome(&self, lane: Lane, frame: &Frame) -> Result<PostFrameOutcome, ProviderIoError> {
        let bytes = frame
            .encode()
            .map_err(|_| ProviderIoError("encode frame"))?;
        self.post_bytes(lane, &bytes)
    }

    fn post_bytes(&self, lane: Lane, bytes: &[u8]) -> Result<PostFrameOutcome, ProviderIoError> {
        self.host
            .post_frame(lane, bytes)
            .map_err(|_| ProviderIoError("host unavailable"))
    }

    fn resolve_service(&self, service: &str) -> Option<&'static str> {
        if service == self.primary_service {
            Some(self.primary_service)
        } else if service == self.observer_service {
            Some(self.observer_service)
        } else {
            None
        }
    }
}
