use super::callback_gate::{
    BlockingCallbackWaitError, CallbackGate, CallbackHandoff, CallbackHandoffError,
    callback_deadline, run_bounded_blocking_callback,
};
use super::host::{HostLease, HostState};
use super::host_channel::{ProviderBridge, pump_provider};
#[cfg(test)]
use super::module_teardown::TeardownJob;
use super::module_teardown::{FinalResources, ModuleTeardownQueue};
use super::transport::{PluginReply, PluginTransport, copy_bytes, frame};
use crate::catalog::{CatalogInner, StagedArtifact, StagedModuleLoad};
use crate::worker::{
    CallbackCompletion, CallbackWaitError, DestructionReservation, InstanceReservation,
    NativeExecutor, run_bounded_callback,
};
use crate::{
    LoaderError, MAX_NATIVE_CONFIG_BYTES, MAX_NATIVE_IDENTITY_BYTES, MAX_NATIVE_REQUIREMENTS,
};
use async_trait::async_trait;
use libloading::Library;
use rsi_meta::{
    ActivationPlan, CleanupFuture, ConfigValue, FactoryIdentity, MetaError, PluginFactory,
    PreparedActivation, ProviderChannel, Requirement, ServiceEndpoint,
};
use rsi_meta_plugin::{
    ActivateInput, BasicOutput, BytesInput, BytesOutput, CAP_KIND_INSTANCE, CAP_KIND_PREPARED,
    CapId, CapInput, CapOutput, Injection, PLUGIN_ACTIVATE, PLUGIN_CREATE, PLUGIN_ENTRY_SYMBOL,
    PLUGIN_IDENTITY, PLUGIN_PREPARE, PLUGIN_SERVE_PORT, PluginEntryFn, PluginTable, PrepareOutput,
    RIGHT_MUTATE, RIGHT_RETAIN, RawRequirement, STATUS_OK, ServeInput,
};
use serde_json::Value;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;
use tokio::runtime::Handle;

pub(crate) struct ModuleControl {
    transport: Arc<PluginTransport>,
    host: Option<HostLease>,
    library: Option<Library>,
    artifact: Option<StagedArtifact>,
    catalog: Option<Arc<CatalogInner>>,
    queue: Arc<ModuleTeardownQueue>,
    executor: NativeExecutor,
    identity: FactoryIdentity,
    digest: String,
    callback_timeout: Duration,
    factory_gate: Arc<CallbackGate>,
}

// SAFETY: libloading keeps the mapped image live; all PluginTable access goes
// through PluginTransport admission, and factory/instance gates serialize the
// opaque plugin state required by the ABI.
unsafe impl Send for ModuleControl {}
// SAFETY: Same transport and gate invariants as the Send implementation.
unsafe impl Sync for ModuleControl {}

impl fmt::Debug for ModuleControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModuleControl")
            .field("identity", &self.identity)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl ModuleControl {
    /// # Safety
    ///
    /// The staged artifact is trusted executable code and must uphold the ABI
    /// v2 table, callback, pointer, and no-unwind contracts.
    pub(crate) unsafe fn load(
        resources: StagedModuleLoad,
        digest: String,
        executor: NativeExecutor,
        finalizer: DestructionReservation,
        callback_timeout: Duration,
    ) -> Result<Self, LoaderError> {
        let path = resources.artifact().loader_path();
        // SAFETY: Caller explicitly trusts execution of the verified staging
        // file, which remains retained by `resources` through mapping lifetime.
        let library = unsafe { Library::new(&path) }?;
        let queue = ModuleTeardownQueue::new(executor.clone(), finalizer);
        let catalog = resources.catalog();
        // SAFETY: Symbol name and function type are the complete ABI v2 entry
        // contract. Resolve it before creating a persistent HostTable lease:
        // symbol lookup failure means no plugin code has received host state.
        let entry = match unsafe { library.get::<PluginEntryFn>(PLUGIN_ENTRY_SYMBOL) } {
            Ok(entry) => *entry,
            Err(error) => {
                enqueue_unentered_release(&queue, None, library, resources);
                return Err(error.into());
            }
        };
        let host = match HostLease::new(
            catalog.host_capability_limit(),
            catalog.host_output_limit(),
            Arc::clone(&catalog.host_resources),
        ) {
            Ok(host) => host,
            Err(error) => {
                enqueue_unentered_release(&queue, None, library, resources);
                return Err(error);
            }
        };
        let host_table = host.table();
        let mut plugin = PluginTable::EMPTY;
        // SAFETY: Both complete aligned tables remain live for the synchronous
        // entry exchange; plugin output capacity is exact.
        let status = unsafe {
            entry(
                &raw const host_table,
                &raw mut plugin,
                PluginTable::STRUCT_SIZE,
            )
        };
        if !plugin.is_compatible_for_host(rsi_meta_plugin::ABI_MINOR) {
            host.retire_without_plugin();
            let error = LoaderError::IncompatibleAbi {
                host_major: rsi_meta_plugin::ABI_MAJOR,
                host_minor: rsi_meta_plugin::ABI_MINOR,
                plugin_major: plugin.header.abi_major,
                plugin_minor: plugin.header.abi_minor,
            };
            enqueue_unentered_release(&queue, Some(host), library, resources);
            return Err(error);
        }

        let transport = Arc::new(PluginTransport::new(plugin));
        let (artifact, catalog) = resources.into_parts();
        let mut module = Self {
            transport,
            host: Some(host),
            library: Some(library),
            artifact: Some(artifact),
            catalog: Some(catalog),
            queue,
            executor,
            identity: FactoryIdentity::Artifact {
                plugin: "entry-failed".to_owned(),
                sha256: digest.clone(),
            },
            digest,
            callback_timeout,
            factory_gate: Arc::new(CallbackGate::new()),
        };
        if status != STATUS_OK {
            drop(module);
            return Err(LoaderError::PluginEntry { status });
        }
        // A compatible table returned with non-OK status transfers cleanup
        // authority only. IDENTITY is therefore first called after status OK.
        module.identity = read_identity(&module.transport, &module.digest)?;
        Ok(module)
    }

    pub(crate) fn staged_artifact(&self) -> &StagedArtifact {
        self.artifact
            .as_ref()
            .expect("live native module retains staged artifact")
    }

    pub(super) fn transport(&self) -> &Arc<PluginTransport> {
        &self.transport
    }

    pub(super) fn host(&self) -> &Arc<HostState> {
        self.host
            .as_ref()
            .expect("live native module retains host table")
            .state()
    }

    fn try_factory_gate(
        &self,
        operation: &'static str,
    ) -> Result<super::callback_gate::CallbackAdmission, LoaderError> {
        self.factory_gate.acquire_factory(operation)
    }

    fn poison_factory(&self) {
        self.factory_gate.poison();
    }
}

