use super::{NativeAddonControl, NativeAddonControlContract, NativeAddonUpdateError};
use async_trait::async_trait;
use rsi_api_protocol::ApiRegistrarContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_native_addons_api::{
    NativeAddonAdministration, NativeAddonsApi, RefreshFailure, RefreshReceipt,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct NativeAddonApiFactory;
#[async_trait]
impl PluginFactory for NativeAddonApiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "native API configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<NativeAddonControlContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let owner = Arc::new(Owner {
            stop: CancellationToken::new(),
            api: Mutex::new(None),
        });
        let cleanup = owner.clone();
        // Reserve cleanup before publication. Activation's effect setup window
        // cannot drain until the synchronous registration is installed below.
        plan.defer(
            "close local native addon control API",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.stop.cancel();
                    let api = cleanup
                        .api
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(api) = api {
                        api.close().await;
                    }
                    Ok(())
                })
            }),
        )?;
        let administration = Arc::new(Administration {
            control: plan.local::<NativeAddonControlContract>()?,
            stop: owner.stop.clone(),
        });
        let api = NativeAddonsApi::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            administration,
        )
        .map_err(|_| MetaError::Activation("native control API registration failed".into()))?;
        *owner
            .api
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(api);
        Ok(())
    }
}
struct Owner {
    stop: CancellationToken,
    api: Mutex<Option<NativeAddonsApi>>,
}
#[derive(Debug)]
struct Administration {
    control: Arc<dyn NativeAddonControl>,
    stop: CancellationToken,
}
#[async_trait]
impl NativeAddonAdministration for Administration {
    async fn refresh(&self) -> Result<RefreshReceipt, RefreshFailure> {
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Err(RefreshFailure::Closed),
            result = self.control.refresh() => result.map(|value| RefreshReceipt {
                source_revision: value.source_revision,
                changed: value.changed,
                selected: value.selected,
            }).map_err(|error| match error {
                NativeAddonUpdateError::Store(_) => RefreshFailure::Source,
                NativeAddonUpdateError::Load(_) => RefreshFailure::Load,
                NativeAddonUpdateError::Selection(_) => RefreshFailure::Selection,
                NativeAddonUpdateError::Busy => RefreshFailure::Busy,
                NativeAddonUpdateError::Closed => RefreshFailure::Closed,
                NativeAddonUpdateError::Retained => RefreshFailure::Retained,
            }),
        }
    }
}
