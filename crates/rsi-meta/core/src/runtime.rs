use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use rsi_meta_loader::{
    LoadedPlugin, PluginLoader, PluginMailboxOptions, PluginPackage, StagedPlugin,
};
use rsi_meta_plugin::{CallOutcome, Lane};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};

#[cfg(test)]
use crate::frame::durable_command_unavailable;
use crate::frame::{DurablePluginCommand, LifecyclePhase, PluginFrame, PluginFrameBody};
use crate::model::{GenerationLease, InstanceId, ServiceKey};
use crate::protocol::{
    Command, CommandEnvelope, CommandOutcomeEnvelope, StreamEnvelope, StreamId, StreamKind,
};
use crate::{HostError, Result};

const DATA_QUEUE_CAPACITY: usize = 128;
const LIFECYCLE_QUEUE_CAPACITY: usize = 4;
const STREAM_EVENT_CAPACITY: usize = 64;
const MAX_STREAMS_PER_GENERATION: usize = 128;
const STREAM_BYTE_BUDGET: u64 = 16 * 1024 * 1024;
pub(crate) const RUNTIME_TICK_INTERVAL: Duration = Duration::from_millis(250);
const PLUGIN_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(30);

const OP_OPEN: &str = "open";
const OP_CREDIT: &str = "credit";
const OP_HALF_CLOSE: &str = "half_close";
const OP_CANCEL: &str = "cancel";
const EVENT_CREDIT: &str = "credit";
const EVENT_END: &str = "end";
const EVENT_CANCEL: &str = "cancel";

pub(crate) const STATE_SERVICE: &str = "state.cas";
pub(crate) const TICK_SERVICE: &str = "runtime.tick";

#[derive(Debug)]
pub(crate) struct HostServiceCall {
    pub composition_id: String,
    pub instance_id: InstanceId,
    pub request_id: String,
    pub service: String,
    pub operation: String,
    pub payload: Value,
    pub reply: oneshot::Sender<Result<PluginFrame>>,
}

pub(crate) struct PluginCommandRequest {
    pub composition_id: String,
    pub instance_id: InstanceId,
    pub generation: u64,
    pub envelope: CommandEnvelope,
    pub reply: Option<oneshot::Sender<Result<CommandOutcomeEnvelope>>>,
}

#[derive(Debug)]
pub(crate) struct RuntimeFault {
    pub instance: InstanceId,
    pub generation: u64,
    pub reason: String,
}

#[derive(Clone)]
pub(crate) struct RuntimeHandle {
    instance: InstanceId,
    generation: u64,
    max_frame_bytes: usize,
    lifecycle: mpsc::Sender<LifecycleCommand>,
    control: mpsc::Sender<ControlCommand>,
    disconnects: mpsc::Sender<ClientDisconnect>,
    data: mpsc::Sender<DataCommand>,
    retired: watch::Receiver<bool>,
    stopped: watch::Receiver<bool>,
    thread: Arc<StdMutex<Option<JoinHandle<()>>>>,
    healthy: Arc<AtomicBool>,
    fault_reason: Arc<StdMutex<Option<String>>>,
}

