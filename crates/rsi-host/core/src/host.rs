use crate::builder::{HostLimits, LinkedRegistration};
use crate::{
    HostError, HostPaths, Profile, ProfileControl, ProfileControlContract, ProfileFragment,
    ProfilePatch, ProfileProgram, ProfileSnapshot, ProfileStatus, ReloadOutcome, Result,
};
use rsi_meta::{
    ConfigValue, Context, FiberHandle, FiberState, LocalContract, LocalContractKey, LocalEvent,
    LocalEventKey, PluginId, ResolvedFactory, Runtime, RuntimeSnapshot, ShutdownOutcome,
    UpdateMode,
};
use rsi_meta_profile::{
    IsolationSpec, ProfileBootstrap, ProfileCompiler, ProfileEnvironment, ProfileResolver,
};
use sha2::{Digest as _, Sha256};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::watch;

/// Pure source/compiler/resolver preview of one frozen Host Profile candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProfilePreview {
    /// Digest of the complete source program and frozen compile environment.
    pub source_digest: String,
    /// Canonical root and transitive include paths in deterministic order.
    pub source_paths: Vec<std::path::PathBuf>,
    /// Enabled resolved leaves in executable order.
    pub leaves: Vec<HostProfilePreviewLeaf>,
}

/// One enabled leaf proven resolvable without preparation or activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProfilePreviewLeaf {
    /// Stable all-tree instance identity.
    pub instance_id: String,
    /// Exact linked plugin identity.
    pub plugin_id: String,
}

pub(crate) const PROFILE_PLUGIN_ID: &str = "rsi.meta.profile";

#[derive(Debug)]
pub(crate) struct LinkedCatalog {
    pub(crate) linked: BTreeMap<PluginId, LinkedRegistration>,
    pub(crate) fragments: Vec<ProfileFragment>,
    pub(crate) local_contracts: BTreeMap<LocalContractKey, TypeId>,
    pub(crate) local_events: BTreeMap<LocalEventKey, TypeId>,
    pub(crate) launch_patches: Vec<ProfilePatch>,
}

impl ProfileResolver for LinkedCatalog {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        let registration = self.linked.get(plugin).ok_or_else(|| {
            rsi_meta_profile::ProfileError::UnknownPlugin {
                plugin: plugin.clone(),
            }
        })?;
        Ok(ResolvedFactory::linked(
            plugin.clone(),
            registration.revision.clone(),
            registration.update_mode,
            Arc::clone(&registration.implementation),
        ))
    }

    fn isolate(
        &self,
        mut context: Context,
        isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        for key in isolation.local() {
            let stable = LocalContractKey::new(key.clone());
            let contract = self.local_contracts.get(&stable).copied().ok_or_else(|| {
                rsi_meta_profile::ProfileError::UnknownLocalContract { key: key.clone() }
            })?;
            context = context.isolate_local_type_fresh(contract, key)?.0;
        }
        for key in isolation.events() {
            let stable = LocalEventKey::new(key.clone());
            let event = self.local_events.get(&stable).copied().ok_or_else(|| {
                rsi_meta_profile::ProfileError::UnknownLocalEvent { key: key.clone() }
            })?;
            context = context.isolate_event_type_fresh(event, key)?.0;
        }
        for key in isolation.portable() {
            context = context.isolate_fresh(key)?.0;
        }
        Ok(context)
    }
}

/// Frozen generic Host before its single top-level Profile starts.
pub struct Host {
    paths: HostPaths,
    platform: String,
    defines: BTreeMap<String, ConfigValue>,
    limits: HostLimits,
    runtime_limits: rsi_meta::RuntimeLimits,
    runtime: Runtime,
    catalog: Arc<LinkedCatalog>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Host")
            .field("paths", &self.paths)
            .field("platform", &self.platform)
            .field("defines", &self.defines.keys())
            .field("plugins", &self.catalog.linked.keys())
            .finish_non_exhaustive()
    }
}

impl Host {
    pub(crate) fn new(
        paths: HostPaths,
        platform: String,
        defines: BTreeMap<String, ConfigValue>,
        limits: HostLimits,
        runtime_limits: rsi_meta::RuntimeLimits,
        runtime: Runtime,
        catalog: LinkedCatalog,
    ) -> Self {
        Self {
            paths,
            platform,
            defines,
            limits,
            runtime_limits,
            runtime,
            catalog: Arc::new(catalog),
        }
    }

