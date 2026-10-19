mod bridge;
mod data;
mod document;
mod export;
mod options;
mod protocol;
use async_trait::async_trait;
use bridge::{Bridge, error};
use rsi::{AddonScope, StandardAddonBuilder, StandardAddonSet};
use rsi_application::{ApplicationLifetime, ApplicationRun, ApplicationRunContract};
use rsi_host::{Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, UpdateMode};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};
use tauri::Manager;

type Owner = Arc<Mutex<Option<Arc<Bridge>>>>;
#[derive(Debug)]
struct DesktopFactory {
    owner: Owner,
    lifetime: Arc<ApplicationLifetime>,
    failed: Arc<AtomicBool>,
}
#[async_trait]
impl PluginFactory for DesktopFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "Desktop entry accepts null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<rsi_gui::GuiApplicationContract>()
            .requiring_local::<rsi_api_protocol::ApiClientContract>()
            .requiring_local::<rsi_web_assets::WebAssetControlContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let api = plan.local::<rsi_api_protocol::ApiClientContract>()?;
        let identity = serde_json::json!({"endpoint_id": api.description().endpoint_id, "principal": {"kind": "local"}}).to_string();
        let bridge = Arc::new(Bridge::new(
            plan.local::<rsi_gui::GuiApplicationContract>()?,
            plan.local::<rsi_web_assets::WebAssetControlContract>()?,
            identity,
            self.lifetime.clone(),
            self.failed.clone(),
        ));
        let owner = self.owner.clone();
        *owner.lock().expect("desktop owner poisoned") = Some(bridge.clone());
        let retiring = bridge.clone();
        plan.defer(
            "close native mailbox and join admitted requests",
            Box::new(move || {
                Box::pin(async move {
                    owner.lock().expect("desktop owner poisoned").take();
                    retiring.close().await;
                    Ok(())
                })
            }),
        )?;
        plan.context()
            .provide_local::<ApplicationRunContract>(Arc::new(Entry(bridge)))?;
        Ok(())
    }
}
#[derive(Debug)]
struct Entry(Arc<Bridge>);
impl ApplicationRun for Entry {
    fn run(
        self: Arc<Self>,
    ) -> futures_util::future::BoxFuture<'static, rsi_application::Result<u8>> {
        Box::pin(async move {
            self.0.stop.cancelled().await;
            Ok(0)
        })
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep generation admission, event-loop ownership, and shutdown ordering together."
)]
fn run() -> Result<i32, String> {
    let Some((assets, host_profile)) = options::parse(std::env::args_os().skip(1), || {
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(error)
    })?
    else {
        println!("{}", options::HELP);
        return Ok(0);
    };
    let _generation = rsi_build_info::GenerationLease::current()
        .map_err(|e| format!("cannot pin Desktop generation: {e}"))?;
    options::require_assets(&assets)?;
    #[cfg(windows)]
    options::require_webview(tauri::webview_version())?;
    let lifetime = Arc::new(ApplicationLifetime::default());
    let owner: Owner = Arc::default();
    let failed = Arc::new(AtomicBool::new(false));
    let (composition, program, data) =
        bootstrap(&assets, &host_profile, &owner, &lifetime, &failed)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(error)?;
    tauri::async_runtime::set(runtime.handle().clone());
    let running = runtime
        .block_on(rsi::start_application(composition, vec![], program))
        .map_err(error)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let status = Arc::new(AtomicI32::new(0));
    let protocol_owner = owner.clone();
    let dialog_owner = owner.clone();
    let built = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .register_asynchronous_uri_scheme_protocol("rsi", move |context, request, responder| {
            protocol::handle(&protocol_owner, context.webview_label(), request, responder);
        })
        .setup(move |app| {
            if let Some(bridge) = dialog_owner
                .lock()
                .expect("desktop owner poisoned")
                .as_ref()
            {
                *bridge.app_handle.lock().expect("desktop dialog poisoned") =
                    Some(app.handle().clone());
            }
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(document::root_url()),
            )
            .on_navigation(document::admits_navigation)
            .title("RSIversi")
            .inner_size(1440.0, 980.0)
            .data_directory(data)
            .build()?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match built {
        Ok(app) => app,
        Err(failure) => {
            lifetime.request_stop();
            runtime.block_on(lifetime.run(&running)).map_err(error)?;
            return Err(error(failure));
        }
    };
    let task = drive_lifetime(
        &runtime,
        app.handle(),
        running,
        &lifetime,
        &stopped,
        &status,
        &failed,
    );
    let signals = lifetime.clone();
    let signal_app = app.handle().clone();
    let signal_owner = owner.clone();
    let signal_task = runtime.spawn(async move {
        tokio::select! { biased;
            () = signals.stopped() => {},
            result = tokio::signal::ctrl_c() => { if result.is_ok() { request_document_close(&signal_app, &signal_owner); } }
        }
    });
    let main_thread = std::thread::current().id();
    let exit = app.run_return(move |app, event| {
        assert_eq!(
            main_thread,
            std::thread::current().id(),
            "Tauri event loop moved off main thread"
        );
        handle_event(app, event, &stopped, &owner);
    });
    runtime.block_on(task).map_err(error)?;
    runtime.block_on(signal_task).map_err(error)?;
    Ok(if exit != 0 {
        exit
    } else {
        status.load(Ordering::Acquire)
    })
}
fn request_document_close(app: &tauri::AppHandle, owner: &Owner) {
    // Hold publication admission until the deadline is registered in cleanup's tracker.
    let guard = owner.lock().expect("desktop owner poisoned");
    let Some(bridge) = guard.as_ref() else {
        return;
    };
    let Some(attempt) = bridge.begin_document_close() else {
        return;
    };
    let lifetime = bridge.lifetime.clone();
    let failed = bridge.failed.clone();
    if app
        .get_webview_window("main")
        .is_none_or(|window| window.eval(document::REQUEST_CLOSE).is_err())
    {
        failed.store(true, Ordering::Release);
        lifetime.request_stop();
        return;
    }
    tauri::async_runtime::spawn(bridge.tasks.track_future(async move {
        tokio::select! { biased;
            () = lifetime.stopped() => {},
            () = attempt.cancelled() => {},
            () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                eprintln!("desktop: document drain timed out"); failed.store(true, Ordering::Release); lifetime.request_stop();
            }
        }
    }));
}

