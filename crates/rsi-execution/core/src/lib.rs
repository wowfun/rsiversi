//! Location-bound provider generations and opaque prepared execution plans.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod duplex;
mod files;
mod ports;
mod retained;
mod target_program;
mod terminal;
pub use duplex::{ExecutionDuplex, ExecutionDuplexExchange};
pub use files::ExecutionFiles;
pub use target_program::TargetProgram;
pub use terminal::{ExecutionPty, ExecutionPtyView};

use async_trait::async_trait;
pub use rsi_api_protocol::HostEpoch;
pub use rsi_execution_protocol::{ExecutionBinding, ExecutionPlanIdentity, ExecutionReview};
pub use rsi_execution_protocol::{
    ExecutionCoordinates, ExecutionLocation, ExecutionLocations, ExecutionTargetId,
};
use rsi_files_protocol::Files;
use rsi_process::{
    DuplexProcessSpec, ManagedDuplexProcess, ManagedProcess, ManagedPtyProcess, ProcessError,
    ProcessSpec, PtyProcessSpec, Result,
};
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessRequest, WorkspaceReadRequest, WorkspaceReadScope,
};
use std::{
    any::Any,
    ffi::OsString,
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> Result<u64> {
    NEXT_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| ProcessError::Capacity)
}

/// Owned admission retained through an accepted operation's actual completion.
pub struct ExecutionOperation {
    _permit: Box<dyn Any + Send + Sync>,
}
impl ExecutionOperation {
    /// Retains a trusted grant owner's permit without exposing its implementation.
    pub fn new<T: Send + Sync + 'static>(permit: T) -> Self {
        Self {
            _permit: Box::new(permit),
        }
    }
}
impl fmt::Debug for ExecutionOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionOperation").finish_non_exhaustive()
    }
}

/// Lifetime and capacity class requested from the trusted admission owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionAdmissionKind {
    /// Whole Tool, model or other bounded caller scope, separate from backend I/O.
    Scope,
    /// A single backend effect, retained until actual settlement.
    Operation,
    /// Current authority only; never reserves effect capacity or starts backend I/O.
    Publication,
}

/// Product-supplied revocable admission; implementations never infer it from coordinates.
pub trait ExecutionAdmission: fmt::Debug + Send + Sync + 'static {
    /// Checks current authority and retains the requested admission class.
    /// Publication must not fail because Scope or Operation capacity is exhausted.
    fn admit(&self, kind: ExecutionAdmissionKind) -> Result<ExecutionOperation>;
}

/// Product-owned location selection at trusted authenticated ingress.
/// Returned leases retain live delegation; coordinates never recreate that authority.
pub trait ExecutionResolver: fmt::Debug + Send + Sync + 'static {
    /// Captures current metadata visibility and retains its gates through a finite enumeration.
    fn visibility(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
    ) -> rsi_api_protocol::Result<ExecutionVisibility>;
    /// Admits a bounded metadata/read operation without requiring a live target connection.
    fn admit(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionOperation>;
    /// Selects one complete provider tuple for this exact live caller.
    fn lease(
        &self,
        origin: rsi_api_protocol::CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionLease>;
}

/// A bounded metadata selector and the independent grant operation admitting its use.
#[derive(Debug)]
pub struct ExecutionVisibility {
    locations: ExecutionLocations,
    _operation: ExecutionOperation,
}
impl ExecutionVisibility {
    /// Constructed only by the trusted product admission owner after checking its grants.
    pub const fn new(locations: ExecutionLocations, operation: ExecutionOperation) -> Self {
        Self {
            locations,
            _operation: operation,
        }
    }
    /// Borrows mechanical query data; retaining the selection alone retains no permission.
    pub const fn locations(&self) -> &ExecutionLocations {
        &self.locations
    }
}
/// Host publication of the product's location and delegation policy.
#[derive(Debug)]
pub struct ExecutionResolverContract;
impl rsi_meta_contract::LocalContract for ExecutionResolverContract {
    const KEY: &'static str = "rsi.execution.resolver";
    type Service = dyn ExecutionResolver;
}

/// Target-resolved executable and complete child environment.
#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedProgram {
    /// Canonical executable in the selected target's namespace.
    pub program: PathBuf,
    /// Complete environment chosen by the target's program policy.
    pub environment: Vec<(OsString, OsString)>,
}

impl fmt::Debug for ResolvedProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedProgram")
            .field("program", &self.program)
            .field("environment_entries", &self.environment.len())
            .finish_non_exhaustive()
    }
}

