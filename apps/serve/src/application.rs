//! An explicitly selected document and its leased application process.
use async_trait::async_trait;
use rsi_credentials_protocol::SecretValue;
use rsi_extension::ExtensionContract;
use rsi_host::HostPaths;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_web_control::Assets;
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

#[cfg(test)]
mod lease_tests;
mod model_catalog;
mod session_command;
mod session_commands;
mod session_controls;
mod session_create;
mod session_history;
mod session_inbox;
mod session_inherited;
mod session_interactions;
mod session_model;
mod session_page;
mod session_tree;
mod tools;
mod workspaces;

#[derive(Debug)]
pub(crate) struct Application {
    pub source: Assets,
    pub upstream: Arc<rsi_api_http::ApplicationRoutes>,
}
#[derive(Debug)]
pub(crate) struct ApplicationContract;
impl LocalContract for ApplicationContract {
    const KEY: &'static str = "rsi.web.application";
    type Service = Application;
}
#[derive(Debug)]
pub(crate) struct Factory(pub HostPaths);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    source: Assets,
    profile: String,
    process: ConfigValue,
    importer: Option<Importer>,
    #[serde(default)]
    tools: bool,
    #[serde(default)]
    replace_preset_tools: std::collections::BTreeSet<String>,
    #[serde(default)]
    routes: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Importer {
    package: String,
    digest: String,
    export: String,
    program: PathBuf,
}
#[derive(Debug)]
struct Imports {
    profile: String,
    selection: Importer,
    workspace: PathBuf,
    store: Arc<rsi_packages::PackageStore>,
    lane: tokio::sync::Mutex<()>,
    leases: tokio::sync::Mutex<std::collections::BTreeMap<String, Arc<rsi_packages::PackageLease>>>,
}
fn register_methods(
    connection: &rsi_extension::Connection,
    handler: &Arc<dyn rsi_extension::Handler>,
    methods: &[&str],
) -> rsi_meta::Result<Vec<rsi_extension::HandlerRegistration>> {
    methods
        .iter()
        .map(|method| {
            connection
                .register_handler(method, handler.clone())
                .map_err(failure)
        })
        .collect()
}
impl Imports {
    fn attach(
        connection: &rsi_extension::Connection,
        profile: String,
        selection: Importer,
        workspace: PathBuf,
        store: Arc<rsi_packages::PackageStore>,
    ) -> rsi_meta::Result<Vec<rsi_extension::HandlerRegistration>> {
        let imports = Arc::new(Self {
            profile,
            selection,
            workspace,
            store,
            lane: tokio::sync::Mutex::new(()),
            leases: tokio::sync::Mutex::new(std::collections::BTreeMap::new()),
        });
        connection.retain_until_settled(imports.clone());
        let handler: Arc<dyn rsi_extension::Handler> = imports;
        register_methods(
            connection,
            &handler,
            &[
                "application.import",
                "application.payloads",
                "application.release",
                "application.roots",
            ],
        )
    }
    async fn payloads(
        &self,
        payload: ConfigValue,
        stop: &tokio_util::sync::CancellationToken,
    ) -> rsi_extension::Result<ConfigValue> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            package: String,
            digest: String,
            export: String,
            dependency_export: String,
        }
        let input: Request =
            serde_json::from_value(payload).map_err(|_| rsi_extension::Error::Protocol)?;
        let mut leases = self.leases.lock().await;
        let lease = if let Some(lease) = leases.get(&input.digest) {
            lease.clone()
        } else {
            if leases.len() >= 32 {
                return Err(rsi_extension::Error::Capacity);
            }
            let store = self.store.clone();
            let package = input.package.clone();
            let digest = input.digest.clone();
            tokio::task::spawn_blocking(move || store.acquire_for_process(&package, &digest))
                .await
                .map_err(|_| rsi_extension::Error::Disconnected)?
                .map_err(|error| package_error(&error))?
        };
        if lease.manifest().name != input.package {
            return Err(rsi_extension::Error::Protocol);
        }
        let profile = lease
            .payload(&input.export)
            .map_err(|error| package_error(&error))?;
        let mut roots = std::collections::BTreeMap::new();
        for dependency in &lease.manifest().dependencies {
            let held = lease
                .dependency_payload(&dependency.name, &input.dependency_export)
                .map_err(|error| package_error(&error))?;
            roots.insert(&dependency.name, held.path().to_path_buf());
        }
        if stop.is_cancelled() {
            return Err(rsi_extension::Error::Cancelled);
        }
        let reply = serde_json::json!({"profile":profile.path(),"roots":roots});
        leases.insert(input.digest, lease);
        Ok(reply)
    }
    async fn release(&self, payload: ConfigValue) -> rsi_extension::Result<ConfigValue> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            digest: String,
        }
        let request: Request =
            serde_json::from_value(payload).map_err(|_| rsi_extension::Error::Protocol)?;
        self.leases.lock().await.remove(&request.digest);
        Ok(ConfigValue::Null)
    }
    async fn roots(&self, payload: ConfigValue) -> rsi_extension::Result<ConfigValue> {
        type Roots = std::collections::BTreeMap<String, String>;
        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum Request {
            Replace { expected: Roots, next: Roots },
            Read {},
        }
        let request: Request =
            serde_json::from_value(payload).map_err(|_| rsi_extension::Error::Protocol)?;
        let store = self.store.clone();
        let profile = self.profile.clone();
        tokio::task::spawn_blocking(move || match request {
            Request::Read {} => store
                .profile_roots(&profile)
                .map(|roots| serde_json::json!(roots)),
            Request::Replace { expected, next } => store
                .replace_profile_roots(&profile, &expected, &next)
                .map(|()| ConfigValue::Null),
        })
        .await
        .map_err(|_| rsi_extension::Error::Disconnected)?
        .map_err(|error| package_error(&error))
    }
}
fn package_error(error: &rsi_packages::Error) -> rsi_extension::Error {
    rsi_extension::Error::Remote(error.to_string().chars().take(1000).collect())
}
#[derive(Clone, Debug)]
struct Sessions {
    owner: Arc<dyn rsi_api_http::BrowserOwner>,
    models: Option<Arc<dyn rsi_ai_protocol::LanguageModels>>,
    defaults: Option<Arc<dyn rsi_session_protocol::AgentSettingsSource>>,
    tools: Option<Arc<tools::Publisher>>,
    ingress: Arc<dyn rsi_session_protocol::SessionIngress>,
    projections: Arc<dyn rsi_agent_turn_protocol::SessionProjections>,
    jobs: Arc<dyn rsi_agent_turn_protocol::TurnJobs>,
    workspaces: Arc<dyn rsi_workspace_protocol::WorkspaceIngress>,
    navigation: Arc<rsi_navigation::Navigation>,
    dispatch: Arc<dyn rsi_api_protocol::ApiDispatch>,
    store: Arc<dyn rsi_agent_store_protocol::SessionStore>,
    routes: Arc<rsi_api_http::ApplicationUpstream>,
}
impl Sessions {
    async fn owner_callback(
        &self,
        connection: &rsi_extension::Connection,
    ) -> rsi_meta::Result<rsi_api_http::ApplicationCallback> {
        self.routes
            .open_callback(&rsi_api_protocol::AuthenticatedDevice {
                id: self.owner.owner_id().await.map_err(failure)?,
                revoked: connection.disconnect_token(),
            })
            .map_err(failure)
    }

