use async_trait::async_trait;
use rsi_api_browser_client::{BrowserClient, BrowserClientConfig, BrowserClientFactory};
use rsi_api_protocol::ApiClientContract;
use rsi_credentials_protocol::SecretValue;
use rsi_gui::{GuiApplication, GuiApplicationContract, GuiApplicationFactory};
use rsi_host::{Profile, ProfileEntry, ProfileProgram, RunningHost};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use serde::Deserialize;
use std::{
    cell::RefCell,
    fmt::Write as _,
    sync::{Arc, Mutex},
};
use tokio_util::task::TaskTracker;
use wasm_bindgen::prelude::*;

#[derive(Default)]
struct Owner {
    running: Option<Arc<RunningHost>>,
    app: Option<Arc<GuiApplication>>,
    connection: Option<Arc<BrowserClient>>,
    config: Option<BrowserClientConfig>,
    revision: Option<u64>,
    busy: bool,
    waiting: bool,
    failed: bool,
    assets: Option<Arc<crate::assets::Assets>>,
    asset_revision: Option<String>,
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
                Some(rsi_gui::display_error(error));
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
                detail.unwrap_or_else(|| rsi_gui::display_error(error)),
            ));
        }
    };
    let Some(app) = running.lookup_local::<GuiApplicationContract>() else {
        let _ = running.shutdown().await;
        OWNER.with(|owner| owner.borrow_mut().failed = true);
        return Err(failure("Web Profile did not publish its application"));
    };
    let connection = running.lookup_local::<BrowserConnection>();
    let identity = serde_json::to_string(&serde_json::json!({"endpoint_id": endpoint,
        "principal": rsi_api_protocol::CallerIdentity::Device { device_id: connection.as_ref().ok_or_else(|| failure("Authenticated caller is unavailable"))?.device_id().clone() }})).map_err(failure)?;
    let assets = running.lookup_local::<crate::assets::AssetsContract>();
    OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner.running = Some(running);
        owner.app = Some(app.clone());
        owner.connection = connection;
        owner.assets = assets;
        owner.asset_revision = None;
        owner.config = Some(config);
        owner.revision = None;
    });
    let _ = app.command(r#"{"action":"refresh"}"#).await;
    Ok(identity)
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
    register_session_views(&mut builder, &mut entries)?;
    register_browser_application(&mut builder, &mut entries, config, token, diagnostic)?;
    // Feature owners must publish before the GUI captures their optional handles.
    rsi_workbench_ui::register(&mut builder, &mut entries).map_err(failure)?;
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

fn register_session_views(
    builder: &mut rsi_host::HostBuilder,
    entries: &mut Vec<ProfileEntry>,
) -> Result<(), JsValue> {
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
    builder
        .register_linked(
            "rsi.session.tree.ui",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_session_tree_ui::SessionTreeUiFactory),
        )
        .map_err(failure)?;
    entries.push(ProfileEntry::new(
        "tree-ui",
        "rsi.session.tree.ui",
        ConfigValue::Null,
    ));
    builder
        .register_linked(
            "rsi.session.files.ui",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_session_files_ui::FilesUiFactory),
        )
        .map_err(failure)?;
    entries.push(ProfileEntry::new(
        "files-ui",
        "rsi.session.files.ui",
        ConfigValue::Null,
    ));
    entries.push(ProfileEntry::new("ui", "rsi.ui", ConfigValue::Null));
    entries.push(ProfileEntry::new(
        "session-ui",
        "rsi.session.ui",
        ConfigValue::Null,
    ));
    Ok(())
}

fn register_browser_application(
    builder: &mut rsi_host::HostBuilder,
    entries: &mut Vec<ProfileEntry>,
    config: &BrowserClientConfig,
    token: Option<SecretValue>,
    diagnostic: Arc<Mutex<Option<String>>>,
) -> Result<(), JsValue> {
    builder
        .register_local_contract::<GuiApplicationContract>()
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
            Arc::new(GuiApplicationFactory),
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
    let mut nonce = [0_u8; 16];
    js_sys::global()
        .unchecked_into::<web_sys::WorkerGlobalScope>()
        .crypto()?
        .get_random_values_with_u8_array(&mut nonce)?;
    let nonce = nonce
        .iter()
        .fold(String::with_capacity(32), |mut text, byte| {
            write!(&mut text, "{byte:02x}").expect("writing to a String cannot fail");
            text
        });
    builder
        .register_local_contract::<crate::assets::AssetsContract>()
        .map_err(failure)?;
    builder
        .register_linked(
            "rsi.web.renderer.leases",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(crate::assets::AssetsFactory(nonce)),
        )
        .map_err(failure)?;
    entries.push(ProfileEntry::new(
        "renderer-leases",
        "rsi.web.renderer.leases",
        ConfigValue::Null,
    ));
    builder
        .register_local_contract::<rsi_ui::UiTargetContract>()
        .map_err(failure)?;
    builder
        .register_linked(
            "rsi.ui.target",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_ui::UiTargetFactory),
        )
        .map_err(failure)?;
    entries.push(ProfileEntry::new(
        "application-target",
        "rsi.ui.target",
        serde_json::json!("application"),
    ));
    Ok(())
}

/// Forwards a closed input document to the ordinary application command owner.
#[wasm_bindgen]
pub async fn command(source: String) -> Result<(), JsValue> {
    let app = application()?;
    app.command(&source).await.map_err(failure)
}

/// Freezes input before the document persists its execution intent.
#[wasm_bindgen]
pub async fn prepare_submission(source: String) -> Result<String, JsValue> {
    application()?
        .prepare_submission(&source)
        .await
        .map_err(failure)
}