impl fmt::Debug for RuntimeHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeHandle")
            .field("instance", &self.instance)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl RuntimeHandle {
    pub(crate) fn instance(&self) -> &InstanceId {
        &self.instance
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub(crate) fn fault_reason(&self) -> Option<String> {
        self.fault_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        plugin_loader: &PluginLoader,
        staged: &StagedPlugin,
        composition_id: String,
        instance: InstanceId,
        generation: u64,
        capabilities: Vec<String>,
        uses_state_service: bool,
        uses_runtime_tick: bool,
        outbound_routes: BTreeMap<ServiceKey, OutboundRoute>,
        plugin_commands: mpsc::Sender<PluginCommandRequest>,
        host_services: mpsc::Sender<HostServiceCall>,
        runtime_faults: mpsc::Sender<RuntimeFault>,
        runtime_ticks: watch::Receiver<u64>,
    ) -> Result<Self> {
        let mailbox_options = PluginMailboxOptions::default();
        let max_frame_bytes = mailbox_options.max_frame_bytes;
        let (loaded, mailbox) = plugin_loader.load_queued(staged, mailbox_options)?;
        let (control_output, data_output) = mailbox.into_lanes();
        let (lifecycle, lifecycle_receiver) = mpsc::channel(LIFECYCLE_QUEUE_CAPACITY);
        let (control, control_receiver) = mpsc::channel(128);
        let (disconnects, disconnect_receiver) = mpsc::channel(MAX_STREAMS_PER_GENERATION);
        let (data, data_receiver) = mpsc::channel(DATA_QUEUE_CAPACITY);
        let (retired_sender, retired) = watch::channel(false);
        let (stopped_sender, stopped) = watch::channel(false);
        let healthy = Arc::new(AtomicBool::new(true));
        let fault_reason = Arc::new(StdMutex::new(None));
        let actor = RuntimeActor {
            composition_id,
            instance: instance.clone(),
            generation,
            capabilities,
            uses_state_service,
            uses_runtime_tick,
            phase: RuntimePhase::Created,
            loaded,
            lifecycle_receiver,
            control_receiver,
            disconnect_receiver,
            data_receiver,
            self_control: control.downgrade(),
            self_data: data.downgrade(),
            control_output,
            data_output,
            control_output_open: true,
            data_output_open: true,
            streams: BTreeMap::new(),
            outbound_routes,
            outbound_streams: BTreeMap::new(),
            retired_sender,
            plugin_commands,
            host_services,
            max_frame_bytes,
            stopped_sender,
            healthy: Arc::clone(&healthy),
            fault_reason: Arc::clone(&fault_reason),
            runtime_faults,
            pending_runtime_fault: None,
            stop_replies: Vec::new(),
            prepare_reply: None,
            runtime_ticks,
        };
        let event_loop = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|source| HostError::PluginRuntimeStart {
                instance: instance.clone(),
                source,
            })?;
        let runtime_thread = std::thread::Builder::new()
            .name(format!("rsi-meta-runtime-{generation}"))
            .spawn(move || event_loop.block_on(actor.run()))
            .map_err(|source| HostError::PluginRuntimeStart {
                instance: instance.clone(),
                source,
            })?;
        Ok(Self {
            instance,
            generation,
            max_frame_bytes,
            lifecycle,
            control,
            disconnects,
            data,
            retired,
            stopped,
            thread: Arc::new(StdMutex::new(Some(runtime_thread))),
            healthy,
            fault_reason,
        })
    }

    pub(crate) async fn prepare(&self, config: Value) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.lifecycle(
            LifecycleCommand::Prepare { config, reply },
            response,
            "prepared",
        )
        .await
    }

    pub(crate) async fn abort_and_stop(&self) {
        let (reply, response) = oneshot::channel();
        let _ = self
            .lifecycle(LifecycleCommand::Abort { reply }, response, "abort")
            .await;
        let _ = self.stop().await;
    }

    /// A durable commit cannot be rolled back, so failure of this acknowledgement
    /// requires the owning host to fail closed and recover in a fresh process.
    pub(crate) async fn committed(&self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.lifecycle(LifecycleCommand::Commit { reply }, response, "committed")
            .await
    }

    pub(crate) async fn retire(&self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.lifecycle(LifecycleCommand::Retire { reply }, response, "retired")
            .await
    }

    async fn lifecycle(
        &self,
        command: LifecycleCommand,
        response: oneshot::Receiver<Result<()>>,
        phase: &'static str,
    ) -> Result<()> {
        tokio::time::timeout(PLUGIN_LIFECYCLE_TIMEOUT, async {
            self.lifecycle
                .send(command)
                .await
                .map_err(|_| HostError::PluginRuntimeClosed {
                    instance: self.instance.clone(),
                })?;
            response.await.map_err(|_| HostError::PluginRuntimeClosed {
                instance: self.instance.clone(),
            })?
        })
        .await
        .map_err(|_| HostError::PluginLifecycleTimeout {
            instance: self.instance.clone(),
            phase,
        })?
    }

    pub(crate) fn open_stream(
        &self,
        consumer: &InstanceId,
        service: ServiceKey,
    ) -> Result<StreamPort> {
        self.open_stream_with_payload(service, json!({"consumer": consumer.0, "sequence": 0}))
    }

    fn open_stream_with_payload(&self, service: ServiceKey, payload: Value) -> Result<StreamPort> {
        if !self.healthy.load(Ordering::Acquire) {
            return Err(HostError::PluginRuntimeClosed {
                instance: self.instance.clone(),
            });
        }
        let stream_id = StreamId::new(uuid::Uuid::now_v7().to_string())
            .expect("UUIDv7 is a valid stream identifier");
        let (events, receiver) = mpsc::channel(STREAM_EVENT_CAPACITY);
        let send_credit = Arc::new(ByteCredit::new());
        let terminal_fallback = Arc::new(TerminalFallback::default());
        let runtime_terminal = Arc::new(AtomicBool::new(false));
        let data_frame_overhead =
            PluginFrame::service_data_request(stream_id.to_string(), service.as_str(), Vec::new())
                .encode()?
                .len();
        let disconnect =
            self.disconnects
                .clone()
                .try_reserve_owned()
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => HostError::PluginQueueFull {
                        instance: self.instance.clone(),
                        lane: "control",
                    },
                    mpsc::error::TrySendError::Closed(_) => HostError::PluginRuntimeClosed {
                        instance: self.instance.clone(),
                    },
                })?;
        self.control
            .try_send(ControlCommand::Open {
                stream_id: stream_id.clone(),
                service: service.clone(),
                payload,
                events,
                send_credit: Arc::clone(&send_credit),
                terminal_fallback: Arc::clone(&terminal_fallback),
                runtime_terminal: Arc::clone(&runtime_terminal),
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => HostError::PluginQueueFull {
                    instance: self.instance.clone(),
                    lane: "control",
                },
                mpsc::error::TrySendError::Closed(_) => HostError::PluginRuntimeClosed {
                    instance: self.instance.clone(),
                },
            })?;
        Ok(StreamPort {
            instance: self.instance.clone(),
            stream_id,
            service,
            control: self.control.clone(),
            disconnect: Some(disconnect),
            data: self.data.clone(),
            events: receiver,
            send_credit,
            terminal_fallback,
            runtime_terminal,
            max_frame_bytes: self.max_frame_bytes,
            data_frame_overhead,
            sequence: 0,
            half_closed: false,
            terminal: false,
        })
    }

    pub(crate) async fn wait_retired(&self) -> Result<()> {
        let mut retired = self.retired.clone();
        tokio::time::timeout(PLUGIN_LIFECYCLE_TIMEOUT, async {
            while !*retired.borrow() {
                if retired.changed().await.is_err() {
                    return Err(HostError::PluginRuntimeClosed {
                        instance: self.instance.clone(),
                    });
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| HostError::PluginLifecycleTimeout {
            instance: self.instance.clone(),
            phase: "retired",
        })?
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        let result = tokio::time::timeout(PLUGIN_LIFECYCLE_TIMEOUT, async {
            if self
                .lifecycle
                .send(LifecycleCommand::Stop { reply })
                .await
                .is_err()
            {
                self.wait_stopped().await;
                return Ok(());
            }
            response.await.map_err(|_| HostError::PluginRuntimeClosed {
                instance: self.instance.clone(),
            })
        })
        .await
        .map_err(|_| HostError::PluginLifecycleTimeout {
            instance: self.instance.clone(),
            phase: "stopped",
        })?;
        if result.is_ok() {
            self.join_thread().await?;
        }
        result
    }

    async fn wait_stopped(&self) {
        let mut stopped = self.stopped.clone();
        while !*stopped.borrow() {
            if stopped.changed().await.is_err() {
                return;
            }
        }
    }

    async fn join_thread(&self) -> Result<()> {
        let thread = self
            .thread
            .lock()
            .expect("runtime thread mutex poisoned")
            .take();
        let Some(thread) = thread else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || thread.join())
            .await
            .map_err(|_| HostError::PluginRuntimeClosed {
                instance: self.instance.clone(),
            })?
            .map_err(|_| HostError::PluginRuntimeClosed {
                instance: self.instance.clone(),
            })
    }
}

pub(crate) async fn abort_prepared_reverse(handles: &[RuntimeHandle]) {
    for handle in handles.iter().rev() {
        handle.abort_and_stop().await;
    }
}

pub(crate) struct StreamPort {
    instance: InstanceId,
    stream_id: StreamId,
    service: ServiceKey,
    control: mpsc::Sender<ControlCommand>,
    disconnect: Option<mpsc::OwnedPermit<ClientDisconnect>>,
    data: mpsc::Sender<DataCommand>,
    events: mpsc::Receiver<Result<StreamEnvelope>>,
    send_credit: Arc<ByteCredit>,
    terminal_fallback: Arc<TerminalFallback>,
    runtime_terminal: Arc<AtomicBool>,
    max_frame_bytes: usize,
    data_frame_overhead: usize,
    sequence: u64,
    half_closed: bool,
    terminal: bool,
}

impl fmt::Debug for StreamPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamPort")
            .field("stream_id", &self.stream_id)
            .field("service", &self.service)
            .field("sequence", &self.sequence)
            .field("half_closed", &self.half_closed)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl StreamPort {
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub(crate) async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        if self.half_closed || self.terminal {
            return Err(HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            });
        }
        let encoded_bytes = self.data_frame_overhead.saturating_add(bytes.len());
        if encoded_bytes > self.max_frame_bytes {
            return Err(HostError::PluginFrameTooLarge {
                instance: self.instance.clone(),
                bytes: encoded_bytes,
                maximum: self.max_frame_bytes,
            });
        }
        let raw_bytes =
            u64::try_from(bytes.len()).map_err(|_| HostError::StreamByteBudgetExceeded {
                stream_id: self.stream_id.to_string(),
                requested: u64::MAX,
                available: STREAM_BYTE_BUDGET,
            })?;
        if raw_bytes > STREAM_BYTE_BUDGET {
            return Err(HostError::StreamByteBudgetExceeded {
                stream_id: self.stream_id.to_string(),
                requested: raw_bytes,
                available: STREAM_BYTE_BUDGET,
            });
        }
        self.send_credit.consume(raw_bytes).await?;
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            HostError::InvalidEnvelope("service stream sequence exhausted u64".to_owned())
        })?;
        self.dispatch_data(bytes).await
    }

    pub(crate) async fn grant_credit(&mut self, bytes: u64) -> Result<()> {
        if self.terminal {
            return Err(HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            });
        }
        if bytes > STREAM_BYTE_BUDGET {
            return Err(HostError::StreamByteBudgetExceeded {
                stream_id: self.stream_id.to_string(),
                requested: bytes,
                available: STREAM_BYTE_BUDGET,
            });
        }
        let (reply, response) = oneshot::channel();
        self.control
            .send(ControlCommand::GrantCredit {
                stream_id: self.stream_id.clone(),
                bytes,
                reply,
            })
            .await
            .map_err(|_| HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            })?;
        response.await.map_err(|_| HostError::StreamClosed {
            stream_id: self.stream_id.to_string(),
        })?
    }

    pub(crate) async fn half_close(&mut self) -> Result<()> {
        if self.half_closed || self.terminal {
            return Err(HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            });
        }
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            HostError::InvalidEnvelope("service stream sequence exhausted u64".to_owned())
        })?;
        let (reply, response) = oneshot::channel();
        self.control
            .send(ControlCommand::HalfClose {
                stream_id: self.stream_id.clone(),
                sequence: self.sequence,
                reply,
            })
            .await
            .map_err(|_| HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            })?;
        response.await.map_err(|_| HostError::StreamClosed {
            stream_id: self.stream_id.to_string(),
        })??;
        self.half_closed = true;
        Ok(())
    }

    pub(crate) async fn cancel(&mut self, reason: String) -> Result<()> {
        if self.terminal {
            return Ok(());
        }
        let (reply, response) = oneshot::channel();
        self.control
            .send(ControlCommand::Cancel {
                stream_id: self.stream_id.clone(),
                reason,
                reply: Some(reply),
            })
            .await
            .map_err(|_| HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            })?;
        response.await.map_err(|_| HostError::StreamClosed {
            stream_id: self.stream_id.to_string(),
        })??;
        self.terminal = true;
        drop(self.disconnect.take());
        self.send_credit.close();
        Ok(())
    }

    pub(crate) async fn recv(&mut self) -> Option<Result<StreamEnvelope>> {
        let result = match self.events.recv().await {
            Some(result) => Some(result),
            None => self.terminal_fallback.take().map(Ok),
        };
        if result.as_ref().is_some_and(|result| {
            result
                .as_ref()
                .is_ok_and(|frame| matches!(frame.kind, StreamKind::End | StreamKind::Cancel))
        }) {
            self.terminal = true;
            drop(self.disconnect.take());
            self.send_credit.close();
        }
        result
    }

    async fn dispatch_data(&self, payload: &[u8]) -> Result<()> {
        let frame = PluginFrame::service_data_request(
            self.stream_id.to_string(),
            self.service.as_str(),
            payload,
        );
        let (reply, response) = oneshot::channel();
        self.data
            .send(DataCommand::Dispatch {
                stream_id: self.stream_id.clone(),
                frame,
                reply,
            })
            .await
            .map_err(|_| HostError::StreamClosed {
                stream_id: self.stream_id.to_string(),
            })?;
        response.await.map_err(|_| HostError::StreamClosed {
            stream_id: self.stream_id.to_string(),
        })?
    }
}

