use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use rsi_meta_frame_contract::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL,
    OP_CREDIT, OP_DATA, OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde_json::{Value, json};

const INITIAL_INPUT_CREDIT: u64 = 1024 * 1024;

#[derive(Debug)]
struct EchoStream {
    output_credit: u64,
    reserved_credit: u64,
    pending_output: VecDeque<PendingPost>,
    input_closed: bool,
}

#[derive(Clone, Debug)]
struct PendingPost {
    bytes: Vec<u8>,
    credit_charge: u64,
    terminal: bool,
}

struct EchoPlugin {
    host: Host,
    prepared: Option<u64>,
    committed: Option<u64>,
    pending_retired: Option<u64>,
    streams: BTreeMap<String, EchoStream>,
}

#[derive(Debug)]
struct EchoError(&'static str);

impl fmt::Display for EchoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl EchoPlugin {
    fn post_outcome(&self, lane: Lane, frame: &Frame) -> Result<PostFrameOutcome, EchoError> {
        let bytes = frame.encode().map_err(|_| EchoError("encode frame"))?;
        self.host
            .post_frame(lane, &bytes)
            .map_err(|_| EchoError("host unavailable"))
    }

    fn post(&self, lane: Lane, frame: &Frame) -> Result<(), EchoError> {
        match self.post_outcome(lane, frame)? {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(EchoError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(EchoError("host closed"))
            }
        }
    }

    fn open(&mut self, request_id: &str, payload: &Value) -> Result<(), EchoError> {
        if payload.get("consumer").and_then(Value::as_str).is_none()
            || payload.get("sequence").and_then(Value::as_u64) != Some(0)
            || self.streams.contains_key(request_id)
        {
            return Err(EchoError("invalid stream open"));
        }
        self.streams.insert(
            request_id.to_owned(),
            EchoStream {
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
                "fixture.echo",
                EVENT_CREDIT,
                json!({"bytes": INITIAL_INPUT_CREDIT}),
            ),
        );
        if posted.is_err() {
            self.streams.remove(request_id);
        }
        posted
    }

    fn grant_output_credit(&mut self, request_id: &str, payload: &Value) -> Result<(), EchoError> {
        let bytes = payload
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or(EchoError("credit bytes missing"))?;
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(EchoError("unknown stream"))?;
        stream.output_credit = stream
            .output_credit
            .checked_add(bytes)
            .ok_or(EchoError("credit overflow"))?;
        self.flush_output(request_id)
    }

    fn echo_data(&mut self, request_id: &str, payload: Value) -> Result<(), EchoError> {
        validate_byte_array(&payload)?;
        let encoded_len = serde_json::to_vec(&payload)
            .map_err(|_| EchoError("encode data payload"))?
            .len() as u64;
        let stream = self
            .streams
            .get(request_id)
            .ok_or(EchoError("unknown stream"))?;
        if stream.input_closed {
            return Err(EchoError("stream input is closed"));
        }
        if stream.output_credit.saturating_sub(stream.reserved_credit) < encoded_len {
            return Err(EchoError("output credit exceeded"));
        }
        self.enqueue_output(
            request_id,
            &Frame::service_event(
                Some(request_id.to_owned()),
                "fixture.echo",
                EVENT_DATA,
                payload,
            ),
            encoded_len,
            false,
        )
    }

    fn end(&mut self, request_id: &str, payload: &Value) -> Result<(), EchoError> {
        if payload.get("sequence").and_then(Value::as_u64).is_none() {
            return Err(EchoError("invalid half close"));
        }
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(EchoError("invalid half close"))?;
        if stream.input_closed {
            return Err(EchoError("invalid half close"));
        }
        stream.input_closed = true;
        self.enqueue_output(
            request_id,
            &Frame::service_event(
                Some(request_id.to_owned()),
                "fixture.echo",
                EVENT_END,
                json!({}),
            ),
            0,
            true,
        )
    }

    fn cancel(&mut self, request_id: &str, payload: Value) -> Result<(), EchoError> {
        if payload.get("reason").and_then(Value::as_str).is_none() {
            return Err(EchoError("invalid cancel"));
        }
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(EchoError("invalid cancel"))?;
        stream.pending_output.clear();
        stream.reserved_credit = 0;
        stream.input_closed = true;
        self.enqueue_output(
            request_id,
            &Frame::service_event(
                Some(request_id.to_owned()),
                "fixture.echo",
                EVENT_CANCEL,
                payload,
            ),
            0,
            true,
        )
    }

    fn enqueue_output(
        &mut self,
        request_id: &str,
        frame: &Frame,
        credit_charge: u64,
        terminal: bool,
    ) -> Result<(), EchoError> {
        let bytes = frame.encode().map_err(|_| EchoError("encode frame"))?;
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(EchoError("unknown stream"))?;
        stream.reserved_credit = stream
            .reserved_credit
            .checked_add(credit_charge)
            .filter(|reserved| *reserved <= stream.output_credit)
            .ok_or(EchoError("output credit exceeded"))?;
        stream.pending_output.push_back(PendingPost {
            bytes,
            credit_charge,
            terminal,
        });
        self.flush_output(request_id)
    }

    fn flush_output(&mut self, request_id: &str) -> Result<(), EchoError> {
        loop {
            let Some(pending) = self
                .streams
                .get(request_id)
                .and_then(|stream| stream.pending_output.front())
                .cloned()
            else {
                return Ok(());
            };
            let outcome = self
                .host
                .post_frame(Lane::Data, &pending.bytes)
                .map_err(|_| EchoError("host unavailable"))?;
            match outcome {
                PostFrameOutcome::Accepted => {
                    let stream = self
                        .streams
                        .get_mut(request_id)
                        .ok_or(EchoError("stream disappeared"))?;
                    stream.pending_output.pop_front();
                    stream.output_credit -= pending.credit_charge;
                    stream.reserved_credit -= pending.credit_charge;
                    if pending.terminal {
                        self.streams.remove(request_id);
                        return Ok(());
                    }
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(EchoError("host closed"));
                }
            }
        }
    }

    fn tick(&mut self, payload: &Value) -> Result<(), EchoError> {
        payload
            .get("tick")
            .and_then(Value::as_u64)
            .ok_or(EchoError("tick must be a u64"))?;
        let request_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for request_id in request_ids {
            self.flush_output(&request_id)?;
        }
        self.flush_retired()
    }

    fn flush_retired(&mut self) -> Result<(), EchoError> {
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
                Err(EchoError("host closed"))
            }
        }
    }
}