fn bootstrap(
    assets: &std::path::Path,
    host_profile: &rsi::HostProfileId,
    owner: &Owner,
    lifetime: &Arc<ApplicationLifetime>,
    failed: &Arc<AtomicBool>,
) -> Result<(rsi::ApplicationComposition, ProfileProgram, PathBuf), String> {
    let paths = rsi::standard_paths().map_err(error)?;
    let data = data::prepare(paths.state())?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(error)?;
    let companion = executable
        .parent()
        .ok_or("Desktop executable directory is absent")?
        .join(format!("rsi{}", std::env::consts::EXE_SUFFIX))
        .canonicalize()
        .map_err(|_| "Install the paired rsi companion beside rsi-desktop")?;
    let coding = rsi::StandardCodingTools::new(
        rsi::capture_standard_bash().map_err(error)?,
        companion,
        rsi::scrub_child_environment(std::env::vars_os()),
    )
    .map_err(error)?;
    let composition = rsi::StandardComposition::new(
        paths,
        rsi::capture_standard_environment().map_err(error)?,
        Some(coding),
        rsi_app_catalog::metadata(),
    )
    .with_user_home(rsi::capture_standard_home().map_err(error)?)
    .map_err(error)?;
    let composition = rsi::ApplicationComposition::new(
        composition,
        std::sync::Arc::new(rsi_app_catalog::Catalog::default()),
        desktop_addons(owner, lifetime, failed)?,
    )
    .map_err(error)?;
    let entries: Vec<_> = [
        (
            "assets",
            "rsi.web.assets",
            serde_json::json!({"directory": assets}),
        ),
        (
            "connection",
            "rsi.application.connection",
            serde_json::json!({"host_profile": host_profile}),
        ),
        ("ui", "rsi.ui", ConfigValue::Null),
        (
            "directory-picker",
            "rsi.directory-picker.client",
            ConfigValue::Null,
        ),
        (
            "application-target",
            "rsi.ui.target",
            serde_json::json!("application"),
        ),
        ("session-ui", "rsi.session.ui", ConfigValue::Null),
        ("tree-ui", "rsi.session.tree.ui", ConfigValue::Null),
        ("files-ui", "rsi.session.files.ui", ConfigValue::Null),
        (
            "workspace-review-ui",
            "rsi.workspace.review.ui",
            ConfigValue::Null,
        ),
        ("setup", "rsi.workbench.setup", ConfigValue::Null),
        ("plugins", "rsi.workbench.plugins", ConfigValue::Null),
        ("navigation", "rsi.workbench.navigation", ConfigValue::Null),
        ("gui", "rsi.application.gui", ConfigValue::Null),
        ("desktop", "rsi.application.desktop", ConfigValue::Null),
    ]
    .into_iter()
    .map(|(id, plugin, config)| ProfileEntry::new(id, plugin, config))
    .collect();
    Ok((
        composition,
        ProfileProgram::from_profile(Profile::new(entries)),
        data,
    ))
}