#[derive(Debug, Default)]
struct TerminalFallback {
    frame: StdMutex<Option<StreamEnvelope>>,
}

impl TerminalFallback {
    fn store(&self, frame: StreamEnvelope) {
        let mut slot = self.frame.lock().expect("terminal fallback mutex poisoned");
        if slot.is_none() {
            *slot = Some(frame);
        }
    }

    fn take(&self) -> Option<StreamEnvelope> {
        self.frame
            .lock()
            .expect("terminal fallback mutex poisoned")
            .take()
    }
}

impl Drop for StreamPort {
    fn drop(&mut self) {
        if !self.terminal && !self.runtime_terminal.load(Ordering::Acquire) {
            // `open_stream` reserves this slot before returning the port, so
            // drop can enqueue synchronously from any thread without blocking,
            // allocating, or depending on a Tokio runtime.
            if let Some(disconnect) = self.disconnect.take() {
                disconnect.send(ClientDisconnect {
                    stream_id: self.stream_id.clone(),
                    reason: "client_disconnected".to_owned(),
                });
            }
            self.send_credit.close();
        }
    }
}

#[derive(Debug)]
struct ByteCredit {
    permits: Arc<Semaphore>,
    available: AtomicU64,
    closed: AtomicBool,
}

impl ByteCredit {
    fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(0)),
            available: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    fn add(&self, bytes: u64) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostError::StreamClosed {
                stream_id: "closed".to_owned(),
            });
        }
        let previous = self
            .available
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |available| {
                available
                    .checked_add(bytes)
                    .filter(|sum| *sum <= STREAM_BYTE_BUDGET)
            })
            .map_err(|available| HostError::StreamByteBudgetExceeded {
                stream_id: "plugin_credit".to_owned(),
                requested: bytes,
                available: STREAM_BYTE_BUDGET.saturating_sub(available),
            })?;
        let _ = previous;
        let permits = usize::try_from(bytes).map_err(|_| HostError::StreamByteBudgetExceeded {
            stream_id: "plugin_credit".to_owned(),
            requested: bytes,
            available: STREAM_BYTE_BUDGET,
        })?;
        self.permits.add_permits(permits);
        Ok(())
    }

    async fn consume(&self, bytes: u64) -> Result<()> {
        let permits = u32::try_from(bytes).map_err(|_| HostError::StreamByteBudgetExceeded {
            stream_id: "send".to_owned(),
            requested: bytes,
            available: STREAM_BYTE_BUDGET,
        })?;
        let permit = Arc::clone(&self.permits)
            .acquire_many_owned(permits)
            .await
            .map_err(|_| HostError::StreamClosed {
                stream_id: "send".to_owned(),
            })?;
        permit.forget();
        self.available.fetch_sub(bytes, Ordering::AcqRel);
        Ok(())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.permits.close();
    }
}

