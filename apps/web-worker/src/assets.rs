//! Worker-owned renderer admission, sharing the existing presentation wake-up path.
use async_trait::async_trait;
use rsi_api_protocol::ApiClientContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_web_assets_api::{AssetsClient, Commit, Observe, Offer};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct AssetsContract;
impl LocalContract for AssetsContract {
    const KEY: &'static str = "rsi.web.renderer.leases";
    type Service = Assets;
}
#[derive(Debug)]
pub(crate) struct Assets {
    client: AssetsClient,
    application: String,
    changed: watch::Sender<Option<Result<Offer, String>>>,
    pending: Mutex<Option<String>>,
    stop: CancellationToken,
}
impl Assets {
    pub fn changes(&self) -> watch::Receiver<Option<Result<Offer, String>>> {
        self.changed.subscribe()
    }
    pub async fn commit(&self, revision: String, accept: bool) -> Result<(), String> {
        {
            let mut pending = self.pending.lock().expect("renderer offer poisoned");
            if self.stop.is_cancelled() || pending.as_ref() != Some(&revision) {
                return Err("renderer offer is stale".into());
            }
            pending.take();
        }
        let result = self
            .client
            .commit(&Commit {
                application: self.application.clone(),
                revision,
                accept,
            })
            .await
            .map_err(rsi_gui::display_error);
        if let Err(error) = &result {
            self.stop.cancel();
            self.changed.send_replace(Some(Err(error.clone())));
        }
        result
    }
}
#[derive(Debug)]
pub(crate) struct AssetsFactory(pub String);
#[async_trait]
impl PluginFactory for AssetsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "renderer leases accept null configuration".into(),
            ));
        }
        Observe {
            application: self.0.clone(),
        }
        .validate()
        .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = AssetsClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let owner = Arc::new(Assets {
            client,
            application: self.0.clone(),
            changed: watch::channel(None).0,
            pending: Mutex::new(None),
            stop: CancellationToken::new(),
        });
        let producer = owner.clone();
        let task = plan.context().runtime().execution().spawn(async move {
            let work = async {
                let mut observation = producer.client.observe(&Observe { application: producer.application.clone() }).await.map_err(rsi_gui::display_error)?;
                loop {
                    let offer = observation.next().await.map_err(rsi_gui::display_error)?;
                    let mut pending = producer.pending.lock().expect("renderer offer poisoned");
                    if pending.is_some() { return Err::<(), String>("renderer server sent an unacknowledged second offer".into()); }
                    *pending = Some(offer.revision.clone());
                    producer.changed.send_replace(Some(Ok(offer)));
                }
            };
            tokio::select! { biased;
                () = producer.stop.cancelled() => {},
                result = work => { if let Err(error) = result { producer.changed.send_replace(Some(Err(error))); } }
            }
        });
        let retiring = owner.clone();
        plan.defer(
            "join renderer generation observation",
            Box::new(move || {
                Box::pin(async move {
                    retiring.stop.cancel();
                    task.await.map_err(|error| error.to_string())?;
                    retiring
                        .pending
                        .lock()
                        .expect("renderer offer poisoned")
                        .take();
                    retiring.changed.send_replace(None);
                    Ok(())
                })
            }),
        )?;
        let supply = plan.context().provide_local::<AssetsContract>(owner)?;
        plan.defer(
            "withdraw renderer admission",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
