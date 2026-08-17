use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    EVENT_CANCEL, EVENT_CREDIT, EVENT_END, Frame, FrameBody, LifecyclePhase, OP_CANCEL, OP_CREDIT,
    OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT, RUNTIME_TICK_SERVICE,
};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

const STREAM_CREDIT_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerConfig {
    request_id: String,
    message: String,
}

#[derive(Debug)]
struct Candidate {
    generation: u64,
    config: ConsumerConfig,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    request_prefix: String,
    message: String,
}

#[derive(Debug)]
struct ProxyStream {
    inner_id: String,
    input_credit: u64,
    output_credit: u64,
    reserved_input_credit: u64,
    reserved_output_credit: u64,
    pending_posts: VecDeque<PendingPost>,
    half_closed: bool,
}

#[derive(Clone, Debug)]
struct PendingPost {
    bytes: Vec<u8>,
    input_charge: u64,
    output_charge: u64,
    terminal_outer: bool,
}

struct NestedConsumer {
    host: Host,
    candidate: Option<Candidate>,
    active: Option<Active>,
    pending_retired: Option<u64>,
    pending_retirement_frames: VecDeque<Frame>,
    streams: BTreeMap<String, ProxyStream>,
    outer_by_inner: BTreeMap<String, String>,
}

#[derive(Debug)]
struct ConsumerError(&'static str);

impl fmt::Display for ConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl NestedConsumer {
    #[allow(clippy::needless_pass_by_value)] // Protocol call sites construct one-shot frames inline.
    fn post(&self, lane: Lane, frame: Frame) -> Result<(), ConsumerError> {
        let bytes = frame.encode().map_err(|_| ConsumerError("encode frame"))?;
        match self
            .host
            .post_frame(lane, &bytes)
            .map_err(|_| ConsumerError("host unavailable"))?
        {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(ConsumerError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(ConsumerError("host closed"))
            }
        }
    }

    fn open_outer(&mut self, outer_id: &str, payload: &Value) -> Result<(), ConsumerError> {
        if payload.get("consumer").and_then(Value::as_str).is_none()
            || payload.get("sequence").and_then(Value::as_u64) != Some(0)
            || self.streams.contains_key(outer_id)
        {
            return Err(ConsumerError("invalid outer stream open"));
        }
        let active = self
            .active
            .as_ref()
            .ok_or(ConsumerError("consumer is not committed"))?;
        let inner_id = format!("{}/{}", active.request_prefix, outer_id);
        if self.outer_by_inner.contains_key(&inner_id) {
            return Err(ConsumerError("inner stream id collision"));
        }
        let message = active.message.clone();
        self.streams.insert(
            outer_id.to_owned(),
            ProxyStream {
                inner_id: inner_id.clone(),
                input_credit: 0,
                output_credit: 0,
                reserved_input_credit: 0,
                reserved_output_credit: 0,
                pending_posts: VecDeque::new(),
                half_closed: false,
            },
        );
        self.outer_by_inner
            .insert(inner_id.clone(), outer_id.to_owned());
        self.enqueue_frame(
            outer_id,
            Frame::service_request(
                inner_id,
                "fixture.echo",
                OP_OPEN,
                json!({
                    "consumer": "fixture.nested-consumer",
                    "sequence": 0,
                    "proxy_message": message,
                }),
            ),
            0,
            0,
            false,
        )
    }

    fn grant_outer_output(&mut self, outer_id: &str, payload: &Value) -> Result<(), ConsumerError> {
        let bytes = credit_bytes(payload)?;
        {
            let stream = self
                .streams
                .get_mut(outer_id)
                .ok_or(ConsumerError("unknown outer stream"))?;
            stream.output_credit = add_credit(stream.output_credit, bytes)?;
        }
        self.flush_pending(outer_id)?;
        let Some(stream) = self.streams.get(outer_id) else {
            // Credit can race a terminal that was retained under backpressure.
            return Ok(());
        };
        if stream
            .pending_posts
            .iter()
            .any(|pending| pending.terminal_outer)
        {
            return Ok(());
        }
        let inner_id = stream.inner_id.clone();
        self.enqueue_frame(
            outer_id,
            Frame::service_request(inner_id, "fixture.echo", OP_CREDIT, json!({"bytes": bytes})),
            0,
            0,
            false,
        )
    }

    fn outer_data(&mut self, outer_id: &str, payload: Vec<u8>) -> Result<(), ConsumerError> {
        let raw_bytes = payload.len() as u64;
        let inner_id = {
            let stream = self
                .streams
                .get(outer_id)
                .ok_or(ConsumerError("unknown outer stream"))?;
            if stream.half_closed
                || stream
                    .input_credit
                    .saturating_sub(stream.reserved_input_credit)
                    < raw_bytes
            {
                return Err(ConsumerError("outer DATA exceeds inner credit"));
            }
            stream.inner_id.clone()
        };
        self.enqueue_frame(
            outer_id,
            Frame::service_data_request(inner_id, "fixture.echo", payload),
            raw_bytes,
            0,
            false,
        )
    }

    fn outer_half_close(&mut self, outer_id: &str, payload: Value) -> Result<(), ConsumerError> {
        if payload.get("sequence").and_then(Value::as_u64).is_none() {
            return Err(ConsumerError("half-close sequence missing"));
        }
        let inner_id = {
            let stream = self
                .streams
                .get_mut(outer_id)
                .ok_or(ConsumerError("unknown outer stream"))?;
            if stream.half_closed {
                return Err(ConsumerError("duplicate half close"));
            }
            stream.half_closed = true;
            stream.inner_id.clone()
        };
        self.enqueue_frame(
            outer_id,
            Frame::service_request(inner_id, "fixture.echo", OP_HALF_CLOSE, payload),
            0,
            0,
            false,
        )
    }

    fn cancel_outer(&mut self, outer_id: &str, payload: Value) -> Result<(), ConsumerError> {
        let stream = self
            .streams
            .get_mut(outer_id)
            .ok_or(ConsumerError("unknown outer stream"))?;
        let inner_id = stream.inner_id.clone();
        stream.pending_posts.clear();
        stream.reserved_input_credit = 0;
        stream.reserved_output_credit = 0;
        stream.half_closed = true;
        self.push_frame(
            outer_id,
            Frame::service_request(inner_id, "fixture.echo", OP_CANCEL, payload.clone()),
            0,
            0,
            false,
        )?;
        self.push_frame(
            outer_id,
            Frame::service_event(
                Some(outer_id.to_owned()),
                "fixture.nested-consumer",
                EVENT_CANCEL,
                payload,
            ),
            0,
            0,
            true,
        )?;
        self.flush_pending(outer_id)
    }

    fn inner_event(
        &mut self,
        inner_id: &str,
        event: &str,
        payload: Value,
    ) -> Result<(), ConsumerError> {
        let outer_id = self
            .outer_by_inner
            .get(inner_id)
            .cloned()
            .ok_or(ConsumerError("unknown inner stream"))?;
        match event {
            EVENT_CREDIT => {
                let bytes = credit_bytes(&payload)?;
                let stream = self
                    .streams
                    .get_mut(&outer_id)
                    .ok_or(ConsumerError("outer stream disappeared"))?;
                stream.input_credit = add_credit(stream.input_credit, bytes)?;
                self.enqueue_frame(
                    &outer_id,
                    Frame::service_event(
                        Some(outer_id.clone()),
                        "fixture.nested-consumer",
                        EVENT_CREDIT,
                        payload,
                    ),
                    0,
                    0,
                    false,
                )
            }
            EVENT_END | EVENT_CANCEL => {
                let stream = self
                    .streams
                    .get(&outer_id)
                    .ok_or(ConsumerError("outer stream disappeared"))?;
                if stream
                    .pending_posts
                    .iter()
                    .any(|pending| pending.terminal_outer)
                {
                    return self.flush_pending(&outer_id);
                }
                self.enqueue_frame(
                    &outer_id,
                    Frame::service_event(
                        Some(outer_id.clone()),
                        "fixture.nested-consumer",
                        event,
                        payload,
                    ),
                    0,
                    0,
                    true,
                )
            }
            _ => Err(ConsumerError("unknown inner stream event")),
        }
    }

    fn inner_data(&mut self, inner_id: &str, payload: Vec<u8>) -> Result<(), ConsumerError> {
        let outer_id = self
            .outer_by_inner
            .get(inner_id)
            .cloned()
            .ok_or(ConsumerError("unknown inner stream"))?;
        let raw_bytes = payload.len() as u64;
        let stream = self
            .streams
            .get(&outer_id)
            .ok_or(ConsumerError("outer stream disappeared"))?;
        if stream
            .output_credit
            .saturating_sub(stream.reserved_output_credit)
            < raw_bytes
        {
            return Err(ConsumerError("inner DATA exceeds outer credit"));
        }
        self.enqueue_frame(
            &outer_id,
            Frame::service_data_event(&outer_id, "fixture.nested-consumer", payload),
            0,
            raw_bytes,
            false,
        )
    }

    fn enqueue_frame(
        &mut self,
        outer_id: &str,
        frame: Frame,
        input_charge: u64,
        output_charge: u64,
        terminal_outer: bool,
    ) -> Result<(), ConsumerError> {
        self.push_frame(outer_id, frame, input_charge, output_charge, terminal_outer)?;
        self.flush_pending(outer_id)
    }

    #[allow(clippy::needless_pass_by_value)] // This is the one-shot frame serialization boundary.
    fn push_frame(
        &mut self,
        outer_id: &str,
        frame: Frame,
        input_charge: u64,
        output_charge: u64,
        terminal_outer: bool,
    ) -> Result<(), ConsumerError> {
        let bytes = frame.encode().map_err(|_| ConsumerError("encode frame"))?;
        let stream = self
            .streams
            .get_mut(outer_id)
            .ok_or(ConsumerError("outer stream disappeared"))?;
        let reserved_input_credit = stream
            .reserved_input_credit
            .checked_add(input_charge)
            .filter(|reserved| *reserved <= stream.input_credit)
            .ok_or(ConsumerError("outer DATA exceeds inner credit"))?;
        let reserved_output_credit = stream
            .reserved_output_credit
            .checked_add(output_charge)
            .filter(|reserved| *reserved <= stream.output_credit)
            .ok_or(ConsumerError("inner DATA exceeds outer credit"))?;
        stream.reserved_input_credit = reserved_input_credit;
        stream.reserved_output_credit = reserved_output_credit;
        stream.pending_posts.push_back(PendingPost {
            bytes,
            input_charge,
            output_charge,
            terminal_outer,
        });
        Ok(())
    }

    fn flush_pending(&mut self, outer_id: &str) -> Result<(), ConsumerError> {
        loop {
            let Some(pending) = self
                .streams
                .get(outer_id)
                .and_then(|stream| stream.pending_posts.front())
                .cloned()
            else {
                return Ok(());
            };
            match self
                .host
                .post_frame(Lane::Data, &pending.bytes)
                .map_err(|_| ConsumerError("host unavailable"))?
            {
                PostFrameOutcome::Accepted => {
                    let stream = self
                        .streams
                        .get_mut(outer_id)
                        .ok_or(ConsumerError("outer stream disappeared"))?;
                    stream.pending_posts.pop_front();
                    stream.input_credit -= pending.input_charge;
                    stream.reserved_input_credit -= pending.input_charge;
                    stream.output_credit -= pending.output_charge;
                    stream.reserved_output_credit -= pending.output_charge;
                    if pending.terminal_outer {
                        let stream = self
                            .streams
                            .remove(outer_id)
                            .ok_or(ConsumerError("outer stream disappeared"))?;
                        self.outer_by_inner.remove(&stream.inner_id);
                        return Ok(());
                    }
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(ConsumerError("host closed"));
                }
            }
        }
    }

    fn tick(&mut self, payload: &Value) -> Result<(), ConsumerError> {
        payload
            .get("tick")
            .and_then(Value::as_u64)
            .ok_or(ConsumerError("tick must be a u64"))?;
        self.flush_retirement_frames()?;
        let outer_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for outer_id in outer_ids {
            self.flush_pending(&outer_id)?;
        }
        self.flush_retired()
    }

    fn flush_retired(&mut self) -> Result<(), ConsumerError> {
        if !self.pending_retirement_frames.is_empty() {
            return Ok(());
        }
        let Some(generation) = self.pending_retired else {
            return Ok(());
        };
        let bytes = Frame::lifecycle(LifecyclePhase::Retired, generation, None)
            .encode()
            .map_err(|_| ConsumerError("encode frame"))?;
        match self
            .host
            .post_frame(Lane::Control, &bytes)
            .map_err(|_| ConsumerError("host unavailable"))?
        {
            PostFrameOutcome::Accepted => {
                self.pending_retired = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(ConsumerError("host closed"))
            }
        }
    }

    fn retire(&mut self, generation: u64) -> Result<(), ConsumerError> {
        let streams = std::mem::take(&mut self.streams);
        self.outer_by_inner.clear();
        for (outer_id, stream) in streams {
            let reason = json!({"reason": "consumer_retired"});
            self.pending_retirement_frames
                .push_back(Frame::service_request(
                    stream.inner_id,
                    "fixture.echo",
                    OP_CANCEL,
                    reason.clone(),
                ));
            self.pending_retirement_frames
                .push_back(Frame::service_event(
                    Some(outer_id),
                    "fixture.nested-consumer",
                    EVENT_CANCEL,
                    reason,
                ));
        }
        self.active = None;
        self.pending_retired = Some(generation);
        self.flush_retirement_frames()?;
        self.flush_retired()
    }

    fn flush_retirement_frames(&mut self) -> Result<(), ConsumerError> {
        while let Some(frame) = self.pending_retirement_frames.front() {
            let bytes = frame.encode().map_err(|_| ConsumerError("encode frame"))?;
            match self
                .host
                .post_frame(Lane::Data, &bytes)
                .map_err(|_| ConsumerError("host unavailable"))?
            {
                PostFrameOutcome::Accepted => {
                    self.pending_retirement_frames.pop_front();
                }
                PostFrameOutcome::WouldBlock => return Ok(()),
                PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                    return Err(ConsumerError("host closed"));
                }
            }
        }
        Ok(())
    }
}

