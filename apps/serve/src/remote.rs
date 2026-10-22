//! Application entry that attaches to a Host-owned listener.
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi::application_services::RemoteServiceContract;
use rsi_application::{ApplicationError, ApplicationRun, ApplicationRunContract, RsiError};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_web_control::{Assets, Command, Reply, Request, decode_frame, encode_frame};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncReadExt as _;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Product application adapter; HTTP work remains in the selected Host.
#[derive(Debug)]
pub struct RemoteFactory {
    web: Option<Arc<crate::WebLaunch>>,
    arguments: Vec<OsString>,
}
impl RemoteFactory {
    /// Foreground authenticated Serve attachment.
    pub fn serve(arguments: Vec<OsString>) -> Self {
        Self {
            web: None,
            arguments,
        }
    }
    /// Short browser launch attachment.
    pub fn web(web: Arc<crate::WebLaunch>) -> Self {
        Self {
            web: Some(web),
            arguments: Vec::new(),
        }
    }
    fn command(&self, config: &ConfigValue) -> rsi::Result<Command> {
        if let Some(web) = &self.web {
            if !config.is_null() {
                return Err(RsiError::Boot("Web takes application arguments".into()));
            }
            let (port, assets, _) = web.requested()?;
            Ok(Command::Launch {
                port,
                assets: assets.map(|path| Assets::Directory { path }),
            })
        } else {
            let (config, assets) = if config.is_null() {
                let arguments = self
                    .arguments
                    .strip_prefix(&[OsString::from("--")])
                    .unwrap_or(&self.arguments);
                crate::arguments::parse_product(arguments)?
            } else {
                if !self.arguments.is_empty() {
                    return Err(RsiError::Boot(
                        "Serve accepts either configuration or arguments".into(),
                    ));
                }
                (
                    serde_json::from_value::<rsi_api_http::HttpConfig>(config.clone())
                        .map_err(|e| RsiError::Boot(e.to_string()))?,
                    None,
                )
            };
            config
                .validate()
                .map_err(|e| RsiError::Boot(e.to_string()))?;
            Ok(Command::Serve { config, assets })
        }
    }
}
impl rsi_application::ApplicationDiagnostic for RemoteFactory {
    fn take_diagnostic(&self) -> Option<RsiError> {
        self.web.as_ref().and_then(|web| web.take_diagnostic())
    }
}
#[async_trait]
impl PluginFactory for RemoteFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::with_state(
            ConfigValue::Null,
            self.command(config)
                .map_err(|e| MetaError::InvalidInput(e.to_string()))?,
            32 * 1024,
        )
        .requiring_local::<RemoteServiceContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let frontend = match self
            .web
            .as_ref()
            .and_then(|web| web.environment.frontend.clone())
        {
            Some(recipe) => Some(
                crate::FrontendRuntime::prepare(plan.context(), recipe)
                    .await
                    .map_err(|error| self.web.as_ref().expect("frontend Web").failure(error))?,
            ),
            None => None,
        };
        let runner = Arc::new(Runner {
            frontend,
            owner: plan.local::<RemoteServiceContract>()?,
            command: plan.take_state::<Command>()?,
            web: self.web.clone(),
            started: Mutex::new(false),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            execution: plan.context().runtime().execution().clone(),
        });
        let supply = plan
            .context()
            .provide_local::<ApplicationRunContract>(runner.clone())?;
        plan.defer(
            "close listener client attachment",
            Box::new(move || {
                Box::pin(async move {
                    {
                        let _started = runner.started.lock().expect("application entry");
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
}
#[derive(Debug)]
struct Runner {
    frontend: Option<crate::FrontendRuntime>,
    owner: Arc<rsi_service_host::HostOwnerMetadata>,
    command: Command,
    web: Option<Arc<crate::WebLaunch>>,
    started: Mutex<bool>,
    stop: CancellationToken,
    tasks: TaskTracker,
    execution: rsi_meta::Execution,
}
impl ApplicationRun for Runner {
    fn returns_after_launch(&self) -> bool {
        self.frontend.is_none() && matches!(&self.command, Command::Launch { .. })
    }
    fn run(self: Arc<Self>) -> BoxFuture<'static, rsi_application::Result<u8>> {
        let mut started = self.started.lock().expect("application entry");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(ApplicationError::ShuttingDown) });
        }
        if *started {
            return Box::pin(async { Err(ApplicationError::AlreadyStarted) });
        }
        *started = true;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let runner = self.clone();
        let task = self.tasks.track_future(async move {
            let result = runner.execute().await;
            let code = match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("error: {error}");
                    2
                }
            };
            let _ = sender.send(code);
        });
        drop(started);
        drop(self.execution.spawn(task));
        Box::pin(async move { receiver.await.map_err(|_| ApplicationError::TaskStopped) })
    }
}
/// Connects to an exact native Host identity and negotiates only Web control.
pub async fn request(
    owner: &rsi_service_host::HostOwnerMetadata,
    command: Command,
) -> Result<(Box<dyn rsi_service_host::LocalStream>, Reply), String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = rsi_service_host::connect_local_service(owner, "web")
            .await
            .map_err(|e| e.to_string())?;
        encode_frame(
            &mut stream,
            &Request {
                epoch: owner.host_epoch.clone(),
                protocol: rsi_web_control::protocol_descriptor(),
                command,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        let reply = decode_frame(&mut stream).await.map_err(|e| e.to_string())?;
        if let Reply::Error { message } = reply {
            return Err(message);
        }
        Ok((stream, reply))
    })
    .await
    .map_err(|_| "Web control readiness deadline elapsed".to_owned())?
}
impl Runner {
    async fn execute(&self) -> Result<(), String> {
        let mut frontend = if let Some(runtime) = &self.frontend {
            let Command::Launch { assets, .. } = &self.command else {
                return Err("frontend requires a Web launch".into());
            };
            let assets = assets.clone().unwrap_or_default();
            Some(tokio::select! {
                () = self.stop.cancelled() => return Ok(()),
                result = runtime.start(&assets) => result?,
            })
        } else {
            None
        };
        let result = self.execute_with_frontend(frontend.as_mut()).await;
        if let Some(frontend) = frontend {
            frontend.close().await;
        }
        result
    }
    async fn execute_with_frontend(
        &self,
        mut frontend: Option<&mut crate::FrontendSession>,
    ) -> Result<(), String> {
        let command = if let Some(frontend) = &frontend {
            let Command::Launch { assets, .. } = &self.command else {
                return Err("frontend requires a Web launch".into());
            };
            Command::Watch {
                assets: assets.clone().unwrap_or_default(),
                frontend_origin: frontend.origin().into(),
            }
        } else {
            self.command.clone()
        };
        let (mut stream, reply) = tokio::select! {
            () = self.stop.cancelled() => return Ok(()),
            result = request(&self.owner, command) => result?,
        };
        let Reply::Ready { address, ticket } = reply else {
            return Err("listener did not report readiness".into());
        };
        if let Some(web) = &self.web {
            let ticket = ticket.ok_or("missing launch ticket")?;
            let origin = if let Some(frontend) = frontend.as_mut() {
                tokio::select! {
                    () = self.stop.cancelled() => return Ok(()),
                    ready = frontend.configure(address) => ready?,
                }
                frontend.origin().to_owned()
            } else {
                format!("http://{address}")
            };
            let url = format!("{origin}#rsi-launch={ticket}");
            println!("rsi web: {url}");
            if web.requested().map_err(|e| e.to_string())?.2
                && let Some(opener) = &web.environment.opener
                && opener.open(&url).await.is_err()
            {
                eprintln!("Could not open the browser; use the launch link printed above.");
            }
            if let Some(frontend) = &frontend {
                let mut byte = [0];
                return tokio::select! {
                    () = self.stop.cancelled() => Ok(()),
                    () = frontend.stopped() => Err("frontend exited during watch".into()),
                    _ = stream.read(&mut byte) => Err("Host watch listener stopped".into()),
                };
            }
            return Ok(());
        }
        let origin = match &self.command {
            Command::Serve { config, .. } => config.public_origin.as_str(),
            _ => "",
        };
        println!(
            "{}",
            serde_json::json!({"event":"serving", "origin":origin, "bind":address.to_string()})
        );
        let mut signals = crate::service::Signals::new()?;
        let mut byte = [0];
        tokio::select! {
            () = self.stop.cancelled() => Ok(()),
            _ = signals.next(false) => Ok(()),
            _ = stream.read(&mut byte) => Err("Host listener stopped".into()),
        }
    }
}