fn enqueue_unentered_release(
    queue: &Arc<ModuleTeardownQueue>,
    host: Option<HostLease>,
    library: Library,
    resources: StagedModuleLoad,
) {
    let (artifact, catalog) = resources.into_parts();
    queue.enqueue(Box::new(move || {
        drop(host);
        drop(artifact);
        drop(catalog);
        drop(library);
    }));
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum CleanupDisposition {
    Armed,
    Rejected,
}

pub(super) struct CleanupMove {
    state: Mutex<Option<CleanupDisposition>>,
    resolved: Condvar,
}

impl CleanupMove {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(None),
            resolved: Condvar::new(),
        })
    }

    pub(super) fn resolve(&self, disposition: CleanupDisposition) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.is_none());
        *state = Some(disposition);
        self.resolved.notify_all();
    }

    fn wait(&self) -> CleanupDisposition {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.is_none() {
            state = self
                .resolved
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.expect("resolved cleanup move has a disposition")
    }
}

/// Resolves a cleanup transfer as rejected if host mutation unwinds before it
/// can publish the ordinary `HOST_EFFECT_DEFER` disposition.
pub(super) struct CleanupMoveResolution {
    moved: Arc<CleanupMove>,
    resolved: bool,
}

impl CleanupMoveResolution {
    pub(super) fn new(moved: Arc<CleanupMove>) -> Self {
        Self {
            moved,
            resolved: false,
        }
    }

    pub(super) fn resolve(mut self, disposition: CleanupDisposition) {
        self.resolved = true;
        self.moved.resolve(disposition);
    }
}

impl Drop for CleanupMoveResolution {
    fn drop(&mut self) {
        if !self.resolved {
            self.moved.resolve(CleanupDisposition::Rejected);
        }
    }
}

/// Single owner for one plugin cleanup capability transferred by `HOST_EFFECT_DEFER`.
///
/// Scheduling happens before this owner can disappear. The reserved module
/// lane waits for the host-call disposition: an armed transfer always runs the
/// cleanup and then releases its lease, while a rejected transfer remains
/// plugin-owned and performs no foreign operation.
pub(super) struct CleanupLease {
    moved: Arc<CleanupMove>,
    schedule: Option<CleanupSchedule>,
}

type CleanupCompletion = tokio::sync::oneshot::Sender<Result<(), String>>;
type CleanupSchedule =
    Box<dyn FnOnce(Arc<CleanupMove>, Option<CleanupCompletion>) + Send + 'static>;

impl CleanupLease {
    pub(super) fn new(module: Arc<ModuleControl>, capability: CapId) -> (Self, Arc<CleanupMove>) {
        Self::with_schedule(Box::new(move |moved, completion| {
            let queue = Arc::clone(&module.queue);
            queue.enqueue(Box::new(move || {
                let result = if moved.wait() == CleanupDisposition::Rejected {
                    Ok(())
                } else {
                    let owned = PluginCap::new(capability, &module);
                    let run = module.transport.run_cleanup(capability);
                    let release = module.transport.release(owned.consume());
                    match (run, release) {
                        (Ok(()), Ok(())) => Ok(()),
                        (Err(run), Ok(())) => Err(run.to_string()),
                        (Ok(()), Err(release)) => Err(release.to_string()),
                        (Err(run), Err(release)) => Err(format!(
                            "{run}; cleanup lease release also failed: {release}"
                        )),
                    }
                };
                if let Some(completion) = completion {
                    let _ = completion.send(result);
                }
            }));
        }))
    }