    /// Returns the frozen filesystem authority.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Returns a canonical digest of the generic Host inputs frozen by its builder.
    ///
    /// The digest includes paths, platform, defines, compiler and Runtime limits,
    /// linked factory revisions and update modes, registered marker keys, linked
    /// fragments, and launch patches. It deliberately has no top-level Profile
    /// source, current Profile digest, application argument, session identity, or
    /// credential-store value.
    pub fn composition_digest(&self) -> Result<String> {
        let preview = self.preview(Profile::default())?;
        let mut digest = Sha256::new();
        hash_component(&mut digest, b"domain", b"rsi.host.composition.v1");
        hash_component(
            &mut digest,
            b"compiled-empty-program",
            preview.source_digest.as_bytes(),
        );
        hash_host_limits(&mut digest, &self.limits);
        hash_runtime_limits(&mut digest, &self.runtime_limits);
        for (plugin, registration) in &self.catalog.linked {
            hash_component(&mut digest, b"plugin", plugin.as_str().as_bytes());
            hash_component(&mut digest, b"revision", registration.revision.as_bytes());
            hash_component(
                &mut digest,
                b"update-mode",
                match registration.update_mode {
                    UpdateMode::Replayable => b"replayable",
                    UpdateMode::RestartRequired => b"restart-required",
                },
            );
        }
        for key in self.catalog.local_contracts.keys() {
            hash_component(&mut digest, b"local-contract", key.as_str().as_bytes());
        }
        for key in self.catalog.local_events.keys() {
            hash_component(&mut digest, b"local-event", key.as_str().as_bytes());
        }
        Ok(hex::encode(digest.finalize()))
    }

    /// Reports whether this exact Local contract marker is in the frozen catalog.
    pub fn has_local_contract<C: LocalContract>(&self) -> bool {
        TypeId::of::<C>() == TypeId::of::<ProfileControlContract>()
            || self
                .catalog
                .local_contracts
                .get(&LocalContractKey::new(C::KEY))
                .is_some_and(|contract| *contract == TypeId::of::<C>())
    }

    /// Reports whether this exact Local event marker is in the frozen catalog.
    pub fn has_local_event<E: LocalEvent>(&self) -> bool {
        self.catalog
            .local_events
            .get(&LocalEventKey::new(E::KEY))
            .is_some_and(|event| *event == TypeId::of::<E>())
    }

    /// Purely compiles and resolves one in-memory Profile without Runtime mutation.
    pub fn preview(&self, profile: Profile) -> Result<HostProfilePreview> {
        self.preview_program(ProfileProgram::from_profile(profile))
    }

    /// Purely compiles and resolves one Profile file without Runtime mutation.
    pub fn preview_file(&self, path: impl Into<std::path::PathBuf>) -> Result<HostProfilePreview> {
        self.preview_program(ProfileProgram::from_file(path))
    }

    fn preview_program(&self, program: ProfileProgram) -> Result<HostProfilePreview> {
        let environment = ProfileEnvironment::new(
            self.paths.config().to_path_buf(),
            self.paths.state().to_path_buf(),
            self.paths.cache().to_path_buf(),
            self.platform.clone(),
            self.defines.clone(),
        )?;
        let program = program
            .with_linked_fragments(self.catalog.fragments.clone())
            .with_launch_patches(self.catalog.launch_patches.clone());
        let candidate =
            ProfileCompiler::new(environment, self.limits.profile.clone()).compile(&program)?;
        let mut leaves = Vec::with_capacity(candidate.leaves().len());
        for leaf in candidate.leaves() {
            let _resolved = self.catalog.resolve(leaf.plugin())?;
            leaves.push(HostProfilePreviewLeaf {
                instance_id: leaf.id().as_str().to_owned(),
                plugin_id: leaf.plugin().as_str().to_owned(),
            });
        }
        Ok(HostProfilePreview {
            source_digest: candidate.source_digest().to_owned(),
            source_paths: candidate.watch_paths().to_vec(),
            leaves,
        })
    }

    /// Starts one immutable in-memory root after the linked prefix.
    pub async fn start(self, profile: Profile) -> Result<RunningHost> {
        self.start_program(ProfileProgram::from_profile(profile))
            .await
    }

