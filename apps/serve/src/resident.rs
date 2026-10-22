//! Ordinary Host plugin supervising independently leased HTTP listener children.
use async_trait::async_trait;
use rsi::application_services::{LocalBrowserAdministration, LocalBrowserAdministrationContract};
use rsi_api_http::{HttpAssetsContract, HttpListener, HttpListenerContract};
use rsi_api_protocol::{
    ApiDispatchContract, ConnectionDescriptionContract, DeviceAuthentication,
    DeviceAuthenticationContract,
};
use rsi_host::{HostPaths, ProfileEntry, ProfileFragment};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberHandle, FiberState, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, UpdateMode,
};
use rsi_service_host::{LocalServiceListener, LocalStream, ServiceOwnerContract};
use rsi_web_assets::WebAssetControlContract;
use rsi_web_control::{Assets, Command, Reply, Request, decode_frame, encode_frame};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

/// Interface offers of the linked native product, including its optional UI transport.
pub fn supported_protocols() -> Vec<rsi_api_protocol::ProtocolDescriptor> {
    let mut protocols = rsi::product_protocols();
    protocols.extend([
        rsi_web_control::protocol_descriptor(),
        rsi_web_assets_api::protocol_descriptor(),
    ]);
    protocols
}

/// Explicit application-layer contribution to the resident Service composition.
pub fn service_addons(paths: HostPaths) -> rsi::Result<rsi::StandardAddonSet> {
    let mut builder = rsi::StandardAddonBuilder::new("rsi.resident.web");
    builder
        .register_local_contract::<HttpAssetsContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<HttpListenerContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<WebAssetControlContract>()
        .map_err(boot)?;
    builder
        .register_linked(
            "rsi.service.web",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(ManagerFactory(paths)),
        )
        .map_err(boot)?;
    builder
        .register_fragment(ProfileFragment::new(
            "rsi.resident.web",
            [ProfileEntry::new(
                "rsi.service.web",
                "rsi.service.web",
                ConfigValue::Null,
            )],
        ))
        .map_err(boot)?;
    rsi::StandardAddonSet::new([builder.build().map_err(boot)?]).map_err(boot)
}
fn boot(error: impl std::fmt::Display) -> rsi::RsiError {
    rsi::RsiError::Boot(error.to_string())
}
fn failure(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[derive(Debug)]
struct ManagerFactory(HostPaths);
#[async_trait]
impl PluginFactory for ManagerFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(failure("Web manager takes no configuration"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ServiceOwnerContract>()
            .requiring_local::<ApiDispatchContract>()
            .requiring_local::<ConnectionDescriptionContract>()
            .requiring_local::<DeviceAuthenticationContract>()
            .requiring_local::<LocalBrowserAdministrationContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let listener = LocalServiceListener::bind(plan.local::<ServiceOwnerContract>()?, "web")
            .map_err(failure)?;
        let manager = Arc::new(Manager {
            context: plan.context().clone(),
            paths: self.0.clone(),
            epoch: plan
                .local::<ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            browser: tokio::sync::Mutex::new(None),
            creation: tokio::sync::Mutex::new(()),
            desktop_creation: tokio::sync::Mutex::new(()),
            generation: std::sync::atomic::AtomicU64::new(0),
            assets: std::sync::Mutex::new(Vec::new()),
            attachments: tokio::sync::Semaphore::new(16),
            administration: plan.local::<LocalBrowserAdministrationContract>()?,
            authentication: plan.local::<DeviceAuthenticationContract>()?,
        });
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let task = plan.context().runtime().execution().spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let stream = tokio::select! { biased;
                    () = stop.cancelled() => break,
                    _ = tasks.join_next(), if !tasks.is_empty() => continue,
                    stream = listener.accept() => stream,
                };
                let Ok(mut stream) = stream else {
                    tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(50)) => {} }
                    continue;
                };
                if tasks.len() >= 32 {
                    let _ = tokio::time::timeout(Duration::from_millis(100), encode_frame(&mut stream,
                        &Reply::Error { message: "Web control capacity exhausted".into() })).await;
                    continue;
                }
                let manager = manager.clone();
                let stop = stop.child_token();
                tasks.spawn(async move { manager.serve(stream, stop).await; });
            }
            // Each admitted creation finishes adoption before cancellation disposes it.
            while tasks.join_next().await.is_some() {}
            manager.stop().await.map_err(|e| e.to_string())
        });
        plan.defer(
            "drain resident Web listeners",
            Box::new(move || {
                Box::pin(async move {
                    drop(guard);
                    task.await.map_err(|_| "Web manager failed".to_owned())?
                })
            }),
        )
    }
}

