//! Explicit native staging; source publication is independent of Runtime apply.
use crate::{
    AddonScope, NativeAddonRecord, NativeAddonStore, StandardAddonBuilder, StandardAddonSet,
};
use rsi_agent_composition::{AgentCompositionSnapshot, AgentCompositionSource};
use rsi_agent_presets::AgentPresetCatalog;
use rsi_host::HostPaths;
use rsi_meta::FactoryIdentity;
use rsi_meta_native_loader::NativeCatalog;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

/// Categorical selection state without source paths or native diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAddonHealth {
    /// Desired and staged selections match and new snapshots are admitted.
    Ready,
    /// Desired selection changed and needs an explicit refresh.
    Pending,
    /// The latest refresh failed; no stale snapshot is admitted.
    Failed,
    /// Desired source metadata cannot currently be validated.
    InvalidSource,
    /// Native finalization retained resources and permanently closed admission.
    Retained,
    /// The staging owner closed future selection.
    Closed,
}

/// A staging failure; it never claims Runtime rollback or source-file rollback.
#[derive(Debug, thiserror::Error)]
pub enum NativeAddonUpdateError {
    /// Desired source or content-addressed storage could not be read.
    #[error(transparent)]
    Store(#[from] crate::NativeAddonError),
    /// Native loading rejected the selected bytes.
    #[error("native addon loading failed: {0}")]
    Load(#[from] rsi_meta_native_loader::LoaderError),
    /// The complete proposed Agent selection is invalid.
    #[error("invalid native Agent selection: {0}")]
    Selection(&'static str),
    /// Another caller is staging this manager's selection.
    #[error("native addon refresh is already running")]
    Busy,
    /// Future selection is closed.
    #[error("native addon manager is closed")]
    Closed,
    /// Native resources were retained after failure; process recovery is required.
    #[error("native finalization retained resources; process recovery is required")]
    Retained,
}
type Result<T> = std::result::Result<T, NativeAddonUpdateError>;

/// Staging receipt, independent of Agent generation construction or Runtime apply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeAddonRefresh {
    /// Validated source revision at completion.
    pub source_revision: u64,
    /// Whether an executable catalog snapshot was replaced.
    pub changed: bool,
    /// Number of selected native factories.
    pub selected: usize,
}

/// Bounded, path-free observations from the source, staging owner and sole Loader.
/// These observations are not a transaction across filesystem and Loader activity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeAddonInspection {
    /// New-selection admission and refresh state.
    pub health: NativeAddonHealth,
    /// Current source revision, absent if metadata is invalid.
    pub source_revision: Option<u64>,
    /// Revision used for the latest successfully staged selection.
    pub staged_revision: Option<u64>,
    /// Validated desired records; empty when source metadata is unavailable.
    pub desired: Vec<NativeAddonRecord>,
    /// Last successfully staged native records; existing pins may also retain older ones.
    pub staged: Vec<NativeAddonRecord>,
    /// Loader's retained failed-finalization count.
    pub retained_failed_finalizations: usize,
    /// Actual live private staging bytes in the Loader, including retained failures.
    pub staging_bytes: u64,
    /// Native callback bodies still admitted by the Loader.
    pub active_callbacks: usize,
    /// Live native instances, including their actual destruction lifetime.
    pub active_instances: usize,
    /// Host capabilities still held across the ABI boundary.
    pub host_capabilities: usize,
    /// Host output tokens still held across the ABI boundary.
    pub host_outputs: usize,
}

struct Staged {
    revision: u64,
    selected: Vec<NativeAddonRecord>,
    snapshot: Arc<AgentCompositionSnapshot>,
}
struct State {
    current: Option<Staged>,
    failed: bool,
}

/// One explicit Agent staging owner. Blocking refresh must be supervised by its caller.
/// It never constructs another Loader, runs builds, or starts background work.
pub struct NativeAddonManager {
    store: Arc<NativeAddonStore>,
    loader: NativeCatalog,
    paths: HostPaths,
    linux_tools: bool,
    presets: AgentPresetCatalog,
    base: StandardAddonSet,
    refresh: Mutex<()>,
    state: Mutex<State>,
    closed: AtomicBool,
    retained: AtomicBool,
}
impl std::fmt::Debug for NativeAddonManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeAddonManager")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .field("retained", &self.retained.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
impl NativeAddonManager {
    pub(crate) fn new(
        store: Arc<NativeAddonStore>,
        loader: NativeCatalog,
        paths: HostPaths,
        linux_tools: bool,
        presets: AgentPresetCatalog,
        base: StandardAddonSet,
    ) -> rsi_host::Result<Self> {
        let snapshot = AgentCompositionSnapshot::new(presets.clone(), base.agent_catalog()?);
        Ok(Self {
            store,
            loader,
            paths,
            linux_tools,
            presets,
            base,
            refresh: Mutex::new(()),
            state: Mutex::new(State {
                current: Some(Staged {
                    revision: 0,
                    selected: Vec::new(),
                    snapshot: Arc::new(snapshot),
                }),
                failed: false,
            }),
            closed: AtomicBool::new(false),
            retained: AtomicBool::new(false),
        })
    }