    /// Starts one required root file with transitive source watching.
    pub async fn start_file(self, path: impl Into<std::path::PathBuf>) -> Result<RunningHost> {
        self.start_program(ProfileProgram::from_file(path)).await
    }

    /// Starts the one direct Profile bootstrap from an explicit source program.
    pub async fn start_program(self, program: ProfileProgram) -> Result<RunningHost> {
        let environment = ProfileEnvironment::new(
            self.paths.config().to_path_buf(),
            self.paths.state().to_path_buf(),
            self.paths.cache().to_path_buf(),
            self.platform.clone(),
            self.defines.clone(),
        )?;
        let program = program
            .with_linked_fragments(self.catalog.fragments.clone())
            .with_launch_patches(self.catalog.launch_patches.clone());
        let runtime = self.runtime.clone();
        let resolver = Arc::clone(&self.catalog) as Arc<dyn ProfileResolver>;
        let limits = self.limits.profile.clone();
        let bootstrap = tokio::task::spawn_blocking(move || {
            ProfileBootstrap::prepare(&runtime, resolver, program, environment, limits)
        })
        .await
        .map_err(|_| HostError::Bootstrap("Profile preparation task failed".to_owned()))??;
        let control = bootstrap.control();
        let applied = self
            .runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    PROFILE_PLUGIN_ID,
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    bootstrap.factory(),
                ),
                ConfigValue::Null,
            )
            .await;
        let profile_fiber = match applied {
            Ok(handle) if matches!(handle.snapshot().state, FiberState::Active) => handle,
            Ok(handle) => {
                let state = handle.snapshot().state;
                let _ = self.runtime.shutdown().await;
                return Err(HostError::Bootstrap(format!(
                    "Profile Fiber settled as {state:?}"
                )));
            }
            Err(error) => {
                let _ = self.runtime.shutdown().await;
                return Err(error.into());
            }
        };
        Ok(RunningHost {
            paths: self.paths,
            runtime: self.runtime,
            catalog: self.catalog,
            profile_fiber,
            control,
        })
    }
}