impl Plugin for NestedConsumer {
    type Error = ConsumerError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            candidate: None,
            active: None,
            pending_retired: None,
            pending_retirement_frames: VecDeque::new(),
            streams: BTreeMap::new(),
            outer_by_inner: BTreeMap::new(),
        })
    }

    #[allow(clippy::too_many_lines)] // One exhaustive protocol-state transition table.
    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| ConsumerError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                config: Some(config),
            } if lane == Lane::Control => {
                let config: ConsumerConfig = serde_json::from_value(config)
                    .map_err(|_| ConsumerError("invalid consumer config"))?;
                if config.request_id.is_empty() {
                    return Err(ConsumerError("request_id must not be empty"));
                }
                self.candidate = Some(Candidate { generation, config });
                if let Err(error) = self.post(
                    Lane::Control,
                    Frame::lifecycle(LifecyclePhase::Prepared, generation, None),
                ) {
                    self.candidate = None;
                    return Err(error);
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => {
                if self
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.generation == generation)
                {
                    self.candidate = None;
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control => {
                let candidate = self
                    .candidate
                    .take()
                    .filter(|candidate| candidate.generation == generation)
                    .ok_or(ConsumerError("generation was not prepared"))?;
                self.active = Some(Active {
                    generation,
                    request_prefix: candidate.config.request_id,
                    message: candidate.config.message,
                });
                Ok(())
            }
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data
                && self.active.is_some()
                && service == "fixture.nested-consumer" =>
            {
                match operation.as_str() {
                    OP_OPEN => self.open_outer(&request_id, &payload),
                    OP_CREDIT => self.grant_outer_output(&request_id, &payload),
                    OP_HALF_CLOSE => self.outer_half_close(&request_id, payload),
                    OP_CANCEL => self.cancel_outer(&request_id, payload),
                    _ => Err(ConsumerError("unknown outer stream operation")),
                }
            }
            FrameBody::ServiceDataRequest {
                request_id,
                service,
                payload,
            } if lane == Lane::Data
                && self.active.is_some()
                && service == "fixture.nested-consumer" =>
            {
                self.outer_data(&request_id, payload)
            }
            FrameBody::ServiceDataEvent {
                request_id,
                service,
                payload,
            } if lane == Lane::Data && service == "fixture.echo" => {
                self.inner_data(&request_id, payload)
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                payload,
            } if lane == Lane::Data && service == "fixture.echo" => {
                self.inner_event(&request_id, &event, payload)
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
            } if lane == Lane::Control
                && self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.generation == generation) =>
            {
                self.retire(generation)
            }
            _ => Err(ConsumerError("frame rejected in current lifecycle state")),
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        self.streams.clear();
        self.outer_by_inner.clear();
        self.active = None;
        self.candidate = None;
        self.pending_retired = None;
        Ok(())
    }
}

fn credit_bytes(payload: &Value) -> Result<u64, ConsumerError> {
    payload
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or(ConsumerError("credit bytes missing"))
}

fn add_credit(current: u64, added: u64) -> Result<u64, ConsumerError> {
    current
        .checked_add(added)
        .filter(|credit| *credit <= STREAM_CREDIT_LIMIT)
        .ok_or(ConsumerError("credit overflow"))
}

rsi_meta_plugin::export_plugin!(NestedConsumer);
