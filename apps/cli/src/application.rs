#[cfg(any(target_os = "linux", windows))]
use super::standard_coding_tools;
use super::{
    ApplicationInvocation, ProfileCatalog, RsiError, capture_standard_environment, standard_paths,
};

pub(super) fn print_help(invocation: &ApplicationInvocation) -> bool {
    if invocation
        .arguments
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
        .any(|argument| matches!(argument.to_str(), Some("-h" | "--help")))
    {
        print!("{}", help(invocation.profile.as_str()));
        return true;
    }
    false
}

fn help(profile: &str) -> &'static str {
    match profile {
        "tui" => rsi_terminal::TUI_HELP,
        "cli" => rsi_terminal::CLI_HELP,
        "headless" => rsi_terminal::HEADLESS_HELP,
        "acp" => rsi_acp_app::HELP,
        "web" => rsi_serve::WEB_HELP,
        "serve" => rsi_serve::HELP,
        "desktop" => rsi_app_catalog::DESKTOP_HELP,
        "devices" => rsi_terminal::DEVICES_HELP,
        "inspector" => rsi_terminal::INSPECTOR_HELP,
        "addons" => rsi_terminal::NATIVE_ADDONS_HELP,
        _ => super::cli::RUN_HELP,
    }
}

pub(super) async fn run_application(invocation: ApplicationInvocation) -> u8 {
    match start(invocation).await {
        Ok(exit) => exit,
        Err(error) => report_error(&error),
    }
}