    fn with_schedule(schedule: CleanupSchedule) -> (Self, Arc<CleanupMove>) {
        let moved = CleanupMove::new();
        (
            Self {
                moved: Arc::clone(&moved),
                schedule: Some(schedule),
            },
            moved,
        )
    }

    #[cfg(test)]
    fn new_for_test(schedule: CleanupSchedule) -> (Self, Arc<CleanupMove>) {
        Self::with_schedule(schedule)
    }

    pub(super) fn into_future(mut self) -> CleanupFuture {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.schedule(Some(sender));
        Box::pin(async move {
            receiver.await.unwrap_or_else(|error| {
                Err(format!("cleanup destruction lane disconnected: {error}"))
            })
        })
    }

    fn schedule(&mut self, completion: Option<CleanupCompletion>) {
        if let Some(schedule) = self.schedule.take() {
            schedule(Arc::clone(&self.moved), completion);
        }
    }
}

impl Drop for CleanupLease {
    fn drop(&mut self) {
        self.schedule(None);
    }
}

impl Drop for ModuleControl {
    fn drop(&mut self) {
        let catalog = self.catalog.take();
        let resources = FinalResources::new(
            Arc::clone(&self.transport),
            self.host
                .take()
                .expect("module host resources are single-use"),
            self.library
                .take()
                .expect("module library resources are single-use"),
            self.artifact.take(),
            catalog,
        );
        self.queue.enqueue(Box::new(move || resources.finalize()));
    }
}

struct PluginCap {
    id: CapId,
    module: Arc<ModuleControl>,
    release_on_drop: bool,
}

impl PluginCap {
    fn new(id: CapId, module: &Arc<ModuleControl>) -> Self {
        Self {
            id,
            module: Arc::clone(module),
            release_on_drop: true,
        }
    }

    fn consume(mut self) -> CapId {
        self.release_on_drop = false;
        self.id
    }
}

impl Drop for PluginCap {
    fn drop(&mut self) {
        if !self.release_on_drop {
            return;
        }
        let transport = Arc::clone(&self.module.transport);
        let capability = self.id;
        self.module.queue.enqueue(Box::new(move || {
            let _ = transport.release(capability);
        }));
    }
}

/// Worker-owned CREATE result. If the receiver times out or disappears, Drop
/// converts a successfully created instance lease into `DESTROY_INSTANCE` and
/// carries the live-instance reservation through that destruction job.
struct CreateOutcome {
    module: Arc<ModuleControl>,
    result: Option<Result<PluginCap, LoaderError>>,
    prepared: Option<NativePrepared>,
    reservation: Option<InstanceReservation>,
}

impl CreateOutcome {
    fn into_instance_parts(mut self) -> Result<(PluginCap, InstanceReservation), LoaderError> {
        let result = self
            .result
            .take()
            .expect("create outcome result is consumed once");
        // CREATE has returned, so the borrowed prepared lease can retire before
        // either handing the instance off or reporting its error.
        drop(self.prepared.take());
        match result {
            Ok(cap) => Ok((
                cap,
                self.reservation
                    .take()
                    .expect("create outcome retains instance admission"),
            )),
            Err(error) => Err(error),
        }
    }
}

impl Drop for CreateOutcome {
    fn drop(&mut self) {
        let result = self.result.take();
        // Queue PREPARED release while the outcome's module owner still
        // prevents factory finalization from being enqueued.
        drop(self.prepared.take());
        let Some(Ok(cap)) = result else {
            return;
        };
        let instance = cap.consume();
        let reservation = self.reservation.take();
        let transport = Arc::clone(&self.module.transport);
        let pending = self.module.executor.begin_instance_destruction();
        self.module.queue.enqueue(Box::new(move || {
            {
                let _pending = pending;
                let _ = transport.destroy_instance(instance);
            }
            drop(reservation);
        }));
    }
}

struct NativePrepared {
    module: Arc<ModuleControl>,
    cap: PluginCap,
    requirements: Vec<Requirement>,
}

pub struct NativeFactory {
    pub(super) module: Arc<ModuleControl>,
}

impl NativeFactory {
    pub(crate) fn from_module(module: Arc<ModuleControl>) -> Self {
        Self { module }
    }

    pub fn module_digest(&self) -> &str {
        &self.module.digest
    }
}

