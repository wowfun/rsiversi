use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL, OP_CREDIT,
    OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE, STATE_EVENT_APPLIED,
    STATE_EVENT_CONFLICT, STATE_EVENT_VALUE, STATE_OP_COMPARE_AND_SWAP, STATE_OP_GET,
};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde_json::{Value, json};

const INPUT_CREDIT: u64 = 1024 * 1024;
const STREAM_CREDIT_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
struct PendingIncrement {
    key: String,
    state_request_id: String,
}

#[derive(Debug)]
struct PendingOutput {
    payload: Value,
    data_accepted: bool,
}

#[derive(Debug)]
struct CounterStream {
    input_credit: u64,
    output_credit: u64,
    pending: Option<PendingIncrement>,
    pending_state_frame: Option<Frame>,
    pending_output: Option<PendingOutput>,
    half_closed: bool,
}

struct CasCounter {
    host: Host,
    prepared: Option<u64>,
    active: Option<u64>,
    pending_retired: Option<u64>,
    pending_terminals: VecDeque<Frame>,
    streams: BTreeMap<String, CounterStream>,
    stream_by_state_request: BTreeMap<String, String>,
}

#[derive(Debug)]
struct CounterError(&'static str);

impl fmt::Display for CounterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl CasCounter {
    fn post_outcome(&self, lane: Lane, frame: &Frame) -> Result<PostFrameOutcome, CounterError> {
        let bytes = frame.encode().map_err(|_| CounterError("encode frame"))?;
        self.host
            .post_frame(lane, &bytes)
            .map_err(|_| CounterError("host unavailable"))
    }

    fn post(&self, lane: Lane, frame: &Frame) -> Result<(), CounterError> {
        match self.post_outcome(lane, frame)? {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(CounterError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(CounterError("host closed"))
            }
        }
    }

    fn open(&mut self, stream_id: &str, payload: &Value) -> Result<(), CounterError> {
        if payload.get("consumer").and_then(Value::as_str).is_none()
            || payload.get("sequence").and_then(Value::as_u64) != Some(0)
            || self.streams.contains_key(stream_id)
        {
            return Err(CounterError("invalid stream open"));
        }
        self.streams.insert(
            stream_id.to_owned(),
            CounterStream {
                input_credit: INPUT_CREDIT,
                output_credit: 0,
                pending: None,
                pending_state_frame: None,
                pending_output: None,
                half_closed: false,
            },
        );
        let posted = self.post(
            Lane::Data,
            &Frame::service_event(
                Some(stream_id.to_owned()),
                "fixture.cas-counter",
                EVENT_CREDIT,
                json!({"bytes": INPUT_CREDIT}),
            ),
        );
        if posted.is_err() {
            self.streams.remove(stream_id);
        }
        posted
    }

    fn grant_output_credit(
        &mut self,
        stream_id: &str,
        payload: &Value,
    ) -> Result<(), CounterError> {
        let bytes = u64_field(payload, "bytes")?;
        let stream = self
            .streams
            .get_mut(stream_id)
            .ok_or(CounterError("unknown stream"))?;
        stream.output_credit = stream
            .output_credit
            .checked_add(bytes)
            .filter(|credit| *credit <= STREAM_CREDIT_LIMIT)
            .ok_or(CounterError("credit overflow"))?;
        self.flush_state_request(stream_id)?;
        self.flush_output(stream_id)
    }

    fn start_increment(&mut self, stream_id: &str, payload: &[u8]) -> Result<(), CounterError> {
        let request = decode_data(payload)?;
        let key = string_field(&request, "key")?;
        let raw_bytes = payload.len() as u64;
        let read_id = format!("{stream_id}/read");
        let stream = self
            .streams
            .get_mut(stream_id)
            .ok_or(CounterError("unknown stream"))?;
        if stream.half_closed || stream.pending.is_some() || stream.pending_output.is_some() {
            return Err(CounterError("stream already has an increment"));
        }
        if stream.input_credit < raw_bytes {
            return Err(CounterError("input credit exceeded"));
        }
        stream.input_credit -= raw_bytes;
        stream.pending = Some(PendingIncrement {
            key: key.clone(),
            state_request_id: read_id.clone(),
        });
        self.stream_by_state_request
            .insert(read_id.clone(), stream_id.to_owned());
        self.streams
            .get_mut(stream_id)
            .ok_or(CounterError("stream disappeared"))?
            .pending_state_frame = Some(Frame::service_request(
            read_id,
            "state.cas",
            STATE_OP_GET,
            json!({"key": key}),
        ));
        self.flush_state_request(stream_id)
    }

    fn state_response(
        &mut self,
        request_id: &str,
        event: &str,
        payload: Value,
    ) -> Result<(), CounterError> {
        let stream_id = self
            .stream_by_state_request
            .remove(request_id)
            .ok_or(CounterError("unknown state response"))?;
        let (key, expected_request) = {
            let stream = self
                .streams
                .get(&stream_id)
                .ok_or(CounterError("stream disappeared"))?;
            let pending = stream
                .pending
                .as_ref()
                .ok_or(CounterError("no pending increment"))?;
            (pending.key.clone(), pending.state_request_id.clone())
        };
        if expected_request != request_id || string_field(&payload, "key")? != key {
            return Err(CounterError("mismatched state response"));
        }
        match event {
            STATE_EVENT_VALUE | STATE_EVENT_CONFLICT => {
                let version = u64_field(&payload, "version")?;
                let current = nullable_counter(&payload)?;
                let next = current
                    .checked_add(1)
                    .ok_or(CounterError("counter overflow"))?;
                let cas_id = format!("{stream_id}/cas/{version}");
                let stream = self
                    .streams
                    .get_mut(&stream_id)
                    .ok_or(CounterError("stream disappeared"))?;
                stream.pending = Some(PendingIncrement {
                    key: key.clone(),
                    state_request_id: cas_id.clone(),
                });
                self.stream_by_state_request
                    .insert(cas_id.clone(), stream_id.clone());
                self.streams
                    .get_mut(&stream_id)
                    .ok_or(CounterError("stream disappeared"))?
                    .pending_state_frame = Some(Frame::service_request(
                    cas_id,
                    "state.cas",
                    STATE_OP_COMPARE_AND_SWAP,
                    json!({
                        "key": key,
                        "expected_version": version,
                        "value": next,
                    }),
                ));
                self.flush_state_request(&stream_id)
            }
            STATE_EVENT_APPLIED => {
                let stream = self
                    .streams
                    .get_mut(&stream_id)
                    .ok_or(CounterError("stream disappeared"))?;
                stream.pending = None;
                stream.pending_output = Some(PendingOutput {
                    payload,
                    data_accepted: false,
                });
                self.flush_output(&stream_id)
            }
            _ => Err(CounterError("unknown state response event")),
        }
    }

    fn flush_output(&mut self, stream_id: &str) -> Result<(), CounterError> {
        let Some((payload, data_accepted)) = self
            .streams
            .get(stream_id)
            .and_then(|stream| stream.pending_output.as_ref())
            .map(|pending| (pending.payload.clone(), pending.data_accepted))
        else {
            return Ok(());
        };
        if !data_accepted {
            let data = encode_data(&payload)?;
            let raw_bytes = data.len() as u64;
            let stream = self
                .streams
                .get(stream_id)
                .ok_or(CounterError("stream disappeared"))?;
            if stream.output_credit < raw_bytes {
                return Ok(());
            }
            match self.post_outcome(
                Lane::Data,
                &Frame::service_data_event(stream_id, "fixture.cas-counter", data),
            )? {
                PostFrameOutcome::Accepted => {
                    let stream = self
                        .streams
                        .get_mut(stream_id)
                        .ok_or(CounterError("stream disappeared"))?;
                    stream.output_credit -= raw_bytes;
                    stream
                        .pending_output
                        .as_mut()
                        .ok_or(CounterError("pending output disappeared"))?
                        .data_accepted = true;
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(CounterError("host closed"));
                }
            }
        }

        match self.post_outcome(
            Lane::Data,
            &Frame::service_event(
                Some(stream_id.to_owned()),
                "fixture.cas-counter",
                EVENT_END,
                json!({}),
            ),
        )? {
            PostFrameOutcome::Accepted => {
                self.remove_stream(stream_id);
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(CounterError("host closed"))
            }
        }
    }

    fn flush_state_request(&mut self, stream_id: &str) -> Result<(), CounterError> {
        let Some(frame) = self
            .streams
            .get(stream_id)
            .and_then(|stream| stream.pending_state_frame.clone())
        else {
            return Ok(());
        };
        match self.post_outcome(Lane::Data, &frame)? {
            PostFrameOutcome::Accepted => {
                self.streams
                    .get_mut(stream_id)
                    .ok_or(CounterError("stream disappeared"))?
                    .pending_state_frame = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(CounterError("host closed"))
            }
        }
    }

    fn tick(&mut self, payload: &Value) -> Result<(), CounterError> {
        payload
            .get("tick")
            .and_then(Value::as_u64)
            .ok_or(CounterError("tick must be a u64"))?;
        self.flush_terminals()?;
        let stream_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for stream_id in stream_ids {
            self.flush_state_request(&stream_id)?;
            if self.streams.contains_key(&stream_id) {
                self.flush_output(&stream_id)?;
            }
        }
        self.flush_retired()
    }

    fn flush_retired(&mut self) -> Result<(), CounterError> {
        if !self.pending_terminals.is_empty() {
            return Ok(());
        }
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
                Err(CounterError("host closed"))
            }
        }
    }

    fn cancel(&mut self, stream_id: &str, payload: Value) -> Result<(), CounterError> {
        if !self.remove_stream(stream_id) {
            return Err(CounterError("unknown stream"));
        }
        self.pending_terminals.push_back(Frame::service_event(
            Some(stream_id.to_owned()),
            "fixture.cas-counter",
            EVENT_CANCEL,
            payload,
        ));
        self.flush_terminals()
    }

    fn flush_terminals(&mut self) -> Result<(), CounterError> {
        while let Some(frame) = self.pending_terminals.front().cloned() {
            match self.post_outcome(Lane::Data, &frame)? {
                PostFrameOutcome::Accepted => {
                    self.pending_terminals.pop_front();
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(CounterError("host closed"));
                }
            }
        }
        Ok(())
    }

    fn remove_stream(&mut self, stream_id: &str) -> bool {
        let Some(stream) = self.streams.remove(stream_id) else {
            return false;
        };
        if let Some(pending) = stream.pending {
            self.stream_by_state_request
                .remove(&pending.state_request_id);
        }
        true
    }

    fn retire(&mut self, generation: u64) -> Result<(), CounterError> {
        let stream_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for stream_id in stream_ids {
            let _ = self.cancel(&stream_id, json!({"reason": "provider_retired"}));
        }
        self.active = None;
        self.pending_retired = Some(generation);
        self.flush_retired()
    }
}

