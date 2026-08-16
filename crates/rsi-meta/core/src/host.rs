use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use arc_swap::ArcSwap;
use futures_util::Stream;
use rsi_meta_loader::{ContentHash, PluginLoader};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::composition::{
    InstanceFingerprint, affected_instances, build_inspections, build_lock, composition_event,
    install_pair, instance_fingerprints, launch_and_prepare_pumping_services,
    normalize_prepared_for_install, prepare_pair, resolve_prepared, validate_paths,
    write_lock_create_new,
};
use crate::domain::{
    ApplyRequest, ApplyResult, CompositionDigest, CompositionProject, CompositionWorkspace,
    EventPage, HostEventRecord, HostSnapshot, InstallRequest, InstallResult, OperationId,
    ShutdownReceipt, TokenRotation,
};
use crate::frame::PluginFrame;
use crate::model::{
    CompositionMode, DesiredState, GraphRevision, GraphSnapshot, InstanceId, RetirementPhase,
    RetiringInstanceSnapshot, RouteKey, RoutingSnapshot, ServiceKey,
};
use crate::persistence::{CasResult, Persistence, StoredCommand};
use crate::protocol::{
    Command, CommandEnvelope, CommandOutcome, CommandOutcomeEnvelope, CompositionChangeSource,
    Event, EventEnvelope, PluginInspection,
};
use crate::recovery::{
    read_optional_bytes, recover_pending_applies, remove_file_and_sync_parent,
    restore_previous_pair,
};
use crate::resolver::dependency_waves;
mod plugin_control;
pub(crate) mod registry;

use registry::RegistryActor;

use crate::runtime::{
    HostServiceCall, PluginCommandRequest, RuntimeLaunchContext, StreamPort, abort_prepared_reverse,
};
use crate::{HostError, Result};

pub(crate) const MAX_COMPOSITION_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CONFIG_SCHEMA_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompositionFiles {
    pub manifest_path: PathBuf,
    pub lock_path: PathBuf,
}

impl CompositionFiles {
    pub fn new(manifest_path: impl Into<PathBuf>, lock_path: impl Into<PathBuf>) -> Self {
        Self {
            manifest_path: manifest_path.into(),
            lock_path: lock_path.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub workspace: CompositionWorkspace,
    pub command_capacity: usize,
    pub event_capacity: usize,
}

impl OpenOptions {
    pub fn new(workspace: CompositionWorkspace) -> Self {
        Self {
            workspace,
            command_capacity: 128,
            event_capacity: 256,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceOpenRequest {
    pub consumer: InstanceId,
    pub service: ServiceKey,
}

/// Generation-pinned, bounded bidirectional service stream.
///
/// Each direction is FIFO, DATA is byte-credit limited, and the pinned runtime
/// remains alive until the stream reaches its single terminal frame or drops.
pub struct ServiceStream {
    provider: InstanceId,
    port: StreamPort,
    _lease: crate::model::GenerationLease,
}

impl fmt::Debug for ServiceStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceStream")
            .field("stream_id", &self.port.stream_id())
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl ServiceStream {
    pub fn stream_id(&self) -> &str {
        self.port.stream_id()
    }

    pub fn provider(&self) -> &InstanceId {
        &self.provider
    }

    /// Sends one DATA frame.
    ///
    /// # Errors
    ///
    pub async fn send(&mut self, payload: &[u8]) -> Result<()> {
        self.port.send(payload).await
    }

    /// Grants encoded JSON payload bytes for plugin-to-caller DATA frames.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is closed, the byte budget is
    /// exceeded, or the provider rejects the credit frame.
    pub async fn grant_credit(&mut self, bytes: u64) -> Result<()> {
        self.port.grant_credit(bytes).await
    }

    /// Receives the next FIFO stream frame. Exactly one terminal END or CANCEL
    /// is emitted before the stream closes.
    pub async fn recv(&mut self) -> Option<Result<crate::protocol::StreamEnvelope>> {
        self.port.recv().await
    }

    /// Half-closes the caller-to-provider direction.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is already closed or dispatch fails.
    pub async fn half_close(&mut self) -> Result<()> {
        self.port.half_close().await
    }

    /// Cancels both stream directions with a structured reason.
    ///
    /// # Errors
    ///
    /// Returns an error when cancellation cannot reach the runtime.
    pub async fn cancel(&mut self, reason: impl Into<String>) -> Result<()> {
        self.port.cancel(reason.into()).await
    }
}

pub struct EventStream {
    replay: mpsc::Receiver<Result<HostEventRecord>>,
    replay_complete: bool,
    replay_failed: bool,
    live: BroadcastStream<EventEnvelope>,
    last_cursor: u64,
}

const EVENT_REPLAY_PAGE_SIZE: u32 = 64;
const EVENT_REPLAY_CHANNEL_CAPACITY: usize = 1;

impl fmt::Debug for EventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventStream")
            .field("replay_buffered", &self.replay.len())
            .field("replay_complete", &self.replay_complete)
            .field("replay_failed", &self.replay_failed)
            .field("last_cursor", &self.last_cursor)
            .finish_non_exhaustive()
    }
}

impl EventStream {
    fn new(
        replay: mpsc::Receiver<Result<HostEventRecord>>,
        live: broadcast::Receiver<EventEnvelope>,
        after_cursor: u64,
    ) -> Self {
        Self {
            replay,
            replay_complete: false,
            replay_failed: false,
            live: BroadcastStream::new(live),
            last_cursor: after_cursor,
        }
    }

    pub async fn recv(&mut self) -> Option<Result<HostEventRecord>> {
        futures_util::StreamExt::next(self).await
    }
}

impl Stream for EventStream {
    type Item = Result<HostEventRecord>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.replay_failed {
                return Poll::Ready(None);
            }
            if !self.replay_complete {
                match self.replay.poll_recv(context) {
                    Poll::Ready(Some(Ok(event))) if event.cursor <= self.last_cursor => continue,
                    Poll::Ready(Some(Ok(event))) => {
                        self.last_cursor = event.cursor;
                        return Poll::Ready(Some(Ok(event)));
                    }
                    Poll::Ready(Some(Err(error))) => {
                        self.replay_complete = true;
                        self.replay_failed = true;
                        return Poll::Ready(Some(Err(error)));
                    }
                    Poll::Ready(None) => self.replay_complete = true,
                    Poll::Pending => return Poll::Pending,
                }
            }
            match Pin::new(&mut self.live).poll_next(context) {
                Poll::Ready(Some(Ok(event))) if event.cursor <= self.last_cursor => {}
                Poll::Ready(Some(Ok(event))) => {
                    self.last_cursor = event.cursor;
                    return Poll::Ready(Some(Ok(event.into())));
                }
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(skipped)))) => {
                    self.replay_failed = true;
                    return Poll::Ready(Some(Err(HostError::SubscriberLagged { skipped })));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Clone)]
pub struct CompositionHost {
    inner: Arc<HostInner>,
}

impl fmt::Debug for CompositionHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompositionHost")
            .field("snapshot", &self.snapshot())
            .finish_non_exhaustive()
    }
}