impl fmt::Debug for NativeFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFactory")
            .field("identity", &self.module.identity)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PluginFactory for NativeFactory {
    fn identity(&self) -> FactoryIdentity {
        self.module.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let module = Arc::clone(&self.module);
        let gate = module
            .try_factory_gate("prepare")
            .map_err(meta_from_loader)?;
        let desired = desired.clone();
        let worker = Arc::clone(&module);
        let completion = Arc::new(CallbackCompletion::new());
        let worker_completion = Arc::clone(&completion);
        let receiver = module
            .executor
            .spawn_blocking_callback("prepare", move || {
                let mut gate = gate;
                gate.arm();
                let result = prepare_native(worker, &desired);
                CallbackHandoff::new(result, gate, worker_completion)
            })
            .map_err(meta_from_loader)?;
        let timeout_module = Arc::clone(&module);
        let on_timeout = move || timeout_module.poison_factory();
        match run_bounded_blocking_callback(
            &receiver,
            &completion,
            module.callback_timeout,
            &on_timeout,
        ) {
            Ok(result) => result.map_err(meta_from_loader),
            Err(BlockingCallbackWaitError::TimedOut) => {
                Err(MetaError::Timeout("native prepare callback"))
            }
            Err(BlockingCallbackWaitError::Disconnected) => {
                module.poison_factory();
                Err(MetaError::Activation(
                    "native prepare worker disconnected".to_owned(),
                ))
            }
        }
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let prepared = plan.take_state::<NativePrepared>()?;
        if !Arc::ptr_eq(&prepared.module, &self.module) {
            return Err(MetaError::Activation(
                "native prepared state belongs to another module".to_owned(),
            ));
        }
        let requirements = prepared.requirements.clone();
        let context = plan.context().clone();
        let lineage = plan.lineage_call_id().0;
        let instance = create_instance(Arc::clone(&self.module), prepared, context.clone()).await?;
        let cleanup_instance = Arc::clone(&instance);
        plan.defer(
            "native instance destruction",
            Box::new(move || cleanup_instance.destroy_future()),
        )?;
        let injections = requirements
            .iter()
            .map(|requirement| {
                plan.inject(&requirement.key).cloned().ok_or_else(|| {
                    MetaError::Activation(format!(
                        "missing injected capability {}",
                        requirement.key
                    ))
                })
            })
            .collect::<rsi_meta::Result<Vec<_>>>()?;
        drop(plan);
        activate_instance(context, lineage, &requirements, injections, &instance).await
    }
}

pub(super) struct NativeInstance {
    module: Arc<ModuleControl>,
    cap: Mutex<Option<PluginCap>>,
    reservation: Mutex<Option<InstanceReservation>>,
    gate: Arc<CallbackGate>,
    active: AtomicBool,
    destroyed: AtomicBool,
}

impl NativeInstance {
    fn cap_id(&self) -> Result<CapId, LoaderError> {
        self.cap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|cap| cap.id)
            .ok_or_else(|| LoaderError::Callback {
                operation: "instance callback",
                message: "native instance is destroyed".to_owned(),
            })
    }

    fn destroy_future(self: Arc<Self>) -> CleanupFuture {
        Box::pin(async move { self.destroy().await.map_err(|error| error.to_string()) })
    }

    fn enqueue_destruction(
        &self,
        capability: CapId,
        reservation: Option<InstanceReservation>,
        completion: Option<tokio::sync::oneshot::Sender<Result<(), LoaderError>>>,
    ) {
        let transport = Arc::clone(&self.module.transport);
        let gate = Arc::clone(&self.gate);
        let pending = self.module.executor.begin_instance_destruction();
        self.module.queue.enqueue(Box::new(move || {
            let result = {
                let _pending = pending;
                gate.wait_idle();
                transport.destroy_instance(capability)
            };
            drop(reservation);
            if let Some(completion) = completion {
                let _ = completion.send(result);
            }
        }));
    }

    async fn destroy(self: &Arc<Self>) -> Result<(), LoaderError> {
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.active.store(false, Ordering::Release);
        let cap = self
            .cap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("first instance destruction owns plugin capability")
            .consume();
        let reservation = self
            .reservation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("first instance destruction owns instance reservation");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.enqueue_destruction(cap, Some(reservation), Some(sender));
        receiver.await.map_err(|error| LoaderError::Callback {
            operation: "instance destruction",
            message: format!("destruction lane disconnected: {error}"),
        })?
    }
}

impl Drop for NativeInstance {
    fn drop(&mut self) {
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(cap) = self
            .cap
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let capability = cap.consume();
        let reservation = self
            .reservation
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.enqueue_destruction(capability, reservation, None);
    }
}

pub(super) struct NativeEndpoint {
    instance: Weak<NativeInstance>,
    port: Arc<[u8]>,
}

impl NativeEndpoint {
    pub(super) fn new(instance: Weak<NativeInstance>, port: Vec<u8>) -> Self {
        Self {
            instance,
            port: port.into(),
        }
    }
}

