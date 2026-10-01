//! Standard product selection of complete native or SSH execution providers.
use async_trait::async_trait;
use rsi_api_protocol::{ApiError, CallOrigin};
use rsi_execution::{
    ExecutionAdmission, ExecutionAdmissionKind, ExecutionLease, ExecutionLocation,
    ExecutionOperation, ExecutionProvider, ExecutionResolver, ExecutionResolverContract,
    ResolvedProgram,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub(crate) fn register(
    builder: &mut crate::StandardAddonBuilder,
    programs: BTreeMap<String, ResolvedProgram>,
) -> rsi_host::Result<()> {
    builder.register_local_contract::<ExecutionResolverContract>()?;
    builder.register_linked(
        "rsi.execution.access",
        env!("CARGO_PKG_VERSION"),
        rsi_meta::UpdateMode::RestartRequired,
        Arc::new(Factory { programs }),
    )?;
    builder.register_fragment(rsi_host::ProfileFragment::new(
        "rsi.standard.execution-access",
        [rsi_host::ProfileEntry::new(
            "rsi.execution.access",
            "rsi.execution.access",
            ConfigValue::Null,
        )],
    ))?;
    Ok(())
}
#[derive(Debug)]
struct Factory {
    programs: BTreeMap<String, ResolvedProgram>,
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Execution resolver requires null configuration".into(),
            ));
        }
        let plan = PreparedActivation::new(config.clone())
            .requiring_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>()
            .requiring_local::<rsi_process::ProcessContract>()
            .requiring_local::<rsi_process::DuplexProcessContract>()
            .requiring_local::<rsi_process::PtyProcessContract>()
            .requiring_local::<rsi_files_protocol::FilesContract>();
        #[cfg(target_os = "linux")]
        let plan = plan.requiring_local::<crate::ssh_targets::Contract>();
        Ok(plan)
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let local = rsi_execution_local::provider(
            plan.local::<rsi_api_protocol::ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            rsi_execution_local::NativeCapabilities {
                sandbox: plan.local::<rsi_sandbox::SandboxContract>()?,
                process: plan.local::<rsi_process::ProcessContract>()?,
                duplex: plan.local::<rsi_process::DuplexProcessContract>()?,
                pty: plan.local::<rsi_process::PtyProcessContract>()?,
                files: plan.local::<rsi_files_protocol::FilesContract>()?,
            },
            self.programs.clone(),
        )
        .await
        .map_err(|_| MetaError::Activation("Local execution provider unavailable".into()))?;
        let stop = CancellationToken::new();
        let owner = Arc::new(Resolver {
            local,
            stop: stop.clone(),
            slots: Arc::new(Semaphore::new(64)),
            operations: Arc::new(Semaphore::new(64)),
            #[cfg(target_os = "linux")]
            ssh: plan.local::<crate::ssh_targets::Contract>()?,
        });
        let supply = plan
            .context()
            .provide_local::<ExecutionResolverContract>(owner)?;
        plan.defer(
            "withdraw execution resolver",
            Box::new(move || {
                Box::pin(async move {
                    stop.cancel();
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Resolver {
    local: ExecutionProvider,
    stop: CancellationToken,
    slots: Arc<Semaphore>,
    operations: Arc<Semaphore>,
    #[cfg(target_os = "linux")]
    ssh: Arc<crate::ssh_targets::Manager>,
}
impl ExecutionResolver for Resolver {
    fn visibility(
        &self,
        origin: &CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        let local = self.admit(origin, &ExecutionLocation::Local)?;
        #[cfg(target_os = "linux")]
        {
            let visibility = self.ssh.visibility(origin)?;
            Ok(rsi_execution::ExecutionVisibility::new(
                visibility.locations().clone(),
                ExecutionOperation::new((local, visibility)),
            ))
        }
        #[cfg(not(target_os = "linux"))]
        Ok(rsi_execution::ExecutionVisibility::new(
            rsi_execution::ExecutionLocations::only(std::collections::BTreeSet::from([
                ExecutionLocation::Local,
            ]))
            .map_err(|_| ApiError::Capacity)?,
            local,
        ))
    }
    fn admit(
        &self,
        origin: &CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionOperation> {
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        match location {
            ExecutionLocation::Local => LocalAdmission {
                origin: origin.clone(),
                stop: self.stop.clone(),
                slots: self.slots.clone(),
                operations: self.operations.clone(),
            }
            .admit(ExecutionAdmissionKind::Scope)
            .map_err(process_error),
            #[cfg(target_os = "linux")]
            ExecutionLocation::Ssh { target } => self.ssh.admit(origin, target),
            #[cfg(not(target_os = "linux"))]
            ExecutionLocation::Ssh { .. } => Err(ApiError::Unavailable),
        }
    }
    fn lease(
        &self,
        origin: CallOrigin,
        location: &ExecutionLocation,
    ) -> rsi_api_protocol::Result<ExecutionLease> {
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        match location {
            ExecutionLocation::Local => self
                .local
                .lease(Arc::new(LocalAdmission {
                    origin,
                    stop: self.stop.clone(),
                    slots: self.slots.clone(),
                    operations: self.operations.clone(),
                }))
                .map_err(process_error),
            #[cfg(target_os = "linux")]
            ExecutionLocation::Ssh { target } => self.ssh.lease(origin, target),
            #[cfg(not(target_os = "linux"))]
            ExecutionLocation::Ssh { .. } => Err(ApiError::Unavailable),
        }
    }
}
#[derive(Debug)]
struct LocalAdmission {
    origin: CallOrigin,
    stop: CancellationToken,
    slots: Arc<Semaphore>,
    operations: Arc<Semaphore>,
}
impl ExecutionAdmission for LocalAdmission {
    fn admit(&self, kind: ExecutionAdmissionKind) -> rsi_process::Result<ExecutionOperation> {
        if self.stop.is_cancelled() {
            return Err(rsi_process::ProcessError::ShuttingDown);
        }
        if let CallOrigin::Device(device) = &self.origin
            && device.revoked.is_cancelled()
        {
            return Err(rsi_process::ProcessError::Api(ApiError::Unauthorized));
        }
        let slots = match kind {
            ExecutionAdmissionKind::Scope => &self.slots,
            ExecutionAdmissionKind::Operation => &self.operations,
            ExecutionAdmissionKind::Publication => return Ok(ExecutionOperation::new(())),
        };
        Ok(ExecutionOperation::new(
            slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| rsi_process::ProcessError::Capacity)?,
        ))
    }
}
fn process_error(error: rsi_process::ProcessError) -> ApiError {
    match error {
        rsi_process::ProcessError::Api(error) => error,
        rsi_process::ProcessError::Capacity => ApiError::Capacity,
        rsi_process::ProcessError::ShuttingDown => ApiError::ShuttingDown,
        _ => ApiError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_api_protocol::{AuthenticatedDevice, DeviceId};

    #[test]
    fn local_admission_revokes_new_work_without_releasing_an_accepted_operation() {
        let revoked = CancellationToken::new();
        let slots = Arc::new(Semaphore::new(1));
        let stop = CancellationToken::new();
        let gate = LocalAdmission {
            origin: CallOrigin::Device(AuthenticatedDevice {
                id: DeviceId::from_bytes([1; 16]),
                revoked: revoked.clone(),
            }),
            stop: stop.clone(),
            slots: slots.clone(),
            operations: Arc::new(Semaphore::new(1)),
        };
        let accepted = gate.admit(ExecutionAdmissionKind::Scope).unwrap();
        assert!(matches!(
            gate.admit(ExecutionAdmissionKind::Scope),
            Err(rsi_process::ProcessError::Capacity)
        ));
        let io = gate.admit(ExecutionAdmissionKind::Operation).unwrap();
        assert!(matches!(
            gate.admit(ExecutionAdmissionKind::Operation),
            Err(rsi_process::ProcessError::Capacity)
        ));
        drop(gate.admit(ExecutionAdmissionKind::Publication).unwrap());
        drop(io);
        revoked.cancel();
        assert!(matches!(
            gate.admit(ExecutionAdmissionKind::Scope),
            Err(rsi_process::ProcessError::Api(ApiError::Unauthorized))
        ));
        assert_eq!(slots.available_permits(), 0);
        drop(accepted);
        assert_eq!(slots.available_permits(), 1);
        let local = LocalAdmission {
            origin: CallOrigin::Local,
            stop: stop.clone(),
            slots,
            operations: Arc::new(Semaphore::new(1)),
        };
        let accepted = local.admit(ExecutionAdmissionKind::Scope).unwrap();
        stop.cancel();
        assert!(matches!(
            local.admit(ExecutionAdmissionKind::Scope),
            Err(rsi_process::ProcessError::ShuttingDown)
        ));
        assert_eq!(local.slots.available_permits(), 0);
        drop(accepted);
        assert_eq!(local.slots.available_permits(), 1);
    }
}