    /// Stages the complete current selection synchronously. This may execute trusted
    /// native code on the supplied Loader's bounded callback lanes, but no build command.
    pub fn refresh(&self) -> Result<NativeAddonRefresh> {
        let _refresh = match self.refresh.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => return Err(NativeAddonUpdateError::Busy),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
        self.check_admission()?;
        let result = self.stage();
        if result.is_err() {
            self.state().failed = true;
        }
        result
    }

    fn stage(&self) -> Result<NativeAddonRefresh> {
        let before = self.store.snapshot()?;
        {
            let mut state = self.state();
            if !state.failed
                && let Some(current) = &mut state.current
                && current.selected == before.enabled
            {
                current.revision = before.revision;
                return Ok(NativeAddonRefresh {
                    source_revision: before.revision,
                    changed: false,
                    selected: before.enabled.len(),
                });
            }
        }
        self.preflight(&before.enabled)?;
        let mut builder = StandardAddonBuilder::new("rsi.native.local");
        for record in &before.enabled {
            self.check_admission()?;
            let factory = self
                .loader
                .load_exact(self.store.artifact_path(record)?, record.artifact_sha256())?;
            if factory.identity()
                != &FactoryIdentity::native(record.plugin(), record.artifact_sha256())
            {
                return Err(NativeAddonUpdateError::Selection(
                    "ABI plugin identity differs from manifest",
                ));
            }
            builder
                .register_resolved(AddonScope::Agent, factory)
                .map_err(|_| NativeAddonUpdateError::Selection("native factory declaration"))?;
            for key in record.portable_services() {
                builder
                    .isolate_agent_portable(key.clone())
                    .map_err(|_| NativeAddonUpdateError::Selection("Portable isolation"))?;
            }
        }
        let addons = if before.enabled.is_empty() {
            self.base.clone()
        } else {
            let addon = builder
                .build()
                .map_err(|_| NativeAddonUpdateError::Selection("native addon declaration"))?;
            self.base
                .merged(addon)
                .map_err(|_| NativeAddonUpdateError::Selection("combined addon catalog"))?
        };
        let compiler = crate::agent_preset::standard_agent_profile_compiler(
            &self.paths,
            self.linux_tools,
            &addons,
        )
        .map_err(|_| NativeAddonUpdateError::Selection("Agent compiler declaration"))?;
        let contributions = addons
            .agent_catalog()
            .map_err(|_| NativeAddonUpdateError::Selection("Agent contribution catalog"))?;
        let snapshot = Arc::new(AgentCompositionSnapshot::new(
            self.presets.clone().with_compiler(compiler),
            contributions,
        ));
        let after = self.store.snapshot()?;
        if after.enabled != before.enabled {
            return Err(NativeAddonUpdateError::Selection(
                "enabled selection changed during staging",
            ));
        }
        self.check_admission()?;
        let mut state = self.state();
        if self.closed.load(Ordering::Acquire) {
            return Err(NativeAddonUpdateError::Closed);
        }
        state.current = Some(Staged {
            revision: after.revision,
            selected: after.enabled,
            snapshot,
        });
        state.failed = false;
        Ok(NativeAddonRefresh {
            source_revision: after.revision,
            changed: true,
            selected: before.enabled.len(),
        })
    }