impl fmt::Debug for NativeEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeEndpoint")
            .field("port_bytes", &self.port.len())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ServiceEndpoint for NativeEndpoint {
    async fn serve(
        &self,
        invocation: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        let instance = self
            .instance
            .upgrade()
            .ok_or_else(|| MetaError::Service("native instance is retiring".to_owned()))?;
        if !instance.active.load(Ordering::Acquire) {
            return Err(MetaError::Service("native instance is inactive".to_owned()));
        }
        let lineage = invocation.lineage_call_id().0;
        let gate = instance
            .gate
            .acquire_instance(lineage)
            .map_err(meta_service)?;
        let runtime_handle = Handle::current();
        let callback_frame = instance
            .module
            .host()
            .callback_frame(runtime_handle.clone());
        let (provider, mut commands) =
            ProviderBridge::new(Arc::clone(&callback_frame), invocation.cancellation());
        let provider_cap = HostState::provider_cap(&callback_frame, Arc::clone(&provider))
            .map_err(meta_service)?;
        drop(provider);
        let cap = instance.cap_id().map_err(meta_service)?;
        let port = Arc::clone(&self.port);
        let worker_instance = Arc::clone(&instance);
        let worker_frame = Arc::clone(&callback_frame);
        let completion = Arc::new(CallbackCompletion::new());
        let worker_completion = Arc::clone(&completion);
        let receiver = instance
            .module
            .executor
            .spawn_callback("serve", move || {
                let mut gate = gate;
                gate.arm();
                let input = ServeInput {
                    header: frame::<ServeInput>(),
                    callback_id: lineage,
                    instance: cap,
                    provider: provider_cap,
                    port: raw_bytes(&port),
                };
                let result = worker_instance
                    .module
                    .transport
                    .call::<_, BasicOutput>(PLUGIN_SERVE_PORT, &input, "serve")
                    .and_then(PluginReply::into_result)
                    .map(|_| ());
                worker_frame.seal();
                CallbackHandoff::new(result, gate, worker_completion)
            })
            .map_err(meta_service)?;
        let deadline = callback_deadline(instance.module.callback_timeout)?;
        let timeout_instance = Arc::clone(&instance);
        let timeout_runtime = invocation.provider_context().runtime().clone();
        let timeout_frame = Arc::clone(&callback_frame);
        let disconnected_instance = Arc::clone(&instance);
        let disconnected_runtime = timeout_runtime.clone();
        let disconnected_frame = Arc::clone(&callback_frame);
        let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            timeout_instance.gate.poison();
            timeout_frame.seal();
            timeout_runtime.mark_terminal("trusted native plugin service callback timed out");
        });
        let (sender, bounded) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result =
                match run_bounded_callback(receiver, completion, deadline, on_timeout).await {
                    Ok(result) => match result.into_inner() {
                        Ok(result) => result,
                        Err(CallbackHandoffError::TimedOut) => {
                            Err(LoaderError::Timeout("native serve callback"))
                        }
                    },
                    Err(CallbackWaitError::TimedOut) => {
                        Err(LoaderError::Timeout("native serve callback"))
                    }
                    Err(CallbackWaitError::Disconnected(error)) => {
                        disconnected_instance.gate.poison();
                        disconnected_frame.seal();
                        disconnected_runtime
                            .mark_terminal("trusted native plugin service callback disconnected");
                        Err(LoaderError::Callback {
                            operation: "serve",
                            message: format!("worker disconnected: {error}"),
                        })
                    }
                };
            let _ = sender.send(result);
        });
        pump_provider(&mut channel, &mut commands, bounded).await
    }
}

fn read_identity(
    transport: &Arc<PluginTransport>,
    digest: &str,
) -> Result<FactoryIdentity, LoaderError> {
    let input = CapInput {
        header: frame::<CapInput>(),
        capability: transport.factory(),
    };
    let reply = transport.call::<_, BytesOutput>(PLUGIN_IDENTITY, &input, "identity")?;
    if reply.status() != STATUS_OK {
        return reply.into_result().map(|_| unreachable!());
    }
    let value = *reply.value();
    require_owned_payload(&reply, value.bytes.len != 0, "identity")?;
    let bytes = copy_bytes(value.bytes, MAX_NATIVE_IDENTITY_BYTES, "identity")?;
    let plugin = String::from_utf8(bytes).map_err(|_| LoaderError::Protocol {
        operation: "identity",
        message: "plugin identity is not UTF-8".to_owned(),
    })?;
    if plugin.is_empty() {
        return Err(LoaderError::Protocol {
            operation: "identity",
            message: "plugin identity is empty".to_owned(),
        });
    }
    reply.release()?;
    Ok(FactoryIdentity::Artifact {
        plugin,
        sha256: digest.to_owned(),
    })
}

fn prepare_native(
    module: Arc<ModuleControl>,
    desired: &Value,
) -> Result<PreparedActivation, LoaderError> {
    let bytes = serde_json::to_vec(desired)?;
    if bytes.len() > MAX_NATIVE_CONFIG_BYTES {
        return Err(LoaderError::InvalidInput(
            "native desired configuration exceeds ABI bound".to_owned(),
        ));
    }
    let input = BytesInput {
        header: frame::<BytesInput>(),
        receiver: module.transport.factory(),
        bytes: raw_bytes(&bytes),
    };
    let reply = module
        .transport
        .call::<_, PrepareOutput>(PLUGIN_PREPARE, &input, "prepare")?;
    if reply.status() != STATUS_OK {
        return reply.into_result().map(|_| unreachable!());
    }
    let value = *reply.value();
    require_owned_payload(
        &reply,
        value.normalized_config.len != 0
            || value.requirement_count != 0
            || value.prepared.is_structurally_valid(),
        "prepare",
    )?;
    validate_plugin_cap(
        value.prepared,
        module.transport.issuer(),
        CAP_KIND_PREPARED,
        RIGHT_RETAIN | RIGHT_MUTATE,
        "prepare",
    )?;
    let normalized = copy_bytes(
        value.normalized_config,
        MAX_NATIVE_CONFIG_BYTES,
        "normalized configuration",
    )?;
    let requirements = copy_requirements(value.requirements, value.requirement_count)?;
    let retained_bytes =
        usize::try_from(value.retained_bytes).map_err(|_| LoaderError::Protocol {
            operation: "prepare",
            message: "prepared retained-byte declaration exceeds usize".to_owned(),
        })?;
    module.transport.retain(value.prepared)?;
    let cap = PluginCap::new(value.prepared, &module);
    reply.release()?;
    let config: Value = serde_json::from_slice(&normalized)?;
    let state = NativePrepared {
        module,
        cap,
        requirements: requirements.clone(),
    };
    let mut prepared = PreparedActivation::with_state(config, state, retained_bytes);
    for requirement in requirements {
        prepared = prepared.requiring(requirement);
    }
    Ok(prepared)
}

