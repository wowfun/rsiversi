use super::{Adapter, activation, invalid, portable};
use async_trait::async_trait;
use rsi_application::ScopedProfile;
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, ContractVersion, LocalContract, PluginFactory,
    PreparedActivation, Requirement, UpdateMode,
};
use rsi_ui::{PresentationBinding, PresentationBindingOwner, UiBusinessApiContract, UiError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct Bound {
    context: Context,
    pub adapter: Adapter,
}
#[derive(Debug)]
pub(super) struct BoundContract;
impl LocalContract for BoundContract {
    const KEY: &'static str = "rsi.ui.portable.bound-call";
    type Service = Bound;
}
#[derive(Debug)]
struct Factory {
    service: String,
    stop: CancellationToken,
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(invalid());
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring(Requirement::new(
                self.service.clone(),
                portable::CONTRACT,
                ContractVersion(portable::VERSION),
            ))
            .requiring_local::<UiBusinessApiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let business = plan.local::<UiBusinessApiContract>()?;
        business.scope.validate().map_err(activation)?;
        let key = rsi_ui::fresh_identity("ui-api").map_err(activation)?;
        let operations = business
            .client
            .operations()
            .iter()
            .map(|operation| operation.id.clone())
            .collect::<Vec<_>>();
        let api = rsi_api_portable::export_api(&plan, &key, business.client.clone(), &operations)?;
        let bound = Arc::new(Bound {
            context: plan.context().clone(),
            adapter: Adapter {
                capability: plan.inject(&self.service).ok_or_else(invalid)?.clone(),
                service: self.service.clone(),
                business_api: false,
                grant: Some(api),
                scope: Some(business.scope.clone()),
                stop: self.stop.clone(),
            },
        });
        let supply = plan.context().provide_local::<BoundContract>(bound)?;
        plan.defer(
            "withdraw bound UI caller",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Owner {
    profile: Arc<ScopedProfile>,
    execution: rsi_meta::Execution,
    stop: CancellationToken,
    closed: AtomicBool,
}
#[async_trait]
impl PresentationBindingOwner for Owner {
    fn retire(&self) {
        self.stop.cancel();
    }
    async fn close(&self) -> rsi_ui::Result<()> {
        self.retire();
        let clean = self.profile.shutdown().await.is_clean();
        self.closed.store(true, Ordering::Release);
        if clean {
            Ok(())
        } else {
            Err(UiError::Action(
                "Portable presentation cleanup failed".into(),
            ))
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.stop.cancel();
        let profile = self.profile.clone();
        self.execution.spawn(async move {
            profile.shutdown().await;
        });
    }
}
pub(super) async fn bind(
    parent: Context,
    service: String,
    stop: CancellationToken,
) -> rsi_ui::Result<Option<PresentationBinding>> {
    if stop.is_cancelled() {
        return Err(UiError::Retired);
    }
    // Fail before child allocation instead of leaving an unsupported target Pending.
    parent
        .lookup_local::<UiBusinessApiContract>()
        .ok_or_else(|| UiError::Invalid("target has no explicit business API grant".into()))?;
    let mut builder = HostBuilder::without_paths(std::env::consts::OS);
    builder
        .register_local_contract::<BoundContract>()
        .map_err(error)?;
    builder
        .register_linked(
            "rsi.ui.portable.bound",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(Factory {
                service,
                stop: stop.clone(),
            }),
        )
        .map_err(error)?;
    let host = builder.build().map_err(error)?;
    let profile = Arc::new(
        ScopedProfile::start(
            &host,
            &parent,
            ProfileProgram::from_profile(Profile::new(vec![ProfileEntry::new(
                "bound",
                "rsi.ui.portable.bound",
                ConfigValue::Null,
            )])),
        )
        .await
        .map_err(error)?,
    );
    // Install a Drop cleanup owner before looking up any fallible publication.
    let owner = Arc::new(Owner {
        profile,
        execution: parent.runtime().execution().clone(),
        stop: stop.clone(),
        closed: AtomicBool::new(false),
    });
    if stop.is_cancelled() {
        owner.close().await?;
        return Err(UiError::Retired);
    }
    let bound = owner
        .profile
        .lookup_local::<BoundContract>()
        .ok_or_else(|| UiError::Invalid("bound UI caller did not publish".into()))?;
    Ok(Some(PresentationBinding::new(bound.context.clone(), owner)))
}
fn error(error: impl std::fmt::Display) -> UiError {
    UiError::Action(error.to_string())
}
