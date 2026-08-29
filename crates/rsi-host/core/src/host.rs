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
use rsi_meta_profile::{IsolationSpec, ProfileBootstrap, ProfileEnvironment, ProfileResolver};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::watch;

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
        runtime: Runtime,
        catalog: LinkedCatalog,
    ) -> Self {
        Self {
            paths,
            platform,
            defines,
            limits,
            runtime,
            catalog: Arc::new(catalog),
        }
    }

    /// Returns the frozen filesystem authority.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
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