#[allow(clippy::too_many_lines)] // Keep activation, watch ownership and orderly retirement in one lifetime.
async fn start(invocation: ApplicationInvocation) -> rsi::Result<u8> {
    let paths = standard_paths()?;
    let catalog = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata());
    let mut configuration = match super::launch::current() {
        Ok(startup) => startup.configuration()?,
        Err(_) => rsi::LaunchConfiguration::default(),
    };
    let watch = super::launch::current().is_ok_and(|startup| startup.options.watch);
    #[cfg(any(target_os = "linux", windows))]
    let mut sources =
        super::source_build::WatchSources::open(&paths, &configuration, watch).await?;
    loop {
        let profile = {
            let configuration = configuration.clone();
            let catalog = catalog.clone();
            let id = invocation.profile.clone();
            tokio::task::spawn_blocking(move || configuration.application_profile(&catalog, &id))
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))??
        };
        #[cfg(any(target_os = "linux", windows))]
        let coding = standard_coding_tools()?;
        #[cfg(not(any(target_os = "linux", windows)))]
        let coding = None;
        let program = profile
            .program()
            .map_err(|error| RsiError::Boot(error.to_string()))?
            .with_source_watch(watch);
        let mut composition = rsi::StandardComposition::new(
            paths.clone(),
            capture_standard_environment()?,
            coding,
            rsi_app_catalog::metadata(),
        )
        .with_user_home(rsi::capture_standard_home()?)?
        .with_source_watch(watch);
        #[cfg(any(target_os = "linux", windows))]
        {
            composition = composition
                .with_host_launcher(std::sync::Arc::new(super::host_cli::ProductHostLauncher));
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let configuration = configuration.clone();
            let catalog = catalog.clone();
            let service =
                tokio::task::spawn_blocking(move || configuration.service_profile(&catalog))
                    .await
                    .map_err(|error| RsiError::Boot(error.to_string()))??;
            composition = composition.with_launch_service(service);
        }
        let mut web = web_environment()?;
        #[cfg(any(target_os = "linux", windows))]
        if watch {
            web.frontend = super::source_build::frontend(&paths, &configuration)?;
        }
        let application = rsi::ApplicationComposition::new(
            composition,
            std::sync::Arc::new(rsi_app_catalog::Catalog::new(web)),
            rsi::StandardAddonSet::default(),
        )
        .map_err(|e| RsiError::Boot(e.to_string()))?;
        let started =
            rsi::start_application(application, invocation.arguments.clone(), program).await;
        let running = started?;
        let lifetime = rsi_application::ApplicationLifetime::default().retain_background(watch);
        #[cfg(any(target_os = "linux", windows))]
        let observation = if watch {
            let active = rsi_service_host::ServiceHostPaths::from_host_paths(&paths)
                .and_then(|paths| paths.read_metadata());
            match active {
                Ok(None) => Ok(None),
                Ok(Some(_)) => rsi::acquire_configuration_watch(&paths).await.map(Some),
                Err(error) => Err(RsiError::Boot(error.to_string())),
            }
        } else {
            Ok(None)
        };
        #[cfg(not(any(target_os = "linux", windows)))]
        let observation: rsi::Result<Option<()>> = Ok(None);
        let mut observation = match observation {
            Ok(observation) => observation,
            Err(error) => {
                running.shutdown().await;
                return Err(error);
            }
        };
        let run = lifetime.run(&running);
        tokio::pin!(run);
        let mut reconfigure = None;
        let result = tokio::select! {
            result = &mut run => Some(result),
            result = async {
                #[cfg(any(target_os = "linux", windows))]
                {
                    loop {
                        let package = sources.observe().await?;
                    match source_requires_restart(&paths, &configuration, invocation.profile.as_str(), &package).await {
                        Ok(true) => break Ok::<(), RsiError>(()),
                        Ok(false) => {},
                        Err(error) => eprintln!("Application update rejected; current generation retained: {error}"),
                        }
                    }
                }
                #[cfg(not(any(target_os = "linux", windows)))]
                { std::future::pending::<rsi::Result<()>>().await }
            }, if watch => {
                lifetime.request_stop();
                run.await.map_err(|error| RsiError::Run(error.to_string()))?;
                result?;
                None
            },
            next = changed_configuration(&paths, &configuration, &invocation.profile), if watch => {
                lifetime.request_stop();
                run.await.map_err(|error| RsiError::Run(error.to_string()))?;
                reconfigure = Some(next?);
                None
            },
            signal = tokio::signal::ctrl_c(), if watch => {
                lifetime.request_stop();
                let result = run.await;
                signal.map_err(|error| RsiError::Run(error.to_string()))?;
                Some(result)
            }
            () = async {
                #[cfg(any(target_os = "linux", windows))]
                if let Some(lease) = &mut observation {
                    lease.disconnected().await;
                    return;
                }
                std::future::pending::<()>().await;
            }, if watch => {
                lifetime.request_stop();
                run.await.map_err(|error| RsiError::Run(error.to_string()))?;
                return Err(RsiError::Run("Service Host disconnected during observation".into()));
            }
        };
        drop(observation);
        if let Some(result) = result {
            return result.map_err(|error| match error {
                rsi_application::ApplicationError::MissingEntry => {
                    RsiError::Boot(error.to_string())
                }
                _ => RsiError::Run(error.to_string()),
            });
        }
        if let Some(next) = reconfigure {
            #[cfg(any(target_os = "linux", windows))]
            if next.sources != configuration.sources {
                sources.close().await;
                sources = super::source_build::WatchSources::open(&paths, &next, watch).await?;
            }
            configuration = next;
        }
        eprintln!("Application source changed; resolving the selected package again.");
    }
}