async fn create_instance(
    module: Arc<ModuleControl>,
    prepared: NativePrepared,
    context: rsi_meta::Context,
) -> rsi_meta::Result<Arc<NativeInstance>> {
    let gate = module
        .try_factory_gate("create")
        .map_err(meta_from_loader)?;
    let reservation = module
        .executor
        .reserve_instance()
        .map_err(meta_from_loader)?;
    let cap = prepared.cap.id;
    let worker_module = Arc::clone(&module);
    let completion = Arc::new(CallbackCompletion::new());
    let worker_completion = Arc::clone(&completion);
    let receiver = module
        .executor
        .spawn_callback("create", move || {
            let mut gate = gate;
            gate.arm();
            let result = create_native(&worker_module, cap);
            let outcome = CreateOutcome {
                module: worker_module,
                result: Some(result),
                prepared: Some(prepared),
                reservation: Some(reservation),
            };
            CallbackHandoff::new(outcome, gate, worker_completion)
        })
        .map_err(meta_from_loader)?;
    let deadline = callback_deadline(module.callback_timeout)?;
    let timeout_module = Arc::clone(&module);
    let timeout_runtime = context.runtime().clone();
    let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        timeout_module.poison_factory();
        timeout_runtime.mark_terminal("trusted native plugin create callback timed out");
    });
    let outcome = match run_bounded_callback(receiver, completion, deadline, on_timeout).await {
        Ok(outcome) => match outcome.into_inner() {
            Ok(outcome) => outcome,
            Err(CallbackHandoffError::TimedOut) => {
                return Err(MetaError::Timeout("native create callback"));
            }
        },
        Err(CallbackWaitError::TimedOut) => {
            return Err(MetaError::Timeout("native create callback"));
        }
        Err(CallbackWaitError::Disconnected(error)) => {
            module.poison_factory();
            context
                .runtime()
                .mark_terminal("trusted native plugin create callback disconnected");
            return Err(MetaError::Activation(format!(
                "native create worker disconnected: {error}"
            )));
        }
    };
    let (cap, reservation) = outcome.into_instance_parts().map_err(meta_from_loader)?;
    Ok(Arc::new(NativeInstance {
        module,
        cap: Mutex::new(Some(cap)),
        reservation: Mutex::new(Some(reservation)),
        gate: Arc::new(CallbackGate::new()),
        active: AtomicBool::new(false),
        destroyed: AtomicBool::new(false),
    }))
}

fn create_native(module: &Arc<ModuleControl>, prepared: CapId) -> Result<PluginCap, LoaderError> {
    let input = CapInput {
        header: frame::<CapInput>(),
        capability: prepared,
    };
    let reply = module
        .transport
        .call::<_, CapOutput>(PLUGIN_CREATE, &input, "create")?;
    if reply.status() != STATUS_OK {
        return reply.into_result().map(|_| unreachable!());
    }
    let capability = reply.value().capability;
    require_owned_payload(&reply, true, "create")?;
    validate_plugin_cap(
        capability,
        module.transport.issuer(),
        CAP_KIND_INSTANCE,
        RIGHT_RETAIN | RIGHT_MUTATE,
        "create",
    )?;
    module.transport.retain(capability)?;
    let cap = PluginCap::new(capability, module);
    reply.release()?;
    Ok(cap)
}