/// Restores saved input only to an exact Session, or explicitly creates a Fresh replacement.
#[wasm_bindgen]
pub async fn restore_session(source: String) -> Result<String, JsValue> {
    application()?
        .restore_session(&source)
        .await
        .map_err(failure)
}

/// Dispatches or queries one exact saved Rust JSON request.
#[wasm_bindgen]
pub async fn dispatch_submission(
    pane: String,
    generation: String,
    opaque: String,
    mode: String,
) -> Result<String, JsValue> {
    application()?
        .dispatch_submission(
            rsi_gui::SurfaceId::parse(&pane).map_err(failure)?,
            &generation,
            &opaque,
            &mode,
        )
        .await
        .map_err(failure)
}

/// Imports bounded binary source bytes without encoding them into a JSON command.
#[wasm_bindgen]
pub async fn import_image(
    pane: String,
    generation: String,
    source: js_sys::Uint8Array,
) -> Result<String, JsValue> {
    let app = application()?;
    if source.length() as usize > rsi_gui::MAXIMUM_UPLOAD_BYTES || source.length() == 0 {
        return Err(failure("Image source must contain 1 byte to 16 MiB"));
    }
    if !app.image_import_available() {
        return Err(failure("An image operation is still in progress"));
    }
    let reference = app
        .import_image(
            rsi_gui::SurfaceId::parse(&pane).map_err(failure)?,
            &generation,
            source.to_vec().into(),
        )
        .await
        .map_err(failure)?;
    serde_json::to_string(&reference).map_err(failure)
}

/// Copies a validated canonical object into the transferable document response.
#[wasm_bindgen]
pub async fn read_image(selection: String) -> Result<js_sys::Uint8Array, JsValue> {
    let object = application()?
        .read_image(&selection)
        .await
        .map_err(failure)?;
    Ok(js_sys::Uint8Array::from(object.bytes.as_ref()))
}

/// Reads a bounded model-local source; the Worker derives its exact target and revision.
#[wasm_bindgen]
pub async fn ui_source(source: String) -> Result<js_sys::Uint8Array, JsValue> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Source {
        ticket: String,
        name: String,
        offset: u64,
        maximum: usize,
    }
    if source.len() > 1024 {
        return Err(failure("UI source selection exceeds its limit"));
    }
    let source: Source = serde_json::from_str(&source).map_err(failure)?;
    let bytes = application()?
        .read_ui_source(&source.ticket, &source.name, source.offset, source.maximum)
        .await
        .map_err(failure)?;
    Ok(js_sys::Uint8Array::from(bytes.as_bytes()))
}

struct Waiting;
impl Drop for Waiting {
    fn drop(&mut self) {
        OWNER.with(|owner| owner.borrow_mut().waiting = false);
    }
}

/// Delivers one coalesced view; the document bridge acknowledges it before requesting another.
#[wasm_bindgen]
pub async fn next_view(base: Option<String>) -> Result<JsValue, JsValue> {
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
    let assets = OWNER
        .with(|owner| owner.borrow().assets.clone())
        .ok_or_else(|| failure("Renderer lease owner is absent"))?;
    let mut offers = assets.changes();
    let previous_offer = OWNER.with(|owner| owner.borrow().asset_revision.clone());
    let mut changed = app.changes();
    let offer = loop {
        let offer = offers.borrow_and_update().clone();
        if let Some(offer) = offer {
            let offer = offer.map_err(failure)?;
            if base.is_none()
                || base != app.frame_id()
                || revision != Some(*changed.borrow_and_update())
                || previous_offer.as_ref() != Some(&offer.revision)
            {
                break offer;
            }
        }
        tokio::select! { biased;
            () = app.closed() => return Err(failure("Web application closed")),
            result = offers.changed() => result.map_err(failure)?,
            result = changed.changed() => result.map_err(failure)?,
        }
    };
    let revision = *changed.borrow_and_update();
    let frame = app.next_frame(base.as_deref()).map_err(failure)?;
    let text = std::str::from_utf8(frame.as_bytes()).map_err(failure)?;
    let result = js_sys::Array::of2(
        &JsValue::from_str(&app.frame_id().expect("encoded frame")),
        &JsValue::from_str(text),
    );
    result.push(&JsValue::from_str(
        &serde_json::to_string(&offer).map_err(failure)?,
    ));
    OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner.revision = Some(revision);
        owner.asset_revision = Some(offer.revision);
    });
    Ok(result.into())
}

/// Settles a renderer generation only after the document finishes its DOM lifecycle.
#[wasm_bindgen]
pub async fn commit_renderer(revision: String, accept: bool) -> Result<(), JsValue> {
    let assets = OWNER
        .with(|owner| owner.borrow().assets.clone())
        .ok_or_else(|| failure("Renderer lease owner is absent"))?;
    assets.commit(revision, accept).await.map_err(failure)
}

/// Drains the actual Profile and optionally clears this origin's browser credential cookie.
#[wasm_bindgen]
pub async fn disconnect(sign_out: bool) -> Result<JsValue, JsValue> {
    let _busy = Busy::enter()?;
    let (running, config) = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner.app.take();
        owner.assets.take();
        owner.asset_revision.take();
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
        let device = OWNER
            .with(|owner| {
                owner
                    .borrow()
                    .connection
                    .as_ref()
                    .map(|connection| connection.device_id().clone())
            })
            .ok_or_else(|| failure("Authenticated browser identity is absent"))?;
        BrowserClient::logout(Execution::browser().map_err(failure)?, &config, &device)
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
fn application() -> Result<Arc<GuiApplication>, JsValue> {
    OWNER
        .with(|owner| owner.borrow().app.clone())
        .ok_or_else(|| failure("Web application is not connected"))
}
fn failure(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&rsi_gui::display_error(error))
}
