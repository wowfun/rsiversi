use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use rsi_meta_frame_contract::{
    DurableCommand, EVENT_CANCEL, EVENT_CREDIT, EVENT_DATA, EVENT_END, Frame, FrameBody,
    LifecyclePhase, OP_CANCEL, OP_CREDIT, OP_DATA, OP_HALF_CLOSE, OP_OPEN, RUNTIME_TICK_EVENT,
    RUNTIME_TICK_SERVICE, STATE_EVENT_APPLIED, STATE_EVENT_CONFLICT, STATE_EVENT_VALUE,
    STATE_OP_COMPARE_AND_SWAP, STATE_OP_GET,
};
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{Lane, PostFrameOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

const SERVICE: &str = "fixture.lifecycle-probe";
const INITIAL_INPUT_CREDIT: u64 = 1024 * 1024;
const MAX_ABORTED_PREPARE_REQUESTS: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RetireMode {
    Ack,
    Hold,
    Reject,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeConfig {
    fail_prepare: bool,
    retire_mode: RetireMode,
    tag: String,
    #[serde(default)]
    prepare_action: PrepareAction,
    #[serde(default)]
    stream_fault: StreamFault,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PrepareAction {
    #[default]
    StateGetThenAck,
    StateWriteThenAck,
    MalformedStateThenFail,
    NormalAck,
    DurableThenAck,
    OutboundOpenThenAck,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum StreamFault {
    #[default]
    None,
    WrongService,
    UnknownEvent,
    NonByteData,
    MalformedJson,
}

#[derive(Debug)]
struct Candidate {
    generation: u64,
    config: ProbeConfig,
    state_key: Option<String>,
    state_request_id: Option<String>,
    state: CandidateState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateState {
    Reading,
    Prepared,
    Failed,
}

#[derive(Debug)]
struct Active {
    generation: u64,
    config: ProbeConfig,
    retiring: bool,
}

#[derive(Debug)]
struct ProbeStream {
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

struct LifecycleProbe {
    host: Host,
    candidate: Option<Candidate>,
    active: Option<Active>,
    pending_retired: Option<u64>,
    aborted_prepare_requests: VecDeque<String>,
    streams: BTreeMap<String, ProbeStream>,
}

#[derive(Debug)]
struct ProbeError(&'static str);

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl LifecycleProbe {
    fn discard_aborted_prepare_response(&mut self, request_id: &str) -> bool {
        let Some(position) = self
            .aborted_prepare_requests
            .iter()
            .position(|aborted| aborted == request_id)
        else {
            return false;
        };
        self.aborted_prepare_requests.remove(position);
        true
    }

    #[allow(clippy::too_many_lines)] // Keep the configurable prepare transcript in one state transition.
    fn prepare(
        &mut self,
        generation: u64,
        config: Option<serde_json::Value>,
    ) -> Result<(), ProbeError> {
        let config = config.ok_or(ProbeError("prepare config missing"))?;
        let config: ProbeConfig =
            serde_json::from_value(config).map_err(|_| ProbeError("invalid prepare config"))?;
        if config.fail_prepare {
            return self.post(
                Lane::Control,
                Frame::lifecycle(
                    LifecyclePhase::PrepareFailed,
                    generation,
                    Some(json!({
                        "code": "configured_prepare_failure",
                        "message": "prepare rejected by fixture configuration",
                    })),
                ),
            );
        }
        if self.candidate.is_some() {
            return Err(ProbeError("prepare already in progress"));
        }
        let action = config.prepare_action;
        self.candidate = Some(Candidate {
            generation,
            config,
            state_key: None,
            state_request_id: None,
            state: CandidateState::Reading,
        });
        let result = match action {
            PrepareAction::StateGetThenAck => {
                let state_key = format!(
                    "prepare/{}",
                    self.candidate
                        .as_ref()
                        .expect("candidate was just installed")
                        .config
                        .tag
                );
                let state_request_id = format!("prepare/{generation}/state");
                let candidate = self
                    .candidate
                    .as_mut()
                    .expect("candidate was just installed");
                candidate.state_key = Some(state_key.clone());
                candidate.state_request_id = Some(state_request_id.clone());
                self.post(
                    Lane::Data,
                    Frame::service_request(
                        state_request_id,
                        "state.cas",
                        STATE_OP_GET,
                        json!({"key": state_key}),
                    ),
                )
            }
            PrepareAction::StateWriteThenAck => {
                let state_key = format!(
                    "prepare/{}",
                    self.candidate
                        .as_ref()
                        .expect("candidate was just installed")
                        .config
                        .tag
                );
                let state_request_id = format!("prepare/{generation}/state-write");
                let candidate = self
                    .candidate
                    .as_mut()
                    .expect("candidate was just installed");
                candidate.state_key = Some(state_key.clone());
                candidate.state_request_id = Some(state_request_id.clone());
                self.post(
                    Lane::Data,
                    Frame::service_request(
                        state_request_id,
                        "state.cas",
                        STATE_OP_COMPARE_AND_SWAP,
                        json!({
                            "key": state_key,
                            "expected_version": 0,
                            "value": {"probe": "must-not-persist"},
                        }),
                    ),
                )
            }
            PrepareAction::MalformedStateThenFail => {
                let state_request_id = format!("prepare/{generation}/malformed-state");
                self.candidate
                    .as_mut()
                    .expect("candidate was just installed")
                    .state_request_id = Some(state_request_id.clone());
                self.post(
                    Lane::Data,
                    Frame::service_request(state_request_id, "state.cas", STATE_OP_GET, json!({})),
                )
            }
            PrepareAction::NormalAck => self.ack_prepared(generation),
            PrepareAction::DurableThenAck => self
                .post(
                    Lane::Control,
                    Frame::durable_command(
                        format!("probe-prepare-{generation}"),
                        DurableCommand::ApplyManifestPath {
                            manifest_path: "probe-forbidden.toml".into(),
                            lock_path: "probe-forbidden.lock".into(),
                        },
                    ),
                )
                .and_then(|()| self.ack_prepared(generation)),
            PrepareAction::OutboundOpenThenAck => self
                .candidate
                .as_mut()
                .ok_or(ProbeError("candidate was just installed"))
                .map(|candidate| {
                    candidate.state_request_id = Some(format!("prepare/{generation}/outbound"));
                    candidate
                        .state_request_id
                        .clone()
                        .expect("outbound request id was just installed")
                })
                .and_then(|request_id| {
                    self.post(
                        Lane::Data,
                        Frame::service_request(
                            request_id,
                            "fixture.echo",
                            OP_OPEN,
                            json!({"consumer": "fixture.lifecycle-probe", "sequence": 0}),
                        ),
                    )
                }),
        };
        if let Err(error) = result {
            self.candidate = None;
            return Err(error);
        }
        Ok(())
    }

    fn ack_prepared(&mut self, generation: u64) -> Result<(), ProbeError> {
        self.post(
            Lane::Control,
            Frame::lifecycle(LifecyclePhase::Prepared, generation, None),
        )?;
        self.candidate
            .as_mut()
            .filter(|candidate| candidate.generation == generation)
            .ok_or(ProbeError("prepare candidate disappeared"))?
            .state = CandidateState::Prepared;
        Ok(())
    }

    fn prepare_state_response(
        &mut self,
        request_id: &str,
        event: &str,
        payload: &Value,
    ) -> Result<(), ProbeError> {
        if self.discard_aborted_prepare_response(request_id) {
            return Ok(());
        }
        let candidate = self
            .candidate
            .as_ref()
            .filter(|candidate| {
                candidate.state == CandidateState::Reading
                    && candidate.state_request_id.as_deref() == Some(request_id)
            })
            .ok_or(ProbeError("unexpected prepare state response"))?;
        let action = candidate.config.prepare_action;
        let generation = candidate.generation;
        let (phase, config, next_state) = match action {
            PrepareAction::StateGetThenAck => {
                if payload.get("key").and_then(Value::as_str) != candidate.state_key.as_deref() {
                    return Err(ProbeError("mismatched prepare state key"));
                }
                match event {
                    STATE_EVENT_VALUE => (LifecyclePhase::Prepared, None, CandidateState::Prepared),
                    STATE_EVENT_CONFLICT => (
                        LifecyclePhase::PrepareFailed,
                        Some(json!({
                            "code": "state_read_failed",
                            "message": "state.cas prepare read returned conflict",
                        })),
                        CandidateState::Failed,
                    ),
                    _ => return Err(ProbeError("unknown prepare state response")),
                }
            }
            PrepareAction::StateWriteThenAck => match event {
                STATE_EVENT_CONFLICT
                    if payload.get("reason").and_then(Value::as_str)
                        == Some("prepare_read_only") =>
                {
                    (LifecyclePhase::Prepared, None, CandidateState::Prepared)
                }
                STATE_EVENT_APPLIED => (
                    LifecyclePhase::PrepareFailed,
                    Some(json!({
                        "code": "prepare_write_applied",
                        "message": "state.cas write was applied during prepare",
                    })),
                    CandidateState::Failed,
                ),
                STATE_EVENT_CONFLICT => (
                    LifecyclePhase::PrepareFailed,
                    Some(json!({
                        "code": "prepare_write_wrong_conflict",
                        "message": "state.cas write returned an unexpected conflict",
                    })),
                    CandidateState::Failed,
                ),
                _ => return Err(ProbeError("unknown prepare state response")),
            },
            PrepareAction::MalformedStateThenFail => {
                let rejected = event == STATE_EVENT_CONFLICT
                    && payload.get("reason").and_then(Value::as_str)
                        == Some("host_service_rejected")
                    && payload.get("code").and_then(Value::as_str) == Some("invalid_request");
                if !rejected {
                    return Err(ProbeError("malformed state request was not rejected"));
                }
                (
                    LifecyclePhase::PrepareFailed,
                    Some(json!({
                        "code": "malformed_state_rejected",
                        "message": "host rejected malformed state.cas prepare request",
                    })),
                    CandidateState::Failed,
                )
            }
            PrepareAction::NormalAck
            | PrepareAction::DurableThenAck
            | PrepareAction::OutboundOpenThenAck => {
                return Err(ProbeError("unexpected prepare state response"));
            }
        };
        self.post(Lane::Control, Frame::lifecycle(phase, generation, config))?;
        self.candidate
            .as_mut()
            .ok_or(ProbeError("prepare candidate disappeared"))?
            .state = next_state;
        Ok(())
    }

    fn prepare_outbound_response(
        &mut self,
        request_id: &str,
        event: &str,
        payload: &Value,
    ) -> Result<(), ProbeError> {
        if self.discard_aborted_prepare_response(request_id) {
            return Ok(());
        }
        let candidate = self
            .candidate
            .as_ref()
            .filter(|candidate| {
                candidate.state == CandidateState::Reading
                    && candidate.config.prepare_action == PrepareAction::OutboundOpenThenAck
                    && candidate.state_request_id.as_deref() == Some(request_id)
            })
            .ok_or(ProbeError("unexpected prepare outbound response"))?;
        let generation = candidate.generation;
        if event == EVENT_CANCEL
            && payload.get("reason").and_then(Value::as_str)
                == Some("service_unavailable_during_prepare")
        {
            return self.ack_prepared(generation);
        }
        self.post(
            Lane::Control,
            Frame::lifecycle(
                LifecyclePhase::PrepareFailed,
                generation,
                Some(json!({
                    "code": "prepare_outbound_not_rejected",
                    "message": "outbound service open was not rejected during prepare",
                })),
            ),
        )?;
        self.candidate
            .as_mut()
            .ok_or(ProbeError("prepare candidate disappeared"))?
            .state = CandidateState::Failed;
        Ok(())
    }

    fn abort(&mut self, generation: u64) -> Result<(), ProbeError> {
        if self
            .candidate
            .as_ref()
            .is_none_or(|candidate| candidate.generation != generation)
        {
            return Err(ProbeError("abort generation is not prepared"));
        }
        let candidate = self.candidate.take().expect("matched candidate generation");
        if let Some(request_id) = candidate.state_request_id {
            if self.aborted_prepare_requests.len() == MAX_ABORTED_PREPARE_REQUESTS {
                self.aborted_prepare_requests.pop_front();
            }
            self.aborted_prepare_requests.push_back(request_id);
        }
        Ok(())
    }

    fn commit(&mut self, generation: u64) -> Result<(), ProbeError> {
        if self.candidate.as_ref().is_none_or(|candidate| {
            candidate.generation != generation || candidate.state != CandidateState::Prepared
        }) {
            return Err(ProbeError("commit generation is not prepared"));
        }
        let candidate = self
            .candidate
            .take()
            .ok_or(ProbeError("prepared candidate disappeared"))?;
        self.active = Some(Active {
            generation,
            config: candidate.config,
            retiring: false,
        });
        self.streams.clear();
        Ok(())
    }

    fn retire(&mut self, generation: u64) -> Result<(), ProbeError> {
        let retire_mode = {
            let active = self
                .active
                .as_mut()
                .filter(|active| active.generation == generation)
                .ok_or(ProbeError("retire generation is not active"))?;
            active.retiring = true;
            active.config.retire_mode
        };
        match retire_mode {
            RetireMode::Ack => {
                self.active = None;
                self.streams.clear();
                self.pending_retired = Some(generation);
                self.flush_retired()
            }
            RetireMode::Hold => Ok(()),
            RetireMode::Reject => Err(ProbeError("retirement rejected by configuration")),
        }
    }

    fn open(&mut self, request_id: &str, payload: &Value) -> Result<(), ProbeError> {
        if payload.get("consumer").and_then(Value::as_str).is_none()
            || payload.get("sequence").and_then(Value::as_u64) != Some(0)
            || self.streams.contains_key(request_id)
        {
            return Err(ProbeError("invalid stream open"));
        }
        self.streams.insert(
            request_id.to_owned(),
            ProbeStream {
                output_credit: 0,
                reserved_credit: 0,
                pending_output: VecDeque::new(),
                input_closed: false,
            },
        );
        let posted = self.post(
            Lane::Data,
            Frame::service_event(
                Some(request_id.to_owned()),
                SERVICE,
                EVENT_CREDIT,
                json!({"bytes": INITIAL_INPUT_CREDIT}),
            ),
        );
        if posted.is_err() {
            self.streams.remove(request_id);
        }
        posted
    }

    fn grant_output_credit(&mut self, request_id: &str, payload: &Value) -> Result<(), ProbeError> {
        let bytes = payload
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or(ProbeError("credit bytes missing"))?;
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProbeError("unknown stream"))?;
        stream.output_credit = stream
            .output_credit
            .checked_add(bytes)
            .ok_or(ProbeError("credit overflow"))?;
        self.flush_output(request_id)
    }

    fn data(&mut self, request_id: &str, payload: &Value) -> Result<(), ProbeError> {
        let input = byte_array(payload)?;
        let tag = &self
            .active
            .as_ref()
            .ok_or(ProbeError("plugin is not committed"))?
            .config
            .tag;
        let mut tagged = Vec::with_capacity(tag.len() + 1 + input.len());
        tagged.extend_from_slice(tag.as_bytes());
        tagged.push(0);
        tagged.extend_from_slice(&input);
        let output = json!(tagged);
        let fault = self
            .active
            .as_ref()
            .ok_or(ProbeError("plugin is not committed"))?
            .config
            .stream_fault;
        let stream = self
            .streams
            .get(request_id)
            .ok_or(ProbeError("unknown stream"))?;
        if stream.input_closed {
            return Err(ProbeError("stream input is closed"));
        }
        if fault == StreamFault::MalformedJson {
            return self.enqueue_bytes(request_id, b"{".to_vec(), 1, false);
        }
        let (service, event, output) = match fault {
            StreamFault::None => (SERVICE, EVENT_DATA, output),
            StreamFault::WrongService => ("fixture.lifecycle-probe.wrong", EVENT_DATA, output),
            StreamFault::UnknownEvent => (SERVICE, "unknown_event", output),
            StreamFault::NonByteData => (SERVICE, EVENT_DATA, json!({"not": "bytes"})),
            StreamFault::MalformedJson => unreachable!("handled before frame construction"),
        };
        let encoded_len = serde_json::to_vec(&output)
            .map_err(|_| ProbeError("encode data payload"))?
            .len() as u64;
        self.enqueue_frame(
            request_id,
            Frame::service_event(Some(request_id.to_owned()), service, event, output),
            encoded_len,
            false,
        )
    }

    fn half_close(&mut self, request_id: &str, payload: &Value) -> Result<(), ProbeError> {
        if payload.get("sequence").and_then(Value::as_u64).is_none() {
            return Err(ProbeError("invalid half close"));
        }
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProbeError("invalid half close"))?;
        if stream.input_closed {
            return Err(ProbeError("invalid half close"));
        }
        stream.input_closed = true;
        self.enqueue_frame(
            request_id,
            Frame::service_event(Some(request_id.to_owned()), SERVICE, EVENT_END, json!({})),
            0,
            true,
        )
    }

    fn cancel(&mut self, request_id: &str, payload: Value) -> Result<(), ProbeError> {
        if payload.get("reason").and_then(Value::as_str).is_none() {
            return Err(ProbeError("invalid cancel"));
        }
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProbeError("invalid cancel"))?;
        stream.pending_output.clear();
        stream.reserved_credit = 0;
        stream.input_closed = true;
        self.enqueue_frame(
            request_id,
            Frame::service_event(Some(request_id.to_owned()), SERVICE, EVENT_CANCEL, payload),
            0,
            true,
        )
    }

    #[allow(clippy::needless_pass_by_value)] // Frames are one-shot values consumed by this queue boundary.
    fn enqueue_frame(
        &mut self,
        request_id: &str,
        frame: Frame,
        credit_charge: u64,
        terminal: bool,
    ) -> Result<(), ProbeError> {
        let bytes = frame.encode().map_err(|_| ProbeError("encode frame"))?;
        self.enqueue_bytes(request_id, bytes, credit_charge, terminal)
    }

    fn enqueue_bytes(
        &mut self,
        request_id: &str,
        bytes: Vec<u8>,
        credit_charge: u64,
        terminal: bool,
    ) -> Result<(), ProbeError> {
        let stream = self
            .streams
            .get_mut(request_id)
            .ok_or(ProbeError("unknown stream"))?;
        stream.reserved_credit = stream
            .reserved_credit
            .checked_add(credit_charge)
            .filter(|reserved| *reserved <= stream.output_credit)
            .ok_or(ProbeError("output credit exceeded"))?;
        stream.pending_output.push_back(PendingPost {
            bytes,
            credit_charge,
            terminal,
        });
        self.flush_output(request_id)
    }

    fn flush_output(&mut self, request_id: &str) -> Result<(), ProbeError> {
        loop {
            let Some(pending) = self
                .streams
                .get(request_id)
                .and_then(|stream| stream.pending_output.front())
                .cloned()
            else {
                return Ok(());
            };
            match self
                .host
                .post_frame(Lane::Data, &pending.bytes)
                .map_err(|_| ProbeError("host unavailable"))?
            {
                PostFrameOutcome::Accepted => {
                    let stream = self
                        .streams
                        .get_mut(request_id)
                        .ok_or(ProbeError("stream disappeared"))?;
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
                    return Err(ProbeError("host closed"));
                }
            }
        }
    }

    fn tick(&mut self, payload: &Value) -> Result<(), ProbeError> {
        payload
            .get("tick")
            .and_then(Value::as_u64)
            .ok_or(ProbeError("tick must be a u64"))?;
        let request_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for request_id in request_ids {
            self.flush_output(&request_id)?;
        }
        self.flush_retired()
    }

    fn flush_retired(&mut self) -> Result<(), ProbeError> {
        let Some(generation) = self.pending_retired else {
            return Ok(());
        };
        let bytes = Frame::lifecycle(LifecyclePhase::Retired, generation, None)
            .encode()
            .map_err(|_| ProbeError("encode frame"))?;
        match self
            .host
            .post_frame(Lane::Control, &bytes)
            .map_err(|_| ProbeError("host unavailable"))?
        {
            PostFrameOutcome::Accepted => {
                self.pending_retired = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(ProbeError("host closed"))
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)] // Lifecycle call sites construct one-shot frames inline.
    fn post(&self, lane: Lane, frame: Frame) -> Result<(), ProbeError> {
        let bytes = frame.encode().map_err(|_| ProbeError("encode frame"))?;
        self.post_bytes(lane, &bytes)
    }

    fn post_bytes(&self, lane: Lane, bytes: &[u8]) -> Result<(), ProbeError> {
        match self
            .host
            .post_frame(lane, bytes)
            .map_err(|_| ProbeError("host unavailable"))?
        {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(ProbeError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(ProbeError("host closed"))
            }
        }
    }
}

impl Plugin for LifecycleProbe {
    type Error = ProbeError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            candidate: None,
            active: None,
            pending_retired: None,
            aborted_prepare_requests: VecDeque::new(),
            streams: BTreeMap::new(),
        })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| ProbeError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                config,
            } if lane == Lane::Control => self.prepare(generation, config),
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => self.abort(generation),
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control => self.commit(generation),
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control => self.retire(generation),
            FrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data
                && service == SERVICE
                && self.active.as_ref().is_some_and(|active| !active.retiring) =>
            {
                match operation.as_str() {
                    OP_OPEN => self.open(&request_id, &payload),
                    OP_CREDIT => self.grant_output_credit(&request_id, &payload),
                    OP_DATA => self.data(&request_id, &payload),
                    OP_HALF_CLOSE => self.half_close(&request_id, &payload),
                    OP_CANCEL => self.cancel(&request_id, payload),
                    _ => Err(ProbeError("unknown stream operation")),
                }
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                payload,
            } if lane == Lane::Data && service == "state.cas" => {
                self.prepare_state_response(&request_id, &event, &payload)
            }
            FrameBody::ServiceEvent {
                request_id: Some(request_id),
                service,
                event,
                payload,
            } if lane == Lane::Data && service == "fixture.echo" => {
                self.prepare_outbound_response(&request_id, &event, &payload)
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
            _ => Err(ProbeError("frame rejected in current lifecycle state")),
        }
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        self.candidate = None;
        self.active = None;
        self.pending_retired = None;
        self.aborted_prepare_requests.clear();
        self.streams.clear();
        Ok(())
    }
}

fn byte_array(value: &Value) -> Result<Vec<u8>, ProbeError> {
    value
        .as_array()
        .ok_or(ProbeError("data is not a byte array"))?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(ProbeError("data contains a non-byte value"))
        })
        .collect()
}

rsi_meta_plugin::export_plugin!(LifecycleProbe);
