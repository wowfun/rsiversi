use super::{Arc, BTreeMap, Contract, Manager, Mutex, Record, Semaphore, State, TaskTracker, api};
use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage_domain::{DomainFacilityContract, DomainSpec};
pub(crate) fn register(builder: &mut crate::StandardAddonBuilder) -> rsi_host::Result<()> {
    builder.register_local_contract::<Contract>()?;
    builder.register_linked(
        "rsi.ssh-targets",
        env!("CARGO_PKG_VERSION"),
        rsi_meta::UpdateMode::RestartRequired,
        Arc::new(Factory),
    )?;
    builder.register_fragment(rsi_host::ProfileFragment::new(
        "rsi.standard.ssh-targets",
        [rsi_host::ProfileEntry::new(
            "rsi.ssh-targets",
            "rsi.ssh-targets",
            ConfigValue::Null,
        )],
    ))?;
    Ok(())
}
#[derive(Debug)]
struct Factory;
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "SSH target owner requires null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<crate::profile_management::Contract>()
            .requiring_local::<rsi_configuration_access::ConfigurationAccessContract>()
            .requiring_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .requiring_local::<rsi_api_protocol::ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.ssh-targets".into(),
                backend: "base".into(),
                version: 1,
                maximum_records: 64,
                maximum_bytes: 2 * 1024 * 1024,
            })
            .await
            .map_err(|_| unavailable())?;
        let mut records = BTreeMap::new();
        for (key, value) in domain.snapshot().await.map_err(|_| unavailable())? {
            let record: Record = serde_json::from_value(value).map_err(|_| unavailable())?;
            record.validate().map_err(|_| unavailable())?;
            if key != record.candidate.target.as_str() {
                return Err(unavailable());
            }
            records.insert(record.candidate.target.clone(), record);
        }
        let description = plan.local::<rsi_api_protocol::ConnectionDescriptionContract>()?;
        let owner = Arc::new(Manager {
            context: plan.context().clone(),
            domain,
            grants: plan.local::<crate::profile_management::Contract>()?,
            configuration: plan.local::<rsi_configuration_access::ConfigurationAccessContract>()?,
            epoch: description.host_epoch.clone(),
            service: description.endpoint_id.as_str().into(),
            state: Mutex::new(State {
                closed: false,
                records,
                connections: BTreeMap::new(),
                next_epoch: first_connection_epoch(&description.host_epoch),
            }),
            writer: Arc::new(Semaphore::new(1)),
            target_writers: super::TargetWriters::default(),
            slots: Arc::new(Semaphore::new(4)),
            effects: Arc::new(Semaphore::new(64)),
            operations: Arc::new(Semaphore::new(64)),
            tasks: TaskTracker::new(),
            stop: tokio_util::sync::CancellationToken::new(),
        });
        let registrations = api::register(
            plan.local::<rsi_api_protocol::ApiRegistrarContract>()?
                .as_ref(),
            owner.clone(),
        )
        .map_err(|_| unavailable())?;
        let supply = plan.context().provide_local::<Contract>(owner.clone())?;
        plan.defer(
            "drain SSH targets",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    owner.close().await;
                    for registration in registrations {
                        registration.close().await;
                    }
                    Ok(())
                })
            }),
        )
    }
}
fn unavailable() -> MetaError {
    MetaError::Activation("SSH target owner unavailable".into())
}

// Reserve the upper half for checked monotonic increments within this Host.
// HostEpoch is a validated 128-bit identity issued from native OS entropy.
fn first_connection_epoch(host: &rsi_api_protocol::HostEpoch) -> u64 {
    let prefix = u64::from_str_radix(&host.as_str()[..16], 16).expect("validated Host epoch");
    (prefix >> 1) + 1
}

#[cfg(test)]
mod tests {
    use super::first_connection_epoch;
    use rsi_api_protocol::HostEpoch;
    #[test]
    fn restarted_host_changes_connection_seed_without_exhausting_increment_space() {
        assert_eq!(first_connection_epoch(&HostEpoch::from_bytes([0; 16])), 1);
        assert_eq!(
            first_connection_epoch(&HostEpoch::from_bytes([255; 16])),
            1_u64 << 63
        );
        assert_ne!(
            first_connection_epoch(&HostEpoch::from_bytes([17; 16])),
            first_connection_epoch(&HostEpoch::from_bytes([18; 16]))
        );
    }
}