    fn preflight(&self, selected: &[NativeAddonRecord]) -> Result<()> {
        let mut plugins: BTreeSet<_> = self
            .base
            .descriptions()
            .map(|entry| entry.plugin.clone())
            .collect();
        if selected.len()
            > rsi_host::HostLimits::default()
                .maximum_factories
                .saturating_sub(plugins.len())
        {
            return Err(NativeAddonUpdateError::Selection("factory capacity"));
        }
        if !selected.is_empty() {
            let reservation = StandardAddonBuilder::new("rsi.native.local")
                .build()
                .map_err(|_| NativeAddonUpdateError::Selection("native addon reservation"))?;
            self.base.merged(reservation).map_err(|_| {
                NativeAddonUpdateError::Selection("native addon identity or capacity")
            })?;
        }
        for record in selected {
            if record.target() != crate::native_addon_target() {
                return Err(NativeAddonUpdateError::Selection(
                    "unsupported native target",
                ));
            }
            if !plugins.insert(record.plugin().to_owned()) {
                return Err(NativeAddonUpdateError::Selection(
                    "duplicate Agent plugin identity",
                ));
            }
        }
        Ok(())
    }

    /// Closes new selection without revoking existing pins or claiming finalization.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.state().current = None;
    }

    /// Observes bounded metadata without native code execution or raw error text.
    pub fn inspect(&self) -> NativeAddonInspection {
        let source = self.store.snapshot().ok();
        let resources = self.loader.snapshot();
        if resources.retained_failed_finalizations != 0 {
            self.retained.store(true, Ordering::Release);
        }
        let state = self.state();
        let staged = state.current.as_ref();
        let health = if self.retained.load(Ordering::Acquire) {
            NativeAddonHealth::Retained
        } else if self.closed.load(Ordering::Acquire) {
            NativeAddonHealth::Closed
        } else if source.is_none() {
            NativeAddonHealth::InvalidSource
        } else if state.failed {
            NativeAddonHealth::Failed
        } else if source.as_ref().map(|source| &source.enabled)
            != staged.map(|staged| &staged.selected)
        {
            NativeAddonHealth::Pending
        } else {
            NativeAddonHealth::Ready
        };
        NativeAddonInspection {
            health,
            source_revision: source.as_ref().map(|source| source.revision),
            staged_revision: staged.map(|staged| staged.revision),
            desired: source.map_or_else(Vec::new, |source| source.enabled),
            staged: staged.map_or_else(Vec::new, |staged| staged.selected.clone()),
            retained_failed_finalizations: resources.retained_failed_finalizations,
            staging_bytes: resources.staging_bytes,
            active_callbacks: resources.active_callbacks,
            active_instances: resources.active_instances,
            host_capabilities: resources.host_capabilities,
            host_outputs: resources.host_outputs,
        }
    }

    fn check_admission(&self) -> Result<()> {
        if self.loader.snapshot().retained_failed_finalizations != 0 {
            self.retained.store(true, Ordering::Release);
        }
        if self.retained.load(Ordering::Acquire) {
            return Err(NativeAddonUpdateError::Retained);
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(NativeAddonUpdateError::Closed);
        }
        Ok(())
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl AgentCompositionSource for NativeAddonManager {
    fn snapshot(&self) -> rsi_meta_profile::Result<Arc<AgentCompositionSnapshot>> {
        let capture = || -> Result<_> {
            self.check_admission()?;
            let source = self.store.snapshot()?;
            let state = self.state();
            if state.failed {
                return Err(NativeAddonUpdateError::Selection("last refresh failed"));
            }
            let current = state
                .current
                .as_ref()
                .ok_or(NativeAddonUpdateError::Closed)?;
            if current.selected != source.enabled {
                return Err(NativeAddonUpdateError::Selection("native refresh pending"));
            }
            self.check_admission()?;
            Ok(Arc::clone(&current.snapshot))
        };
        capture().map_err(|_| rsi_meta_profile::ProfileError::Source {
            message: "native Agent catalog is unavailable; inspect the local addon manager".into(),
        })
    }
}