fn hash_component(digest: &mut Sha256, name: &[u8], value: &[u8]) {
    digest.update(
        u64::try_from(name.len())
            .expect("field name fits u64")
            .to_be_bytes(),
    );
    digest.update(name);
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

fn hash_usize(digest: &mut Sha256, name: &[u8], value: usize) {
    hash_component(
        digest,
        name,
        &u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes(),
    );
}

fn hash_duration(digest: &mut Sha256, name: &[u8], value: std::time::Duration) {
    let mut bytes = [0_u8; 12];
    bytes[..8].copy_from_slice(&value.as_secs().to_be_bytes());
    bytes[8..].copy_from_slice(&value.subsec_nanos().to_be_bytes());
    hash_component(digest, name, &bytes);
}

fn hash_host_limits(digest: &mut Sha256, limits: &HostLimits) {
    let HostLimits {
        profile,
        maximum_linked_plugins,
        maximum_fragments,
        maximum_local_contracts,
        maximum_local_events,
        maximum_identifier_bytes,
    } = limits;
    let rsi_meta_profile::ProfileLimits {
        maximum_document_bytes,
        maximum_source_bytes,
        maximum_source_files,
        maximum_include_depth,
        maximum_steps,
        maximum_nodes,
        maximum_group_depth,
        maximum_identifier_bytes: maximum_profile_identifier_bytes,
        maximum_expression_operations,
        maximum_expression_depth,
        maximum_config_bytes,
        maximum_diagnostic_bytes,
    } = profile;
    for (name, value) in [
        (
            b"profile.maximum-document-bytes".as_slice(),
            *maximum_document_bytes,
        ),
        (b"profile.maximum-source-bytes", *maximum_source_bytes),
        (b"profile.maximum-source-files", *maximum_source_files),
        (b"profile.maximum-include-depth", *maximum_include_depth),
        (b"profile.maximum-steps", *maximum_steps),
        (b"profile.maximum-nodes", *maximum_nodes),
        (b"profile.maximum-group-depth", *maximum_group_depth),
        (
            b"profile.maximum-identifier-bytes",
            *maximum_profile_identifier_bytes,
        ),
        (
            b"profile.maximum-expression-depth",
            *maximum_expression_depth,
        ),
        (b"profile.maximum-config-bytes", *maximum_config_bytes),
        (
            b"profile.maximum-diagnostic-bytes",
            *maximum_diagnostic_bytes,
        ),
        (b"host.maximum-linked-plugins", *maximum_linked_plugins),
        (b"host.maximum-fragments", *maximum_fragments),
        (b"host.maximum-local-contracts", *maximum_local_contracts),
        (b"host.maximum-local-events", *maximum_local_events),
        (b"host.maximum-identifier-bytes", *maximum_identifier_bytes),
    ] {
        hash_usize(digest, name, value);
    }
    hash_component(
        digest,
        b"profile.maximum-expression-operations",
        &maximum_expression_operations.to_be_bytes(),
    );
}

fn hash_runtime_limits(digest: &mut Sha256, limits: &rsi_meta::RuntimeLimits) {
    let rsi_meta::RuntimeLimits {
        topology,
        payloads,
        execution,
        deadlines,
    } = limits;
    let rsi_meta::DeadlineLimits {
        transition,
        service_call,
        shutdown_wait,
    } = deadlines;
    hash_runtime_topology_limits(digest, topology);
    hash_runtime_payload_limits(digest, payloads);
    hash_runtime_execution_limits(digest, execution);
    hash_duration(digest, b"runtime.transition-deadline", *transition);
    hash_duration(digest, b"runtime.service-call-deadline", *service_call);
    hash_duration(digest, b"runtime.shutdown-wait-deadline", *shutdown_wait);
}

fn hash_runtime_topology_limits(digest: &mut Sha256, topology: &rsi_meta::TopologyLimits) {
    let &rsi_meta::TopologyLimits {
        maximum_fibers,
        maximum_fiber_depth,
        maximum_services,
        maximum_dependency_edges,
        maximum_requirements_per_fiber,
        maximum_event_listeners,
        maximum_waterfall_listeners_per_slot,
        maximum_effects_per_fiber,
        maximum_effects,
        maximum_effect_transactions_per_fiber,
        maximum_effect_transactions,
        maximum_context_entries,
        maximum_capability_entries,
        maximum_capabilities_per_message,
        maximum_queued_capability_references,
    } = topology;
    for (name, value) in [
        (b"runtime.maximum-fibers".as_slice(), maximum_fibers),
        (b"runtime.maximum-fiber-depth", maximum_fiber_depth),
        (b"runtime.maximum-services", maximum_services),
        (
            b"runtime.maximum-dependency-edges",
            maximum_dependency_edges,
        ),
        (
            b"runtime.maximum-requirements-per-fiber",
            maximum_requirements_per_fiber,
        ),
        (b"runtime.maximum-event-listeners", maximum_event_listeners),
        (
            b"runtime.maximum-waterfall-listeners-per-slot",
            maximum_waterfall_listeners_per_slot,
        ),
        (
            b"runtime.maximum-effects-per-fiber",
            maximum_effects_per_fiber,
        ),
        (b"runtime.maximum-effects", maximum_effects),
        (
            b"runtime.maximum-effect-transactions-per-fiber",
            maximum_effect_transactions_per_fiber,
        ),
        (
            b"runtime.maximum-effect-transactions",
            maximum_effect_transactions,
        ),
        (b"runtime.maximum-context-entries", maximum_context_entries),
        (
            b"runtime.maximum-capability-entries",
            maximum_capability_entries,
        ),
        (
            b"runtime.maximum-capabilities-per-message",
            maximum_capabilities_per_message,
        ),
        (
            b"runtime.maximum-queued-capability-references",
            maximum_queued_capability_references,
        ),
    ] {
        hash_usize(digest, name, value);
    }
}

fn hash_runtime_payload_limits(digest: &mut Sha256, payloads: &rsi_meta::PayloadLimits) {
    let &rsi_meta::PayloadLimits {
        maximum_identifier_bytes,
        maximum_prepared_state_bytes,
        maximum_message_bytes,
        maximum_config_bytes,
        maximum_retained_plugin_bytes,
        maximum_context_bytes,
        maximum_buffered_message_bytes,
        maximum_json_depth,
        maximum_json_nodes,
        maximum_diagnostic_entries,
        maximum_diagnostic_bytes,
    } = payloads;
    for (name, value) in [
        (
            b"runtime.maximum-identifier-bytes".as_slice(),
            maximum_identifier_bytes,
        ),
        (
            b"runtime.maximum-prepared-state-bytes",
            maximum_prepared_state_bytes,
        ),
        (b"runtime.maximum-message-bytes", maximum_message_bytes),
        (b"runtime.maximum-config-bytes", maximum_config_bytes),
        (
            b"runtime.maximum-retained-plugin-bytes",
            maximum_retained_plugin_bytes,
        ),
        (b"runtime.maximum-context-bytes", maximum_context_bytes),
        (
            b"runtime.maximum-buffered-message-bytes",
            maximum_buffered_message_bytes,
        ),
        (b"runtime.maximum-json-depth", maximum_json_depth),
        (b"runtime.maximum-json-nodes", maximum_json_nodes),
        (
            b"runtime.maximum-diagnostic-entries",
            maximum_diagnostic_entries,
        ),
        (
            b"runtime.maximum-diagnostic-bytes",
            maximum_diagnostic_bytes,
        ),
    ] {
        hash_usize(digest, name, value);
    }
}

fn hash_runtime_execution_limits(digest: &mut Sha256, execution: &rsi_meta::ExecutionLimits) {
    let &rsi_meta::ExecutionLimits {
        maximum_concurrent_preparations,
        maximum_concurrent_reconciliations,
        maximum_concurrent_service_calls,
        channel_capacity,
        maximum_pending_message_sends,
    } = execution;
    for (name, value) in [
        (
            b"runtime.maximum-concurrent-preparations".as_slice(),
            maximum_concurrent_preparations,
        ),
        (
            b"runtime.maximum-concurrent-reconciliations",
            maximum_concurrent_reconciliations,
        ),
        (
            b"runtime.maximum-concurrent-service-calls",
            maximum_concurrent_service_calls,
        ),
        (b"runtime.channel-capacity", channel_capacity),
        (
            b"runtime.maximum-pending-message-sends",
            maximum_pending_message_sends,
        ),
    ] {
        hash_usize(digest, name, value);
    }
}

/// Running single-Profile Host with typed observation and deterministic shutdown.
pub struct RunningHost {
    paths: HostPaths,
    runtime: Runtime,
    catalog: Arc<LinkedCatalog>,
    profile_fiber: FiberHandle,
    control: Arc<dyn ProfileControl>,
}

impl std::fmt::Debug for RunningHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunningHost")
            .field("paths", &self.paths)
            .field("profile", &self.profile_fiber.snapshot())
            .finish_non_exhaustive()
    }
}