impl Plugin for EchoPlugin {
    type Error = EchoError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            prepared: None,
            committed: None,
            pending_retired: None,
            streams: BTreeMap::new(),
        })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| EchoError("invalid frame"))?;
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
                self.committed = Some(generation);
                self.prepared = None;
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control && self.committed == Some(generation) => {
                self.streams.clear();
                self.committed = None;
                self.pending_retired = Some(generation);
                self.flush_retired()
            }
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data && self.committed.is_some() && service == "fixture.echo" => {
                match operation.as_str() {
                    OP_OPEN => self.open(&request_id, &payload),
                    OP_CREDIT => self.grant_output_credit(&request_id, &payload),
                    OP_DATA => self.echo_data(&request_id, payload),
                    OP_HALF_CLOSE => self.end(&request_id, &payload),
                    OP_CANCEL => self.cancel(&request_id, payload),
                    _ => Err(EchoError("unknown stream operation")),
                }
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
            _ => Err(EchoError("frame rejected in current lifecycle state")),
        }
    }
}

fn validate_byte_array(value: &Value) -> Result<(), EchoError> {
    let bytes = value
        .as_array()
        .ok_or(EchoError("data is not a byte array"))?;
    if bytes
        .iter()
        .any(|byte| byte.as_u64().is_none_or(|byte| byte > u8::MAX.into()))
    {
        return Err(EchoError("data contains a non-byte value"));
    }
    Ok(())
}

rsi_meta_plugin::export_plugin!(EchoPlugin);
