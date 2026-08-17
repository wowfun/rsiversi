use super::{
    Arc, AtomicBool, BTreeMap, ByteCredit, CallOutcome, ClientDisconnect, Command, CommandEnvelope,
    ControlCommand, DATA_QUEUE_CAPACITY, DataCommand, DurablePluginCommand, EVENT_CANCEL,
    EVENT_CREDIT, EVENT_END, HostError, HostServiceCall, InstanceId, Lane, LifecycleCommand,
    LifecyclePhase, LoadedPlugin, MAX_STREAMS_PER_GENERATION, OP_CANCEL, OP_CREDIT, OP_HALF_CLOSE,
    OP_OPEN, Ordering, OutboundBridgeCommand, OutboundRoute, PluginCommandRequest, PluginFrame,
    PluginFrameBody, Result, STATE_SERVICE, STREAM_BYTE_BUDGET, ServiceKey, StreamEnvelope,
    StreamId, StreamKind, TICK_SERVICE, TerminalFallback, Value, json, mpsc, oneshot,
    run_outbound_bridge, watch,
};
use crate::frame::{durable_command_result, durable_command_unavailable};

mod stream_state;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RuntimePhase {
    Created,
    Preparing,
    Prepared,
    Committed,
    Retiring,
    Faulted,
}

pub(super) const fn runtime_tick_enabled(uses_runtime_tick: bool, phase: RuntimePhase) -> bool {
    uses_runtime_tick && matches!(phase, RuntimePhase::Committed | RuntimePhase::Retiring)
}

pub(super) struct RuntimeStream {
    service: ServiceKey,
    events: mpsc::Sender<Result<StreamEnvelope>>,
    send_credit: Arc<ByteCredit>,
    terminal_fallback: Arc<TerminalFallback>,
    runtime_terminal: Arc<AtomicBool>,
    receive_credit: u64,
    next_output_sequence: u64,
    terminal: bool,
}

#[allow(clippy::struct_excessive_bools)]
pub(super) struct RuntimeActor {
    pub(super) composition_id: String,
    pub(super) instance: InstanceId,
    pub(super) generation: u64,
    pub(super) capabilities: Vec<String>,
    pub(super) uses_state_service: bool,
    pub(super) uses_runtime_tick: bool,
    pub(super) phase: RuntimePhase,
    pub(super) loaded: LoadedPlugin,
    pub(super) lifecycle_receiver: mpsc::Receiver<LifecycleCommand>,
    pub(super) control_receiver: mpsc::Receiver<ControlCommand>,
    pub(super) disconnect_receiver: mpsc::Receiver<ClientDisconnect>,
    pub(super) data_receiver: mpsc::Receiver<DataCommand>,
    pub(super) self_control: mpsc::WeakSender<ControlCommand>,
    pub(super) self_data: mpsc::WeakSender<DataCommand>,
    pub(super) control_output: rsi_meta_loader::PluginLaneReceiver,
    pub(super) data_output: rsi_meta_loader::PluginLaneReceiver,
    pub(super) control_output_open: bool,
    pub(super) data_output_open: bool,
    pub(super) streams: BTreeMap<StreamId, RuntimeStream>,
    pub(super) outbound_routes: BTreeMap<ServiceKey, OutboundRoute>,
    pub(super) outbound_streams: BTreeMap<String, mpsc::Sender<OutboundBridgeCommand>>,
    pub(super) retired_sender: watch::Sender<bool>,
    pub(super) plugin_commands: mpsc::Sender<PluginCommandRequest>,
    pub(super) host_services: mpsc::Sender<HostServiceCall>,
    pub(super) max_frame_bytes: usize,
    pub(super) stopped_sender: watch::Sender<bool>,
    pub(super) healthy: Arc<AtomicBool>,
    pub(super) fault_reason: Arc<std::sync::Mutex<Option<String>>>,
    pub(super) runtime_faults: mpsc::Sender<super::RuntimeFault>,
    pub(super) pending_runtime_fault: Option<super::RuntimeFault>,
    pub(super) stop_replies: Vec<oneshot::Sender<()>>,
    pub(super) prepare_reply: Option<oneshot::Sender<Result<()>>>,
    pub(super) runtime_ticks: watch::Receiver<u64>,
}

