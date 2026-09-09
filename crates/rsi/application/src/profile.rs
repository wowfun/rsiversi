use crate::Result;
use rsi_host::{Host, HostError, ProfileControl, ProfileProgram, ReloadOutcome};
use rsi_meta::{CleanupReport, Context, FiberState, LocalContract, ResolvedFactory, UpdateMode};
use rsi_meta_scope::{ScopeHandle, ScopeRoot};
use std::sync::Arc;

/// One ordinary Profile mounted below a caller-owned Context.
#[derive(Debug)]
pub struct ScopedProfile {
    scope: ScopeHandle,
    context: Context,
    control: Arc<dyn ProfileControl>,
}
impl ScopedProfile {
    /// Prepares before activating, with fresh Local identities for the frozen catalog.
    pub async fn start(host: &Host, parent: &Context, program: ProfileProgram) -> Result<Self> {
        let isolated = host.isolate_local_context(parent.clone())?;
        let bootstrap = host.prepare_in(parent.runtime(), program).await?;
        let control = bootstrap.control();
        let scopes = ScopeRoot::new(16)?;
        let scope = scopes.create(&isolated).await?;
        let context = scope.context().meta().clone();
        let result = context
            .apply(
                ResolvedFactory::linked(
                    "rsi.profile.child",
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    bootstrap.factory(),
                ),
                serde_json::Value::Null,
            )
            .await;
        let failure = match result {
            Ok(handle) if handle.snapshot().state == FiberState::Active => None,
            Ok(handle) => Some(HostError::Bootstrap(format!(
                "child Profile settled as {:?}",
                handle.snapshot().state
            ))),
            Err(error) => Some(HostError::Meta(error)),
        };
        if let Some(failure) = failure {
            let cleanup = scope.dispose().await;
            return Err(if cleanup.is_clean() {
                failure
            } else {
                HostError::Bootstrap(format!(
                    "{failure}; child rollback reported {} failures",
                    cleanup.total_failures()
                ))
            }
            .into());
        }
        Ok(Self {
            scope,
            context,
            control,
        })
    }
    /// Looks up a capability using this Profile's fixed Local mappings.
    pub fn lookup_local<C: LocalContract>(&self) -> Option<Arc<C::Service>> {
        self.context.lookup_local::<C>()
    }
    /// Reloads only this Profile through its ordinary control capability.
    pub async fn reload(&self) -> rsi_host::Result<ReloadOutcome> {
        self.control.reload().await.map_err(Into::into)
    }
    /// Observes this Profile's convergence and retirement without owning its lifetime.
    pub fn subscribe_profile(&self) -> tokio::sync::watch::Receiver<rsi_host::ProfileStatus> {
        self.control.subscribe()
    }

    /// Returns this Profile's existing bounded convergence status.
    pub fn profile_status(&self) -> rsi_host::ProfileStatus {
        self.control.status()
    }

    /// Returns this Profile's existing redacted desired tree.
    pub fn profile_snapshot(&self) -> rsi_host::ProfileSnapshot {
        self.control.snapshot()
    }

    /// Inspects only this real scope's generation and descendants, without global counters.
    pub fn inspect(
        &self,
        request: rsi_meta::InspectionRequest,
    ) -> rsi_meta::Result<rsi_meta::RuntimeInspection> {
        self.context.inspect(request)
    }
    /// Disposes only this Profile's scope and returns Meta's exact cleanup report.
    pub async fn shutdown(&self) -> CleanupReport {
        self.scope.dispose().await
    }
    pub(crate) fn context(&self) -> Context {
        self.context.clone()
    }
    pub(crate) fn control(&self) -> Arc<dyn ProfileControl> {
        self.control.clone()
    }
}
