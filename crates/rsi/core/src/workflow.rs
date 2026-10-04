//! Coarse Workflow readiness from product-owned Host observations.
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
};
use rsi_meta_profile::{ProfileControlContract, ProfileHealth, ProfileInstanceState, SnapshotNode};
use rsi_session_protocol::{
    WorkflowReadinessContract, WorkflowReadinessSource, WorkflowRuntimeKind, WorkflowRuntimeStatus,
};
use std::sync::Arc;
#[derive(Debug)]
struct Source(Context);
fn enabled(nodes: &[SnapshotNode], parent: bool) -> Option<bool> {
    for node in nodes {
        let active = parent && node.enabled();
        if node
            .plugin()
            .is_some_and(|p| p.as_str() == rsi_session_protocol::PROGRAM_RUNTIME_PLUGIN_ID)
        {
            return Some(active);
        }
        if let Some(found) = enabled(node.children(), active) {
            return Some(found);
        }
    }
    None
}
impl WorkflowReadinessSource for Source {
    fn available(&self) -> bool {
        self.0
            .lookup_local::<rsi_agent_program::ProgramRuntimeContract>()
            .is_some()
    }
    fn runtime(&self) -> WorkflowRuntimeStatus {
        let available = self.available();
        let mut answer = WorkflowRuntimeStatus {
            kind: if available {
                WorkflowRuntimeKind::Available
            } else {
                WorkflowRuntimeKind::Absent
            },
            available,
            host_restart_required: false,
            desired_revision: 0,
            observed_revision: 0,
        };
        if let Some(control) = self.0.lookup_local::<ProfileControlContract>() {
            let status = control.status();
            let snapshot = control.snapshot();
            answer.desired_revision = snapshot.revision();
            answer.observed_revision = status.revision();
            answer.host_restart_required = status.health() == ProfileHealth::RestartRequired;
            answer.kind = match enabled(snapshot.nodes(), true) {
                Some(false) => WorkflowRuntimeKind::Disabled,
                Some(true) if available => WorkflowRuntimeKind::Available,
                Some(true) if status.observed().iter().any(|n| matches!(n.factory(), rsi_meta::FactoryIdentity::Linked { plugin, .. } | rsi_meta::FactoryIdentity::Native { plugin, .. } if plugin.as_str() == rsi_session_protocol::PROGRAM_RUNTIME_PLUGIN_ID) && matches!(n.state(), ProfileInstanceState::Pending(_) | ProfileInstanceState::Loading)) => WorkflowRuntimeKind::Pending,
                Some(true) => WorkflowRuntimeKind::Unavailable,
                None => WorkflowRuntimeKind::Absent,
            };
        }
        answer
    }
}
#[derive(Debug)]
pub(crate) struct ReadinessFactory;
#[async_trait::async_trait]
impl PluginFactory for ReadinessFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Workflow readiness requires null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<WorkflowReadinessContract>(Arc::new(Source(plan.context().clone())))?;
        plan.defer(
            "withdraw workflow readiness",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
