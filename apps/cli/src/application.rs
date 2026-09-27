#[cfg(target_os = "linux")]
use super::standard_coding_tools;
use super::{
    ApplicationInvocation, ProfileCatalog, RsiError, capture_standard_environment,
    profile_management_error, standard_paths,
};

pub(super) async fn run_application(invocation: ApplicationInvocation) -> u8 {
    if invocation
        .arguments
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
        .any(|argument| matches!(argument.to_str(), Some("-h" | "--help")))
    {
        if invocation.profile.as_str() == "web" {
            print!("{}", rsi_serve::WEB_HELP);
            return 0;
        }
        print!(
            "{}\n{}\n{}\n{}\n{}",
            rsi_terminal::HELP,
            rsi_serve::HELP,
            rsi_terminal::DEVICES_HELP,
            rsi_terminal::INSPECTOR_HELP,
            rsi_terminal::NATIVE_ADDONS_HELP
        );
        print!("{}", super::reset_state::HELP);
        return 0;
    }
    match start(invocation).await {
        Ok(exit) => exit,
        Err(error) => report_error(&error),
    }
}

async fn start(invocation: ApplicationInvocation) -> rsi::Result<u8> {
    let paths = standard_paths()?;
    let profile = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata())
        .application(&invocation.profile)
        .map_err(profile_management_error)?;
    #[cfg(target_os = "linux")]
    let coding = standard_coding_tools()?;
    #[cfg(not(target_os = "linux"))]
    let coding = None;
    let program = profile
        .program()
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let reset = invocation
        .reset_state
        .then(rsi_agent_store_sqlite::SqliteStoreResetRequest::new);
    let mut composition = rsi::StandardComposition::new(
        paths,
        capture_standard_environment()?,
        coding,
        rsi_app_catalog::metadata(),
    )
    .with_user_home(rsi::capture_standard_home()?)?;
    if let Some(reset) = &reset {
        composition = composition.with_agent_store_reset(reset.clone());
    }
    let application = rsi::ApplicationComposition::new(
        composition,
        std::sync::Arc::new(rsi_app_catalog::Catalog::new(web_environment()?)),
        rsi::StandardAddonSet::default(),
    )
    .map_err(|e| RsiError::Boot(e.to_string()))?;
    let started = rsi::start_application(application, invocation.arguments, program).await;
    if let Some(reset) = &reset {
        super::reset_state::report(reset.take_receipt().as_ref());
    }
    let running = started?;
    if reset
        .as_ref()
        .is_some_and(rsi_agent_store_sqlite::SqliteStoreResetRequest::is_pending)
    {
        running.shutdown().await;
        return Err(RsiError::Boot(
            "--reset-state requires a local SQLite Agent Store application".into(),
        ));
    }
    rsi_application::ApplicationLifetime::default()
        .run(&running)
        .await
        .map_err(|error| match error {
            rsi_application::ApplicationError::MissingEntry => RsiError::Boot(error.to_string()),
            _ => RsiError::Run(error.to_string()),
        })
}

pub(super) fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}

fn web_environment() -> rsi::Result<rsi_serve::WebLaunchEnvironment> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| RsiError::Boot(format!("cannot locate executable: {error}")))?;
    let assets = executable.parent().map(|parent| parent.join("assets"));
    let ssh = ["SSH_CONNECTION", "SSH_TTY"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let environment = [
        "PATH",
        "HOME",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
        "WSL_INTEROP",
        "WSL_DISTRO_NAME",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (std::ffi::OsString::from(key), value)))
    .collect();
    let command = if std::env::var_os("WSL_INTEROP").is_some() {
        "wslview"
    } else {
        "xdg-open"
    };
    Ok(rsi_serve::WebLaunchEnvironment {
        assets,
        ssh,
        opener: Some(std::sync::Arc::new(rsi_serve::SystemBrowserOpener {
            command: command.into(),
            environment,
        })),
    })
}
