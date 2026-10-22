//! Ordinary Application plugin for an admitted executable export.
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi::application_services::RemoteServiceContract;
use rsi_application::{ApplicationError, ApplicationRun, ApplicationRunContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug)]
pub(super) struct Factory {
    pub paths: rsi_host::HostPaths,
    pub arguments: Vec<OsString>,
    pub frontend: Option<rsi_serve::FrontendRecipe>,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    package: String,
    export: String,
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(config.clone()).map_err(invalid)?;
        for value in [&config.package, &config.export] {
            rsi_packages::validate_identifier(value).map_err(invalid)?;
        }
        Ok(
            PreparedActivation::with_state(ConfigValue::Null, config, 512)
                .requiring_local::<RemoteServiceContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        // The Service plugin has already attached to the selected shared Host.
        let _owner = plan.local::<RemoteServiceContract>()?;
        let config = plan.take_state::<Config>()?;
        let packages = self.paths.state().join("packages");
        let package = config.package.clone();
        let lease = tokio::task::spawn_blocking(move || {
            rsi_packages::PackageStore::open(&packages)?.acquire(&package)
        })
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        lease
            .manifest()
            .admit_interfaces(&rsi_serve::supported_protocols())
            .map_err(invalid)?;
        let executable = lease
            .export(&config.export, rsi_packages::ExportKind::Executable)
            .map_err(invalid)?;
        let frontend = match &self.frontend {
            Some(recipe) if config.package == "desktop" => {
                Some(rsi_serve::FrontendRuntime::prepare(plan.context(), recipe.clone()).await?)
            }
            _ => None,
        };
        let runner = Arc::new(Runner {
            frontend,
            owner: _owner,
            executable,
            export: config.export,
            lease: Arc::new(lease),
            paths: self.paths.clone(),
            arguments: self
                .arguments
                .strip_prefix(&[OsString::from("--")])
                .unwrap_or(&self.arguments)
                .to_vec(),
            environment: rsi::scrub_child_environment(std::env::vars_os()),
            started: Mutex::new(false),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            execution: plan.context().runtime().execution().clone(),
        });
        let supply = plan
            .context()
            .provide_local::<ApplicationRunContract>(runner.clone())?;
        plan.defer(
            "drain installed executable attachment",
            Box::new(move || {
                Box::pin(async move {
                    {
                        let _started = runner.started.lock().expect("executable entry");
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
fn invalid(value: impl std::fmt::Display) -> MetaError {
    MetaError::InvalidInput(value.to_string())
}
#[derive(Debug)]
struct Runner {
    frontend: Option<rsi_serve::FrontendRuntime>,
    owner: Arc<rsi_service_host::HostOwnerMetadata>,
    executable: std::path::PathBuf,
    export: String,
    lease: Arc<rsi_packages::PackageLease>,
    paths: rsi_host::HostPaths,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    started: Mutex<bool>,
    stop: CancellationToken,
    tasks: TaskTracker,
    execution: rsi_meta::Execution,
}
impl ApplicationRun for Runner {
    fn run(self: Arc<Self>) -> BoxFuture<'static, rsi_application::Result<u8>> {
        let mut started = self.started.lock().expect("executable entry");
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
            let result = runner.execute().await.unwrap_or_else(|error| {
                eprintln!("error: {error}");
                2
            });
            let _ = sender.send(result);
        });
        drop(started);
        drop(self.execution.spawn(task));
        Box::pin(async move { receiver.await.map_err(|_| ApplicationError::TaskStopped) })
    }
}
impl Runner {
    async fn execute(&self) -> Result<u8, String> {
        let mut frontend = if let Some(runtime) = &self.frontend {
            let mut assets = rsi_web_control::Assets::default();
            if let Some(index) = self.arguments.iter().position(|arg| arg == "--assets") {
                let path = std::path::PathBuf::from(
                    self.arguments
                        .get(index + 1)
                        .ok_or("missing assets directory")?,
                );
                assets = rsi_web_control::Assets::Directory {
                    path: rsi::normalize_launch_path(
                        &path,
                        &std::env::current_dir().map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?,
                };
            }
            Some(tokio::select! {
                () = self.stop.cancelled() => return Ok(0),
                result = runtime.start(&assets) => {
                    let session = result?;
                    (session, assets)
                },
            })
        } else {
            None
        };
        let result = self.execute_with_frontend(frontend.as_mut()).await;
        if let Some((session, _)) = frontend {
            session.close().await;
        }
        result
    }
    async fn execute_with_frontend(
        &self,
        mut frontend: Option<&mut (rsi_serve::FrontendSession, rsi_web_control::Assets)>,
    ) -> Result<u8, String> {
        let mut listener = None;
        let launch = if let Some((session, assets)) = frontend.as_mut() {
            let (stream, reply) = rsi_serve::request_listener(
                &self.owner,
                rsi_web_control::Command::Watch {
                    assets: assets.clone(),
                    frontend_origin: session.origin().into(),
                },
            )
            .await?;
            let rsi_web_control::Reply::Ready {
                address,
                ticket: Some(ticket),
            } = reply
            else {
                return Err("Host did not report a watch listener".into());
            };
            session.configure(address).await?;
            listener = Some(stream);
            Some(rsi_web_control::DesktopLaunch {
                url: format!("{}#rsi-launch={ticket}", session.origin()),
            })
        } else {
            None
        };
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .env_clear()
            .envs(self.environment.clone())
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true);
        for (flag, path) in [
            ("--config-home", self.paths.config()),
            ("--state-home", self.paths.state()),
            ("--cache-home", self.paths.cache()),
        ] {
            command.arg(flag).arg(path);
        }
        command
            .arg("--package")
            .arg(&self.lease.manifest().name)
            .arg("--package-digest")
            .arg(self.lease.digest())
            .arg("--export")
            .arg(&self.export);
        command.args(&self.arguments);
        if launch.is_some() {
            command.arg("--frontend");
        }
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let mut control = child.stdin.take();
        if let Some(launch) = launch {
            rsi_web_control::encode_frame(
                control.as_mut().ok_or("Desktop control pipe missing")?,
                &launch,
            )
            .await
            .map_err(|_| "Desktop launch handoff failed")?;
        }
        let mut byte = [0];
        let mut frontend_failed = false;
        let status = tokio::select! {
            status = child.wait() => status,
            failed = async {
                use tokio::io::AsyncReadExt as _;
                tokio::select! {
                    () = self.stop.cancelled() => false,
                    () = async {
                        if let Some((session,_)) = &frontend { session.stopped().await; }
                        else { std::future::pending::<()>().await; }
                    } => true,
                    () = async {
                        if let Some(stream) = &mut listener { let _ = stream.read(&mut byte).await; }
                        else { std::future::pending::<()>().await; }
                    } => true,
                }
            } => {
                frontend_failed = failed;
                drop(control);
                if let Ok(status) = tokio::time::timeout(Duration::from_secs(35), child.wait()).await {
                    status
                } else {
                    child.kill().await.map_err(|e| e.to_string())?;
                    child.wait().await
                }
            },
        }.map_err(|e| e.to_string())?;
        // The executable and exact dependency closure stay leased through reaping.
        let _ = &self.lease;
        if frontend_failed {
            return Err("Desktop frontend or Host listener stopped".into());
        }
        Ok(status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1))
    }
}
