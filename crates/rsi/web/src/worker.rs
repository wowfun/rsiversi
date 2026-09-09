use crate::{WebApplication, WebApplicationContract, WebApplicationFactory};
use async_trait::async_trait;
use rsi_api_browser_client::{BrowserClient, BrowserClientConfig, BrowserClientFactory};
use rsi_api_protocol::ApiClientContract;
use rsi_credentials_protocol::SecretValue;
use rsi_host::{Profile, ProfileEntry, ProfileProgram, RunningHost};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use serde::Deserialize;
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
};
use tokio_util::task::TaskTracker;
use wasm_bindgen::prelude::*;

#[derive(Default)]
struct Owner {
    running: Option<Arc<RunningHost>>,
    app: Option<Arc<WebApplication>>,
    connection: Option<Arc<BrowserClient>>,
    config: Option<BrowserClientConfig>,
    revision: Option<u64>,
    busy: bool,
    waiting: bool,
    failed: bool,
}
thread_local! { static OWNER: RefCell<Owner> = RefCell::new(Owner::default()); }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    endpoint_id: rsi_api_protocol::EndpointId,
    #[serde(default)]
    #[serde(rename = "id")]
    _id: Option<rsi_api_protocol::DeviceId>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Debug)]
struct BrowserConnection;
impl LocalContract for BrowserConnection {
    const KEY: &'static str = "rsi.web.connection.observation";
    type Service = BrowserClient;
}

#[derive(Debug)]
struct AuthenticatedFactory {
    token: Mutex<Option<SecretValue>>,
    diagnostic: Arc<Mutex<Option<String>>>,
}
#[async_trait]
impl PluginFactory for AuthenticatedFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        BrowserClientFactory.prepare(desired)
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let result = self.connect(plan).await;
        if let Err(error) = &result {
            *self
                .diagnostic
                .lock()
                .expect("Web authentication diagnostic poisoned") =
                Some(crate::application::error(error));
        }
        result
    }
}
impl AuthenticatedFactory {
    async fn connect(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<BrowserClientConfig>()?;
        let execution = plan.context().runtime().execution().clone();
        let tasks = TaskTracker::new();
        let retiring = tasks.clone();
        plan.defer(
            "join browser authentication",
            Box::new(move || {
                Box::pin(async move {
                    retiring.close();
                    retiring.wait().await;
                    Ok(())
                })
            }),
        )?;
        let token = self
            .token
            .lock()
            .expect("Web authentication poisoned")
            .take();
        if let Some(token) = token {
            let login_config = config.clone();
            let login_execution = execution.clone();
            execution
                .spawn(tasks.track_future(async move {
                    BrowserClient::login(login_execution, &login_config, token).await
                }))
                .await
                .map_err(|_| MetaError::Activation("browser login task stopped".into()))?
                .map_err(|error| MetaError::Activation(error.to_string()))?;
        }
        let client = BrowserClientFactory::connect_in(&mut plan, config).await?;
        let supplies = vec![
            plan.context()
                .provide_local::<ApiClientContract>(client.clone())?,
            plan.context().provide_local::<BrowserConnection>(client)?,
        ];
        plan.defer(
            "withdraw Web browser connection",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}

struct Busy;
impl Busy {
    fn enter() -> Result<Self, JsValue> {
        OWNER.with(|owner| {
            let mut owner = owner.borrow_mut();
            if owner.busy {
                return Err(failure("Connection work is already in progress"));
            }
            if owner.failed {
                return Err(failure("Reload this Worker after its failed lifecycle"));
            }
            owner.busy = true;
            Ok(Self)
        })
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        OWNER.with(|owner| owner.borrow_mut().busy = false);
    }
}

/// Starts one real Worker Profile from a registration receipt or an existing-cookie endpoint.
#[wasm_bindgen]
pub async fn connect(receipt: String, allow_loopback_http: bool) -> Result<String, JsValue> {
    let _busy = Busy::enter()?;
    if OWNER.with(|owner| owner.borrow().running.is_some()) {
        return Err(failure("Disconnect the current application first"));
    }
    let receipt = zeroize::Zeroizing::new(receipt);
    if receipt.len() > 2048 {
        return Err(failure("Device receipt exceeds its limit"));
    }
    let receipt: Receipt = serde_json::from_str(&receipt)
        .map_err(|_| failure("Paste a valid device registration receipt"))?;
    if let Some(label) = &receipt.label {
        rsi_api_protocol::DeviceRecord::validate_label(label).map_err(failure)?;
    }
    let token = receipt
        .token
        .map(SecretValue::new)
        .transpose()
        .map_err(failure)?;
    let config = BrowserClientConfig {
        endpoint_id: receipt.endpoint_id,
        allow_loopback_http,
    };
    let endpoint = config.endpoint_id.as_str().to_owned();
    let diagnostic = Arc::new(Mutex::new(None));
    let (host, program) = prepare_profile(&config, token, diagnostic.clone())?;
    let running = match host.start_program(program).await {
        Ok(running) => Arc::new(running),
        Err(error) => {
            OWNER.with(|owner| owner.borrow_mut().failed = true);
            let detail = diagnostic
                .lock()
                .expect("Web authentication diagnostic poisoned")
                .take();
            return Err(failure(
                detail.unwrap_or_else(|| crate::application::error(error)),
            ));
        }
    };
    let Some(app) = running.lookup_local::<WebApplicationContract>() else {
        let _ = running.shutdown().await;
        OWNER.with(|owner| owner.borrow_mut().failed = true);
        return Err(failure("Web Profile did not publish its application"));
    };
    let connection = running.lookup_local::<BrowserConnection>();
    OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner.running = Some(running);
        owner.app = Some(app.clone());
        owner.connection = connection;
        owner.config = Some(config);
        owner.revision = None;
    });
    let _ = app.command(r#"{"action":"refresh"}"#).await;
    Ok(endpoint)
}

fn prepare_profile(
    config: &BrowserClientConfig,
    token: Option<SecretValue>,
    diagnostic: Arc<Mutex<Option<String>>>,
) -> Result<(rsi_host::Host, ProfileProgram), JsValue> {
    let execution = Execution::browser().map_err(failure)?;
    let (mut builder, mut entries) =
        rsi_client_composition::domain_clients("browser").map_err(failure)?;
    builder = builder.execution(execution);
    builder
        .register_local_contract::<rsi_ui::UiContract>()
        .map_err(failure)?;
    builder
        .register_linked(
            "rsi.ui",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_ui::UiFactory),
        )
        .map_err(failure)?;
    builder
        .register_linked(
            "rsi.session.ui",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_session_ui::SessionUiFactory),
        )
        .map_err(failure)?;
    entries.push(ProfileEntry::new("ui", "rsi.ui", ConfigValue::Null));
    entries.push(ProfileEntry::new(
        "session-ui",
        "rsi.session.ui",
        ConfigValue::Null,
    ));
    builder
        .register_local_contract::<WebApplicationContract>()
        .map_err(failure)?;
    builder
        .register_local_contract::<BrowserConnection>()
        .map_err(failure)?;
    builder
        .register_linked(
            "rsi.web.connection",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(AuthenticatedFactory {
                token: Mutex::new(token),
                diagnostic,
            }),
        )
        .map_err(failure)?;
    builder
        .register_linked(
            "rsi.application.web",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(WebApplicationFactory),
        )
        .map_err(failure)?;
    entries.insert(
        0,
        ProfileEntry::new(
            "connection",
            "rsi.web.connection",
            serde_json::to_value(config).map_err(failure)?,
        ),
    );
    entries.push(ProfileEntry::new(
        "application",
        "rsi.application.web",
        ConfigValue::Null,
    ));
    Ok((
        builder.build().map_err(failure)?,
        ProfileProgram::from_profile(Profile::new(entries)),
    ))
}