struct HostInner {
    routing: Arc<ArcSwap<RoutingSnapshot>>,
    cutover: Arc<StdMutex<()>>,
    sender: mpsc::Sender<RegistryMessage>,
    join: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutting_down: AtomicBool,
    shutdown_operation: StdMutex<Option<OperationId>>,
    retirements: RetirementRegistry,
}

type RetirementKey = (InstanceId, u64);
type RetirementRegistry = Arc<StdMutex<BTreeMap<RetirementKey, RetirementEntry>>>;

#[derive(Clone)]
struct RetirementEntry {
    generation: Arc<crate::model::Generation>,
    phase: Arc<AtomicU8>,
    cancel: watch::Sender<bool>,
    done: watch::Receiver<bool>,
}

fn graph_with_retirements(
    graph: &GraphSnapshot,
    retirements: &RetirementRegistry,
) -> GraphSnapshot {
    let mut graph = graph.clone();
    let mut by_instance = BTreeMap::<InstanceId, RetiringInstanceSnapshot>::new();
    for entry in retirements
        .lock()
        .expect("retirement registry mutex poisoned")
        .values()
    {
        let phase = match entry.phase.load(Ordering::Acquire) {
            0 => RetirementPhase::Draining,
            1 => RetirementPhase::Retiring,
            _ => RetirementPhase::Stopping,
        };
        let aggregate = by_instance
            .entry(entry.generation.instance.clone())
            .or_insert_with(|| RetiringInstanceSnapshot {
                instance_id: entry.generation.instance.clone(),
                generation_count: 0,
                lease_count: 0,
                phase,
            });
        aggregate.generation_count = aggregate.generation_count.saturating_add(1);
        aggregate.lease_count = aggregate
            .lease_count
            .saturating_add(entry.generation.lease_count());
        // Report the least advanced phase across the private generations so a
        // caller never mistakes a partially drained aggregate for completed.
        if retirement_phase_rank(phase) < retirement_phase_rank(aggregate.phase) {
            aggregate.phase = phase;
        }
    }
    graph.retiring_instances = by_instance.into_values().collect();
    graph
}

fn with_current_routing<T>(
    cutover: &StdMutex<()>,
    routing: &ArcSwap<RoutingSnapshot>,
    operation: impl FnOnce(&RoutingSnapshot) -> T,
) -> T {
    let _cutover = cutover.lock().expect("routing cutover mutex poisoned");
    let snapshot = routing.load_full();
    operation(&snapshot)
}

fn admit_service(
    snapshot: &RoutingSnapshot,
    request: &ServiceOpenRequest,
) -> Result<(
    InstanceId,
    crate::runtime::RuntimeHandle,
    crate::model::GenerationLease,
)> {
    let Some(instance) = snapshot.graph().instances.get(&request.consumer) else {
        return Err(HostError::UnknownInstance(request.consumer.clone()));
    };
    if !instance.status.is_active() {
        return Err(HostError::InstanceInactive {
            instance: request.consumer.clone(),
        });
    }
    if !instance
        .requires
        .iter()
        .any(|requirement| requirement.service == request.service)
    {
        return Err(HostError::UndeclaredService {
            consumer: request.consumer.clone(),
            service: request.service.clone(),
        });
    }
    let key = RouteKey {
        consumer: request.consumer.clone(),
        service: request.service.clone(),
    };
    let Some(route) = snapshot.route(&key) else {
        return Err(HostError::UnresolvedService {
            consumer: request.consumer.clone(),
            service: request.service.clone(),
        });
    };
    let Some(lease) = snapshot.try_admit_route_lease(&key) else {
        return Err(HostError::PluginRuntimeNotCommitted {
            instance: route.provider.clone(),
        });
    };
    Ok((
        route.provider.clone(),
        route.generation.runtime()?.clone(),
        lease,
    ))
}

fn publish_routing_cutover(
    cutover: &StdMutex<()>,
    routing: &ArcSwap<RoutingSnapshot>,
    old: &RoutingSnapshot,
    retired: &[Arc<crate::model::Generation>],
    next: RoutingSnapshot,
    inside_cutover: impl FnOnce(),
) {
    let _cutover = cutover.lock().expect("routing cutover mutex poisoned");
    next.mark_admitting();
    old.stop_admission();
    for generation in retired {
        generation.stop_admission();
    }
    inside_cutover();
    routing.store(Arc::new(next));
}

const fn retirement_phase_rank(phase: RetirementPhase) -> u8 {
    match phase {
        RetirementPhase::Draining => 0,
        RetirementPhase::Retiring => 1,
        RetirementPhase::Stopping => 2,
    }
}

impl CompositionHost {
    /// Installs a validated pair while no live host owns the workspace.
    ///
    /// The pair becomes active only when a host subsequently opens the workspace.
    ///
    /// # Errors
    ///
    /// Returns an error for an occupied or invalid workspace, invalid candidate,
    /// conflicting operation identity, storage failure, or pair commit failure.
    pub async fn install_offline(request: InstallRequest) -> Result<InstallResult> {
        tokio::task::spawn_blocking(move || crate::recovery::install_offline(&request))
            .await
            .map_err(|_| HostError::RegistryClosed)?
    }

