//! Deterministic Workspace policy seam: no native providers or live credentials.
use rsi_api_protocol::{ApiError, CallOrigin};
use rsi_execution::{
    ExecutionLease, ExecutionLocation, ExecutionOperation, ExecutionResolver,
    ExecutionResolverContract,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Default)]
pub struct Resolver {
    pub ssh_use: AtomicBool,
}
impl ExecutionResolver for Resolver {
    fn visibility(
        &self,
        _: &rsi_api_protocol::CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        panic!("unexpected enumeration in focused resolver fixture")
    }

    fn admit(
        &self,
        origin: &CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionOperation> {
        if let CallOrigin::Device(device) = origin
            && (device.revoked.is_cancelled()
                || (matches!(location, ExecutionLocation::Ssh { .. })
                    && !self.ssh_use.load(Ordering::SeqCst)))
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(ExecutionOperation::new(()))
    }
    fn lease(
        &self,
        origin: CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionLease> {
        let _operation = self.admit(&origin, location)?;
        Err(ApiError::Unavailable)
    }
}
#[derive(Debug)]
pub struct Factory(pub Arc<Resolver>);
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for Factory {
    fn prepare(&self, _: &rsi_meta::ConfigValue) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(serde_json::Value::Null))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ExecutionResolverContract>(self.0.clone())?;
        plan.defer(
            "withdraw fixture resolver",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
