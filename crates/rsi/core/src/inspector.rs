use async_trait::async_trait;
use rsi_api_protocol::{ApiError, ApiRegistrarContract, Result};
use rsi_inspector::{FactoryDeclaration, InspectorApi, InspectorSource, NativeObservation};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, InspectionRequest, MetaError, PluginFactory,
    PreparedActivation, RuntimeInspection,
};
use rsi_meta_profile::{ProfileControlContract, ProfileSnapshot, ProfileStatus};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Default)]
pub(crate) struct InspectorFactory {
    declarations: OnceLock<Arc<[FactoryDeclaration]>>,
}
impl InspectorFactory {
    pub(crate) fn freeze(
        &self,
        declarations: &[crate::AddonFactoryDescription],
    ) -> rsi_host::Result<()> {
        let values = declarations
            .iter()
            .map(|value| FactoryDeclaration {
                addon: value.addon.clone(),
                scope: match value.scope {
                    crate::AddonScope::Service => "service",
                    crate::AddonScope::Agent => "agent",
                    crate::AddonScope::Application => "application",
                    crate::AddonScope::Client => "client",
                }
                .into(),
                identity: value.identity.clone(),
                update_mode: value.update_mode.clone(),
            })
            .collect();
        self.declarations.set(values).map_err(|_| {
            rsi_host::HostError::Bootstrap("Inspector declarations already frozen".into())
        })
    }
}
#[async_trait]
impl PluginFactory for InspectorFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() || self.declarations.get().is_none() {
            return Err(MetaError::InvalidInput(
                "Inspector requires frozen declarations and null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let source = Arc::new(Source {
            context: plan.context().clone(),
            declarations: self
                .declarations
                .get()
                .ok_or_else(|| MetaError::Activation("Inspector declarations unavailable".into()))?
                .clone(),
        });
        let api = InspectorApi::register(plan.local::<ApiRegistrarContract>()?.as_ref(), source)
            .map_err(|_| MetaError::Activation("Inspector registration failed".into()))?;
        plan.defer(
            "close local Inspector",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Source {
    context: Context,
    declarations: Arc<[FactoryDeclaration]>,
}
impl InspectorSource for Source {
    fn runtime(&self, request: InspectionRequest) -> Result<RuntimeInspection> {
        self.context
            .runtime()
            .inspect(request)
            .map_err(|_| ApiError::Unavailable)
    }
    fn profile(&self) -> Result<(ProfileStatus, ProfileSnapshot)> {
        let control = self
            .context
            .lookup_local::<ProfileControlContract>()
            .ok_or(ApiError::Unavailable)?;
        Ok((control.status(), control.snapshot()))
    }
    fn factories(&self) -> &[FactoryDeclaration] {
        &self.declarations
    }
    fn native(&self) -> Result<NativeObservation> {
        #[cfg(unix)]
        {
            use crate::NativeAddonHealth as H;
            use rsi_inspector::NativeHealth as W;
            let control = self
                .context
                .lookup_local::<crate::NativeAddonControlContract>()
                .ok_or(ApiError::Unavailable)?;
            let value = control.inspect();
            let artifact = |value: crate::NativeAddonRecord| rsi_inspector::NativeArtifact {
                id: value.id().into(),
                plugin: value.plugin().into(),
                target: value.target().into(),
                sha256: value.artifact_sha256().into(),
                portable_services: value.portable_services().to_vec(),
            };
            let health = match value.health {
                H::Ready => W::Ready,
                H::Pending => W::Pending,
                H::Failed => W::Failed,
                H::InvalidSource => W::InvalidSource,
                H::Retained => W::Retained,
                H::Closed => W::Closed,
            };
            Ok(NativeObservation {
                health,
                source_revision: value.source_revision.map(|v| v.to_string()),
                staged_revision: value.staged_revision.map(|v| v.to_string()),
                desired: value.desired.into_iter().map(artifact).collect(),
                staged: value.staged.into_iter().map(artifact).collect(),
                retained_failed_finalizations: value.retained_failed_finalizations,
                staging_bytes: value.staging_bytes.to_string(),
                active_callbacks: value.active_callbacks,
                active_instances: value.active_instances,
                host_capabilities: value.host_capabilities,
                host_outputs: value.host_outputs,
            })
        }
        #[cfg(not(unix))]
        Err(ApiError::Unavailable)
    }
}