#[allow(clippy::too_many_lines)] // One callback ownership transaction spans adoption, watchdog, sealing, and gate release.
async fn activate_instance(
    context: rsi_meta::Context,
    lineage: u64,
    requirements: &[Requirement],
    injected: Vec<rsi_meta::Capability>,
    instance: &Arc<NativeInstance>,
) -> rsi_meta::Result<()> {
    let gate = instance
        .gate
        .acquire_instance(lineage)
        .map_err(meta_from_loader)?;
    let callback_frame = instance.module.host().callback_frame(Handle::current());
    let (activation_cap, activation) = HostState::activation_cap(
        &callback_frame,
        context.clone(),
        Arc::downgrade(&instance.module),
        Arc::downgrade(instance),
    )
    .map_err(meta_from_loader)?;
    let mut leases = Vec::with_capacity(requirements.len());
    let mut injections = Vec::with_capacity(requirements.len());
    for (index, capability) in injected.into_iter().enumerate() {
        let lease = instance
            .module
            .host()
            .insert_service(capability)
            .map_err(meta_from_loader)?;
        injections.push(Injection {
            requirement_index: u64::try_from(index).expect("bounded requirement index fits u64"),
            service: lease.id,
        });
        leases.push(lease);
    }
    let cap = instance.cap_id().map_err(meta_from_loader)?;
    let worker_instance = Arc::clone(instance);
    let worker_activation = Arc::clone(&activation);
    let completion = Arc::new(CallbackCompletion::new());
    let worker_completion = Arc::clone(&completion);
    let receiver = instance
        .module
        .executor
        .spawn_callback("activate", move || {
            let mut gate = gate;
            gate.arm();
            let input = ActivateInput {
                header: frame::<ActivateInput>(),
                callback_id: lineage,
                instance: cap,
                activation: activation_cap,
                injections: if injections.is_empty() {
                    core::ptr::null()
                } else {
                    injections.as_ptr()
                },
                injection_count: u64::try_from(injections.len()).unwrap_or(u64::MAX),
            };
            let result = worker_instance
                .module
                .transport
                .call::<_, BasicOutput>(PLUGIN_ACTIVATE, &input, "activate")
                .and_then(PluginReply::into_result)
                .map(|_| ());
            let accepted = worker_activation.accepted();
            worker_activation.seal();
            drop(leases);
            let result = result.and_then(|()| {
                if accepted {
                    Ok(())
                } else {
                    Err(LoaderError::Protocol {
                        operation: "activate",
                        message: "native activation returned without accepted effect transaction"
                            .to_owned(),
                    })
                }
            });
            CallbackHandoff::new(result, gate, worker_completion)
        })
        .map_err(meta_from_loader)?;
    let deadline = callback_deadline(instance.module.callback_timeout)?;
    let timeout_instance = Arc::clone(instance);
    let timeout_runtime = context.runtime().clone();
    let timeout_frame = Arc::clone(&callback_frame);
    let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        timeout_instance.gate.poison();
        timeout_frame.seal();
        timeout_runtime.mark_terminal("trusted native plugin activation callback timed out");
    });
    match run_bounded_callback(receiver, completion, deadline, on_timeout).await {
        Ok(result) => match result.into_inner() {
            Ok(result) => result.map_err(meta_from_loader)?,
            Err(CallbackHandoffError::TimedOut) => {
                return Err(MetaError::Timeout("native activation callback"));
            }
        },
        Err(CallbackWaitError::TimedOut) => {
            return Err(MetaError::Timeout("native activation callback"));
        }
        Err(CallbackWaitError::Disconnected(error)) => {
            instance.gate.poison();
            callback_frame.seal();
            context
                .runtime()
                .mark_terminal("trusted native plugin activation callback disconnected");
            return Err(MetaError::Activation(format!(
                "native activation worker disconnected: {error}"
            )));
        }
    }
    instance.active.store(true, Ordering::Release);
    Ok(())
}

fn copy_requirements(
    pointer: *const RawRequirement,
    count: u64,
) -> Result<Vec<Requirement>, LoaderError> {
    let count = usize::try_from(count).map_err(|_| LoaderError::Protocol {
        operation: "prepare",
        message: "requirement count exceeds usize".to_owned(),
    })?;
    if count > MAX_NATIVE_REQUIREMENTS
        || (count != 0 && pointer.is_null())
        || (count != 0
            && !pointer
                .addr()
                .is_multiple_of(std::mem::align_of::<RawRequirement>()))
    {
        return Err(LoaderError::Protocol {
            operation: "prepare",
            message: "requirement array is malformed or exceeds its bound".to_owned(),
        });
    }
    let raw = if count == 0 {
        &[][..]
    } else {
        // SAFETY: Count, multiplication bound, pointer, and alignment were
        // checked; the adopted prepare output keeps the trusted range live.
        unsafe { std::slice::from_raw_parts(pointer, count) }
    };
    let mut requirements = Vec::with_capacity(count);
    for requirement in raw {
        let key = String::from_utf8(copy_bytes(
            requirement.key,
            MAX_NATIVE_IDENTITY_BYTES,
            "requirement key",
        )?)
        .map_err(|_| LoaderError::Protocol {
            operation: "prepare",
            message: "requirement key is not UTF-8".to_owned(),
        })?;
        let contract = String::from_utf8(copy_bytes(
            requirement.contract,
            MAX_NATIVE_IDENTITY_BYTES,
            "requirement contract",
        )?)
        .map_err(|_| LoaderError::Protocol {
            operation: "prepare",
            message: "requirement contract is not UTF-8".to_owned(),
        })?;
        let version = u32::try_from(requirement.version).map_err(|_| LoaderError::Protocol {
            operation: "prepare",
            message: "requirement version exceeds u32".to_owned(),
        })?;
        requirements.push(Requirement::new(
            key,
            contract,
            rsi_meta::ContractVersion(version),
        ));
    }
    Ok(requirements)
}

fn validate_plugin_cap(
    capability: CapId,
    issuer: u64,
    kind: u32,
    rights: u32,
    operation: &'static str,
) -> Result<(), LoaderError> {
    if !capability.is_structurally_valid()
        || capability.issuer != issuer
        || capability.kind != kind
        || capability.rights != rights
    {
        Err(LoaderError::Protocol {
            operation,
            message: "plugin output capability has wrong issuer, kind, or rights".to_owned(),
        })
    } else {
        Ok(())
    }
}