    /// Opens the durable embedded host and recovers pending graph commits.
    ///
    /// # Errors
    ///
    /// Returns an error when storage, manifest/lock validation, package
    /// staging, graph resolution, or crash recovery fails.
    #[allow(clippy::too_many_lines)]
    pub async fn open(options: OpenOptions) -> Result<Self> {
        let workspace_lease = crate::workspace::WorkspaceLease::acquire(&options.workspace)?;
        let mut persistence = Persistence::open(&options.workspace.database_path)?;
        let loader = PluginLoader::for_current_process(&options.workspace.cache_root);
        let mut next_generation = 1;
        recover_pending_applies(&mut persistence, &loader)?;
        let installed_files = crate::workspace::installed_files(&options.workspace)?;
        let latest_revision = persistence.latest_graph_revision()?;
        let latest_cursor = persistence.latest_cursor()?;
        let token_generation = persistence.token_generation()?;
        let latest_composition_event = persistence.latest_composition_event()?;

        let (events, _) = broadcast::channel(options.event_capacity.max(1));
        let (sender, receiver) = mpsc::channel(options.command_capacity.max(1));
        let (plugin_commands, plugin_command_receiver) =
            mpsc::channel(options.command_capacity.max(1));
        let (host_services, mut host_service_receiver) =
            mpsc::channel(options.command_capacity.max(1));
        let retirements = RetirementRegistry::default();
        let launch_context = RuntimeLaunchContext {
            plugin_commands,
            host_services,
        };

        let (routing, current_hashes, current_fingerprints, plugin_inspections, current_mode) =
            if let Some(files) = &installed_files {
                let prepared = prepare_pair(files, &loader, true)?;
                crate::workspace::require_fresh_process_for_changed_fixed(
                    &options.workspace,
                    &prepared.process_fixed_packages,
                )?;
                let previous_matches = latest_composition_event.as_ref().is_some_and(|event| {
                    matches!(
                        &event.payload,
                        Event::CompositionCommitted {
                            manifest_sha256,
                            lock_sha256,
                            ..
                        } if manifest_sha256 == &prepared.manifest_hash.to_string()
                            && lock_sha256 == &prepared.lock_hash.to_string()
                    )
                });
                let revision = if previous_matches {
                    latest_composition_event
                        .as_ref()
                        .map_or(latest_revision, |event| event.graph_revision)
                } else {
                    GraphRevision(latest_revision.0.saturating_add(1))
                };
                let mut routing =
                    resolve_prepared(&prepared, revision, None, &mut next_generation)?;
                let waves = dependency_waves(routing.graph())?;
                let prepared_runtimes = match launch_and_prepare_pumping_services(
                    &loader,
                    &prepared.manifest.composition.id,
                    &routing,
                    &prepared.runtimes,
                    &waves,
                    &launch_context,
                    &mut host_service_receiver,
                    &mut persistence,
                )
                .await
                {
                    Ok(runtimes) => runtimes,
                    Err(error) => return Err(error),
                };
                let cursor = if previous_matches {
                    latest_cursor
                } else {
                    let event = match persistence.append_event(
                        &prepared.manifest.composition.id,
                        &format!(
                            "system:open:{}:{}:{}",
                            prepared.manifest.composition.id,
                            prepared.manifest_hash,
                            prepared.lock_hash
                        ),
                        revision,
                        composition_event(&prepared, &routing, CompositionChangeSource::Open),
                    ) {
                        Ok(event) => event,
                        Err(error) => {
                            abort_prepared_reverse(&prepared_runtimes).await;
                            return Err(error);
                        }
                    };
                    event.cursor
                };
                routing.set_event_cursor(cursor);
                routing.set_token_generation(token_generation);
                let opened_manifest_hash = prepared.manifest_hash.to_string();
                let opened_lock_hash = prepared.lock_hash.to_string();
                let opened_desired = DesiredState {
                    manifest_sha256: Some(opened_manifest_hash.clone()),
                    lock_sha256: Some(opened_lock_hash.clone()),
                    applied: true,
                    last_rejection_code: None,
                    plugin_restart_requested: false,
                };
                routing.set_active(Some(CompositionDigest {
                    composition_id: prepared.manifest.composition.id.clone(),
                    manifest_sha256: opened_manifest_hash,
                    lock_sha256: opened_lock_hash,
                }));
                if let Err(error) = persistence.set_desired_state(&opened_desired) {
                    abort_prepared_reverse(&prepared_runtimes).await;
                    return Err(error);
                }
                for runtime in &prepared_runtimes {
                    runtime.committed().await;
                    if let Some(generation) = routing.generation(runtime.instance()) {
                        generation.mark_admitting();
                    }
                }
                let fingerprints = instance_fingerprints(&prepared);
                let inspections = build_inspections(&prepared, &routing);
                let mode = prepared.manifest.composition.mode;
                (
                    routing,
                    Some((prepared.manifest_hash, prepared.lock_hash)),
                    fingerprints,
                    inspections,
                    mode,
                )
            } else {
                let mut routing = RoutingSnapshot::new(
                    GraphSnapshot {
                        revision: latest_revision,
                        composition_id: String::new(),
                        instances: BTreeMap::new(),
                        bindings: Vec::new(),
                        retiring_instances: Vec::new(),
                    },
                    BTreeMap::new(),
                    BTreeMap::new(),
                );
                routing.set_event_cursor(latest_cursor);
                routing.set_token_generation(token_generation);
                (
                    routing,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                    CompositionMode::Development,
                )
            };

        crate::workspace::record_loaded_process_fixed(
            &options.workspace,
            current_fingerprints
                .values()
                .filter(|fingerprint| fingerprint.process_fixed)
                .map(|fingerprint| {
                    (
                        fingerprint.package_id.clone(),
                        fingerprint.artifact_hash.to_string(),
                    )
                })
                .collect(),
        )?;

        routing.mark_admitting();
        let routing = Arc::new(ArcSwap::from_pointee(routing));
        let cutover = Arc::new(StdMutex::new(()));
        let actor = RegistryActor {
            persistence,
            loader,
            routing: Arc::clone(&routing),
            cutover: Arc::clone(&cutover),
            events,
            current_hashes,
            installed_files: Some(CompositionFiles::new(
                options.workspace.manifest_path.clone(),
                options.workspace.lock_path.clone(),
            )),
            next_generation,
            current_fingerprints,
            plugin_inspections,
            launch_context,
            plugin_command_receiver,
            host_service_receiver,
            retirements: Arc::clone(&retirements),
            current_mode,
            _workspace_lease: workspace_lease,
        };
        let join = tokio::spawn(actor.run(receiver));
        Ok(Self {
            inner: Arc::new(HostInner {
                routing,
                cutover,
                sender,
                join: Mutex::new(Some(join)),
                shutting_down: AtomicBool::new(false),
                shutdown_operation: StdMutex::new(None),
                retirements,
            }),
        })
    }

    pub fn snapshot(&self) -> HostSnapshot {
        let snapshot = self.inner.routing.load();
        HostSnapshot {
            graph: graph_with_retirements(snapshot.graph(), &self.inner.retirements),
            cursor: snapshot.event_cursor(),
            token_generation: snapshot.token_generation(),
            active: snapshot.active().cloned(),
        }
    }

    /// Replays every durable event after `after_cursor`, then follows live events.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry is closed or the replay boundary
    /// cannot be captured. A later durable replay failure is emitted as one
    /// `EventStream` error item, after which the stream terminates rather than
    /// following live events across a gap.
    pub async fn subscribe(&self, after_cursor: u64) -> Result<EventStream> {
        let (start_sender, start_response) = oneshot::channel();
        self.inner
            .sender
            .send(RegistryMessage::Subscribe {
                reply: start_sender,
            })
            .await
            .map_err(|_| HostError::RegistryClosed)?;
        let start = start_response
            .await
            .map_err(|_| HostError::ResponseDropped)??;
        let (replay_sender, replay_receiver) = mpsc::channel(EVENT_REPLAY_CHANNEL_CAPACITY);
        tokio::spawn(pump_event_replay(
            self.inner.sender.clone(),
            replay_sender,
            after_cursor,
            start.through_cursor,
        ));
        Ok(EventStream::new(replay_receiver, start.live, after_cursor))
    }