async fn changed_configuration(
    paths: &rsi_host::HostPaths,
    previous: &rsi::LaunchConfiguration,
    application: &rsi::ApplicationProfileId,
) -> rsi::Result<rsi::LaunchConfiguration> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_error = None;
    loop {
        interval.tick().await;
        let paths = paths.clone();
        let previous = previous.clone();
        let application = application.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            let explicit = super::launch::current()
                .is_ok_and(|startup| startup.options.homes.config.is_some());
            let candidate = rsi::LaunchConfiguration::load(paths.config(), explicit)?;
            if candidate == previous {
                return Ok(None);
            }
            if candidate.service != previous.service {
                return Err(RsiError::Boot(
                    "service selection changed; restart the Host and watch launcher explicitly"
                        .into(),
                ));
            }
            let catalog = ProfileCatalog::new(paths, rsi_app_catalog::metadata());
            candidate
                .application_profile(&catalog, &application)?
                .program()
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            Ok(Some(candidate))
        })
        .await
        .map_err(|error| RsiError::Boot(error.to_string()))?;
        match candidate {
            Ok(Some(candidate)) => return Ok(candidate),
            Ok(None) => last_error = None,
            Err(error) => {
                let message = error.to_string();
                if last_error.as_ref() != Some(&message) {
                    eprintln!(
                        "Launch configuration update rejected; current configuration retained: {message}"
                    );
                    last_error = Some(message);
                }
            }
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
async fn source_requires_restart(
    paths: &rsi_host::HostPaths,
    configuration: &rsi::LaunchConfiguration,
    application: &str,
    package: &str,
) -> rsi::Result<bool> {
    if package == "web" && matches!(application, "web" | "desktop") {
        let owner = rsi_service_host::ServiceHostPaths::from_host_paths(paths)
            .and_then(|paths| paths.read_metadata())
            .map_err(|e| RsiError::Run(e.to_string()))?
            .ok_or_else(|| RsiError::Run("Host stopped during source publication".into()))?;
        let reply = rsi_serve::request_listener(
            &owner,
            rsi_web_control::Command::Refresh {
                package: package.into(),
            },
        )
        .await
        .map_err(RsiError::Run)?
        .1;
        let rsi_web_control::Reply::Refreshed { restart_required } = reply else {
            return Err(RsiError::Run(
                "Host did not acknowledge source publication".into(),
            ));
        };
        if restart_required
            && application == "web"
            && configuration
                .sources
                .get("web")
                .is_none_or(|source| source.frontend.is_none())
        {
            rsi_serve::request_listener(&owner, rsi_web_control::Command::Stop {})
                .await
                .map_err(RsiError::Run)?;
        }
        return Ok(restart_required);
    }
    let selected = (application == "desktop" && package == "desktop")
        || matches!(configuration.applications.get(application), Some(rsi::LaunchProfile::Package { package:name,.. }) if name == package);
    if selected {
        let catalog = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata());
        let id = rsi::ApplicationProfileId::new(application)
            .map_err(|e| RsiError::Run(e.to_string()))?;
        configuration
            .application_profile(&catalog, &id)?
            .program()
            .map_err(|e| RsiError::Run(e.to_string()))?;
    }
    Ok(selected)
}

pub(super) fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}

pub(super) async fn web_control(stop: bool) -> u8 {
    let result = async {
        super::launch::maintain()?;
        let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&super::launch::paths()?)
            .map_err(|e| RsiError::Boot(e.to_string()))?;
        let owner = paths
            .read_metadata()
            .map_err(|e| RsiError::Boot(e.to_string()))?;
        let reply = if let Some(owner) = owner {
            if rsi_service_host::owner_process_is_current(&owner)
                .map_err(|e| RsiError::Boot(e.to_string()))?
            {
                let command = if stop {
                    rsi_web_control::Command::Stop {}
                } else {
                    rsi_web_control::Command::Status {}
                };
                rsi_serve::request_listener(&owner, command)
                    .await
                    .map_err(RsiError::Boot)?
                    .1
            } else {
                rsi_web_control::Reply::Stopped {}
            }
        } else {
            rsi_web_control::Reply::Stopped {}
        };
        println!(
            "{}",
            serde_json::to_string(&reply).map_err(|e| RsiError::Boot(e.to_string()))?
        );
        Ok::<(), RsiError>(())
    }
    .await;
    result.map_or_else(|error| report_error(&error), |()| 0)
}

pub(super) fn web_environment() -> rsi::Result<rsi_serve::WebLaunchEnvironment> {
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
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
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
    let (command, arguments) = if cfg!(windows) {
        let system = std::env::var_os("SystemRoot")
            .ok_or_else(|| RsiError::Boot("SystemRoot is missing".into()))?;
        (
            std::path::PathBuf::from(system)
                .join("System32/rundll32.exe")
                .into_os_string(),
            vec!["url.dll,FileProtocolHandler".into()],
        )
    } else if std::env::var_os("WSL_INTEROP").is_some() {
        ("wslview".into(), Vec::new())
    } else {
        ("xdg-open".into(), Vec::new())
    };
    Ok(rsi_serve::WebLaunchEnvironment {
        frontend: None,
        assets,
        ssh,
        opener: Some(std::sync::Arc::new(rsi_serve::SystemBrowserOpener {
            command,
            arguments,
            environment,
        })),
    })
}
