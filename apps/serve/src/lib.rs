//! Native authenticated HTTP application over independently owned service capabilities.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod arguments;
mod local_web;
pub use local_web::HELP as WEB_HELP;
pub use local_web::{
    BrowserOpener, LocalWebAssetsFactory, SystemBrowserOpener, WebLaunch, WebLaunchEnvironment,
};
mod service;
mod web;
use rsi::application_services::{
    LocalBrowserAdministrationContract, ServingService, ServingServiceContract,
};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_http::{HttpConfig, HttpFactory, HttpListener, HttpListenerContract};
use rsi_api_protocol::{
    ApiDispatchContract, ConnectionDescriptionContract, DeviceAuthenticationContract,
};
use rsi_application::{ApplicationError, ApplicationRun, ApplicationRunContract, RsiError};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, FiberState, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, UpdateMode,
};
use std::{
    ffi::OsString,
    io::Write as _,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Pure launcher help; displaying it never initializes an application.
pub const HELP: &str = "Serve: --profile serve --bind ADDRESS --origin ORIGIN\n\
  --tls-certificate FILE --tls-key FILE   Production TLS\n\
  --dev-http                            Explicit loopback HTTP development\n";

/// Ordinary owner of HTTP application configuration, signals and its single entry.
#[derive(Debug)]
pub struct ServeFactory {
    web_assets: bool,
    local_web: Option<Arc<WebLaunch>>,
    arguments: Vec<OsString>,
    diagnostic: Mutex<Option<RsiError>>,
}
impl ServeFactory {
    /// Freezes arguments without opening files, sockets or services.
    pub fn new(arguments: Vec<OsString>) -> Self {
        Self {
            web_assets: false,
            local_web: None,
            arguments,
            diagnostic: Mutex::new(None),
        }
    }
    /// Selects an independently composed immutable Web asset capability.
    pub fn with_web_assets(arguments: Vec<OsString>) -> Self {
        Self {
            web_assets: true,
            ..Self::new(arguments)
        }
    }
    /// Selects the local one-command Web entry and its frozen invocation.
    pub fn local_web(launch: Arc<WebLaunch>) -> Self {
        Self {
            web_assets: true,
            local_web: Some(launch),
            ..Self::new(Vec::new())
        }
    }
    /// Takes this owner's bounded diagnostic; generic Profile failures stay redacted.
    pub fn take_diagnostic(&self) -> Option<RsiError> {
        self.diagnostic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    fn failure(&self, error: impl std::fmt::Display) -> MetaError {
        let mut message = error.to_string();
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        *self
            .diagnostic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(RsiError::Boot(message.clone()));
        MetaError::InvalidInput(message)
    }
    fn configuration(&self, desired: &ConfigValue) -> Result<HttpConfig, RsiError> {
        if let Some(web) = &self.local_web {
            if !desired.is_null() {
                return Err(RsiError::Boot(
                    "local Web takes application arguments".into(),
                ));
            }
            let options = web.options()?;
            return Ok(HttpConfig {
                bind: ([127, 0, 0, 1], options.port).into(),
                public_origin: "http://127.0.0.1".into(),
                tls: None,
                allow_loopback_http: true,
            });
        }
        if desired.is_null() {
            return arguments::parse(&self.arguments);
        }
        if !self.arguments.is_empty() {
            return Err(RsiError::Boot(
                "Serve accepts either Profile HTTP configuration or arguments, not both".into(),
            ));
        }
        let config: HttpConfig = serde_json::from_value(desired.clone())
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        config
            .validate()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        Ok(config)
    }
}
#[async_trait]
impl PluginFactory for ServeFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = self
            .configuration(desired)
            .map_err(|error| self.failure(error))?;
        let encoded = serde_json::to_vec(&config).map_err(|error| self.failure(error))?;
        if encoded.len() > 64 * 1024 {
            return Err(self.failure("Serve HTTP configuration exceeds 64 KiB"));
        }
        let mut prepared =
            PreparedActivation::with_state(ConfigValue::Null, config, encoded.len() + 4096)
                .requiring_local::<ApiDispatchContract>()
                .requiring_local::<DeviceAuthenticationContract>()
                .requiring_local::<ConnectionDescriptionContract>()
                .requiring_local::<ServingServiceContract>();
        if self.local_web.is_some() {
            prepared = prepared.requiring_local::<LocalBrowserAdministrationContract>();
        }
        Ok(if self.web_assets {
            prepared
                .requiring_local::<rsi_api_http::HttpAssetsContract>()
                .requiring_local::<rsi_web_assets::WebAssetControlContract>()
        } else {
            prepared
        })
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<HttpConfig>()?;
        let mut origin = config.public_origin.clone();
        let context = plan
            .context()
            .clone()
            .isolate_local_fresh::<HttpListenerContract>()?
            .0;
        let context = if self.web_assets {
            web::prepare(&mut plan, context).await?
        } else {
            context
        };
        let mut launch_link = None;
        let mut opener = None;
        let mut local_listener = None;
        let factory: Arc<dyn PluginFactory> = if let Some(web) = &self.local_web {
            let options = web.options().map_err(|error| self.failure(error))?;
            let device = plan
                .local::<LocalBrowserAdministrationContract>()?
                .launch()
                .await
                .map_err(|error| self.failure(error))?;
            let (ticket, secret) =
                local_web::Ticket::new(context.runtime().execution(), device.token)
                    .map_err(|error| self.failure(error))?;
            let cleanup = ticket.clone();
            plan.defer(
                "retire Web launch ticket",
                Box::new(move || {
                    Box::pin(async move {
                        cleanup.close();
                        Ok(())
                    })
                }),
            )?;
            launch_link = Some(secret);
            if options.open {
                opener = web.environment.opener.clone();
            }
            let factory = Arc::new(rsi_api_http::LocalBrowserHttpFactory::new(ticket));
            local_listener = Some(factory.clone());
            factory
        } else if self.web_assets {
            Arc::new(rsi_api_http::StaticHttpFactory)
        } else {
            Arc::new(HttpFactory)
        };
        let listener = context
            .apply(
                ResolvedFactory::linked(
                    "rsi.serve.http",
                    env!("CARGO_PKG_VERSION"),
                    UpdateMode::RestartRequired,
                    factory,
                ),
                if self.local_web.is_some() {
                    serde_json::json!(config.bind.port())
                } else {
                    serde_json::to_value(config).map_err(|error| self.failure(error))?
                },
            )
            .await?;
        if listener.snapshot().state != FiberState::Active {
            let state = listener.snapshot().state;
            let report = listener.dispose().await;
            let detail = local_listener
                .and_then(|factory| factory.take_diagnostic())
                .unwrap_or_else(|| format!("HTTP listener activation failed: {state:?}"));
            return Err(self.failure(format!(
                "{detail}; {} cleanup failures",
                report.total_failures()
            )));
        }
        let listener = context
            .lookup_local::<HttpListenerContract>()
            .ok_or_else(|| self.failure("HTTP listener did not publish its capability"))?;
        if self.local_web.is_some() {
            origin = if listener.address().port() == 80 {
                "http://127.0.0.1".into()
            } else {
                format!("http://{}", listener.address())
            };
        }
        let runner = Arc::new(Runner {
            listener,
            origin,
            launch_link: Mutex::new(launch_link),
            opener,
            service: plan.local::<ServingServiceContract>()?,
            started: Mutex::new(false),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            execution: context.runtime().execution().clone(),
        });
        publish_runner(&plan, runner)
    }
}