enum LifecycleCommand {
    Prepare {
        config: Value,
        reply: oneshot::Sender<Result<()>>,
    },
    Commit {
        reply: oneshot::Sender<Result<()>>,
    },
    Retire {
        reply: oneshot::Sender<Result<()>>,
    },
    Abort {
        reply: oneshot::Sender<Result<()>>,
    },
    Stop {
        reply: oneshot::Sender<()>,
    },
}

enum ControlCommand {
    Open {
        stream_id: StreamId,
        service: ServiceKey,
        payload: Value,
        events: mpsc::Sender<Result<StreamEnvelope>>,
        send_credit: Arc<ByteCredit>,
        terminal_fallback: Arc<TerminalFallback>,
        runtime_terminal: Arc<AtomicBool>,
    },
    GrantCredit {
        stream_id: StreamId,
        bytes: u64,
        reply: oneshot::Sender<Result<()>>,
    },
    HalfClose {
        stream_id: StreamId,
        sequence: u64,
        reply: oneshot::Sender<Result<()>>,
    },
    Cancel {
        stream_id: StreamId,
        reason: String,
        reply: Option<oneshot::Sender<Result<()>>>,
    },
    HostServiceResponse(PluginFrame),
    PluginCommandResponse(PluginFrame),
}

struct ClientDisconnect {
    stream_id: StreamId,
    reason: String,
}