impl RunningHost {
    /// Returns the frozen filesystem authority.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Returns bounded status from typed Profile control.
    pub fn profile_status(&self) -> ProfileStatus {
        self.control.status()
    }

    /// Returns the redacted desired Profile tree.
    pub fn profile_snapshot(&self) -> ProfileSnapshot {
        self.control.snapshot()
    }

    /// Subscribes to last-value Profile status changes.
    pub fn subscribe_profile(&self) -> watch::Receiver<ProfileStatus> {
        self.control.subscribe()
    }

    /// Rebuilds and converges from the complete immutable source program.
    pub async fn reload(&self) -> Result<ReloadOutcome> {
        self.control.reload().await.map_err(Into::into)
    }

    /// Reports whether this exact Local contract marker is in the frozen catalog.
    pub fn has_local_contract<C: LocalContract>(&self) -> bool {
        TypeId::of::<C>() == TypeId::of::<ProfileControlContract>()
            || self
                .catalog
                .local_contracts
                .get(&LocalContractKey::new(C::KEY))
                .is_some_and(|contract| *contract == TypeId::of::<C>())
    }

    /// Reports whether this exact Local event marker is in the frozen catalog.
    pub fn has_local_event<E: LocalEvent>(&self) -> bool {
        self.catalog
            .local_events
            .get(&LocalEventKey::new(E::KEY))
            .is_some_and(|event| *event == TypeId::of::<E>())
    }

    /// Looks up one active explicitly registered Local contract.
    pub fn lookup_local<C: LocalContract>(&self) -> Option<Arc<C::Service>> {
        self.has_local_contract::<C>()
            .then(|| self.runtime.root().lookup_local::<C>())
            .flatten()
    }

    /// Observes Meta without exposing mutable Runtime authority.
    pub fn runtime_snapshot(&self) -> RuntimeSnapshot {
        self.runtime.snapshot()
    }

    /// Starts or joins deterministic Runtime teardown.
    pub async fn shutdown(&self) -> ShutdownOutcome {
        self.runtime.shutdown().await
    }
}