struct Manager {
    attachments: tokio::sync::Semaphore,
    assets: std::sync::Mutex<Vec<std::sync::Weak<ListenerAssets>>>,
    context: Context,
    paths: HostPaths,
    epoch: rsi_api_protocol::HostEpoch,
    browser: tokio::sync::Mutex<Option<Listener>>,
    creation: tokio::sync::Mutex<()>,
    desktop_creation: tokio::sync::Mutex<()>,
    generation: std::sync::atomic::AtomicU64,
    administration: Arc<dyn LocalBrowserAdministration>,
    authentication: Arc<dyn DeviceAuthentication>,
}
struct Listener {
    _assets: Option<Arc<ListenerAssets>>,
    fiber: FiberHandle,
    http: Arc<dyn HttpListener>,
    source: Option<Assets>,
    requested_port: Option<u16>,
    tickets: Option<Arc<crate::tickets::Tickets>>,
}
struct ListenerAssets {
    source: Assets,
    control: Arc<rsi_web_assets::WebAssetControl>,
    package: tokio::sync::Mutex<Option<Arc<rsi_packages::PackageLease>>>,
}
impl Listener {
    fn ready(&self, ticket: Option<String>) -> Reply {
        Reply::Ready {
            address: self.http.address(),
            ticket,
        }
    }
    async fn close(self) -> rsi_meta::Result<()> {
        if let Some(tickets) = &self.tickets {
            tickets.clear();
        }
        let report = self.fiber.dispose().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(failure("listener cleanup failed"))
        }
    }
}
impl Manager {
    async fn desktop(&self, assets: Assets) -> rsi_meta::Result<Listener> {
        let _creation = self.desktop_creation.lock().await;
        let state = self.paths.state().to_owned();
        let identity =
            tokio::task::spawn_blocking(move || crate::desktop_port::DesktopPort::load(&state))
                .await
                .map_err(failure)?
                .map_err(|error| failure(format!("Desktop port identity: {error}")))?;
        let port = identity.port.map_or(0, std::num::NonZeroU16::get);
        let listener = self
            .start(Some(assets), Some(port), None, None)
            .await
            .map_err(|error| {
                failure(format!(
                    "Desktop listener on port {port}: {error}; close any other Desktop window or release the retained port"
                ))
            })?;
        if identity.port.is_none() {
            let bound = std::num::NonZeroU16::new(listener.http.address().port())
                .expect("bound TCP listener has a nonzero port");
            let published = tokio::task::spawn_blocking(move || identity.publish(bound))
                .await
                .map_err(failure)
                .and_then(|result| result.map_err(failure));
            if let Err(error) = published {
                listener.close().await?;
                return Err(failure(format!("Desktop port publication: {error}")));
            }
        }
        Ok(listener)
    }
    async fn acquire_package(
        &self,
        name: &str,
    ) -> rsi_meta::Result<Arc<rsi_packages::PackageLease>> {
        let path = self.paths.state().join("packages");
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            rsi_packages::PackageStore::open(&path)?
                .acquire(&name)
                .map(Arc::new)
        })
        .await
        .map_err(failure)?
        .map_err(failure)
    }
    async fn ticket(&self, listener: &Listener) -> rsi_meta::Result<String> {
        let device = self.administration.launch().await.map_err(failure)?;
        // A cached device must never resurrect a revoked or rotated credential.
        self.authentication
            .authenticate(&device.token)
            .map_err(failure)?;
        let ticket = listener
            .tickets
            .as_ref()
            .ok_or_else(|| failure("listener has no browser tickets"))?
            .mint(device.token)
            .map_err(failure)?;
        Ok(ticket.expose_secret().to_owned())
    }
    async fn launch(&self, port: Option<u16>, assets: Option<Assets>) -> rsi_meta::Result<Reply> {
        use futures_util::FutureExt as _;
        use std::sync::atomic::Ordering;
        let generation = self.generation.load(Ordering::Acquire);
        let _creation = self.creation.lock().await;
        if generation != self.generation.load(Ordering::Acquire) {
            return Err(failure("Web launch was stopped"));
        }
        let mut browser = self.browser.lock().await;
        if browser
            .as_ref()
            .is_some_and(|active| active.http.stopped().now_or_never().is_some())
            && let Some(stopped) = browser.take()
        {
            drop(browser);
            stopped.close().await?;
            browser = self.browser.lock().await;
        }
        if let Some(active) = browser.as_ref() {
            if port.is_some_and(|p| {
                Some(p) != active.requested_port && p != active.http.address().port()
            }) || assets
                .as_ref()
                .is_some_and(|source| Some(source) != active.source.as_ref())
            {
                return Err(failure(
                    "Web is already running with another port or source; use rsi web stop first",
                ));
            }
        } else {
            drop(browser);
            let listener = self
                .start(
                    Some(assets.unwrap_or_default()),
                    Some(port.unwrap_or(8787)),
                    None,
                    None,
                )
                .await?;
            let ticket = match self.ticket(&listener).await {
                Ok(ticket) => ticket,
                Err(error) => {
                    listener.close().await?;
                    return Err(error);
                }
            };
            browser = self.browser.lock().await;
            if generation != self.generation.load(Ordering::Acquire) {
                drop(browser);
                listener.close().await?;
                return Err(failure("Web launch was stopped"));
            }
            let ready = listener.ready(Some(ticket));
            *browser = Some(listener);
            return Ok(ready);
        }
        let active = browser.as_ref().expect("adopted listener");
        let ticket = self.ticket(active).await?;
        if generation != self.generation.load(Ordering::Acquire) {
            return Err(failure("Web launch was stopped"));
        }
        Ok(active.ready(Some(ticket)))
    }
    async fn stop(&self) -> rsi_meta::Result<()> {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // Creation owns cleanup until it observes the fence. Status stays free.
        let _creation = self.creation.lock().await;
        let listener = self.browser.lock().await.take();
        if let Some(listener) = listener {
            listener.close().await?;
        }
        Ok(())
    }
    async fn status(&self) -> rsi_meta::Result<Reply> {
        use futures_util::FutureExt as _;
        let mut browser = self.browser.lock().await;
        if browser
            .as_ref()
            .is_some_and(|active| active.http.stopped().now_or_never().is_some())
        {
            let stopped = browser.take().expect("observed listener");
            drop(browser);
            stopped.close().await?;
            return Ok(Reply::Stopped {});
        }
        Ok(browser
            .as_ref()
            .map_or(Reply::Stopped {}, |active| active.ready(None)))
    }
    async fn refresh(&self, package: &str) -> rsi_meta::Result<Reply> {
        let listeners = {
            let mut assets = self.assets.lock().expect("listener assets");
            assets.retain(|value| value.strong_count() != 0);
            assets
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .collect::<Vec<_>>()
        };
        let mut restart_required = false;
        for active in listeners {
            let Assets::Package {
                package: name,
                export,
            } = &active.source
            else {
                continue;
            };
            if name != package {
                continue;
            }
            let mut retained = active.package.lock().await;
            let lease = self.acquire_package(name).await?;
            if retained
                .as_ref()
                .is_some_and(|old| old.digest() == lease.digest())
            {
                continue;
            }
            lease
                .manifest()
                .admit_interfaces(&supported_protocols())
                .map_err(failure)?;
            let directory = lease
                .export(export, rsi_packages::ExportKind::WebAssets)
                .map_err(failure)?;
            let previous = active.control.revision().map_err(failure)?;
            let candidate = active
                .control
                .stage_verified(directory)
                .map_err(failure)?
                .wait()
                .await
                .map_err(failure)?;
            match candidate.publish(&previous) {
                Ok(_) => {
                    *retained = Some(lease);
                }
                Err(rsi_web_assets::AssetError::RestartRequired) => {
                    restart_required = true;
                }
                Err(error) => return Err(failure(error)),
            }
        }
        Ok(Reply::Refreshed { restart_required })
    }
    async fn start(
        &self,
        source: Option<Assets>,
        port: Option<u16>,
        config: Option<rsi_api_http::HttpConfig>,
        frontend_origin: Option<rsi_api_http::LoopbackBrowserOrigin>,
    ) -> rsi_meta::Result<Listener> {
        let (directory, package) = match &source {
            Some(Assets::Directory { path })
                if path.is_absolute() && path.as_os_str().len() <= 16 * 1024 =>
            {
                (Some(path.clone()), None)
            }
            Some(Assets::Directory { .. }) => {
                return Err(failure("assets require an absolute bounded directory"));
            }
            Some(Assets::Package { package, export }) => {
                let lease = self.acquire_package(package).await?;
                lease
                    .manifest()
                    .admit_interfaces(&supported_protocols())
                    .map_err(|_| failure("Web package requires an unsupported wire interface"))?;
                (
                    Some(
                        lease
                            .export(export, rsi_packages::ExportKind::WebAssets)
                            .map_err(failure)?,
                    ),
                    Some(lease),
                )
            }
            None => (None, None),
        };
        let context = self
            .context
            .clone()
            .isolate_local_fresh::<HttpListenerContract>()?
            .0
            .isolate_local_fresh::<HttpAssetsContract>()?
            .0
            .isolate_local_fresh::<WebAssetControlContract>()?
            .0;
        let tickets = source.as_ref().filter(|_| config.is_none()).map(|_| {
            Arc::new(crate::tickets::Tickets::new(
                context.runtime().execution().clone(),
            ))
        });
        let fiber = context
            .apply(
                linked(
                    "rsi.web.listener",
                    Arc::new(ListenerFactory {
                        directory,
                        _package: package.clone(),
                        tickets: tickets.clone(),
                        port,
                        config,
                        frontend_origin,
                    }),
                ),
                ConfigValue::Null,
            )
            .await?;
        if fiber.snapshot().state != FiberState::Active {
            let state = fiber.snapshot().state;
            fiber.dispose().await;
            return Err(failure(format!(
                "HTTP listener activation failed: {state:?}; check Web assets and requested port"
            )));
        }
        let Some(http) = context.lookup_local::<HttpListenerContract>() else {
            fiber.dispose().await;
            return Err(failure("listener did not publish readiness"));
        };
        let assets = source
            .as_ref()
            .map(|source| -> rsi_meta::Result<_> {
                let assets = Arc::new(ListenerAssets {
                    source: source.clone(),
                    control: context
                        .lookup_local::<WebAssetControlContract>()
                        .ok_or_else(|| failure("missing asset publication owner"))?,
                    package: tokio::sync::Mutex::new(package),
                });
                let mut retained = self.assets.lock().expect("listener assets");
                retained.retain(|value| value.strong_count() != 0);
                retained.push(Arc::downgrade(&assets));
                Ok(assets)
            })
            .transpose()?;
        Ok(Listener {
            _assets: assets,
            fiber,
            http,
            source,
            requested_port: port,
            tickets,
        })
    }
    async fn serve(&self, mut stream: Box<dyn LocalStream>, stop: CancellationToken) {
        let request = tokio::select! {
            () = stop.cancelled() => return,
            result = tokio::time::timeout(Duration::from_secs(3), decode_frame::<Request>(&mut stream)) => result,
        };
        let Ok(Ok(request)) = request else {
            return;
        };
        let _attachment_slot = if matches!(
            request.command,
            Command::Serve { .. } | Command::Desktop { .. } | Command::Watch { .. }
        ) {
            if let Ok(slot) = self.attachments.try_acquire() {
                Some(slot)
            } else {
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    encode_frame(
                        &mut stream,
                        &Reply::Error {
                            message: "Web attachment capacity exhausted".into(),
                        },
                    ),
                )
                .await;
                return;
            }
        } else {
            None
        };
        let mut attachment = None;
        let result = if request.epoch != self.epoch
            || request.protocol != rsi_web_control::protocol_descriptor()
        {
            Err(failure("Web control Host generation or interface differs"))
        } else {
            match request.command {
                Command::Launch { port, assets } => self.launch(port, assets).await,
                Command::Stop {} => self.stop().await.map(|()| Reply::Stopped {}),
                Command::Status {} => self.status().await,
                Command::Refresh { package } => self.refresh(&package).await,
                Command::Serve { config, assets } => {
                    match self.start(assets, None, Some(config), None).await {
                        Ok(listener) => {
                            let reply = listener.ready(None);
                            attachment = Some(listener);
                            Ok(reply)
                        }
                        Err(error) => Err(error),
                    }
                }
                Command::Desktop { assets } => match self.desktop(assets).await {
                    Ok(listener) => {
                        let reply = self
                            .ticket(&listener)
                            .await
                            .map(|ticket| listener.ready(Some(ticket)));
                        attachment = Some(listener);
                        reply
                    }
                    Err(error) => Err(error),
                },
                Command::Watch {
                    assets,
                    frontend_origin,
                } => match rsi_api_http::LoopbackBrowserOrigin::new(frontend_origin) {
                    Err(error) => Err(failure(error)),
                    Ok(origin) => match self.start(Some(assets), Some(0), None, Some(origin)).await
                    {
                        Ok(listener) => {
                            let reply = self
                                .ticket(&listener)
                                .await
                                .map(|ticket| listener.ready(Some(ticket)));
                            attachment = Some(listener);
                            reply
                        }
                        Err(error) => Err(error),
                    },
                },
            }
        };
        let reply = result.unwrap_or_else(|error| Reply::Error {
            message: error.to_string().chars().take(2048).collect(),
        });
        let sent =
            tokio::time::timeout(Duration::from_secs(3), encode_frame(&mut stream, &reply)).await;
        if let Some(listener) = attachment {
            if matches!(sent, Ok(Ok(()))) && matches!(reply, Reply::Ready { .. }) {
                let mut byte = [0];
                tokio::select! { () = stop.cancelled() => {}, _ = stream.read(&mut byte) => {}, _ = listener.http.stopped() => {} }
            }
            let _ = listener.close().await;
        }
    }
}
fn linked(id: &str, factory: Arc<dyn PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(
        id,
        env!("CARGO_PKG_VERSION"),
        UpdateMode::RestartRequired,
        factory,
    )
}
#[derive(Debug)]
struct ListenerFactory {
    directory: Option<PathBuf>,
    _package: Option<Arc<rsi_packages::PackageLease>>,
    tickets: Option<Arc<crate::tickets::Tickets>>,
    port: Option<u16>,
    config: Option<rsi_api_http::HttpConfig>,
    frontend_origin: Option<rsi_api_http::LoopbackBrowserOrigin>,
}
#[async_trait]
impl PluginFactory for ListenerFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if let Some(config) = &self.config {
            config.validate().map_err(failure)?;
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiDispatchContract>()
            .requiring_local::<ConnectionDescriptionContract>()
            .requiring_local::<DeviceAuthenticationContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let mut context = plan.context().clone();
        if let Some(directory) = &self.directory {
            apply(
                &context,
                "rsi.web.assets",
                Arc::new(rsi_web_assets::VerifiedWebAssetsFactory),
                serde_json::json!({"directory": directory}),
            )
            .await?;
            let assets = context
                .lookup_local::<WebAssetControlContract>()
                .ok_or_else(|| failure("missing assets owner"))?;
            let factory = crate::web::WebApiFactory {
                service: plan.local::<ApiDispatchContract>()?,
                description: plan.local::<ConnectionDescriptionContract>()?,
                assets,
            };
            context = context.isolate_local_fresh::<ApiDispatchContract>()?.0;
            apply(
                &context,
                "rsi.web.api",
                Arc::new(factory),
                ConfigValue::Null,
            )
            .await?;
        }
        let (factory, config): (Arc<dyn PluginFactory>, _) = match &self.tickets {
            Some(tickets) => {
                let mut factory = rsi_api_http::LocalBrowserHttpFactory::new(tickets.clone());
                if let Some(origin) = &self.frontend_origin {
                    factory = factory.with_frontend_origin(origin.clone());
                }
                (Arc::new(factory), serde_json::json!(self.port.unwrap_or(0)))
            }
            None => (
                if self.directory.is_some() {
                    Arc::new(rsi_api_http::StaticHttpFactory)
                } else {
                    Arc::new(rsi_api_http::HttpFactory)
                },
                serde_json::to_value(&self.config).map_err(failure)?,
            ),
        };
        apply(&context, "rsi.web.http", factory, config).await
    }
}
async fn apply(
    context: &Context,
    id: &str,
    factory: Arc<dyn PluginFactory>,
    config: ConfigValue,
) -> rsi_meta::Result<()> {
    let child = context.apply(linked(id, factory), config).await?;
    if child.snapshot().state == FiberState::Active {
        Ok(())
    } else {
        Err(failure(format!(
            "{id} activation failed: {:?}",
            child.snapshot().state
        )))
    }
}