enum DataCommand {
    Dispatch {
        stream_id: StreamId,
        frame: PluginFrame,
        reply: oneshot::Sender<Result<()>>,
    },
    OutboundEvent(PluginFrame),
    OutboundClosed {
        request_id: String,
    },
}

struct OutboundRoute {
    provider: InstanceId,
    runtime: RuntimeHandle,
    _lease: GenerationLease,
}

enum OutboundBridgeCommand {
    Data(Vec<u8>),
    Control { operation: String, payload: Value },
}

mod actor;
mod bridge;

#[cfg(test)]
use actor::runtime_tick_enabled;
use actor::{RuntimeActor, RuntimePhase};
use bridge::run_outbound_bridge;

pub(crate) struct PreparedRuntimeInstance {
    pub instance: InstanceId,
    pub package: PluginPackage,
    pub staged: StagedPlugin,
    pub resolved_config: Value,
    pub redacted_config: Value,
    pub config_audit_hash: rsi_meta_loader::ContentHash,
    pub capabilities: Vec<String>,
    pub process_fixed: bool,
    pub uses_state_service: bool,
    pub uses_runtime_tick: bool,
    pub config_schema_path: Option<std::path::PathBuf>,
    pub config_schema_hash: Option<rsi_meta_loader::ContentHash>,
    pub config_schema: Option<Value>,
}