/// Backend-owned move-only prepared payload. Callers cannot recover or substitute it.
pub struct BackendPlan {
    payload: Box<dyn Any + Send + Sync>,
    stamp: EnforcementStamp,
    environment: Vec<(OsString, OsString)>,
}
impl fmt::Debug for BackendPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackendPlan")
            .field("stamp", &self.stamp)
            .finish_non_exhaustive()
    }
}
impl BackendPlan {
    /// Creates a plan inside its trusted backend, after that backend's validation.
    pub fn new<T: Send + Sync + 'static>(
        payload: T,
        stamp: EnforcementStamp,
        environment: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            payload: Box::new(payload),
            stamp,
            environment,
        }
    }
    /// Consumes the exact implementation payload; the lease checks its seal first.
    pub fn take<T: Send + Sync + 'static>(self) -> Result<T> {
        self.payload
            .downcast::<T>()
            .map(|payload| *payload)
            .map_err(|_| ProcessError::InvalidInput("execution plan backend differs".into()))
    }
}

/// One immutable backend providing every operation in a location's namespace.
#[async_trait]
pub trait ExecutionBackend: fmt::Debug + Send + Sync + 'static {
    /// Resolves an explicit target path; never consults a different machine.
    async fn canonicalize(&self, path: &str) -> Result<String>;
    /// Resolves an exact configured program selector with its complete target environment.
    async fn resolve_program(&self, selector: &str) -> Result<ResolvedProgram>;
    /// Resolves a trusted contribution's explicit Local configuration through this backend.
    /// Non-native backends reject this operation; SSH never receives its arguments.
    async fn resolve_local_program(&self, _program: ResolvedProgram) -> Result<ResolvedProgram> {
        Err(ProcessError::Unsupported)
    }
    /// Resolves explicit target configuration; native backends reject this separate operation.
    async fn resolve_target_program(&self, _program: TargetProgram) -> Result<ResolvedProgram> {
        Err(ProcessError::Unsupported)
    }
    /// Prepares exact target enforcement before execution approval.
    async fn prepare(
        &self,
        request: ProcessRequest,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<BackendPlan>;
    /// Prepares `ReadOnly` pipes with read-only host scratch and isolated networking.
    async fn prepare_source_reader(
        &self,
        _request: ProcessRequest,
        _environment: Vec<(OsString, OsString)>,
    ) -> Result<BackendPlan> {
        Err(ProcessError::Unsupported)
    }
    /// Admits a prepared batch invocation and retains its pin through settlement.
    async fn spawn(
        &self,
        spec: ProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedProcess>;
    /// Admits a prepared duplex invocation and retains its pin through settlement.
    async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedDuplexProcess>;
    /// Admits a prepared terminal and retains its pin through settlement.
    async fn spawn_pty(
        &self,
        spec: PtyProcessSpec<BackendPlan>,
        pin: ExecutionPin,
    ) -> Result<ManagedPtyProcess>;
    /// Issues a read scope in the same target namespace and provider generation.
    async fn workspace_read(&self, request: WorkspaceReadRequest) -> Result<WorkspaceReadScope>;
    /// Returns the matching Files provider; the lease wraps it with per-operation admission.
    fn files(&self) -> Arc<dyn Files>;
}

#[derive(Debug)]
struct Provider {
    host_epoch: HostEpoch,
    location: ExecutionLocation,
    target_revision: u64,
    connection_epoch: u64,
    generation: u64,
    backend: Arc<dyn ExecutionBackend>,
}
/// A single frozen provider tuple, independent of any caller's grant.
#[derive(Clone, Debug)]
pub struct ExecutionProvider(Arc<Provider>);
impl ExecutionProvider {
    /// Freezes the complete provider tuple; SSH requires nonzero revision and epoch.
    pub fn new(
        host_epoch: HostEpoch,
        location: ExecutionLocation,
        target_revision: u64,
        connection_epoch: u64,
        backend: Arc<dyn ExecutionBackend>,
    ) -> Result<Self> {
        let valid = match location {
            ExecutionLocation::Local => target_revision == 0 && connection_epoch == 0,
            ExecutionLocation::Ssh { .. } => target_revision != 0 && connection_epoch != 0,
        };
        if !valid {
            return Err(ProcessError::InvalidInput(
                "execution binding revision or epoch is invalid".into(),
            ));
        }
        Ok(Self(Arc::new(Provider {
            host_epoch,
            location,
            target_revision,
            connection_epoch,
            generation: next_generation()?,
            backend,
        })))
    }
    /// Issues an exact lease only after successful admission from the supplied grant owner.
    pub fn lease(&self, admission: Arc<dyn ExecutionAdmission>) -> Result<ExecutionLease> {
        let _operation = admission.admit(ExecutionAdmissionKind::Publication)?;
        Ok(ExecutionLease(Arc::new(Lease {
            provider: self.0.clone(),
            admission,
            binding: ExecutionBinding::new(
                self.0.host_epoch.clone(),
                self.0.location.clone(),
                self.0.target_revision,
                self.0.connection_epoch,
                self.0.generation,
                next_generation()?,
            )
            .map_err(|_| ProcessError::InvalidInput("invalid execution binding".into()))?,
            next_plan: AtomicU64::new(1),
            files: ExecutionFiles::new(self.0.clone()),
        })))
    }
}

#[derive(Debug)]
struct Lease {
    provider: Arc<Provider>,
    admission: Arc<dyn ExecutionAdmission>,
    binding: ExecutionBinding,
    next_plan: AtomicU64,
    files: ExecutionFiles,
}
/// Cloneable exact provider tuple and admission owner; never serialized or resolved again.
#[derive(Clone, Debug)]
pub struct ExecutionLease(Arc<Lease>);

/// An admitted backend's lifetime pin, including the exact admission owner.
#[derive(Clone, Debug)]
pub struct ExecutionPin(ExecutionLease);
impl ExecutionPin {
    /// Admits a new input/write/resize operation against the original grant owner.
    pub fn admit(&self) -> Result<ExecutionOperation> {
        self.0.0.admission.admit(ExecutionAdmissionKind::Operation)
    }
    pub(crate) fn publication(&self) -> Result<ExecutionOperation> {
        self.0
            .0
            .admission
            .admit(ExecutionAdmissionKind::Publication)
    }
    /// Returns exact correlation metadata without granting authority.
    pub fn binding(&self) -> &ExecutionBinding {
        self.0.binding()
    }
    /// Retains the full tuple alongside the native Sandbox resources through child settlement.
    pub fn retain_native(self, mut process: ConfinedProcess) -> ConfinedProcess {
        process.owner = Some(rsi_sandbox::ProcessPlanOwner::new((
            process.owner.take(),
            self,
        )));
        process
    }
}

/// Target-selected program and complete environment, sealed to their issuing lease.
#[derive(Clone, Debug)]
pub struct ExecutionProgram {
    lease: Arc<Lease>,
    resolved: ResolvedProgram,
}
impl ExecutionProgram {
    /// Returns the exact target executable without consulting this machine's filesystem.
    pub fn path(&self) -> &std::path::Path {
        &self.resolved.program
    }
    /// Returns the selected complete child environment without granting execution permission.
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.resolved.environment
    }
}