impl RuntimeActor {
    pub(super) async fn run(mut self) {
        loop {
            if let Some(fault) = self.pending_runtime_fault.take()
                && self.runtime_faults.send(fault).await.is_err()
            {
                break;
            }
            tokio::select! {
                command = self.lifecycle_receiver.recv() => {
                    match command {
                        Some(command) => {
                            if self.handle_lifecycle(command) {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                Some(disconnect) = self.disconnect_receiver.recv() => {
                    let _ = self.cancel_stream(&disconnect.stream_id, &disconnect.reason);
                }
                command = self.control_receiver.recv() => {
                    match command {
                        Some(command) => {
                            if self.handle_control(command) {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                frame = self.control_output.recv(), if self.control_output_open => {
                    match frame {
                        Some(frame) => self.handle_plugin_output(Lane::Control, frame.payload()),
                        None => self.control_output_open = false,
                    }
                }
                changed = self.runtime_ticks.changed(), if runtime_tick_enabled(self.uses_runtime_tick, self.phase) => {
                    if changed.is_err() {
                        self.uses_runtime_tick = false;
                        continue;
                    }
                    let tick = *self.runtime_ticks.borrow_and_update();
                    let _ = self.dispatch_runtime_tick(tick);
                }
                frame = self.data_output.recv(), if self.data_output_open => {
                    match frame {
                        Some(frame) => self.handle_plugin_output(Lane::Data, frame.payload()),
                        None => self.data_output_open = false,
                    }
                }
                Some(command) = self.data_receiver.recv() => self.handle_data(command),
                else => break,
            }
        }
        self.close_all_streams(StreamKind::End, None);
        self.healthy.store(false, Ordering::Release);
        self.fail_pending_prepare("runtime stopped before prepare completed");
        let _ = self.loaded.shutdown();
        let stop_replies = std::mem::take(&mut self.stop_replies);
        drop(self.loaded);
        self.stopped_sender.send_replace(true);
        for reply in stop_replies {
            let _ = reply.send(());
        }
    }

    fn handle_lifecycle(&mut self, command: LifecycleCommand) -> bool {
        match command {
            LifecycleCommand::Prepare { config, reply } => {
                if self.prepare_reply.is_some() || self.phase != RuntimePhase::Created {
                    let _ =
                        reply.send(Err(self.lifecycle_transition_error(LifecyclePhase::Prepare)));
                    return false;
                }
                match self.dispatch(
                    Lane::Control,
                    &PluginFrame::lifecycle(LifecyclePhase::Prepare, self.generation, Some(config)),
                ) {
                    Ok(()) => {
                        self.phase = RuntimePhase::Preparing;
                        self.prepare_reply = Some(reply);
                    }
                    Err(error) => {
                        self.phase = RuntimePhase::Faulted;
                        self.healthy.store(false, Ordering::Release);
                        let _ = reply.send(Err(error));
                    }
                }
                false
            }
            LifecycleCommand::Commit { reply } => {
                let phase = LifecyclePhase::Committed;
                let valid_transition = self.phase == RuntimePhase::Prepared;
                let result = if valid_transition {
                    self.dispatch(
                        Lane::Control,
                        &PluginFrame::lifecycle(phase, self.generation, None),
                    )
                } else {
                    Err(self.lifecycle_transition_error(phase))
                };
                if valid_transition {
                    self.phase = RuntimePhase::Committed;
                }
                let _ = reply.send(result);
                false
            }
            LifecycleCommand::Retire { reply } => {
                let phase = LifecyclePhase::Retire;
                let valid_transition = self.phase == RuntimePhase::Committed;
                let result = if valid_transition {
                    self.dispatch(
                        Lane::Control,
                        &PluginFrame::lifecycle(phase, self.generation, None),
                    )
                } else {
                    Err(self.lifecycle_transition_error(phase))
                };
                if valid_transition {
                    self.phase = RuntimePhase::Retiring;
                }
                let _ = reply.send(result);
                false
            }
            LifecycleCommand::Abort { reply } => {
                let result = self.dispatch(
                    Lane::Control,
                    &PluginFrame::lifecycle(LifecyclePhase::Abort, self.generation, None),
                );
                self.fail_pending_prepare("plugin prepare was aborted");
                self.phase = RuntimePhase::Faulted;
                self.healthy.store(false, Ordering::Release);
                let _ = reply.send(result);
                false
            }
            LifecycleCommand::Stop { reply } => {
                self.fail_pending_prepare("runtime stopped during prepare");
                self.stop_replies.push(reply);
                true
            }
        }
    }

    fn handle_control(&mut self, command: ControlCommand) -> bool {
        match command {
            command @ ControlCommand::Open { .. } => {
                self.handle_open(command);
                false
            }
            ControlCommand::GrantCredit {
                stream_id,
                bytes,
                reply,
            } => {
                let result = self.grant_credit(&stream_id, bytes);
                if let Err(error) = &result {
                    self.fail_stream_reason(&stream_id, &error.to_string());
                }
                let _ = reply.send(result);
                false
            }
            ControlCommand::HalfClose {
                stream_id,
                sequence,
                reply,
            } => {
                let result = self.dispatch_stream_request(
                    &stream_id,
                    OP_HALF_CLOSE,
                    json!({"sequence": sequence}),
                );
                if let Err(error) = &result {
                    self.fail_stream_reason(&stream_id, &error.to_string());
                }
                let _ = reply.send(result);
                false
            }
            ControlCommand::Cancel {
                stream_id,
                reason,
                reply,
            } => {
                let result = self.cancel_stream(&stream_id, &reason);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                false
            }
            ControlCommand::HostServiceResponse(frame) => {
                if let Err(error) = self.dispatch(Lane::Data, &frame) {
                    self.protocol_fault(&format!(
                        "cannot deliver host-service response to plugin: {error}"
                    ));
                }
                false
            }
            ControlCommand::PluginCommandResponse(frame) => {
                if let Err(error) = self.dispatch(Lane::Control, &frame) {
                    self.protocol_fault(&format!(
                        "cannot deliver durable-command response to plugin: {error}"
                    ));
                }
                false
            }
        }
    }

    fn dispatch_runtime_tick(&mut self, tick: u64) -> Result<()> {
        if !runtime_tick_enabled(self.uses_runtime_tick, self.phase) {
            return Ok(());
        }
        self.dispatch(
            Lane::Control,
            &PluginFrame::service_event(None, TICK_SERVICE, "tick", json!({"tick": tick})),
        )
    }

    fn handle_open(&mut self, command: ControlCommand) {
        let ControlCommand::Open {
            stream_id,
            service,
            payload,
            events,
            send_credit,
            terminal_fallback,
            runtime_terminal,
        } = command
        else {
            unreachable!("handle_open receives only Open commands")
        };
        if self.phase != RuntimePhase::Committed {
            runtime_terminal.store(true, Ordering::Release);
            let _ = events.try_send(Err(HostError::PluginRuntimeNotCommitted {
                instance: self.instance.clone(),
            }));
            return;
        }
        if self
            .streams
            .len()
            .saturating_add(self.outbound_streams.len())
            >= MAX_STREAMS_PER_GENERATION
        {
            runtime_terminal.store(true, Ordering::Release);
            let mut terminal = StreamEnvelope::new(stream_id.clone(), StreamKind::Cancel);
            terminal.payload = Some(json!({"reason": "stream_limit"}));
            if events.try_send(Ok(terminal.clone())).is_err() {
                terminal_fallback.store(terminal);
            }
            send_credit.close();
            return;
        }
        self.streams.insert(
            stream_id.clone(),
            RuntimeStream {
                service: service.clone(),
                events,
                send_credit,
                terminal_fallback,
                runtime_terminal,
                receive_credit: 0,
                next_output_sequence: 0,
                terminal: false,
            },
        );
        let frame =
            PluginFrame::service_request(stream_id.clone(), service.as_str(), OP_OPEN, payload);
        if let Err(error) = self.dispatch(Lane::Data, &frame) {
            self.fail_stream(&stream_id, error);
        }
    }

    fn handle_data(&mut self, command: DataCommand) {
        match command {
            DataCommand::Dispatch {
                stream_id,
                frame,
                reply,
            } => {
                let result = self.dispatch(Lane::Data, &frame);
                if let Err(error) = &result {
                    self.fail_stream_reason(&stream_id, &error.to_string());
                }
                let _ = reply.send(result);
            }
            DataCommand::OutboundEvent(frame) => {
                if let Err(error) = self.dispatch(Lane::Data, &frame) {
                    self.protocol_fault(&format!(
                        "cannot deliver nested service event to consumer plugin: {error}"
                    ));
                }
            }
            DataCommand::OutboundClosed { request_id } => {
                self.outbound_streams.remove(&request_id);
            }
        }
    }

    fn cancel_stream(&mut self, stream_id: &StreamId, reason: &str) -> Result<()> {
        let terminal_payload = json!({"reason": reason});
        let result = self.dispatch_stream_request(stream_id, OP_CANCEL, terminal_payload.clone());
        // Cancellation is host-owned and terminal even when a plugin accepts
        // the frame but never sends a matching acknowledgement.
        self.finish_stream(stream_id, StreamKind::Cancel, Some(terminal_payload));
        result
    }

    fn dispatch(&mut self, lane: Lane, frame: &PluginFrame) -> Result<()> {
        let bytes = frame.encode()?;
        if bytes.len() > self.max_frame_bytes {
            return Err(HostError::PluginFrameTooLarge {
                instance: self.instance.clone(),
                bytes: bytes.len(),
                maximum: self.max_frame_bytes,
            });
        }
        match self.loaded.dispatch(lane, &bytes) {
            CallOutcome::Ok => Ok(()),
            outcome => Err(HostError::PluginCallFailed {
                instance: self.instance.clone(),
                operation: format!("{lane:?} dispatch"),
                outcome: format!("{outcome:?}"),
            }),
        }
    }

    #[allow(clippy::too_many_lines)] // one exhaustive protocol-state transition table
    fn handle_plugin_output(&mut self, lane: Lane, bytes: &[u8]) {
        if bytes.len() > self.max_frame_bytes {
            self.protocol_fault("plugin emitted an oversized frame");
            return;
        }
        let frame = match PluginFrame::decode(bytes) {
            Ok(frame) => frame,
            Err(error) => {
                self.protocol_fault(&format!("plugin emitted a malformed frame: {error}"));
                return;
            }
        };
        match frame.body {
            PluginFrameBody::Lifecycle {
                phase: LifecyclePhase::Prepared,
                generation,
                config,
            } if lane == Lane::Control && generation == self.generation => {
                if self.phase != RuntimePhase::Preparing || config.is_some() {
                    self.protocol_fault("invalid Prepared lifecycle acknowledgement");
                    return;
                }
                self.phase = RuntimePhase::Prepared;
                let Some(reply) = self.prepare_reply.take() else {
                    self.protocol_fault("Prepared arrived without a pending prepare");
                    return;
                };
                let _ = reply.send(Ok(()));
            }
            PluginFrameBody::Lifecycle {
                phase: LifecyclePhase::PrepareFailed,
                generation,
                config,
            } if lane == Lane::Control && generation == self.generation => {
                if self.phase != RuntimePhase::Preparing {
                    self.protocol_fault("PrepareFailed arrived outside prepare");
                    return;
                }
                let error = match decode_prepare_failure(&self.instance, config) {
                    Ok(error) => error,
                    Err(reason) => {
                        self.protocol_fault(&reason.to_string());
                        return;
                    }
                };
                self.phase = RuntimePhase::Faulted;
                self.healthy.store(false, Ordering::Release);
                let Some(reply) = self.prepare_reply.take() else {
                    self.protocol_fault("PrepareFailed arrived without a pending prepare");
                    return;
                };
                let _ = reply.send(Err(error));
            }
            PluginFrameBody::Lifecycle {
                phase: LifecyclePhase::Retired,
                generation,
                config,
            } if lane == Lane::Control && generation == self.generation => {
                if self.phase == RuntimePhase::Retiring && config.is_none() {
                    self.retired_sender.send_replace(true);
                } else {
                    self.protocol_fault("Retired arrived outside retirement");
                }
            }
            PluginFrameBody::DurableCommand {
                command_id,
                command:
                    DurablePluginCommand::ApplyManifestPath {
                        manifest_path,
                        lock_path,
                    },
            } if lane == Lane::Control && self.phase == RuntimePhase::Committed => {
                let envelope = CommandEnvelope::new(
                    command_id.clone(),
                    Command::ApplyManifestPath {
                        manifest_path,
                        lock_path,
                    },
                );
                let (reply, response) = oneshot::channel();
                match self.plugin_commands.try_send(PluginCommandRequest {
                    composition_id: self.composition_id.clone(),
                    instance_id: self.instance.clone(),
                    generation: self.generation,
                    envelope,
                    reply: Some(reply),
                }) {
                    Ok(()) => {
                        let control = self.self_control.clone();
                        let instance = self.instance.clone();
                        tokio::spawn(async move {
                            let result = response.await.unwrap_or(Err(HostError::RegistryClosed));
                            if matches!(
                                result.as_ref().map(|outcome| &outcome.payload),
                                Err(_) | Ok(crate::protocol::CommandOutcome::Rejected { .. })
                            ) {
                                tracing::warn!(
                                    plugin_instance = %instance,
                                    operation_id = %command_id,
                                    "plugin durable command did not apply"
                                );
                            }
                            if let Some(control) = control.upgrade() {
                                let frame = durable_command_result(command_id, result);
                                let _ = control
                                    .send(ControlCommand::PluginCommandResponse(frame))
                                    .await;
                            }
                        });
                    }
                    Err(error) => {
                        let reason = match error {
                            mpsc::error::TrySendError::Full(_) => "command_queue_full",
                            mpsc::error::TrySendError::Closed(_) => "registry_unavailable",
                        };
                        let frame = PluginFrame::service_event(
                            Some(command_id),
                            "control.apply-manifest",
                            "failed",
                            json!({"code": reason}),
                        );
                        if let Err(error) = self.dispatch(Lane::Control, &frame) {
                            self.protocol_fault(&format!(
                                "cannot deliver durable-command admission failure: {error}"
                            ));
                        }
                    }
                }
            }
            PluginFrameBody::ServiceEvent {
                request_id: Some(stream_id),
                service,
                event,
                payload,
            } if lane == Lane::Data => {
                let Ok(stream_id) = StreamId::new(stream_id) else {
                    self.protocol_fault("plugin emitted an invalid stream identifier");
                    return;
                };
                self.handle_service_event(&stream_id, &service, &event, payload);
            }
            PluginFrameBody::ServiceDataEvent {
                request_id: stream_id,
                service,
                payload,
            } if lane == Lane::Data => {
                let Ok(stream_id) = StreamId::new(stream_id) else {
                    self.protocol_fault("plugin emitted an invalid stream identifier");
                    return;
                };
                self.handle_service_data_event(&stream_id, &service, payload);
            }
            PluginFrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data && matches!(service.as_str(), STATE_SERVICE) => {
                self.handle_host_service(request_id, service, operation, payload);
            }
            PluginFrameBody::ServiceRequest {
                request_id,
                service,
                operation,
                payload,
            } if lane == Lane::Data => {
                if self.phase == RuntimePhase::Committed {
                    self.handle_outbound_service_request(request_id, service, operation, payload);
                } else {
                    self.reject_outbound(
                        &request_id,
                        &service,
                        "service_unavailable_during_prepare",
                    );
                }
            }
            PluginFrameBody::ServiceDataRequest {
                request_id,
                service,
                payload,
            } if lane == Lane::Data => {
                if self.phase == RuntimePhase::Committed {
                    self.handle_outbound_service_data(&request_id, &service, payload);
                } else {
                    self.reject_outbound(
                        &request_id,
                        &service,
                        "service_unavailable_during_prepare",
                    );
                }
            }
            PluginFrameBody::DurableCommand { command_id, .. } if lane == Lane::Control => {
                let frame = durable_command_unavailable(command_id);
                if let Err(error) = self.dispatch(Lane::Control, &frame) {
                    tracing::warn!(
                        plugin_instance = %self.instance,
                        %error,
                        "plugin did not accept lifecycle durable-command rejection"
                    );
                }
            }
            _ => self.protocol_fault("plugin emitted a frame on an invalid lane or lifecycle"),
        }
    }

    fn handle_outbound_service_request(
        &mut self,
        request_id: String,
        service: String,
        operation: String,
        payload: Value,
    ) {
        if operation == OP_OPEN {
            if self.outbound_streams.contains_key(&request_id) {
                self.reject_outbound(&request_id, &service, "duplicate_stream_open");
                return;
            }
            if self
                .streams
                .len()
                .saturating_add(self.outbound_streams.len())
                >= MAX_STREAMS_PER_GENERATION
            {
                self.reject_outbound(&request_id, &service, "stream_limit");
                return;
            }
            let key = ServiceKey::new(service.clone());
            let Some(route) = self.outbound_routes.get(&key) else {
                self.reject_outbound(&request_id, &service, "unresolved_service");
                return;
            };
            let port = match route
                .runtime
                .open_stream_with_payload(ServiceKey::new(service.clone()), payload)
            {
                Ok(port) => port,
                Err(error) => {
                    self.reject_outbound(&request_id, &service, &error.to_string());
                    return;
                }
            };
            let Some(data) = self.self_data.upgrade() else {
                self.reject_outbound(&request_id, &service, "runtime_stopping");
                return;
            };
            let (commands, command_receiver) = mpsc::channel(DATA_QUEUE_CAPACITY);
            self.outbound_streams.insert(request_id.clone(), commands);
            tokio::spawn(run_outbound_bridge(
                request_id,
                service,
                route.provider.clone(),
                port,
                command_receiver,
                data,
            ));
            return;
        }

        let Some(commands) = self.outbound_streams.get(&request_id).cloned() else {
            self.reject_outbound(&request_id, &service, "unknown_stream");
            return;
        };
        let command = OutboundBridgeCommand::Control { operation, payload };
        if commands.try_send(command).is_err() {
            self.outbound_streams.remove(&request_id);
            self.reject_outbound(&request_id, &service, "stream_backpressure");
        }
    }

    fn handle_outbound_service_data(&mut self, request_id: &str, service: &str, payload: Vec<u8>) {
        let Some(commands) = self.outbound_streams.get(request_id).cloned() else {
            self.reject_outbound(request_id, service, "unknown_stream");
            return;
        };
        if commands
            .try_send(OutboundBridgeCommand::Data(payload))
            .is_err()
        {
            self.outbound_streams.remove(request_id);
            self.reject_outbound(request_id, service, "stream_backpressure");
        }
    }

    fn reject_outbound(&mut self, request_id: &str, service: &str, reason: &str) {
        let _ = self.dispatch(
            Lane::Data,
            &PluginFrame::service_event(
                Some(request_id.to_owned()),
                service,
                EVENT_CANCEL,
                json!({"reason": reason}),
            ),
        );
    }

    #[allow(clippy::too_many_lines)] // validates and maps the complete host-service request boundary
    fn handle_host_service(
        &mut self,
        request_id: String,
        service: String,
        operation: String,
        payload: Value,
    ) {
        if !matches!(
            self.phase,
            RuntimePhase::Preparing | RuntimePhase::Prepared | RuntimePhase::Committed
        ) {
            let frame = PluginFrame::service_event(
                Some(request_id),
                service,
                "conflict",
                json!({"reason": "service_unavailable_in_lifecycle_phase"}),
            );
            let _ = self.dispatch(Lane::Data, &frame);
            return;
        }
        if service == STATE_SERVICE && !self.uses_state_service {
            let frame = PluginFrame::service_event(
                Some(request_id),
                service,
                "conflict",
                json!({"reason": "service_not_injected"}),
            );
            let _ = self.dispatch(Lane::Data, &frame);
            return;
        }
        if !self
            .capabilities
            .iter()
            .any(|capability| capability == &service)
        {
            let frame = PluginFrame::service_event(
                Some(request_id),
                service,
                "conflict",
                json!({"reason": "capability_not_declared"}),
            );
            let _ = self.dispatch(Lane::Data, &frame);
            return;
        }
        if self.phase != RuntimePhase::Committed
            && matches!(operation.as_str(), "compare_and_swap" | "delete")
        {
            let frame = PluginFrame::service_event(
                Some(request_id),
                service,
                "conflict",
                json!({"reason": "prepare_read_only"}),
            );
            let _ = self.dispatch(Lane::Data, &frame);
            return;
        }
        let (reply, response) = oneshot::channel();
        let response_request_id = request_id.clone();
        let response_service = service.clone();
        if self
            .host_services
            .try_send(HostServiceCall {
                composition_id: self.composition_id.clone(),
                instance_id: self.instance.clone(),
                request_id,
                service,
                operation,
                payload,
                reply,
            })
            .is_err()
        {
            let frame = PluginFrame::service_event(
                Some(response_request_id),
                response_service,
                "conflict",
                json!({
                    "reason": "host_service_unavailable",
                    "code": "request_queue_unavailable",
                }),
            );
            if let Err(error) = self.dispatch(Lane::Data, &frame) {
                self.protocol_fault(&format!(
                    "cannot deliver host-service saturation response to plugin: {error}"
                ));
            }
            return;
        }
        let Some(control) = self.self_control.upgrade() else {
            return;
        };
        tokio::spawn(async move {
            let frame = match response.await {
                Ok(Ok(frame)) => frame,
                Ok(Err(error)) => PluginFrame::service_event(
                    Some(response_request_id),
                    response_service,
                    "conflict",
                    json!({
                        "reason": "host_service_rejected",
                        "code": host_service_error_code(&error),
                    }),
                ),
                Err(_) => PluginFrame::service_event(
                    Some(response_request_id),
                    response_service,
                    "conflict",
                    json!({
                        "reason": "host_service_unavailable",
                        "code": "response_dropped",
                    }),
                ),
            };
            let _ = control
                .send(ControlCommand::HostServiceResponse(frame))
                .await;
        });
    }

    fn lifecycle_transition_error(&self, phase: LifecyclePhase) -> HostError {
        HostError::InvalidEnvelope(format!(
            "invalid {:?} lifecycle transition from {:?} for plugin {}",
            phase, self.phase, self.instance
        ))
    }

    fn fail_pending_prepare(&mut self, message: &str) {
        if let Some(reply) = self.prepare_reply.take() {
            let _ = reply.send(Err(HostError::InvalidEnvelope(message.to_owned())));
        }
    }
}

fn host_service_error_code(error: &HostError) -> &'static str {
    match error {
        HostError::InvalidEnvelope(_) | HostError::Unsupported(_) => "invalid_request",
        HostError::Sqlite(_) => "storage_error",
        _ => "host_error",
    }
}

fn decode_prepare_failure(instance: &InstanceId, config: Option<Value>) -> Result<HostError> {
    let config = config
        .and_then(|config| config.as_object().cloned())
        .ok_or_else(|| HostError::InvalidEnvelope("PrepareFailed config is missing".to_owned()))?;
    if config
        .keys()
        .any(|key| !matches!(key.as_str(), "code" | "message"))
    {
        return Err(HostError::InvalidEnvelope(
            "PrepareFailed config contains an unknown field".to_owned(),
        ));
    }
    let code = config
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
        .ok_or_else(|| HostError::InvalidEnvelope("PrepareFailed code is invalid".to_owned()))?;
    let message = config
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("plugin rejected shadow prepare");
    if message.len() > 256 || message.chars().any(char::is_control) {
        return Err(HostError::InvalidEnvelope(
            "PrepareFailed message is invalid".to_owned(),
        ));
    }
    Ok(HostError::PluginPrepareFailed {
        instance: instance.clone(),
        code: code.to_owned(),
        message: message.to_owned(),
    })
}
