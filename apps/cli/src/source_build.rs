//! Caller-owned source leases; both installed and debug CLI use this path.
use rsi::{BuildRecipe, BuildUpdate, LocalHostLauncher as _, RsiError};
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;

pub(super) struct WatchSources {
    tasks: tokio::task::JoinSet<()>,
    events: mpsc::Receiver<rsi::Result<(String, BuildUpdate)>>,
}
impl WatchSources {
    pub async fn open(
        paths: &rsi_host::HostPaths,
        configuration: &rsi::LaunchConfiguration,
        enabled: bool,
    ) -> rsi::Result<Self> {
        let (events, receiver) = mpsc::channel(16);
        let mut owner = Self {
            tasks: tokio::task::JoinSet::new(),
            events: receiver,
        };
        if !enabled || configuration.sources.is_empty() {
            return Ok(owner);
        }
        if configuration.sources.len() > 8 {
            return Err(error(
                "at most eight simultaneous source recipes are supported",
            ));
        }
        super::host_cli::ProductHostLauncher
            .ensure_started(paths)
            .await?;
        let environment = rsi::scrub_child_environment(std::env::vars_os())
            .into_iter()
            .map(|(k, v)| {
                Ok((
                    k.into_string()
                        .map_err(|_| error("source environment key is not UTF-8"))?,
                    v.into_string()
                        .map_err(|_| error("source environment value is not UTF-8"))?,
                ))
            })
            .collect::<rsi::Result<BTreeMap<_, _>>>()?;
        let mut pending = Vec::new();
        for (name, source) in &configuration.sources {
            let directory = rsi::normalize_launch_path(&source.directory, paths.config())?;
            let command = resolve_command(&source.command, &directory, &environment, paths)?;
            let recipe = BuildRecipe {
                package: name.clone(),
                directory,
                command,
                output: source.output.clone(),
                inputs: source.inputs.clone(),
                environment: environment.clone(),
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            let lease = loop {
                match rsi::acquire_source_build(paths, recipe.clone()).await {
                    Ok(lease) => break lease,
                    Err(rsi::SourceBuildError::Busy) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    Err(failure) => return Err(error(failure)),
                }
            };
            pending.push((name.clone(), lease));
        }
        for (name, mut lease) in pending {
            loop {
                let update = tokio::select! {
                    update = lease.next() => update?,
                    _ = tokio::signal::ctrl_c() => return Err(error("source startup interrupted")),
                };
                report(&name, &update);
                match update {
                    BuildUpdate::Published { .. } => break,
                    BuildUpdate::Failed { .. } => {
                        let store =
                            rsi_packages::PackageStore::open(&paths.state().join("packages"))
                                .map_err(error)?;
                        if store.acquire(&name).is_ok() {
                            break;
                        }
                    }
                    BuildUpdate::Building {} => {}
                }
            }
            let events = events.clone();
            owner.tasks.spawn(async move {
                loop {
                    let update = lease.next().await.map(|update| (name.clone(), update));
                    let failed = update.is_err();
                    if events.send(update).await.is_err() || failed {
                        break;
                    }
                }
            });
        }
        Ok(owner)
    }
    pub async fn close(&mut self) {
        self.tasks.shutdown().await;
    }
    pub async fn observe(&mut self) -> rsi::Result<String> {
        if self.tasks.is_empty() {
            return std::future::pending().await;
        }
        while let Some(event) = self.events.recv().await {
            let (name, update) = event?;
            report(&name, &update);
            if matches!(
                update,
                BuildUpdate::Published {
                    restart_required: false,
                    ..
                }
            ) {
                return Ok(name);
            }
        }
        Err(error("source build Host disconnected"))
    }
}
// JoinSet Drop aborts only client readers. Their stream EOF releases Host leases;
// the Host's tracked build worker owns cancellation and process-tree settlement.
fn report(name: &str, update: &BuildUpdate) {
    match update {
        BuildUpdate::Building {} => eprintln!("Source {name}: building"),
        BuildUpdate::Published {
            digest,
            restart_required,
        } => eprintln!(
            "Source {name}: published {digest}{}",
            if *restart_required {
                "; restart-required: relaunch with the new core/TUI executable (installed core: rsi core upgrade --local PACKAGE_DIRECTORY)"
            } else {
                ""
            }
        ),
        BuildUpdate::Failed { reason } => eprintln!("Source {name}: {reason}"),
    }
}
fn error(value: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(value.to_string())
}
fn resolve_command(
    command: &[String],
    directory: &Path,
    environment: &BTreeMap<String, String>,
    paths: &rsi_host::HostPaths,
) -> rsi::Result<Vec<String>> {
    let name = command
        .first()
        .ok_or_else(|| error("source command is empty"))?;
    let path = Path::new(name);
    let candidates = if path.is_absolute() {
        vec![path.to_owned()]
    } else if path.components().count() > 1 {
        vec![directory.join(path)]
    } else {
        let selected = environment
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .map(|(_, v)| v);
        std::env::split_paths(&OsString::from(
            selected.ok_or_else(|| error("source command requires caller PATH"))?,
        ))
        .filter(|p| p.is_absolute())
        .map(|root| root.join(path))
        .collect()
    };
    let mut extensions = vec![String::new()];
    if cfg!(windows) && path.extension().is_none() {
        extensions.extend([".exe", ".com", ".cmd", ".bat"].map(str::to_owned));
    }
    let executable = candidates
        .into_iter()
        .flat_map(|candidate| {
            extensions.iter().map(move |extension| {
                let mut value = candidate.as_os_str().to_os_string();
                value.push(extension);
                PathBuf::from(value)
            })
        })
        .find(|path| path.is_file())
        .ok_or_else(|| error("source executable is not available in the caller PATH"))?;
    // Invocation spelling is part of the tool contract (cargo/rustup and
    // BusyBox dispatch on argv[0]); canonicalizing a symlink changes the tool.
    let executable = rsi::normalize_launch_path(&executable, directory)?;
    let mut result = Vec::new();
    #[cfg(windows)]
    if executable
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
    {
        result.push(
            super::host_runtime::publish(paths)?
                .to_str()
                .ok_or_else(|| error("runtime path is not UTF-8"))?
                .into(),
        );
        result.push("__source-command".into());
    }
    #[cfg(not(windows))]
    let _ = paths;
    result.push(
        executable
            .to_str()
            .ok_or_else(|| error("source executable is not UTF-8"))?
            .into(),
    );
    result.extend_from_slice(&command[1..]);
    Ok(result)
}
pub(super) fn frontend(
    paths: &rsi_host::HostPaths,
    configuration: &rsi::LaunchConfiguration,
) -> rsi::Result<Option<rsi_serve::FrontendRecipe>> {
    let Some(source) = configuration.sources.get("web") else {
        return Ok(None);
    };
    let Some(frontend) = &source.frontend else {
        return Ok(None);
    };
    let directory = rsi::normalize_launch_path(&source.directory, paths.config())?;
    let environment = rsi::scrub_child_environment(std::env::vars_os());
    let utf8 = environment
        .iter()
        .map(|(k, v)| {
            Ok((
                k.to_str()
                    .ok_or_else(|| error("frontend environment key is not UTF-8"))?
                    .to_owned(),
                v.to_str()
                    .ok_or_else(|| error("frontend environment value is not UTF-8"))?
                    .to_owned(),
            ))
        })
        .collect::<rsi::Result<BTreeMap<_, _>>>()?;
    let command = resolve_command(&frontend.command, &directory, &utf8, paths)?;
    Ok(Some(rsi_serve::FrontendRecipe {
        directory,
        command,
        environment,
        packages: paths.state().join("packages"),
        port: frontend.port,
    }))
}
#[cfg(windows)]
pub(super) fn run_batch_helper() -> Option<u8> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("__source-command")) {
        return None;
    }
    let Some(program) = arguments.next() else {
        return Some(2);
    };
    let path = Path::new(&program);
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
    {
        return Some(2);
    }
    // Rust's Windows Command implementation owns batch argument escaping. This
    // native trampoline remains in the Process-owned Job with its descendants.
    Some(
        std::process::Command::new(program)
            .args(arguments)
            .creation_flags(0x0800_0000)
            .status()
            .ok()
            .and_then(|status| status.code())
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
    )
}