/// Move-only prepared process, consumable only through the lease that created it.
#[derive(Debug)]
pub struct PreparedProcess {
    lease: Arc<Lease>,
    plan: BackendPlan,
    identity: ExecutionPlanIdentity,
}
impl PreparedProcess {
    /// Returns immutable identity for approval binding and diagnostics.
    pub const fn identity(&self) -> &ExecutionPlanIdentity {
        &self.identity
    }
    /// Returns the backend's actual selected enforcement.
    pub const fn enforcement(&self) -> &EnforcementStamp {
        &self.plan.stamp
    }
    /// Returns the complete environment fixed by the target policy during preparation.
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.plan.environment
    }
    /// Clones the exact tuple for deferred work; it never resolves a newer generation.
    pub fn lease(&self) -> ExecutionLease {
        ExecutionLease(self.lease.clone())
    }
}

impl PartialEq for ExecutionLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ExecutionLease {}
impl ExecutionLease {
    /// Admits one bounded operation through this exact lease's live delegation.
    pub fn admit(&self) -> Result<ExecutionOperation> {
        self.0.admission.admit(ExecutionAdmissionKind::Scope)
    }
    /// Returns this lease's immutable correlation metadata.
    pub fn binding(&self) -> &ExecutionBinding {
        &self.0.binding
    }
    /// Resolves coordinates only through the pinned target's filesystem owner.
    pub async fn canonicalize(&self, path: &str) -> Result<ExecutionCoordinates> {
        if path.is_empty() || path.len() > 16 * 1024 || path.contains('\0') {
            return Err(ProcessError::InvalidInput(
                "execution path exceeds its bound".into(),
            ));
        }
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let (backend, path) = (self.0.provider.backend.clone(), path.to_owned());
        let path = retained::operation(ExecutionPin(self.clone()), permit, async move {
            backend.canonicalize(&path).await
        })
        .await?;
        ExecutionCoordinates::new(self.binding().location().clone(), path).map_err(|_| {
            ProcessError::InvalidInput("execution provider returned invalid coordinates".into())
        })
    }
    /// Resolves a program and environment only through this target's policy.
    pub async fn resolve_program(&self, selector: &str) -> Result<ExecutionProgram> {
        if selector.is_empty() || selector.len() > 128 || selector.chars().any(char::is_control) {
            return Err(ProcessError::InvalidInput(
                "execution program selector exceeds its bound".into(),
            ));
        }
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let (backend, selector) = (self.0.provider.backend.clone(), selector.to_owned());
        retained::operation(ExecutionPin(self.clone()), permit, async move {
            backend.resolve_program(&selector).await
        })
        .await
        .map(|resolved| ExecutionProgram {
            lease: self.0.clone(),
            resolved,
        })
    }
    /// Validates and seals explicit Local contribution configuration without changing providers.
    pub async fn resolve_local_program(
        &self,
        program: ResolvedProgram,
    ) -> Result<ExecutionProgram> {
        if *self.binding().location() != ExecutionLocation::Local {
            return Err(ProcessError::InvalidInput(
                "Local program configuration cannot enter a remote lease".into(),
            ));
        }
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let backend = self.0.provider.backend.clone();
        retained::operation(ExecutionPin(self.clone()), permit, async move {
            backend.resolve_local_program(program).await
        })
        .await
        .map(|resolved| ExecutionProgram {
            lease: self.0.clone(),
            resolved,
        })
    }
    /// Resolves trusted explicit remote configuration through this exact target.
    pub async fn resolve_target_program(&self, program: TargetProgram) -> Result<ExecutionProgram> {
        if *self.binding().location() == ExecutionLocation::Local {
            return Err(ProcessError::InvalidInput(
                "target program requires a remote lease".into(),
            ));
        }
        program.validate()?;
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let backend = self.0.provider.backend.clone();
        retained::operation(ExecutionPin(self.clone()), permit, async move {
            backend.resolve_target_program(program).await
        })
        .await
        .map(|resolved| ExecutionProgram {
            lease: self.0.clone(),
            resolved,
        })
    }
    /// Creates an opaque plan and its approval identity while retaining this exact lease.
    pub async fn prepare(
        &self,
        request: ProcessRequest<ExecutionProgram>,
    ) -> Result<PreparedProcess> {
        self.prepare_view(request, false).await
    }
    /// Prepares an explicit source-reader view through this exact provider and lease.
    pub async fn prepare_source_reader(
        &self,
        request: ProcessRequest<ExecutionProgram>,
    ) -> Result<PreparedProcess> {
        if request.mode != rsi_sandbox::SandboxMode::ReadOnly
            || request.stdio != rsi_sandbox::ProcessStdio::Pipes
        {
            return Err(ProcessError::InvalidInput(
                "source reader requires ReadOnly pipes".into(),
            ));
        }
        self.prepare_view(request, true).await
    }
    async fn prepare_view(
        &self,
        request: ProcessRequest<ExecutionProgram>,
        source_reader: bool,
    ) -> Result<PreparedProcess> {
        if !Arc::ptr_eq(&self.0, &request.program.lease) {
            return Err(ProcessError::InvalidInput(
                "resolved program belongs to a different lease".into(),
            ));
        }
        let mut environment = Vec::new();
        let request = request.map_program(|program| {
            environment = program.resolved.environment;
            program.resolved.program
        });
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let sequence = self
            .0
            .next_plan
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| ProcessError::Capacity)?;
        let backend = self.0.provider.backend.clone();
        let plan = retained::operation(ExecutionPin(self.clone()), permit, async move {
            if source_reader {
                backend.prepare_source_reader(request, environment).await
            } else {
                backend.prepare(request, environment).await
            }
        })
        .await?;
        if source_reader
            && (plan.stamp.scratch != rsi_sandbox::SandboxScratch::Host
                || plan.stamp.network != rsi_sandbox::SandboxNetwork::Isolated
                || plan.stamp.filesystem != rsi_sandbox::SandboxFileSystem::ReadOnly
                || plan.stamp.requested != rsi_sandbox::SandboxMode::ReadOnly)
        {
            return Err(ProcessError::Unsupported);
        }
        Ok(PreparedProcess {
            lease: self.0.clone(),
            plan,
            identity: ExecutionPlanIdentity::new(self.0.binding.clone(), sequence)
                .map_err(|_| ProcessError::Capacity)?,
        })
    }
    fn consume(&self, plan: PreparedProcess) -> Result<BackendPlan> {
        if !Arc::ptr_eq(&self.0, &plan.lease) {
            return Err(ProcessError::InvalidInput(
                "execution plan belongs to a different lease".into(),
            ));
        }
        Ok(plan.plan)
    }
    fn check_environment(
        plan: &PreparedProcess,
        environment: &[(OsString, OsString)],
    ) -> Result<()> {
        if plan.environment() != environment {
            return Err(ProcessError::InvalidInput(
                "prepared execution environment differs".into(),
            ));
        }
        Ok(())
    }
    /// Consumes the exact prepared batch plan after checking its seal, before backend I/O.
    pub async fn spawn(&self, spec: ProcessSpec<PreparedProcess>) -> Result<ManagedProcess> {
        Self::check_environment(&spec.process, &spec.environment)?;
        let spec = spec.try_map_process(|plan| self.consume(plan))?;
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let pin = ExecutionPin(self.clone());
        let backend = self.0.provider.backend.clone();
        let backend_pin = pin.clone();
        retained::start(
            permit,
            async move { backend.spawn(spec, backend_pin).await },
        )
        .await
        .map(|process| ports::batch(process, pin))
    }
    /// Consumes the exact prepared duplex plan; foreign leases perform no backend I/O.
    pub async fn spawn_duplex(
        &self,
        spec: DuplexProcessSpec<PreparedProcess>,
    ) -> Result<ManagedDuplexProcess> {
        self.spawn_duplex_backend(spec)
            .await
            .map(|process| ports::duplex(process, ExecutionPin(self.clone())))
    }
    /// Spawns a retained protocol server; each finite exchange requires current caller admission.
    pub async fn spawn_duplex_server(
        &self,
        spec: DuplexProcessSpec<PreparedProcess>,
    ) -> Result<ExecutionDuplex> {
        self.spawn_duplex_backend(spec)
            .await
            .map(|inner| ExecutionDuplex {
                inner,
                pin: ExecutionPin(self.clone()),
            })
    }
    async fn spawn_duplex_backend(
        &self,
        spec: DuplexProcessSpec<PreparedProcess>,
    ) -> Result<ManagedDuplexProcess> {
        Self::check_environment(&spec.process, &spec.environment)?;
        let spec = spec.try_map_process(|plan| self.consume(plan))?;
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let pin = ExecutionPin(self.clone());
        let backend = self.0.provider.backend.clone();
        let backend_pin = pin.clone();
        retained::start(permit, async move {
            backend.spawn_duplex(spec, backend_pin).await
        })
        .await
    }
    /// Spawns a terminal view whose subsequent I/O remains bound to this caller's lease.
    pub async fn spawn_pty(
        &self,
        spec: PtyProcessSpec<PreparedProcess>,
    ) -> Result<ManagedPtyProcess> {
        self.spawn_pty_backend(spec)
            .await
            .map(|process| ports::pty(process, ExecutionPin(self.clone())))
    }
    /// Spawns a retained terminal resource; each input owner must obtain its own exact-provider view.
    pub async fn spawn_terminal(
        &self,
        spec: PtyProcessSpec<PreparedProcess>,
    ) -> Result<ExecutionPty> {
        self.spawn_pty_backend(spec)
            .await
            .map(|inner| ExecutionPty {
                inner,
                pin: ExecutionPin(self.clone()),
            })
    }
    /// Consumes the exact prepared PTY plan; foreign leases perform no backend I/O.
    async fn spawn_pty_backend(
        &self,
        spec: PtyProcessSpec<PreparedProcess>,
    ) -> Result<ManagedPtyProcess> {
        Self::check_environment(&spec.process, &spec.environment)?;
        let spec = spec.try_map_process(|plan| self.consume(plan))?;
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let pin = ExecutionPin(self.clone());
        let backend = self.0.provider.backend.clone();
        let backend_pin = pin.clone();
        retained::start(
            permit,
            async move { backend.spawn_pty(spec, backend_pin).await },
        )
        .await
    }
    /// Obtains read scope from the pinned Sandbox rather than a location lookup.
    pub async fn workspace_read(
        &self,
        request: WorkspaceReadRequest,
    ) -> Result<WorkspaceReadScope> {
        let permit = self.0.admission.admit(ExecutionAdmissionKind::Operation)?;
        let backend = self.0.provider.backend.clone();
        retained::operation(ExecutionPin(self.clone()), permit, async move {
            backend.workspace_read(request).await
        })
        .await
    }
    /// Returns a Files view that pins the same tuple and consults this admission owner.
    pub fn files(&self) -> Result<Arc<dyn Files>> {
        self.0.files.view(self)
    }
    /// Retains a distinct token scope; future accesses require a current provider-matching view.
    pub fn retain_files(&self) -> Result<ExecutionFiles> {
        let _operation = ExecutionPin(self.clone()).publication()?;
        Ok(ExecutionFiles::new(self.0.provider.clone()))
    }
}