fn desktop_addons(
    owner: &Owner,
    lifetime: &Arc<ApplicationLifetime>,
    failed: &Arc<AtomicBool>,
) -> Result<StandardAddonSet, String> {
    let mut addon = StandardAddonBuilder::new("rsi.desktop");
    let scope = AddonScope::Application;
    addon
        .register_local_contract_at::<rsi_gui::GuiApplicationContract>(scope)
        .map_err(error)?;
    addon
        .register_local_contract_at::<rsi_workbench_ui::NavigationFeatureContract>(scope)
        .map_err(error)?;
    for (name, factory) in [
        (
            "rsi.application.gui",
            Arc::new(rsi_gui::GuiApplicationFactory) as Arc<dyn PluginFactory>,
        ),
        (
            "rsi.workbench.navigation",
            Arc::new(rsi_workbench_ui::NavigationFeatureFactory),
        ),
        (
            "rsi.application.desktop",
            Arc::new(DesktopFactory {
                owner: owner.clone(),
                lifetime: lifetime.clone(),
                failed: failed.clone(),
            }),
        ),
    ] {
        addon
            .register_factory(
                scope,
                name,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                factory,
            )
            .map_err(error)?;
    }
    StandardAddonSet::new([addon.build().map_err(error)?]).map_err(error)
}

fn drive_lifetime(
    runtime: &tokio::runtime::Runtime,
    app: &tauri::AppHandle,
    running: rsi_host::RunningHost,
    lifetime: &Arc<ApplicationLifetime>,
    stopped: &Arc<AtomicBool>,
    status: &Arc<AtomicI32>,
    failed: &Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    let app_handle = app.clone();
    let completion = stopped.clone();
    let exit_status = status.clone();
    let retained = lifetime.clone();
    let execution_failed = failed.clone();
    runtime.spawn(async move {
        let result = retained.run(&running).await;
        let code = match result {
            Ok(code) => {
                if execution_failed.load(Ordering::Acquire) {
                    1
                } else {
                    i32::from(code)
                }
            }
            Err(error) => {
                eprintln!("desktop cleanup: {error}");
                1
            }
        };
        exit_status.store(code, Ordering::Release);
        completion.store(true, Ordering::Release);
        eprintln!("desktop: Application cleanup completed with status {code}");
        app_handle.exit(code);
    })
}

fn handle_event(
    app: &tauri::AppHandle,
    event: tauri::RunEvent,
    stopped: &AtomicBool,
    owner: &Owner,
) {
    match event {
        tauri::RunEvent::WindowEvent {
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            if !stopped.load(Ordering::Acquire) {
                api.prevent_close();
                request_document_close(app, owner);
            }
        }
        tauri::RunEvent::ExitRequested { api, .. } => {
            if stopped.load(Ordering::Acquire) {
                eprintln!("desktop: main-thread exit after Application cleanup");
            } else {
                api.prevent_exit();
                request_document_close(app, owner);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_addons_compose_with_the_shared_application_setup_catalog() {
        // Catalog construction is pure; these paths are never created or opened.
        let root = std::env::temp_dir().join("rsi-desktop-catalog-contract");
        let service = rsi::StandardComposition::new(
            rsi_host::HostPaths::new(root.join("config"), root.join("state"), root.join("cache"))
                .unwrap(),
            std::collections::BTreeMap::new(),
            None,
            rsi_app_catalog::metadata(),
        );
        let extras = desktop_addons(
            &Owner::default(),
            &Arc::new(ApplicationLifetime::default()),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let composition = rsi::ApplicationComposition::new(
            service,
            std::sync::Arc::new(rsi_app_catalog::Catalog::default()),
            extras,
        )
        .unwrap();
        let (host, _) = rsi::standard_application_host(composition, vec![])
            .expect("desktop and standard catalogs must have disjoint registrations");
        assert!(host.has_local_contract::<rsi_workbench_ui::SetupFeatureContract>());
    }
}