impl fmt::Debug for PreparedRuntimeInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedRuntimeInstance")
            .field("instance", &self.instance)
            .field("package", &self.package.manifest().package)
            .field("redacted_config", &self.redacted_config)
            .field("config_audit_hash", &self.config_audit_hash)
            .field("capabilities", &self.capabilities)
            .field("process_fixed", &self.process_fixed)
            .field("uses_state_service", &self.uses_state_service)
            .field("config_schema_path", &self.config_schema_path)
            .field("config_schema_hash", &self.config_schema_hash)
            .finish_non_exhaustive()
    }
}

pub(crate) struct RuntimeLaunchContext {
    pub plugin_commands: mpsc::Sender<PluginCommandRequest>,
    pub host_services: mpsc::Sender<HostServiceCall>,
    pub runtime_faults: mpsc::Sender<RuntimeFault>,
    pub runtime_ticks: watch::Receiver<u64>,
}

pub(crate) async fn launch_and_prepare(
    loader: &PluginLoader,
    composition_id: &str,
    routing: &crate::model::RoutingSnapshot,
    instances: &BTreeMap<InstanceId, PreparedRuntimeInstance>,
    waves: &[Vec<InstanceId>],
    context: &RuntimeLaunchContext,
) -> Result<Vec<RuntimeHandle>> {
    prepare_waves(
        waves,
        |instance_id| async move {
            let Some(instance) = instances.get(&instance_id) else {
                return Ok(None);
            };
            let generation = routing
                .generation(&instance_id)
                .ok_or_else(|| HostError::UnknownInstance(instance_id.clone()))?;
            if generation.runtime_opt().is_some() {
                return Ok(None);
            }
            let outbound_routes = routing
                .routes
                .iter()
                .filter(|(key, _)| key.consumer == instance_id)
                .map(|(key, target)| {
                    let runtime = target.generation.runtime()?.clone();
                    Ok((
                        key.service.clone(),
                        OutboundRoute {
                            provider: target.provider.clone(),
                            runtime,
                            _lease: target.generation.dependency_lease(),
                        },
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            let runtime = RuntimeHandle::start(
                loader,
                &instance.staged,
                composition_id.to_owned(),
                instance.instance.clone(),
                generation.id,
                instance.capabilities.clone(),
                instance.uses_state_service,
                instance.uses_runtime_tick,
                outbound_routes,
                context.plugin_commands.clone(),
                context.host_services.clone(),
                context.runtime_faults.clone(),
                context.runtime_ticks.clone(),
            )?;
            if let Err(error) = runtime.prepare(instance.resolved_config.clone()).await {
                runtime.abort_and_stop().await;
                return Err(error);
            }
            if let Err(error) = generation.attach_runtime(runtime.clone()) {
                runtime.abort_and_stop().await;
                return Err(error);
            }
            Ok(Some(runtime))
        },
        |prepared| async move { abort_prepared_reverse(&prepared).await },
    )
    .await
}

async fn prepare_waves<T, E, Prepare, PrepareFuture, Abort, AbortFuture>(
    waves: &[Vec<InstanceId>],
    mut prepare: Prepare,
    abort_reverse: Abort,
) -> std::result::Result<Vec<T>, E>
where
    Prepare: FnMut(InstanceId) -> PrepareFuture,
    PrepareFuture: std::future::Future<Output = std::result::Result<Option<T>, E>>,
    Abort: FnOnce(Vec<T>) -> AbortFuture,
    AbortFuture: std::future::Future<Output = ()>,
{
    let mut prepared = Vec::new();
    let mut abort_reverse = Some(abort_reverse);
    for wave in waves {
        let results = futures_util::future::join_all(wave.iter().cloned().map(&mut prepare)).await;
        let mut failure = None;
        for result in results {
            match result {
                Ok(Some(runtime)) => prepared.push(runtime),
                Err(error) if failure.is_none() => failure = Some(error),
                Ok(None) | Err(_) => {}
            }
        }
        if let Some(error) = failure {
            abort_reverse
                .take()
                .expect("prepare failure aborts at most once")(prepared)
            .await;
            return Err(error);
        }
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_command_outside_committed_phase_gets_an_explicit_failure() {
        let frame = durable_command_unavailable("hmr-content-id".to_owned());
        assert_eq!(
            frame.body,
            PluginFrameBody::ServiceEvent {
                request_id: Some("hmr-content-id".to_owned()),
                service: "control.apply-manifest".to_owned(),
                event: "failed".to_owned(),
                payload: json!({"code": "command_unavailable_during_lifecycle"}),
            }
        );
    }

    fn idle_runtime_handle() -> (
        RuntimeHandle,
        mpsc::Receiver<LifecycleCommand>,
        watch::Sender<bool>,
    ) {
        let (lifecycle, receiver) = mpsc::channel(1);
        let (control, _) = mpsc::channel(1);
        let (disconnects, _) = mpsc::channel(1);
        let (data, _) = mpsc::channel(1);
        let (retired_sender, retired) = watch::channel(false);
        let (_, stopped) = watch::channel(false);
        (
            RuntimeHandle {
                instance: InstanceId::new("timeout-probe"),
                generation: 1,
                max_frame_bytes: 1024 * 1024,
                lifecycle,
                control,
                disconnects,
                data,
                retired,
                stopped,
                thread: Arc::new(StdMutex::new(None)),
                healthy: Arc::new(AtomicBool::new(true)),
                fault_reason: Arc::new(StdMutex::new(None)),
            },
            receiver,
            retired_sender,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_acknowledgements_have_a_hard_deadline() {
        let (runtime, mut control, _retired) = idle_runtime_handle();
        let prepare = tokio::spawn(async move { runtime.prepare(json!({})).await });
        let pending_command = control.recv().await.expect("prepare command");
        tokio::time::advance(PLUGIN_LIFECYCLE_TIMEOUT).await;
        let error = prepare
            .await
            .expect("prepare task")
            .expect_err("missing Prepared must time out");
        assert!(matches!(
            error,
            HostError::PluginLifecycleTimeout {
                phase: "prepared",
                ..
            }
        ));
        drop(pending_command);
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_deadline_includes_queue_admission() {
        let (runtime, _lifecycle, _retired) = idle_runtime_handle();
        let (reply, _response) = oneshot::channel();
        assert!(
            runtime
                .lifecycle
                .try_send(LifecycleCommand::Commit { reply })
                .is_ok()
        );
        let prepare = tokio::spawn(async move { runtime.prepare(json!({})).await });
        tokio::time::advance(PLUGIN_LIFECYCLE_TIMEOUT).await;
        let error = prepare
            .await
            .expect("prepare task")
            .expect_err("full lifecycle queue must time out");
        assert!(matches!(
            error,
            HostError::PluginLifecycleTimeout {
                phase: "prepared",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn committed_reports_a_closed_runtime() {
        let (runtime, lifecycle, _retired) = idle_runtime_handle();
        drop(lifecycle);
        let error = runtime
            .committed()
            .await
            .expect_err("closed runtime must reject commit acknowledgement");
        assert!(matches!(error, HostError::PluginRuntimeClosed { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn retirement_acknowledgements_have_a_hard_deadline() {
        let (runtime, _control, _retired) = idle_runtime_handle();
        let wait = tokio::spawn(async move { runtime.wait_retired().await });
        tokio::time::advance(PLUGIN_LIFECYCLE_TIMEOUT).await;
        let error = wait
            .await
            .expect("retirement task")
            .expect_err("missing Retired must time out");
        assert!(matches!(
            error,
            HostError::PluginLifecycleTimeout {
                phase: "retired",
                ..
            }
        ));
    }

    #[test]
    fn runtime_ticks_continue_only_through_committed_retirement() {
        assert!(!runtime_tick_enabled(true, RuntimePhase::Created));
        assert!(!runtime_tick_enabled(true, RuntimePhase::Preparing));
        assert!(!runtime_tick_enabled(true, RuntimePhase::Prepared));
        assert!(runtime_tick_enabled(true, RuntimePhase::Committed));
        assert!(runtime_tick_enabled(true, RuntimePhase::Retiring));
        assert!(!runtime_tick_enabled(true, RuntimePhase::Faulted));
        assert!(!runtime_tick_enabled(false, RuntimePhase::Committed));
        assert!(!runtime_tick_enabled(false, RuntimePhase::Retiring));
    }

    #[test]
    fn runtime_health_is_visible_to_generation_reuse() {
        let (runtime, _control, _retired) = idle_runtime_handle();
        assert!(runtime.is_healthy());
        let generation = Arc::new(crate::model::Generation::new(
            1,
            InstanceId::new("timeout-probe"),
        ));
        generation
            .attach_runtime(runtime.clone())
            .expect("attach runtime");
        generation.mark_admitting();
        assert!(generation.try_admit_lease().is_some());
        runtime.healthy.store(false, Ordering::Release);
        assert!(!runtime.is_healthy());
        assert!(
            generation.try_admit_lease().is_none(),
            "faulted runtime must stop new admission immediately"
        );
    }

    #[tokio::test]
    async fn independent_prepare_branches_enter_together_and_abort_in_reverse_order() {
        let waves = vec![
            vec![InstanceId::new("provider-a"), InstanceId::new("provider-b")],
            vec![
                InstanceId::new("consumer-ok"),
                InstanceId::new("consumer-fail"),
            ],
        ];
        let provider_gate = Arc::new(tokio::sync::Barrier::new(3));
        let consumer_gate = Arc::new(tokio::sync::Barrier::new(3));
        let (entered_sender, mut entered_receiver) = mpsc::unbounded_channel::<String>();
        let (aborted_sender, mut aborted_receiver) = mpsc::unbounded_channel::<String>();
        let runner = {
            let provider_gate = Arc::clone(&provider_gate);
            let consumer_gate = Arc::clone(&consumer_gate);
            tokio::spawn(async move {
                prepare_waves(
                    &waves,
                    move |instance| {
                        let entered_sender = entered_sender.clone();
                        let gate = if instance.0.starts_with("provider-") {
                            Arc::clone(&provider_gate)
                        } else {
                            Arc::clone(&consumer_gate)
                        };
                        async move {
                            entered_sender
                                .send(instance.0.clone())
                                .expect("record entered prepare");
                            gate.wait().await;
                            if instance.0 == "consumer-fail" {
                                Err("prepare_failed")
                            } else {
                                Ok(Some(instance.0))
                            }
                        }
                    },
                    move |prepared| async move {
                        for instance in prepared.into_iter().rev() {
                            aborted_sender.send(instance).expect("record reverse abort");
                        }
                    },
                )
                .await
            })
        };

        let mut providers = vec![
            entered_receiver
                .recv()
                .await
                .expect("first provider entered"),
            entered_receiver
                .recv()
                .await
                .expect("second provider entered"),
        ];
        providers.sort();
        assert_eq!(providers, ["provider-a", "provider-b"]);
        provider_gate.wait().await;

        let mut consumers = vec![
            entered_receiver
                .recv()
                .await
                .expect("first consumer entered"),
            entered_receiver
                .recv()
                .await
                .expect("second consumer entered"),
        ];
        consumers.sort();
        assert_eq!(consumers, ["consumer-fail", "consumer-ok"]);
        consumer_gate.wait().await;

        assert_eq!(
            runner.await.expect("wave runner task"),
            Err("prepare_failed")
        );
        let aborted = vec![
            aborted_receiver.recv().await.expect("abort consumer"),
            aborted_receiver.recv().await.expect("abort provider b"),
            aborted_receiver.recv().await.expect("abort provider a"),
        ];
        assert_eq!(aborted, ["consumer-ok", "provider-b", "provider-a"]);
        assert!(aborted_receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn dropping_stream_outside_runtime_delivers_cancel_after_full_control_queue_drains() {
        let (control, mut control_receiver) = mpsc::channel(1);
        control
            .send(ControlCommand::HostServiceResponse(
                PluginFrame::service_event(None, "test", "filler", json!({})),
            ))
            .await
            .expect("fill control queue");
        let (data, _data_receiver) = mpsc::channel(1);
        let (disconnects, mut disconnect_receiver) = mpsc::channel(MAX_STREAMS_PER_GENERATION);
        assert_eq!(disconnects.max_capacity(), MAX_STREAMS_PER_GENERATION);
        let disconnect = disconnects
            .try_reserve_owned()
            .expect("reserve bounded disconnect slot");
        let (event_sender, events) = mpsc::channel(1);
        let runtime_terminal = Arc::new(AtomicBool::new(false));
        let port = StreamPort {
            instance: InstanceId::new("provider"),
            stream_id: StreamId::new("drop-stream").expect("valid stream id"),
            service: ServiceKey::new("fixture.echo"),
            control,
            disconnect: Some(disconnect),
            data,
            events,
            send_credit: Arc::new(ByteCredit::new()),
            terminal_fallback: Arc::new(TerminalFallback::default()),
            runtime_terminal,
            max_frame_bytes: 1024 * 1024,
            data_frame_overhead: 32,
            sequence: 0,
            half_closed: false,
            terminal: false,
        };
        std::thread::spawn(move || drop(port))
            .join()
            .expect("drop stream outside the Tokio runtime");
        let _ = control_receiver.recv().await.expect("queued filler");
        let cancel = disconnect_receiver
            .recv()
            .await
            .expect("disconnect cancellation");
        assert_eq!(cancel.stream_id.as_str(), "drop-stream");
        assert_eq!(cancel.reason, "client_disconnected");
        drop(event_sender);
    }
}