    async fn submit_internal(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
        command.validate()?;
        let (reply, response) = oneshot::channel();
        self.inner
            .sender
            .send(RegistryMessage::Submit { command, reply })
            .await
            .map_err(|_| HostError::RegistryClosed)?;
        response.await.map_err(|_| HostError::ResponseDropped)?
    }

    /// Applies one candidate to the live graph, or previews a required process boundary.
    ///
    /// # Errors
    ///
    /// Returns a durable operation rejection, storage failure, or lifecycle error.
    pub async fn apply(&self, request: ApplyRequest) -> Result<ApplyResult> {
        request.operation_id.validate()?;
        let files = project_files(&request.project)?;
        let mut command = CommandEnvelope::new(
            request.operation_id.0,
            Command::ApplyManifestPath {
                manifest_path: files.manifest_path,
                lock_path: files.lock_path,
            },
        );
        command.expected_graph_revision = request.expected_revision;
        match self.submit_internal(command).await?.payload {
            CommandOutcome::Applied { graph } => Ok(ApplyResult::Applied {
                snapshot: self.snapshot_for_operation(graph),
            }),
            CommandOutcome::NoChange { graph } => Ok(ApplyResult::Unchanged {
                snapshot: self.snapshot_for_operation(graph),
            }),
            CommandOutcome::RestartRequired {
                current,
                candidate,
                packages,
            } => Ok(ApplyResult::RestartRequired {
                current,
                candidate,
                packages,
            }),
            CommandOutcome::Rejected { code, message } => Err(HostError::OperationRejected {
                code,
                message,
                details: BTreeMap::new(),
            }),
            other => Err(HostError::InvalidEnvelope(format!(
                "registry returned non-apply result {other:?}"
            ))),
        }
    }

    fn snapshot_for_operation(&self, _stored_graph: GraphSnapshot) -> HostSnapshot {
        // A durable operation result may be replayed after later cutovers. The
        // stored result decides the result kind, while the public snapshot must
        // always come from one current routing publication; combining its
        // cursor/digest with a historical graph would create a replay gap.
        self.snapshot()
    }

    /// Returns one finite page of durable host events.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry is unavailable or durable replay fails.
    pub async fn events_after(&self, after_cursor: u64, limit: u32) -> Result<EventPage> {
        let (reply, response) = oneshot::channel();
        self.inner
            .sender
            .send(RegistryMessage::ReplayEvents {
                after_cursor,
                through_cursor: u64::MAX,
                limit,
                reply,
            })
            .await
            .map_err(|_| HostError::RegistryClosed)?;
        let events = response
            .await
            .map_err(|_| HostError::ResponseDropped)??
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(EventPage { events })
    }

    /// Inspects one mounted plugin instance without creating a durable operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot accept or answer the request.
    pub async fn inspect_plugin(
        &self,
        instance_id: InstanceId,
    ) -> Result<Option<crate::PluginInspection>> {
        let (reply, response) = oneshot::channel();
        self.inner
            .sender
            .send(RegistryMessage::InspectPlugin { instance_id, reply })
            .await
            .map_err(|_| HostError::RegistryClosed)?;
        response
            .await
            .map_err(|_| HostError::ResponseDropped)
            .map(|inspection| inspection.map(Into::into))
    }

    /// Durably advances the bearer-token generation once for this operation.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or conflicting operation identity, a
    /// durable-store failure, or registry termination.
    pub async fn rotate_token(&self, operation_id: OperationId) -> Result<TokenRotation> {
        operation_id.validate()?;
        match self
            .submit_internal(CommandEnvelope::new(operation_id.0, Command::RotateToken))
            .await?
            .payload
        {
            CommandOutcome::TokenRotated { generation } => Ok(TokenRotation { generation }),
            CommandOutcome::Rejected { code, message } => Err(HostError::OperationRejected {
                code,
                message,
                details: BTreeMap::new(),
            }),
            other => Err(HostError::InvalidEnvelope(format!(
                "registry returned non-token result {other:?}"
            ))),
        }
    }

    /// Resolves a declared injection and pins its current provider generation.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown/inactive consumers, undeclared injections,
    /// or unresolved optional injections.
    pub fn open_service(&self, request: ServiceOpenRequest) -> Result<ServiceStream> {
        let (provider, runtime, lease) =
            with_current_routing(&self.inner.cutover, &self.inner.routing, |snapshot| {
                admit_service(snapshot, &request)
            })?;
        let port = runtime.open_stream(&request.consumer, request.service)?;
        Ok(ServiceStream {
            provider,
            port,
            _lease: lease,
        })
    }