fn publish_runner(plan: &ActivationPlan, runner: Arc<Runner>) -> rsi_meta::Result<()> {
    let observation = plan
        .context()
        .provide_local::<HttpListenerContract>(runner.listener.clone())?;
    plan.defer(
        "withdraw Serve listener observation",
        Box::new(move || {
            Box::pin(async move {
                drop(observation);
                Ok(())
            })
        }),
    )?;
    let supply = plan
        .context()
        .provide_local::<ApplicationRunContract>(runner.clone())?;
    plan.defer(
        "withdraw Serve application",
        Box::new(move || {
            Box::pin(async move {
                {
                    let _started = runner.started.lock().expect("Serve entry poisoned");
                    runner.stop.cancel();
                    runner.tasks.close();
                }
                drop(supply);
                runner.tasks.wait().await;
                Ok(())
            })
        }),
    )
}

#[derive(Debug)]
struct Runner {
    listener: Arc<dyn HttpListener>,
    service: Arc<dyn ServingService>,
    origin: String,
    launch_link: Mutex<Option<rsi_credentials_protocol::SecretValue>>,
    opener: Option<Arc<dyn BrowserOpener>>,
    started: Mutex<bool>,
    stop: CancellationToken,
    tasks: TaskTracker,
    execution: Execution,
}
impl ApplicationRun for Runner {
    fn run(self: Arc<Self>) -> BoxFuture<'static, rsi_application::Result<u8>> {
        let mut started = self.started.lock().expect("Serve entry poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(ApplicationError::ShuttingDown) });
        }
        if *started {
            return Box::pin(async { Err(ApplicationError::AlreadyStarted) });
        }
        *started = true;
        let runner = self.clone();
        let (sender, receiver) = oneshot::channel();
        let work = self.tasks.track_future(async move {
            let _ = sender.send(runner.serve().await);
        });
        drop(started);
        drop(self.execution.spawn(work));
        Box::pin(async move { receiver.await.map_err(|_| ApplicationError::TaskStopped) })
    }
}
impl Runner {
    async fn serve(&self) -> u8 {
        if self.stop.is_cancelled() {
            return 0;
        }
        let result: Result<(), String> = async {
            let mut signals = service::Signals::new()?;
            let mut reloading: Option<BoxFuture<'static, Result<(), String>>> = None;
            let launch = self.launch_link.lock().expect("launch link poisoned").take();
            let open = async {
                if let Some(ticket) = launch {
                    let url = format!("{}#rsi-launch={}", self.origin, ticket.expose_secret());
                    writeln!(std::io::stdout(), "rsi web: {url}").map_err(|error| error.to_string())?;
                    if let Some(opener) = &self.opener && opener.open(&url).await.is_err() {
                        let _ = writeln!(std::io::stderr(), "Could not open the browser; use the launch link printed above.");
                    }
                } else {
                    writeln!(std::io::stdout(), "{}", serde_json::json!({"event":"serving", "origin":self.origin, "bind":self.listener.address().to_string()})).map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            };
            tokio::pin!(open);
            let mut opening = true;
            loop {
                tokio::select! { biased;
                    () = self.stop.cancelled() => return Ok(()),
                    result = &mut open, if opening => { result?; opening = false; },
                    event = signals.next(reloading.is_none()) => match event {
                        service::SignalEvent::Stop => return Ok(()),
                        service::SignalEvent::Reload => {
                            let service = self.service.clone();
                            reloading = Some(Box::pin(async move { service.reload().await }));
                        },
                    },
                    result = self.listener.stopped() => return result.map_err(|error| error.to_string()),
                    result = self.service.stopped() => return result.and(Err("selected service stopped".into())),
                    result = reload_finished(&mut reloading) => {
                        if let Err(error) = result { let _ = writeln!(std::io::stderr(), "Service reload failed: {error}"); }
                        reloading = None;
                    },
                }
            }
        }.await;
        match result {
            Ok(()) => 0,
            Err(error) => {
                let _ = writeln!(std::io::stderr(), "Serve failed: {error}");
                1
            }
        }
    }
}

async fn reload_finished(
    reloading: &mut Option<BoxFuture<'static, Result<(), String>>>,
) -> Result<(), String> {
    match reloading {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

impl rsi_application::ApplicationDiagnostic for ServeFactory {
    fn take_diagnostic(&self) -> Option<RsiError> {
        ServeFactory::take_diagnostic(self)
    }
}
impl rsi_application::ApplicationDiagnostic for LocalWebAssetsFactory {
    fn take_diagnostic(&self) -> Option<RsiError> {
        self.0.take_diagnostic()
    }
}
