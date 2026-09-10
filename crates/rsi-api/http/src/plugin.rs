use crate::{HttpConfig, HttpServer, HttpServices};
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiDispatchContract, ApiError, ConnectionDescriptionContract, DeviceAuthenticationContract,
    Result,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use std::{fmt, net::SocketAddr, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Read-only listener lifetime observed by the native Serve application.
#[async_trait]
pub trait HttpListener: fmt::Debug + Send + Sync + 'static {
    /// Actual bound address, including an ephemeral port when configured.
    fn address(&self) -> SocketAddr;
    /// Observes this generation's HTTP failures without access to request data.
    fn diagnostics(&self) -> crate::HttpDiagnostics;
    /// Waits for normal retirement or a listener failure; waiter drop cannot stop serving.
    async fn stopped(&self) -> Result<()>;
}
/// Nominal Local contract for native listener observation.
#[derive(Debug)]
pub struct HttpListenerContract;
impl LocalContract for HttpListenerContract {
    const KEY: &'static str = "rsi.api.http.listener";
    type Service = dyn HttpListener;
}
#[derive(Debug)]
struct Listener {
    address: SocketAddr,
    diagnostics: crate::HttpDiagnostics,
    stopped: watch::Receiver<Option<Result<()>>>,
}
#[async_trait]
impl HttpListener for Listener {
    fn address(&self) -> SocketAddr {
        self.address
    }
    fn diagnostics(&self) -> crate::HttpDiagnostics {
        self.diagnostics.clone()
    }
    async fn stopped(&self) -> Result<()> {
        let mut receiver = self.stopped.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| ApiError::Backend("HTTP task stopped without status".into()))?;
        }
    }
}

/// Ordinary Meta owner of the authenticated HTTP listener.
#[derive(Clone, Debug, Default)]
pub struct HttpFactory;
#[async_trait]
impl PluginFactory for HttpFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: HttpConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = std::mem::size_of::<HttpConfig>()
            + config.public_origin.len()
            + config.tls.as_ref().map_or(0, |files| {
                files.certificate.as_os_str().len() + files.key.as_os_str().len()
            });
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring_local::<ApiDispatchContract>()
                .requiring_local::<DeviceAuthenticationContract>()
                .requiring_local::<ConnectionDescriptionContract>(),
        )
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        activate(plan, None).await
    }
}

/// Ordinary HTTP listener requiring an independently owned static asset provider.
#[derive(Clone, Debug, Default)]
pub struct StaticHttpFactory;
#[async_trait]
impl PluginFactory for StaticHttpFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(HttpFactory
            .prepare(desired)?
            .requiring_local::<crate::HttpAssetsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let assets = plan.local::<crate::HttpAssetsContract>()?;
        activate(plan, Some(assets)).await
    }
}
async fn activate(
    mut plan: ActivationPlan,
    assets: Option<Arc<dyn crate::HttpAssets>>,
) -> rsi_meta::Result<()> {
    let config = plan.take_state::<HttpConfig>()?;
    let execution = plan.context().runtime().execution().clone();
    let description = plan.local::<ConnectionDescriptionContract>()?;
    let services = HttpServices {
        dispatch: plan.local::<ApiDispatchContract>()?,
        authentication: plan.local::<DeviceAuthenticationContract>()?,
        endpoint: description.endpoint_id.clone(),
        epoch: description.host_epoch.clone(),
    };
    let server = HttpServer::bind(execution.clone(), config, services)
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
    let server = if let Some(assets) = assets {
        server.with_assets(assets)
    } else {
        server
    };
    let address = server
        .local_addr()
        .map_err(|error| MetaError::Activation(error.to_string()))?;
    let stop = CancellationToken::new();
    let diagnostics = server.diagnostics();
    let guard = stop.clone().drop_guard();
    let (sender, stopped) = watch::channel(None);
    let task = execution.spawn(async move {
        let result = server.serve(stop).await;
        sender.send_replace(Some(result));
    });
    plan.defer(
        "stop HTTP listener",
        Box::new(move || {
            Box::pin(async move {
                drop(guard);
                task.await
                    .map_err(|_| "HTTP task failed during retirement".to_owned())?;
                Ok(())
            })
        }),
    )?;
    let supply = plan
        .context()
        .provide_local::<HttpListenerContract>(Arc::new(Listener {
            address,
            diagnostics,
            stopped,
        }))?;
    plan.defer(
        "withdraw HTTP listener",
        Box::new(move || {
            Box::pin(async move {
                drop(supply);
                Ok(())
            })
        }),
    )
}