    /// Durably records shutdown, then begins actor termination.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation identity conflicts, the durable command
    /// fails, or a different shutdown operation is already in progress.
    pub async fn request_shutdown(&self, operation_id: OperationId) -> Result<ShutdownReceipt> {
        operation_id.validate()?;
        let command = CommandEnvelope::new(operation_id.0.clone(), Command::Shutdown);
        command.validate()?;
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return self.shutdown_latch_result(operation_id);
        }
        // Acquire queue capacity before publishing the shutdown latch. A task
        // cancelled while the bounded queue is full must not make the host
        // appear to be shutting down when no shutdown command was admitted.
        let permit = self
            .inner
            .sender
            .reserve()
            .await
            .map_err(|_| HostError::RegistryClosed)?;
        if self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            drop(permit);
            return self.shutdown_latch_result(operation_id);
        }
        self.inner
            .shutdown_operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(operation_id.clone());
        let (reply, response) = oneshot::channel();
        permit.send(RegistryMessage::Submit { command, reply });
        let Ok(result) = response.await else {
            self.reset_shutdown_latch();
            return Err(HostError::ResponseDropped);
        };
        match result {
            Ok(outcome) if matches!(outcome.payload, CommandOutcome::ShuttingDown) => {
                Ok(ShutdownReceipt { operation_id })
            }
            Ok(outcome) => {
                self.reset_shutdown_latch();
                Err(HostError::InvalidEnvelope(format!(
                    "registry returned non-shutdown result {:?}",
                    outcome.payload
                )))
            }
            Err(error) => {
                self.reset_shutdown_latch();
                Err(error)
            }
        }
    }

    fn shutdown_latch_result(&self, operation_id: OperationId) -> Result<ShutdownReceipt> {
        let existing = self
            .inner
            .shutdown_operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if existing.as_ref() == Some(&operation_id) {
            Ok(ShutdownReceipt { operation_id })
        } else {
            Err(HostError::OperationRejected {
                code: "shutdown_in_progress".to_owned(),
                message: "a different shutdown operation is already in progress".to_owned(),
                details: BTreeMap::new(),
            })
        }
    }

    /// Waits for any host termination without initiating it.
    ///
    /// # Errors
    ///
    /// Returns `ShutdownDeadline` when the deadline expires, or
    /// `RegistryClosed` when the registry task cannot be joined cleanly.
    pub async fn wait_terminated(&self, deadline: Instant) -> Result<()> {
        let wait = async {
            let mut join = self.inner.join.lock().await;
            let Some(handle) = join.as_mut() else {
                return Ok(());
            };
            handle.await.map_err(|_| HostError::RegistryClosed)?;
            join.take();
            Ok(())
        };
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), wait).await {
            Ok(result) => result,
            Err(_) => Err(HostError::ShutdownDeadline),
        }
    }

    fn reset_shutdown_latch(&self) {
        self.inner.shutting_down.store(false, Ordering::Release);
        self.inner
            .shutdown_operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

pub(crate) fn project_files(project: &CompositionProject) -> Result<CompositionFiles> {
    let lock_path = project
        .lock_path
        .as_deref()
        .ok_or_else(|| HostError::OperationRejected {
            code: "lock_path_required".to_owned(),
            message: "apply requires a manifest and lock pair".to_owned(),
            details: BTreeMap::new(),
        })?;
    Ok(CompositionFiles::new(
        crate::workspace::normalize_absolute(&project.manifest_path)?,
        crate::workspace::normalize_absolute(lock_path)?,
    ))
}

enum RegistryMessage {
    Submit {
        command: CommandEnvelope,
        reply: oneshot::Sender<Result<CommandOutcomeEnvelope>>,
    },
    Subscribe {
        reply: oneshot::Sender<Result<SubscriptionStart>>,
    },
    ReplayEvents {
        after_cursor: u64,
        through_cursor: u64,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<EventEnvelope>>>,
    },
    InspectPlugin {
        instance_id: InstanceId,
        reply: oneshot::Sender<Option<PluginInspection>>,
    },
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
}

struct SubscriptionStart {
    live: broadcast::Receiver<EventEnvelope>,
    through_cursor: u64,
}

async fn pump_event_replay(
    registry: mpsc::Sender<RegistryMessage>,
    replay_output: mpsc::Sender<Result<HostEventRecord>>,
    mut after_cursor: u64,
    through_cursor: u64,
) {
    while after_cursor < through_cursor {
        let (page_sender, page_response) = oneshot::channel();
        if registry
            .send(RegistryMessage::ReplayEvents {
                after_cursor,
                through_cursor,
                limit: EVENT_REPLAY_PAGE_SIZE,
                reply: page_sender,
            })
            .await
            .is_err()
        {
            let _ = replay_output.send(Err(HostError::RegistryClosed)).await;
            return;
        }
        let page = match page_response.await {
            Ok(Ok(page)) => page,
            Ok(Err(error)) => {
                let _ = replay_output.send(Err(error)).await;
                return;
            }
            Err(_) => {
                let _ = replay_output.send(Err(HostError::ResponseDropped)).await;
                return;
            }
        };
        if page.is_empty() {
            let _ = replay_output
                .send(Err(HostError::InvalidEnvelope(format!(
                    "durable event replay ended at cursor {after_cursor} before boundary {through_cursor}"
                ))))
                .await;
            return;
        }
        for event in page {
            after_cursor = event.cursor;
            if replay_output.send(Ok(event.into())).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;

    use super::plugin_control::{
        plugin_candidate_lock_path, plugin_effect_command_id, plugin_provenance_command_id,
        validate_plugin_command_admission, write_plugin_candidate_lock,
    };
    use super::*;
    use crate::model::{
        CompositionLock, Generation, InstanceSnapshot, InstanceStatus, PackageId, PackageSource,
        ScopeId, ServiceRequirement,
    };

    fn test_workspace(root: &Path) -> CompositionWorkspace {
        CompositionWorkspace {
            database_path: root.join("state.sqlite3"),
            cache_root: root.join("cache"),
            manifest_path: root.join("composition.toml"),
            lock_path: root.join("rsi-meta.lock"),
        }
    }

    #[tokio::test]
    async fn subscription_does_not_materialize_the_entire_event_history() {
        let root = tempfile::tempdir().expect("tempdir");
        let database = root.path().join("state.sqlite3");
        let mut persistence = Persistence::open(&database).expect("event store");
        for index in 0..=EVENT_REPLAY_PAGE_SIZE {
            persistence
                .append_event(
                    "demo",
                    &format!("event-{index}"),
                    GraphRevision(0),
                    Event::HostShuttingDown,
                )
                .expect("seed durable event");
        }
        drop(persistence);

        let workspace = test_workspace(root.path());
        let host = CompositionHost::open(OpenOptions::new(workspace))
            .await
            .expect("host");
        let mut stream = host.subscribe(0).await.expect("event subscription");
        assert_eq!(stream.replay.max_capacity(), EVENT_REPLAY_CHANNEL_CAPACITY);
        assert!(
            stream.replay.len() <= EVENT_REPLAY_CHANNEL_CAPACITY,
            "a subscription buffered {} events from an unbounded history",
            stream.replay.len()
        );
        let manifest = root.path().join("rsi-meta.toml");
        let lock = root.path().join("rsi-meta.lock");
        fs::write(
            &manifest,
            r#"format_version = 0
scopes = []
instances = []

[composition]
id = "demo"
mode = "development"
"#,
        )
        .expect("empty composition");
        let project = CompositionProject {
            manifest_path: manifest,
            lock_path: Some(lock),
        };
        let locked = project.lock().expect("lock while replay is buffered");
        assert!(matches!(locked, crate::LockResult::Created { .. }));
        let applied = host
            .apply(ApplyRequest {
                operation_id: OperationId("apply-live-after-replay".to_owned()),
                project,
                expected_revision: None,
            })
            .await
            .expect("apply while replay is buffered");
        assert!(matches!(applied, ApplyResult::Applied { .. }));

        let final_cursor = u64::from(EVENT_REPLAY_PAGE_SIZE) + 2;
        for expected_cursor in 1..=final_cursor {
            let event = stream
                .recv()
                .await
                .expect("durable replay event")
                .expect("valid durable replay event");
            assert_eq!(event.cursor, expected_cursor);
            if expected_cursor == final_cursor {
                assert!(matches!(
                    event.event,
                    crate::HostEvent::CompositionCommitted { .. }
                ));
            }
        }
        drop(stream);
        host.request_shutdown(OperationId("shutdown-after-replay".to_owned()))
            .await
            .expect("request shutdown");
        host.wait_terminated(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("shutdown host");
    }

    #[tokio::test]
    async fn failed_shutdown_persistence_can_be_retried() {
        let root = tempfile::tempdir().expect("tempdir");
        let database = root.path().join("state.sqlite3");
        let host = CompositionHost::open(OpenOptions::new(test_workspace(root.path())))
            .await
            .expect("host");
        let connection = Connection::open(&database).expect("open test store");
        connection
            .execute_batch(
                "CREATE TRIGGER reject_shutdown_event
                 BEFORE INSERT ON control_event
                 BEGIN
                   SELECT RAISE(FAIL, 'injected shutdown persistence failure');
                 END;",
            )
            .expect("install shutdown failure trigger");

        let error = host
            .request_shutdown(OperationId("shutdown-retry".to_owned()))
            .await
            .expect_err("injected persistence failure must reject shutdown");
        assert!(
            error
                .to_string()
                .contains("injected shutdown persistence failure")
        );
        connection
            .execute_batch("DROP TRIGGER reject_shutdown_event;")
            .expect("remove shutdown failure trigger");

        host.request_shutdown(OperationId("shutdown-retry".to_owned()))
            .await
            .expect("request shutdown retry");
        host.wait_terminated(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("shutdown retry");
    }

    #[tokio::test]
    async fn cancelled_shutdown_before_queue_admission_does_not_latch_shutdown() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut options = OpenOptions::new(test_workspace(root.path()));
        options.command_capacity = 1;
        let host = CompositionHost::open(options).await.expect("host");

        let (entered_sender, entered_receiver) = oneshot::channel();
        let (release_sender, release_receiver) = oneshot::channel();
        host.inner
            .sender
            .send(RegistryMessage::Pause {
                entered: entered_sender,
                release: release_receiver,
            })
            .await
            .expect("pause registry");
        entered_receiver.await.expect("registry entered pause");

        let (filler_reply, _filler_response) = oneshot::channel();
        host.inner
            .sender
            .send(RegistryMessage::ReplayEvents {
                after_cursor: 0,
                through_cursor: 0,
                limit: 1,
                reply: filler_reply,
            })
            .await
            .expect("fill registry queue");

        let interrupted_host = host.clone();
        let interrupted = tokio::spawn(async move {
            interrupted_host
                .request_shutdown(OperationId("cancelled-shutdown".to_owned()))
                .await
        });
        tokio::task::yield_now().await;
        assert!(!interrupted.is_finished());
        interrupted.abort();
        let _ = interrupted.await;
        release_sender.send(()).expect("release registry");

        host.request_shutdown(OperationId("cancelled-shutdown".to_owned()))
            .await
            .expect("retry admitted shutdown");
        host.wait_terminated(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("retry terminates registry");
    }

    #[tokio::test]
    async fn replay_pump_requests_one_fixed_page_only_after_its_buffer_drains() {
        let (registry, mut requests) = mpsc::channel(2);
        let (replay_sender, mut replay) = mpsc::channel(EVENT_REPLAY_CHANNEL_CAPACITY);
        let pump = tokio::spawn(pump_event_replay(
            registry,
            replay_sender,
            0,
            u64::from(EVENT_REPLAY_PAGE_SIZE) + 1,
        ));

        let RegistryMessage::ReplayEvents {
            after_cursor,
            through_cursor,
            reply,
            ..
        } = requests.recv().await.expect("first replay page request")
        else {
            panic!("replay pump sent an unexpected registry message")
        };
        assert_eq!(after_cursor, 0);
        assert_eq!(through_cursor, u64::from(EVENT_REPLAY_PAGE_SIZE) + 1);
        reply
            .send(Ok((1..=u64::from(EVENT_REPLAY_PAGE_SIZE))
                .map(|cursor| {
                    EventEnvelope::new(
                        format!("event-{cursor}"),
                        cursor,
                        GraphRevision(0),
                        Event::HostShuttingDown,
                    )
                })
                .collect()))
            .expect("reply with first page");

        for expected_cursor in 1..=u64::from(EVENT_REPLAY_PAGE_SIZE) {
            let event = replay
                .recv()
                .await
                .expect("buffered replay event")
                .expect("valid replay event");
            assert_eq!(event.cursor, expected_cursor);
            if expected_cursor < u64::from(EVENT_REPLAY_PAGE_SIZE) {
                assert!(
                    requests.try_recv().is_err(),
                    "pump fetched another page while the current page was buffered"
                );
            }
        }

        let RegistryMessage::ReplayEvents {
            after_cursor,
            through_cursor,
            reply,
            ..
        } = requests.recv().await.expect("final replay page request")
        else {
            panic!("replay pump sent an unexpected registry message")
        };
        assert_eq!(after_cursor, u64::from(EVENT_REPLAY_PAGE_SIZE));
        assert_eq!(through_cursor, u64::from(EVENT_REPLAY_PAGE_SIZE) + 1);
        reply
            .send(Ok(vec![EventEnvelope::new(
                "last-event",
                through_cursor,
                GraphRevision(0),
                Event::HostShuttingDown,
            )]))
            .expect("reply with final page");
        assert_eq!(
            replay
                .recv()
                .await
                .expect("final replay event")
                .expect("valid final replay event")
                .cursor,
            through_cursor
        );
        assert!(replay.recv().await.is_none());
        pump.await.expect("replay pump");
    }

    #[tokio::test]
    async fn replay_failure_terminates_before_live_events_can_hide_a_gap() {
        let (registry, mut requests) = mpsc::channel(1);
        let (replay_sender, replay) = mpsc::channel(EVENT_REPLAY_CHANNEL_CAPACITY);
        let (events, live) = broadcast::channel(1);
        let pump = tokio::spawn(pump_event_replay(registry, replay_sender, 0, 1));
        let RegistryMessage::ReplayEvents { reply, .. } =
            requests.recv().await.expect("replay page request")
        else {
            panic!("replay pump sent an unexpected registry message")
        };
        reply
            .send(Err(HostError::InvalidEnvelope(
                "corrupt durable event".to_owned(),
            )))
            .expect("reply with replay failure");

        let mut stream = EventStream::new(replay, live, 0);
        assert!(matches!(
            stream.recv().await,
            Some(Err(HostError::InvalidEnvelope(message)))
                if message == "corrupt durable event"
        ));
        events
            .send(EventEnvelope::new(
                "live-after-gap",
                2,
                GraphRevision(0),
                Event::HostShuttingDown,
            ))
            .expect("publish live event");
        assert!(stream.recv().await.is_none());
        pump.await.expect("replay pump");
    }

    #[tokio::test]
    async fn lagged_live_subscription_terminates_before_later_events_can_hide_the_gap() {
        let (replay_sender, replay) = mpsc::channel(EVENT_REPLAY_CHANNEL_CAPACITY);
        drop(replay_sender);
        let (events, live) = broadcast::channel(1);
        let mut stream = EventStream::new(replay, live, 0);

        events
            .send(EventEnvelope::new(
                "event-1",
                1,
                GraphRevision(0),
                Event::HostShuttingDown,
            ))
            .expect("publish first live event");
        events
            .send(EventEnvelope::new(
                "event-2",
                2,
                GraphRevision(0),
                Event::HostShuttingDown,
            ))
            .expect("publish second live event");

        assert!(matches!(
            stream.recv().await,
            Some(Err(HostError::SubscriberLagged { skipped: 1 }))
        ));
        events
            .send(EventEnvelope::new(
                "event-3",
                3,
                GraphRevision(0),
                Event::HostShuttingDown,
            ))
            .expect("publish event after gap");
        assert!(stream.recv().await.is_none());
    }

    fn plugin_gate_fixture(
        root: &Path,
    ) -> (
        RoutingSnapshot,
        BTreeMap<InstanceId, PluginInspection>,
        CompositionFiles,
        PluginCommandRequest,
    ) {
        let manifest_path = root.join("rsi-meta.toml");
        let lock_path = root.join("rsi-meta.lock");
        fs::write(&manifest_path, "manifest").expect("manifest marker");
        fs::write(&lock_path, "lock").expect("lock marker");
        let instance_id = InstanceId::new("hmr");
        let generation = Arc::new(Generation::new(7, instance_id.clone()));
        generation.mark_admitting();
        let package = PackageSource {
            package_id: PackageId::new("fixture.hmr"),
            version: "0.0.1".to_owned(),
            manifest_path: root.join("plugin.toml"),
            target: "test".to_owned(),
            manifest_sha256: ContentHash::digest(b"manifest"),
            artifact_sha256: ContentHash::digest(b"artifact"),
            config_schema_sha256: None,
        };
        let instance = InstanceSnapshot {
            id: instance_id.clone(),
            package,
            scope: ScopeId::new("root"),
            status: InstanceStatus::Active,
            provides: Vec::new(),
            requires: Vec::new(),
        };
        let graph = GraphSnapshot {
            revision: GraphRevision(3),
            composition_id: "demo".to_owned(),
            instances: BTreeMap::from([(instance_id.clone(), instance.clone())]),
            bindings: Vec::new(),
            retiring_instances: Vec::new(),
        };
        let routing = RoutingSnapshot::new(
            graph,
            BTreeMap::new(),
            BTreeMap::from([(instance_id.clone(), generation)]),
        );
        let inspections = BTreeMap::from([(
            instance_id.clone(),
            PluginInspection {
                instance,
                process_fixed: true,
                capabilities: vec!["control.apply-manifest".to_owned()],
                config_schema_path: None,
                config_schema: None,
            },
        )]);
        let installed = CompositionFiles::new(&manifest_path, &lock_path);
        let request = PluginCommandRequest {
            composition_id: "demo".to_owned(),
            instance_id,
            generation: 7,
            envelope: CommandEnvelope::new(
                "content-1",
                Command::ApplyManifestPath {
                    manifest_path,
                    lock_path,
                },
            ),
            reply: None,
        };
        (routing, inspections, installed, request)
    }

    #[test]
    fn plugin_command_policy_rejects_production_path_capability_and_stale_provenance() {
        let root = tempfile::tempdir().expect("tempdir");
        let (routing, mut inspections, installed, mut request) = plugin_gate_fixture(root.path());
        let lock_before = fs::read(&installed.lock_path).expect("lock before gates");

        let production = validate_plugin_command_admission(
            &routing,
            CompositionMode::Production,
            &inspections,
            Some(&installed),
            &request,
        )
        .expect_err("production rejects plugin apply");
        assert_eq!(production.code, "plugin_command_forbidden");

        let Command::ApplyManifestPath { lock_path, .. } = &mut request.envelope.payload else {
            unreachable!("fixture command is apply")
        };
        *lock_path = root.path().join("other.lock");
        fs::write(&*lock_path, "other").expect("other lock");
        let wrong_path = validate_plugin_command_admission(
            &routing,
            CompositionMode::Development,
            &inspections,
            Some(&installed),
            &request,
        )
        .expect_err("wrong pair rejected");
        assert_eq!(wrong_path.code, "plugin_command_path_mismatch");

        request.envelope.payload = Command::ApplyManifestPath {
            manifest_path: installed.manifest_path.clone(),
            lock_path: installed.lock_path.clone(),
        };
        inspections
            .get_mut(&request.instance_id)
            .expect("inspection")
            .capabilities
            .clear();
        let missing_capability = validate_plugin_command_admission(
            &routing,
            CompositionMode::Development,
            &inspections,
            Some(&installed),
            &request,
        )
        .expect_err("capability is mandatory");
        assert_eq!(missing_capability.code, "plugin_command_forbidden");

        inspections
            .get_mut(&request.instance_id)
            .expect("inspection")
            .capabilities
            .push("control.apply-manifest".to_owned());
        request.generation += 1;
        let stale = validate_plugin_command_admission(
            &routing,
            CompositionMode::Development,
            &inspections,
            Some(&installed),
            &request,
        )
        .expect_err("stale generation rejected");
        assert_eq!(stale.code, "plugin_command_stale");
        assert_eq!(
            fs::read(&installed.lock_path).expect("lock after gates"),
            lock_before
        );
        assert_eq!(routing.revision(), GraphRevision(3));
    }

    #[test]
    fn command_identifiers_are_bounded_before_persistence() {
        let command = CommandEnvelope::new("c".repeat(256), Command::QueryGraph);

        assert!(command.validate().is_err());
    }

    #[test]
    fn runtime_reuse_is_invalidated_by_composition_identity_and_resolved_route_changes() {
        let fingerprint = |package: &str| InstanceFingerprint {
            semantic_hash: ContentHash::digest(package.as_bytes()),
            artifact_hash: ContentHash::digest(package.as_bytes()),
            process_fixed: false,
            package_id: PackageId::new(package),
        };
        let fingerprints = BTreeMap::from([
            (InstanceId::new("consumer"), fingerprint("consumer")),
            (InstanceId::new("provider-a"), fingerprint("provider-a")),
            (InstanceId::new("provider-b"), fingerprint("provider-b")),
        ]);
        let graph = |composition_id: &str, provider: &str| GraphSnapshot {
            revision: GraphRevision(1),
            composition_id: composition_id.to_owned(),
            instances: BTreeMap::new(),
            bindings: vec![crate::model::BindingSnapshot {
                consumer: InstanceId::new("consumer"),
                service: ServiceKey::new("fixture.echo"),
                provider: InstanceId::new(provider),
                explicit: false,
            }],
            retiring_instances: Vec::new(),
        };

        let route_change = affected_instances(
            &fingerprints,
            &fingerprints,
            &graph("demo", "provider-a"),
            &graph("demo", "provider-b"),
        );
        assert!(route_change.contains(&InstanceId::new("consumer")));

        let identity_change = affected_instances(
            &fingerprints,
            &fingerprints,
            &graph("old", "provider-a"),
            &graph("new", "provider-a"),
        );
        assert_eq!(identity_change.len(), fingerprints.len());
    }

    #[test]
    fn parallel_service_bindings_form_one_dependency_edge() {
        let package = |id: &str| PackageSource {
            package_id: PackageId::new(id),
            version: "1.0.0".to_owned(),
            manifest_path: PathBuf::from(format!("/{id}/plugin.toml")),
            target: "test".to_owned(),
            manifest_sha256: ContentHash::digest(format!("{id}-manifest")),
            artifact_sha256: ContentHash::digest(format!("{id}-artifact")),
            config_schema_sha256: None,
        };
        let instance = |id: &str, provides: Vec<ServiceKey>, requires| InstanceSnapshot {
            id: InstanceId::new(id),
            package: package(id),
            scope: ScopeId::new("root"),
            status: InstanceStatus::Active,
            provides,
            requires,
        };
        let provider = InstanceId::new("provider");
        let consumer = InstanceId::new("consumer");
        let alpha = ServiceKey::new("fixture.alpha");
        let beta = ServiceKey::new("fixture.beta");
        let graph = GraphSnapshot {
            revision: GraphRevision(1),
            composition_id: "parallel-bindings".to_owned(),
            instances: BTreeMap::from([
                (
                    provider.clone(),
                    instance("provider", vec![alpha.clone(), beta.clone()], Vec::new()),
                ),
                (
                    consumer.clone(),
                    instance(
                        "consumer",
                        Vec::new(),
                        vec![
                            ServiceRequirement {
                                service: alpha.clone(),
                                optional: false,
                            },
                            ServiceRequirement {
                                service: beta.clone(),
                                optional: false,
                            },
                        ],
                    ),
                ),
            ]),
            bindings: vec![
                crate::model::BindingSnapshot {
                    consumer: consumer.clone(),
                    service: alpha,
                    provider: provider.clone(),
                    explicit: false,
                },
                crate::model::BindingSnapshot {
                    consumer: consumer.clone(),
                    service: beta,
                    provider: provider.clone(),
                    explicit: false,
                },
            ],
            retiring_instances: Vec::new(),
        };

        assert_eq!(
            dependency_waves(&graph).unwrap(),
            vec![vec![provider], vec![consumer]]
        );
    }

    #[tokio::test]
    async fn wait_terminated_deadline_includes_join_lock_contention() {
        let temp = tempfile::tempdir().expect("tempdir");
        let host = CompositionHost::open(OpenOptions::new(CompositionWorkspace {
            database_path: temp.path().join("state.sqlite3"),
            cache_root: temp.path().join("cache"),
            manifest_path: temp.path().join("composition.toml"),
            lock_path: temp.path().join("rsi-meta.lock"),
        }))
        .await
        .expect("host");
        let join_guard = host.inner.join.lock().await;

        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            host.wait_terminated(Instant::now() + std::time::Duration::from_millis(10)),
        )
        .await
        .expect("the public deadline must bound waiting for the join mutex");
        assert!(matches!(waited, Err(HostError::ShutdownDeadline)));

        drop(join_guard);
        host.request_shutdown(OperationId("deadline-test-shutdown".to_owned()))
            .await
            .expect("request shutdown");
        host.wait_terminated(Instant::now() + std::time::Duration::from_secs(1))
            .await
            .expect("shutdown");
    }

    #[test]
    fn plugin_effect_id_is_content_bound_and_candidate_pollution_is_replaced() {
        let root = tempfile::tempdir().expect("tempdir");
        let (_, _, installed, mut first) = plugin_gate_fixture(root.path());
        let lock = CompositionLock {
            format_version: 0,
            target: "test-target".to_owned(),
            manifest_sha256: ContentHash::digest(b"manifest"),
            packages: Vec::new(),
        };
        let first_effect = plugin_effect_command_id(&first, &lock).expect("effect identity");
        let first_provenance = plugin_provenance_command_id(&first, GraphRevision(3));
        first.generation = 99;
        assert_eq!(
            plugin_effect_command_id(&first, &lock).expect("stable effect identity"),
            first_effect
        );
        let mut changed_lock = lock.clone();
        changed_lock.manifest_sha256 = ContentHash::digest(b"changed manifest");
        assert_ne!(
            plugin_effect_command_id(&first, &changed_lock).expect("changed effect identity"),
            first_effect
        );
        assert_ne!(
            plugin_provenance_command_id(&first, GraphRevision(4)),
            first_provenance
        );

        let candidate = plugin_candidate_lock_path(&installed.lock_path, &first_effect);
        fs::write(&candidate, "untrusted stale bytes").expect("polluted candidate");
        write_plugin_candidate_lock(&candidate, &lock).expect("replace polluted candidate");
        let expected = toml::to_string_pretty(&lock).expect("lock TOML");
        assert_eq!(
            fs::read(&candidate).expect("candidate bytes"),
            expected.as_bytes()
        );
        write_plugin_candidate_lock(&candidate, &lock).expect("exact residue is reusable");
    }

    #[test]
    fn cutover_mutex_never_exposes_the_stopped_old_snapshot() {
        let root = tempfile::tempdir().expect("tempdir");
        let (old, _, _, _) = plugin_gate_fixture(root.path());
        old.mark_admitting();
        let mut next_graph = old.graph().clone();
        next_graph.revision = GraphRevision(4);
        let next = RoutingSnapshot::new(
            next_graph,
            old.routes.clone(),
            old.generations()
                .map(|generation| (generation.instance.clone(), Arc::clone(generation)))
                .collect(),
        );
        let old = Arc::new(old);
        let routing = Arc::new(ArcSwap::from(Arc::clone(&old)));
        let cutover = Arc::new(StdMutex::new(()));
        let (inside_sender, inside_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let commit = {
            let routing = Arc::clone(&routing);
            let cutover = Arc::clone(&cutover);
            let old = Arc::clone(&old);
            std::thread::spawn(move || {
                publish_routing_cutover(&cutover, &routing, &old, &[], next, || {
                    inside_sender.send(()).expect("announce cutover gap");
                    release_receiver.recv().expect("release cutover");
                });
            })
        };
        inside_receiver
            .recv()
            .expect("commit reached stopped-before-store point");
        let start_reader = Arc::new(std::sync::Barrier::new(2));
        let reader = {
            let routing = Arc::clone(&routing);
            let cutover = Arc::clone(&cutover);
            let start_reader = Arc::clone(&start_reader);
            std::thread::spawn(move || {
                start_reader.wait();
                with_current_routing(&cutover, &routing, RoutingSnapshot::revision)
            })
        };
        start_reader.wait();
        release_sender.send(()).expect("finish cutover");
        commit.join().expect("commit thread");
        assert_eq!(reader.join().expect("reader thread"), GraphRevision(4));
        assert_eq!(routing.load().revision(), GraphRevision(4));
    }
}