/// Forwards a closed input document to the ordinary application command owner.
#[wasm_bindgen]
pub async fn command(source: String) -> Result<(), JsValue> {
    let app = application()?;
    app.command(&source).await.map_err(failure)
}

struct Waiting;
impl Drop for Waiting {
    fn drop(&mut self) {
        OWNER.with(|owner| owner.borrow_mut().waiting = false);
    }
}

/// Delivers one coalesced view; the document bridge acknowledges it before requesting another.
#[wasm_bindgen]
pub async fn next_view() -> Result<JsValue, JsValue> {
    let app = application()?;
    let revision = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        if owner.waiting {
            return Err(failure("A Web view waiter already exists"));
        }
        owner.waiting = true;
        Ok(owner.revision)
    })?;
    let _waiting = Waiting;
    let mut changed = app.changes();
    if revision == Some(*changed.borrow_and_update()) {
        tokio::select! { biased;
            () = app.closed() => return Err(failure("Web application closed")),
            result = changed.changed() => result.map_err(failure)?,
        }
    }
    let revision = *changed.borrow_and_update();
    let frame = app.view().map_err(failure)?;
    let text = std::str::from_utf8(frame.as_bytes()).map_err(failure)?;
    let result = JsValue::from_str(text);
    OWNER.with(|owner| owner.borrow_mut().revision = Some(revision));
    Ok(result)
}

/// Drains the actual Profile and optionally clears this origin's browser credential cookie.
#[wasm_bindgen]
pub async fn disconnect(sign_out: bool) -> Result<JsValue, JsValue> {
    let _busy = Busy::enter()?;
    let (running, config) = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner.app.take();
        (owner.running.take(), owner.config.take())
    });
    if let Some(running) = running {
        let report = running.shutdown().await;
        if !report.is_clean() {
            OWNER.with(|owner| owner.borrow_mut().failed = true);
            return Err(failure("Web cleanup failed; reload the Worker"));
        }
    }
    if sign_out && let Some(config) = config {
        BrowserClient::logout(Execution::browser().map_err(failure)?, &config)
            .await
            .map_err(failure)?;
    }
    Ok(resource_snapshot())
}

/// Reports only bounded lifecycle counters for browser integration verification.
#[wasm_bindgen]
pub fn resource_snapshot() -> JsValue {
    let execution = rsi_meta_execution::browser_resource_snapshot();
    let requests = OWNER.with(|owner| {
        owner
            .borrow()
            .connection
            .as_ref()
            .map_or(0, |client| client.resource_snapshot().active_requests)
    });
    JsValue::from_str(&serde_json::json!({"pending_timers":execution.pending_timers,"active_alarms":execution.active_alarms,"active_requests":requests}).to_string())
}
fn application() -> Result<Arc<WebApplication>, JsValue> {
    OWNER
        .with(|owner| owner.borrow().app.clone())
        .ok_or_else(|| failure("Web application is not connected"))
}
fn failure(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&crate::application::error(error))
}