    fn attach(
        self,
        connection: &rsi_extension::Connection,
    ) -> rsi_meta::Result<Vec<rsi_extension::HandlerRegistration>> {
        let sessions: Arc<dyn rsi_extension::Handler> = Arc::new(self);
        register_methods(
            connection,
            &sessions,
            &[
                "application.session",
                "application.sessions",
                "application.models",
                "application.model.select",
                "application.command",
                "application.inbox",
                "application.create",
                "application.workspaces",
                "application.descendants",
                "application.submit",
                "application.cancel",
                "application.terminal",
                "application.actor",
                "application.terminal.release",
                "application.terminal.wait",
                "application.history",
                "application.interactions",
            ],
        )
    }
}
#[async_trait]
impl rsi_extension::Handler for Sessions {
    async fn call(
        &self,
        method: &str,
        payload: ConfigValue,
        stop: tokio_util::sync::CancellationToken,
    ) -> rsi_extension::Result<ConfigValue> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            request: String,
            session_id: rsi_agent_session_protocol::SessionId,
        }
        if method != "application.session" {
            return tokio::select! {
                biased;
                () = stop.cancelled() => Err(rsi_extension::Error::Cancelled),
                result = async {match method{
                    "application.sessions"=>self.page(payload).await,
                    "application.create"=>self.create(payload).await,
                    "application.workspaces"=>self.workspace_operation(payload).await,
                    "application.models"=>self.model_catalog(payload).await,
                    "application.model.select"=>self.select_model(payload).await,
                    "application.command"=>self.command(payload).await,
                    "application.inbox"=>self.inbox(payload).await,
                    "application.descendants"=>self.descendants(payload).await,
                    "application.history"=>self.history(payload).await,
                    "application.interactions"=>self.interactions(payload).await,
                    "application.submit"=>self.submit(payload).await,
                    "application.cancel"=>self.cancel(payload).await,
                    "application.terminal"=>self.terminal(payload).await,
                    "application.actor"=>self.actor(payload).await,
                    "application.terminal.release"=>self.release_terminal(payload),
                    "application.terminal.wait"=>self.wait_terminal(payload).await,
                    _=>Err(rsi_extension::Error::Protocol),
                }} => result,
            };
        }
        let input: Request =
            serde_json::from_value(payload).map_err(|_| rsi_extension::Error::Protocol)?;
        let origin = self
            .routes
            .caller(&input.request)
            .map_err(|_| rsi_extension::Error::Remote("request has retired".into()))?;
        let service = self.ingress.scoped(origin);
        let read = async {
            match service.read_header(&input.session_id).await {
                Err(rsi_session_protocol::SessionError::NotFound(_)) => {
                    service.attach(&input.session_id).await?.header().await
                }
                result => result,
            }
        };
        let header = tokio::select! {biased;()=stop.cancelled()=>return Err(rsi_extension::Error::Cancelled),
            header=read=>header.map_err(|_|rsi_extension::Error::Remote("Session unavailable to caller".into()))?,
        };
        self.routes
            .caller(&input.request)
            .map_err(|_| rsi_extension::Error::Cancelled)?;
        if header.coordinates().location() != &rsi_execution_protocol::ExecutionLocation::Local {
            return Err(rsi_extension::Error::Remote(
                "Session uses a remote workspace".into(),
            ));
        }
        Ok(
            serde_json::json!({"id":header.session_id(),"cwd":header.canonical_cwd(),"createdAt":header.created_at_ms()}),
        )
    }
}
#[async_trait]
impl rsi_extension::Handler for Imports {
    async fn call(
        &self,
        method: &str,
        payload: ConfigValue,
        stop: tokio_util::sync::CancellationToken,
    ) -> rsi_extension::Result<ConfigValue> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            specifications: Vec<String>,
            options: Option<ConfigValue>,
            expected_roots: std::collections::BTreeMap<String, String>,
        }
        if method == "application.payloads" {
            return self.payloads(payload, &stop).await;
        }
        if method == "application.release" {
            return self.release(payload).await;
        }
        if method == "application.roots" {
            return self.roots(payload).await;
        }
        if method != "application.import" {
            return Err(rsi_extension::Error::Protocol);
        }
        let request: Request =
            serde_json::from_value(payload).map_err(|_| rsi_extension::Error::Protocol)?;
        let _lane = tokio::select! { biased; () = stop.cancelled() => return Err(rsi_extension::Error::Cancelled), lane=self.lane.lock()=>lane };
        let result = rsi::import_profile_packages(
            self.store.clone(),
            rsi::PackageImport {
                package: self.selection.package.clone(),
                digest: Some(self.selection.digest.clone()),
                export: self.selection.export.clone(),
                program: self.selection.program.clone(),
                workspace: self.workspace.clone(),
                specifications: request.specifications,
                options: request.options,
            },
            super::supported_protocols(),
            stop,
            self.profile.clone(),
            request.expected_roots,
        )
        .await
        .map_err(|error| {
            rsi_extension::Error::Remote(error.to_string().chars().take(1000).collect())
        })?;
        Ok(serde_json::json!({"packages":result.packages,"roots":result.roots}))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    protocol: u32,
    port: u16,
    prefixes: Vec<String>,
    timings: Option<OpenTimings>,
    tools: Option<tools::Listener>,
}
impl Ready {
    fn publish(
        self,
        upstream: &rsi_api_http::ApplicationUpstream,
        source: Option<&tools::Publisher>,
        key: SecretValue,
        retired: tokio_util::sync::CancellationToken,
    ) -> rsi_meta::Result<()> {
        let listener = if let Some(source) = source {
            let listener = self
                .tools
                .ok_or_else(|| failure("application tool listener unavailable"))?;
            listener.validate()?;
            Some((source, listener))
        } else {
            None
        };
        upstream
            .publish(([127, 0, 0, 1], self.port).into(), self.prefixes)
            .map_err(failure)?;
        if let Some((source, listener)) = listener {
            source.publish(listener, key, retired);
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenTimings {
    #[serde(rename = "setup_ms")]
    setup: u64,
    #[serde(rename = "composition_ms")]
    composition: u64,
    #[serde(rename = "reconcile_ms")]
    reconcile: u64,
}

async fn open_application(
    connection: &rsi_extension::Connection,
    profile: &std::path::Path,
    key: &SecretValue,
    importer: bool,
    tools: Option<&tools::Publisher>,
    owner: &rsi_api_http::ApplicationCallback,
) -> rsi_meta::Result<Ready> {
    const PROTOCOL: u32 = 6;
    let mut payload = serde_json::json!({"protocol":PROTOCOL,"profile":profile,"key":key.expose_secret(),"importer":importer,"owner":owner.id()});
    if let Some(source) = tools {
        payload["tools"] = serde_json::json!({"outcomeDirectory":source.receipt_directory});
    }
    let timing =
        std::env::var_os("RSI_PACKAGE_TIMINGS").as_deref() == Some(std::ffi::OsStr::new("1"));
    if timing {
        payload["timings"] = true.into();
    }
    let started = std::time::Instant::now();
    let reply = connection
        .request("application.open", payload, connection.disconnect_token())
        .await
        .map_err(failure)?;
    let ready: Ready = serde_json::from_value(reply).map_err(failure)?;
    if ready.protocol != PROTOCOL {
        return Err(failure("incompatible application endpoint"));
    }
    if timing && let Some(row) = &ready.timings {
        eprintln!(
            "application_timing open {} ms, setup {} ms, composition {} ms, reconcile {} ms",
            started.elapsed().as_millis(),
            row.setup,
            row.composition,
            row.reconcile
        );
    }
    Ok(ready)
}
fn failure(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let input: Config = serde_json::from_value(config.clone()).map_err(failure)?;
        rsi_packages::validate_identifier(&input.profile).map_err(failure)?;
        if let Some(importer) = &input.importer {
            rsi_packages::validate_identifier(&importer.package).map_err(failure)?;
            rsi_packages::validate_identifier(&importer.export).map_err(failure)?;
            if !importer.program.is_absolute() {
                return Err(failure("importer program must be absolute"));
            }
        }
        rsi::package_extension_factory(self.0.state().join("packages")).prepare(&input.process)?;
        let use_tools = input.tools;
        let activation = PreparedActivation::with_state(config.clone(), input, 0)
            .requiring_local::<rsi_api_http::BrowserOwnerContract>()
            .requiring_local::<rsi_session_protocol::SessionIngressContract>()
            .requiring_local::<rsi_agent_turn_protocol::SessionProjectionsContract>()
            .requiring_local::<rsi_agent_turn_protocol::TurnJobsContract>()
            .requiring_local::<rsi_workspace_protocol::WorkspaceIngressContract>()
            .requiring_local::<rsi_navigation::NavigationContract>()
            .requiring_local::<rsi_api_protocol::ApiDispatchContract>()
            .requiring_local::<rsi_agent_store_protocol::SessionStoreContract>()
            .requiring_local::<rsi_process::DuplexProcessContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>();
        Ok(if use_tools {
            activation.requiring_local::<rsi_tools_protocol::ToolCatalogProviderContract>()
        } else {
            activation
        })
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config = plan.take_state()?;
        let root = self.0.state().join("packages");
        let profile = self
            .0
            .state()
            .join("web-applications")
            .join(&config.profile);
        let scope = plan.context().clone();
        let lifetime = tokio_util::sync::CancellationToken::new();
        let (mut key, first) = preparing(&config.routes, &lifetime)?;
        let upstream = Arc::new(rsi_api_http::ApplicationRoutes::new(first.clone()));
        let listener = upstream.clone();
        let stop = lifetime.clone();
        let tools = if config.tools {
            let (source, publisher) = tools::Source::new(
                plan.local::<rsi_tools_protocol::ToolCatalogProviderContract>()?,
                lifetime.clone(),
                config.profile.clone(),
                config.replace_preset_tools,
                rsi::application_tool_receipt_directory(&self.0, &config.profile)
                    .map_err(failure)?,
            )?;
            plan.context()
                .provide_local::<rsi_agent_composition_protocol::RequestToolSourceContract>(
                    Arc::new(source),
                )?;
            Some(Arc::new(publisher))
        } else {
            None
        };
        let mut sessions = Sessions {
            owner: plan.local::<rsi_api_http::BrowserOwnerContract>()?,
            models: plan
                .context()
                .lookup_local::<rsi_ai_protocol::LanguageModelsContract>(),
            defaults: plan
                .context()
                .lookup_local::<rsi_session_protocol::AgentSettingsContract>(),
            tools,
            ingress: plan.local::<rsi_session_protocol::SessionIngressContract>()?,
            projections: plan.local::<rsi_agent_turn_protocol::SessionProjectionsContract>()?,
            jobs: plan.local::<rsi_agent_turn_protocol::TurnJobsContract>()?,
            workspaces: plan.local::<rsi_workspace_protocol::WorkspaceIngressContract>()?,
            navigation: plan.local::<rsi_navigation::NavigationContract>()?,
            dispatch: plan.local::<rsi_api_protocol::ApiDispatchContract>()?,
            store: plan.local::<rsi_agent_store_protocol::SessionStoreContract>()?,
            routes: first,
        };
        let task = scope.runtime().execution().clone().spawn(async move {
            loop {
                let child = scope.clone().isolate_local_fresh::<ExtensionContract>()?.0;
                let operation = start_process(child,root.clone(),profile.clone(),config.process.clone(),
                    config.importer.clone(),key,sessions.clone());
                let result = tokio::select! {biased; ()=stop.cancelled()=>return Ok(()), result=operation=>result};
                sessions.routes.retire();
                if stop.is_cancelled() {return Ok(());}
                if let Err(error) = &result { eprintln!("Web application stopped: {error}"); }
                if !sessions.routes.published() {return result;}
                let (next_key, next) = preparing(&config.routes, &stop)?;
                listener.replace(next.clone());
                sessions.routes = next;
                key = next_key;
                tokio::select! {biased; ()=stop.cancelled()=>return Ok(()), ()=tokio::time::sleep(std::time::Duration::from_secs(1))=>{}}
            }
        });
        plan.defer(
            "drain application process preparation",
            Box::new(move || {
                Box::pin(async move {
                    lifetime.cancel();
                    task.await
                        .map_err(|err| err.to_string())?
                        .map_err(|err| err.to_string())
                })
            }),
        )?;
        plan.context()
            .provide_local::<ApplicationContract>(Arc::new(Application {
                source: config.source,
                upstream,
            }))?;
        Ok(())
    }
}

fn preparing(
    routes: &[String],
    lifetime: &tokio_util::sync::CancellationToken,
) -> rsi_meta::Result<(SecretValue, Arc<rsi_api_http::ApplicationUpstream>)> {
    let mut entropy = [0; 32];
    getrandom::fill(&mut entropy).map_err(failure)?;
    let key = SecretValue::new(hex::encode(entropy)).map_err(failure)?;
    let upstream = rsi_api_http::ApplicationUpstream::preparing(
        key.clone(),
        routes.to_vec(),
        lifetime.child_token(),
    )
    .map_err(failure)?;
    Ok((key, Arc::new(upstream)))
}

async fn start_process(
    scope: rsi_meta::Context,
    root: PathBuf,
    profile: PathBuf,
    process: ConfigValue,
    importer: Option<Importer>,
    key: SecretValue,
    sessions: Sessions,
) -> rsi_meta::Result<()> {
    let upstream = sessions.routes.clone();
    let rsi::PackageProcess {
        launch,
        resolved,
        store,
    } = rsi::resolve_package_process(root, process).await?;
    let profile_name = profile
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failure("missing application profile name"))?;
    let engine = format!("application/{profile_name}");
    let packages = format!("imports/{profile_name}");
    let mut bootstrap = std::collections::BTreeMap::from([(
        resolved.package.digest().to_owned(),
        resolved.package.manifest().name.clone(),
    )]);
    if let Some(selection) = &importer {
        bootstrap.insert(selection.digest.clone(), selection.package.clone());
    }
    let old_bootstrap = tokio::task::spawn_blocking({
        let store = store.clone();
        let engine = engine.clone();
        move || store.profile_roots(&engine)
    })
    .await
    .map_err(failure)?
    .map_err(failure)?;
    let directory = tokio::task::spawn_blocking({
        let profile = profile.clone();
        move || rsi_files_native_fs::create_absolute_directory_no_follow(&profile)
    })
    .await
    .map_err(failure)?
    .map_err(failure)?;
    let factory = rsi_extension::ExtensionFactory::new(Arc::new(rsi_extension::RejectRequests))
        .retaining(resolved);
    let child = scope
        .apply(
            super::resident::linked("rsi.web.application.process", Arc::new(factory)),
            serde_json::to_value(launch).map_err(failure)?,
        )
        .await?;
    let result = async {
        let connection = scope
            .lookup_local::<ExtensionContract>()
            .ok_or_else(|| failure("missing application connection"))?;
        let disconnected = connection.disconnect_token();
        let owner = sessions.owner_callback(&connection).await?;
        let tools = sessions.tools.clone();
        let _directory = session_page::watch_directory(&sessions, &connection, owner.id())?;
        let _sessions = sessions.attach(&connection)?;
        let import_available = importer.is_some();
        let _handle = importer
            .map(|selection| {
                Imports::attach(
                    &connection,
                    packages,
                    selection,
                    profile.clone(),
                    store.clone(),
                )
            })
            .transpose()?;
        let ready = open_application(
            &connection,
            &profile,
            &key,
            import_available,
            tools.as_deref(),
            &owner,
        )
        .await?;
        tokio::task::spawn_blocking(move || {
            store.replace_profile_roots(&engine, &old_bootstrap, &bootstrap)
        })
        .await
        .map_err(failure)?
        .map_err(failure)?;
        ready.publish(&upstream, tools.as_deref(), key, disconnected.clone())?;
        disconnected.cancelled().await;
        upstream.retire();
        match connection.disconnect_cause() {
            Some((op, error)) if op != "shutdown" => {
                Err(failure(format!("application connection ({op}): {error}")))
            }
            _ => Ok(()),
        }
    }
    .await;
    let cleanup = child.dispose().await;
    drop(directory);
    if !cleanup.is_clean() && result.is_ok() {
        return Err(failure(format!(
            "application cleanup failed: {:?}",
            cleanup.failures()
        )));
    }
    result
}