fn require_owned_payload<O: super::transport::OutputFrame>(
    reply: &PluginReply<O>,
    required: bool,
    operation: &'static str,
) -> Result<(), LoaderError> {
    if required && !reply.owns_payload() {
        Err(LoaderError::Protocol {
            operation,
            message: "plugin output authority has no owning release token".to_owned(),
        })
    } else {
        Ok(())
    }
}

fn raw_bytes(bytes: &[u8]) -> rsi_meta_plugin::RawBytes {
    rsi_meta_plugin::RawBytes {
        ptr: if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

fn meta_from_loader(error: LoaderError) -> MetaError {
    match error {
        LoaderError::Timeout(operation) => MetaError::Timeout(operation),
        LoaderError::Busy { operation } => MetaError::Busy { operation },
        LoaderError::Reentrant { operation } => MetaError::Reentrant { operation },
        error => MetaError::Activation(error.to_string()),
    }
}

fn meta_service(error: LoaderError) -> MetaError {
    match error {
        LoaderError::Timeout(operation) => MetaError::Timeout(operation),
        LoaderError::Busy { operation } => MetaError::Busy { operation },
        LoaderError::Reentrant { operation } => MetaError::Reentrant { operation },
        error => MetaError::Service(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct FinalizeProbe(Arc<Mutex<Vec<&'static str>>>);

    impl Drop for FinalizeProbe {
        fn drop(&mut self) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push("finalize");
        }
    }

    #[test]
    fn armed_dormant_cleanup_runs_then_releases_before_finalization() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let finalizer = FinalizeProbe(Arc::clone(&operations));
        let (job_sender, job_receiver) = std::sync::mpsc::sync_channel::<TeardownJob>(1);
        let job_operations = Arc::clone(&operations);
        let (lease, moved) = CleanupLease::new_for_test(Box::new(move |moved, completion| {
            job_sender
                .send(Box::new(move || {
                    if moved.wait() == CleanupDisposition::Armed {
                        let mut operations = job_operations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        operations.push("run");
                        operations.push("release");
                    }
                    drop(finalizer);
                    if let Some(completion) = completion {
                        let _ = completion.send(Ok(()));
                    }
                }))
                .unwrap();
        }));

        moved.resolve(CleanupDisposition::Armed);
        drop(lease);
        job_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("dormant cleanup drop queues its teardown")();

        assert_eq!(
            *operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["run", "release", "finalize"]
        );
    }

    #[test]
    fn unresolved_cleanup_drop_rejects_the_transfer_before_teardown_waits() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let (cleanup, moved) = CleanupLease::new_for_test(Box::new(move |moved, _| {
            std::thread::spawn(move || {
                sender.send(moved.wait()).unwrap();
            });
        }));
        let resolution = CleanupMoveResolution::new(moved);

        drop(cleanup);
        drop(resolution);

        assert!(
            matches!(
                receiver.recv_timeout(Duration::from_millis(100)),
                Ok(CleanupDisposition::Rejected)
            ),
            "an unwind before host disposition left module teardown blocked"
        );
    }

    #[test]
    fn armed_cleanup_future_is_persistent_before_its_first_poll() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let (job_sender, job_receiver) = std::sync::mpsc::sync_channel::<TeardownJob>(1);
        let job_operations = Arc::clone(&operations);
        let (lease, moved) = CleanupLease::new_for_test(Box::new(move |moved, completion| {
            job_sender
                .send(Box::new(move || {
                    if moved.wait() == CleanupDisposition::Armed {
                        let mut operations = job_operations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        operations.push("run");
                        operations.push("release");
                    }
                    if let Some(completion) = completion {
                        let _ = completion.send(Ok(()));
                    }
                }))
                .unwrap();
        }));

        moved.resolve(CleanupDisposition::Armed);
        let future = lease.into_future();
        drop(future);
        job_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("future construction queues cleanup before polling")();

        assert_eq!(
            *operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["run", "release"]
        );
    }

    #[test]
    fn rejected_cleanup_drop_before_resolution_is_nonblocking_and_has_no_foreign_effect() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let (job_sender, job_receiver) = std::sync::mpsc::sync_channel::<TeardownJob>(1);
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let job_operations = Arc::clone(&operations);
        let (lease, moved) = CleanupLease::new_for_test(Box::new(move |moved, _completion| {
            job_sender
                .send(Box::new(move || {
                    entered_sender.send(()).unwrap();
                    if moved.wait() == CleanupDisposition::Armed {
                        let mut operations = job_operations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        operations.push("run");
                        operations.push("release");
                    }
                }))
                .unwrap();
        }));
        let (dropped_sender, dropped_receiver) = std::sync::mpsc::sync_channel(1);
        let dropper = std::thread::spawn(move || {
            drop(lease);
            dropped_sender.send(()).unwrap();
        });
        dropped_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cleanup closure drop must not wait for HOST_EFFECT_DEFER resolution");
        dropper.join().unwrap();

        let job = job_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("dropped cleanup queues a disposition-aware job");
        let worker = std::thread::spawn(job);
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("teardown job reaches the disposition wait");
        assert!(
            operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        moved.resolve(CleanupDisposition::Rejected);
        worker.join().unwrap();
        assert!(
            operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
}