impl Plugin for CasCounter {
    type Error = CounterError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            prepared: None,
            active: None,
            pending_retired: None,
            pending_terminals: VecDeque::new(),
            streams: BTreeMap::new(),
            stream_by_state_request: BTreeMap::new(),
        })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| CounterError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                ..
            } if lane == Lane::Control => {
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
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => {
                if self.prepared == Some(generation) {
                    self.prepared = None;
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control && self.prepared == Some(generation) => {
                self.prepared = None;
                self.active = Some(generation);
                Ok(())
            }
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data
                && self.active.is_some()
                && service == "fixture.cas-counter" =>
            {
                match operation.as_str() {
                    OP_OPEN => self.open(&request_id, &payload),
                    OP_CREDIT => self.grant_output_credit(&request_id, &payload),
                    OP_HALF_CLOSE => {
                        let stream = self
                            .streams
                            .get_mut(&request_id)
                            .ok_or(CounterError("unknown stream"))?;
                        stream.half_closed = true;
                        Ok(())
                    }
                    OP_CANCEL => self.cancel(&request_id, payload),
                    _ => Err(CounterError("unknown stream operation")),
                }
            }
            FrameBody::ServiceDataRequest {
                request_id,
                service,
                payload,
            } if lane == Lane::Data
                && self.active.is_some()
                && service == "fixture.cas-counter" =>
            {
                self.start_increment(&request_id, &payload)
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                payload,
            } if lane == Lane::Data && service == "state.cas" => {
                self.state_response(&request_id, &event, payload)
            }
            FrameBody::ServiceEvent {
                service,
                event,
                payload,
                ..
            } if matches!(lane, Lane::Control | Lane::Data)
                && service == RUNTIME_TICK_SERVICE
                && event == RUNTIME_TICK_EVENT =>
            {
                self.tick(&payload)
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control && self.active == Some(generation) => {
                self.retire(generation)
            }
            _ => Err(CounterError("frame rejected in current lifecycle state")),
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        self.streams.clear();
        self.stream_by_state_request.clear();
        self.active = None;
        self.prepared = None;
        self.pending_retired = None;
        Ok(())
    }
}

fn string_field(value: &Value, field: &'static str) -> Result<String, CounterError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(CounterError(field))
}

fn u64_field(value: &Value, field: &'static str) -> Result<u64, CounterError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(CounterError(field))
}

fn nullable_counter(value: &Value) -> Result<u64, CounterError> {
    match value.get("value") {
        Some(Value::Null) => Ok(0),
        Some(value) => value.as_u64().ok_or(CounterError("value")),
        None => Err(CounterError("value")),
    }
}

fn decode_data(payload: &[u8]) -> Result<Value, CounterError> {
    serde_json::from_slice(payload).map_err(|_| CounterError("DATA JSON is invalid"))
}

fn encode_data(value: &Value) -> Result<Vec<u8>, CounterError> {
    serde_json::to_vec(value).map_err(|_| CounterError("encode DATA JSON"))
}

rsi_meta_plugin::export_plugin!(CasCounter);
