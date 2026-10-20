//! Ordinary product development. Fixture state belongs to `fixture-dev`.
#[cfg(unix)]
use crate::supervision::Foreground;
use crate::supervision::{Child, Signals, application_interrupted, application_succeeded};
use rsi_dev_control::{Frame, Message, PROXY_TIMEOUT, STARTUP_TIMEOUT};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{io::BufReader, process::Command, task::JoinSet};
use tokio_util::sync::CancellationToken;

const HELP: &str = "usage: rsi-app-tools dev tui [-- --cwd ABSOLUTE | --resume SESSION | --session-id SESSION]\n       rsi-app-tools dev web [--port 0..65535] [--no-open] [--no-watch]\nStart through scripts/rsiversi.sh dev tui|web or scripts/rsiversi.ps1 dev tui|web.\nUses ordinary product state and credentials. For keyless isolated development use fixture-dev.";
const INTERRUPTED: &str = "development interrupted";
#[cfg(all(test, any(target_os = "linux", windows)))]
mod lifecycle_tests;
#[derive(Debug)]
struct Options {
    surface: String,
    arguments: Vec<OsString>,
    port: u16,
    open: bool,
    watch: bool,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let surface = args
            .first()
            .filter(|arg| matches!(arg.as_str(), "tui" | "web"))
            .ok_or(HELP)?;
        let mut options = Self {
            surface: surface.clone(),
            arguments: Vec::new(),
            port: 8787,
            open: true,
            watch: true,
        };
        if surface == "tui" {
            if args.len() > 1 {
                if args[1] != "--" {
                    return Err(HELP.into());
                }
                options.arguments = args[2..].iter().map(OsString::from).collect();
                rsi_dev_control::validate_tui_arguments(&options.arguments).map_err(|_| HELP)?;
            }
            return Ok(options);
        }
        let mut seen = BTreeSet::new();
        let mut args = args[1..].iter();
        while let Some(argument) = args.next() {
            if !seen.insert(argument) {
                return Err("duplicate development option".into());
            }
            match argument.as_str() {
                "--port" => {
                    let value = args.next().ok_or("--port requires 0..65535")?;
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err("--port requires 0..65535".into());
                    }
                    options.port = value.parse().map_err(|_| "--port requires 0..65535")?;
                }
                "--no-open" => options.open = false,
                "--no-watch" => options.watch = false,
                _ => return Err(HELP.into()),
            }
        }
        Ok(options)
    }
}
fn problem(error: impl std::fmt::Display) -> String {
    error.to_string()
}
pub(crate) fn run(arguments: &[String]) -> Result<(), String> {
    if arguments
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("{HELP}");
        return Ok(());
    }
    let options = Options::parse(arguments)?;
    if std::env::var_os("CARGO_MANIFEST_DIR").is_some()
        || std::env::var_os("CARGO_PKG_NAME").is_some()
    {
        return Err("ordinary dev must start outside cargo run; use scripts/rsiversi.sh dev tui|web or scripts/rsiversi.ps1 dev tui|web".into());
    }
    let root = crate::dist::command_path(
        std::env::current_dir()
            .and_then(|path| path.canonicalize())
            .map_err(problem)?,
    );
    crate::require_repository_root(&root)?;
    #[cfg(windows)]
    if std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map(crate::dist::command_path)
        .map_err(problem)?
        .parent()
        .is_some_and(|path| {
            path == root.join("target/debug") || path == root.join("target/release")
        })
    {
        return Err("use scripts/rsiversi.ps1 dev tui|web so the shared development tool can be relinked while dev runs".into());
    }
    if !cfg!(any(target_os = "linux", windows)) {
        return Err("ordinary development requires Linux/WSL or native Windows".into());
    }
    let caller = std::env::var_os("RSI_DEV_CALLER_CWD").map_or_else(|| root.clone(), PathBuf::from);
    if !caller.is_absolute() || caller.as_os_str().len() > 16 * 1024 {
        return Err("development caller directory must be absolute and bounded".into());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(problem)?
        .block_on(start(root, caller, options))
}
async fn start(root: PathBuf, caller: PathBuf, options: Options) -> Result<(), String> {
    if options.surface == "web" && options.watch {
        repository_pnpm(&root)?;
    }
    let mut signals = Signals::new().map_err(problem)?;
    let parent = root.join("target/rsi-dev");
    std::fs::create_dir_all(&parent).map_err(problem)?;
    let directory = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&parent)
        .map_err(problem)?
        .keep()
        .canonicalize()
        .map_err(problem)?;
    let run_id = format!(
        "{:x}",
        Sha256::digest(directory.as_os_str().as_encoded_bytes())
    )[..32]
        .to_owned();
    eprintln!("Development output: {}", directory.display());
    let cancelled = Arc::new(AtomicBool::new(false));
    let build_cancel = cancelled.clone();
    let build_root = root.clone();
    let surface = options.surface.clone();
    let output = directory.join("publication");
    let mut build = tokio::task::spawn_blocking(move || {
        crate::dist::development(&build_root, &surface, output, build_cancel)
    });
    let artifact = tokio::select! { biased;
        () = signals.interrupted() => { cancelled.store(true, Ordering::SeqCst); let _ = build.await.map_err(problem)?; return Err(format!("build interrupted; retained {}", directory.display())); },
        result = &mut build => result.map_err(problem)?.map_err(|error| format!("{error}; retained {}", directory.display()))?,
    };
    let result = if options.surface == "tui" {
        tui(&artifact, &caller, &options, &mut signals).await
    } else {
        web(
            &root,
            &caller,
            &directory,
            &run_id,
            &artifact,
            &options,
            &mut signals,
        )
        .await
    };
    match result {
        Ok(()) => {
            drop(artifact);
            remove_owned_run(&parent, &directory)?;
            Ok(())
        }
        Err(error) => Err(format!("{error}; retained {}", directory.display())),
    }
}
fn remove_owned_run(parent: &Path, directory: &Path) -> Result<(), String> {
    let parent = parent.canonicalize().map_err(problem)?;
    let resolved = directory.canonicalize().map_err(problem)?;
    if resolved.parent() != Some(parent.as_path())
        || std::fs::symlink_metadata(directory)
            .map_err(problem)?
            .file_type()
            .is_symlink()
    {
        return Err("refusing to remove an unowned development path".into());
    }
    std::fs::remove_dir_all(resolved).map_err(problem)
}
fn binary(artifact: &crate::dist::BuiltApplication) -> PathBuf {
    artifact
        .bundle
        .join(if cfg!(windows) { "rsi.exe" } else { "rsi" })
}
fn product(artifact: &crate::dist::BuiltApplication, caller: &Path) -> Command {
    let mut command = Command::new(binary(artifact));
    command.current_dir(caller).env_remove("RSI_DEV_CALLER_CWD");
    command
}
async fn tui(
    artifact: &crate::dist::BuiltApplication,
    caller: &Path,
    options: &Options,
    signals: &mut Signals,
) -> Result<(), String> {
    #[cfg(windows)]
    if std::env::var_os("WT_SESSION").is_none() {
        return Err("interactive development TUI requires Windows Terminal".into());
    }
    let mut command = product(artifact, caller);
    command.args(["__dev", "tui"]).args(&options.arguments);
    let mut child = Child::spawn_development(command).await?;
    #[cfg(unix)]
    let _foreground = match Foreground::acquire(&child) {
        Ok(foreground) => foreground,
        Err(error) => {
            child.stop().await?;
            return Err(error);
        }
    };
    let outcome = tokio::select! { biased;
        () = signals.interrupted() => Ok(()),
        status = child.wait() => status.map_err(problem).and_then(|status| if application_succeeded(status) { Ok(()) } else { Err(format!("development TUI failed: {status}")) }),
    };
    child.stop().await?;
    outcome
}

struct Peer {
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    run_id: String,
}
impl Peer {
    async fn send(&mut self, message: Message) -> Result<(), String> {
        let frame = Frame::new(&self.run_id, message).map_err(problem)?;
        tokio::time::timeout(
            PROXY_TIMEOUT,
            rsi_dev_control::write(&mut self.input, &frame),
        )
        .await
        .map_err(|_| "control write timeout")?
        .map_err(problem)
    }
    async fn read(&mut self, timeout: std::time::Duration) -> Result<Message, String> {
        tokio::time::timeout(timeout, rsi_dev_control::read(&mut self.output))
            .await
            .map_err(|_| "development readiness timeout")?
            .map_err(problem)?
            .into_message(&self.run_id)
            .map_err(problem)
    }
}
struct Session {
    stop: CancellationToken,
    children: JoinSet<Result<(String, Option<std::process::ExitStatus>), String>>,
    watcher_inputs: Vec<tokio::process::ChildStdin>,
    interrupted: Arc<AtomicBool>,
}
impl Session {
    fn new() -> Self {
        Self {
            stop: CancellationToken::new(),
            children: JoinSet::new(),
            watcher_inputs: Vec::new(),
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }
    fn own(&mut self, mut child: Child, role: &str) {
        let stop = self.stop.clone();
        let role = role.to_owned();
        let interrupted = self.interrupted.clone();
        self.children.spawn(async move {
            let status = tokio::select! { biased;
                () = stop.cancelled() => child.stop_after_eof().await?,
                status = child.wait() => status.map_err(problem)?,
            };
            if application_interrupted(status) {
                interrupted.store(true, Ordering::SeqCst);
            }
            Ok((role, Some(status)))
        });
    }
    async fn peer(
        &mut self,
        mut command: Command,
        role: &str,
        run_id: &str,
    ) -> Result<Peer, String> {
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = Child::spawn_development(command).await?;
        let input = child
            .process
            .stdin
            .take()
            .ok_or("missing child control input")?;
        let output = BufReader::new(
            child
                .process
                .stdout
                .take()
                .ok_or("missing child control output")?,
        );
        self.own(child, role);
        Ok(Peer {
            input,
            output,
            run_id: run_id.into(),
        })
    }
    async fn guard<T>(
        &mut self,
        future: impl std::future::Future<Output = Result<T, String>>,
        signals: &mut Signals,
    ) -> Result<T, String> {
        tokio::select! { biased;
            () = signals.interrupted() => Err(INTERRUPTED.into()),
            result = self.children.join_next() => {
                let (role, status) = result.ok_or("no supervised development children")?.map_err(problem)??;
                if status.is_some_and(application_interrupted) {
                    Err(INTERRUPTED.into())
                } else { Err(format!("{role} stopped unexpectedly: {status:?}")) }
            }
            result = future => result,
        }
    }
    async fn shutdown(&mut self) -> Result<(), String> {
        self.watcher_inputs.clear();
        self.stop.cancel();
        let mut failure = None;
        while let Some(result) = self.children.join_next().await {
            if let Err(error) = result.map_err(problem).and_then(|result| result) {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
async fn web(
    root: &Path,
    caller: &Path,
    directory: &Path,
    run_id: &str,
    artifact: &crate::dist::BuiltApplication,
    options: &Options,
    signals: &mut Signals,
) -> Result<(), String> {
    let mut session = Session::new();
    let result = web_session(
        root,
        caller,
        directory,
        run_id,
        artifact,
        options,
        signals,
        &mut session,
    )
    .await;
    let cleanup = session.shutdown().await;
    cleanup?;
    match result {
        Err(error) if error == INTERRUPTED || session.interrupted.load(Ordering::SeqCst) => Ok(()),
        other => other,
    }
}
#[allow(clippy::too_many_arguments)]
async fn web_session(
    root: &Path,
    caller: &Path,
    directory: &Path,
    run_id: &str,
    artifact: &crate::dist::BuiltApplication,
    options: &Options,
    signals: &mut Signals,
    session: &mut Session,
) -> Result<(), String> {
    let mut frontend = None;
    let mut origin = None;
    let mut assets = artifact.bundle.join("assets");
    let environment = crate::dist::development_environment(root)?;
    if options.watch {
        prepare_frontend(root, directory, &environment, signals).await?;
        assets = directory.join("assets");
        copy_assets(&artifact.bundle.join("assets"), &assets)?;
        let mut command = Command::new("node");
        command
            .arg(root.join("apps/web/development-server.mjs"))
            .current_dir(root.join("apps/web"))
            .env_clear()
            .envs(&environment)
            .stderr(log(directory, "vite.log")?);
        let mut peer = session.peer(command, "Vite", run_id).await?;
        peer.send(Message::StartFrontend {
            port: options.port,
            family: artifact.family.clone(),
        })
        .await?;
        let Message::FrontendBound { origin: reported } =
            session.guard(peer.read(STARTUP_TIMEOUT), signals).await?
        else {
            return Err("expected frontend socket ownership".into());
        };
        let address = rsi_dev_control::loopback_address(&reported).map_err(problem)?;
        if options.port != 0 && address.port() != options.port {
            return Err("frontend reported a different port".into());
        }
        eprintln!("Development frontend: {reported}");
        origin = Some(reported);
        frontend = Some(peer);
        start_renderer_watcher(session, root, directory, &assets, &environment).await?;
    }
    let mut native = product(artifact, caller);
    native.args(["__dev", "web"]);
    let mut backend = session.peer(native, "native Web", run_id).await?;
    backend
        .send(Message::StartBackend {
            port: if options.watch { 0 } else { options.port },
            assets: assets
                .to_str()
                .ok_or("non-UTF-8 development assets path")?
                .into(),
            watch: options.watch,
            frontend_origin: origin.clone(),
        })
        .await?;
    let Message::BackendReady { address } = session
        .guard(backend.read(STARTUP_TIMEOUT), signals)
        .await?
    else {
        return Err("expected backend socket ownership".into());
    };
    session
        .guard(
            release_launch(&mut frontend, &mut backend, &address),
            signals,
        )
        .await?;
    let Message::LaunchReady { url, open_browser } =
        session.guard(backend.read(PROXY_TIMEOUT), signals).await?
    else {
        return Err("expected Web launch readiness".into());
    };
    let expected = origin.unwrap_or_else(|| {
        rsi_dev_control::loopback_origin(address.parse().expect("validated address"))
    });
    if url.split_once('#').map(|(origin, _)| origin) != Some(expected.as_str()) {
        return Err("launch link differs from owned frontend".into());
    }
    println!("rsi web: {url}");
    if options.open && open_browser {
        let opening = async { Ok(open_browser_link(artifact, caller, run_id, &url).await) };
        if session.guard(opening, signals).await?.is_err() {
            eprintln!("Could not open the browser; use the launch link printed above.");
        }
    }
    session
        .guard(std::future::pending::<Result<(), String>>(), signals)
        .await
}
async fn release_launch(
    frontend: &mut Option<Peer>,
    backend: &mut Peer,
    address: &str,
) -> Result<(), String> {
    tokio::time::timeout(PROXY_TIMEOUT, async {
        if let Some(frontend) = frontend {
            frontend
                .send(Message::ConfigureProxy {
                    upstream: format!("http://{address}"),
                })
                .await?;
            if !matches!(frontend.read(PROXY_TIMEOUT).await?, Message::ProxyReady {}) {
                return Err("expected proxy readiness".into());
            }
        }
        backend.send(Message::ReleaseLaunch {}).await
    })
    .await
    .map_err(|_| "proxy readiness timeout".to_owned())?
}
async fn start_renderer_watcher(
    session: &mut Session,
    root: &Path,
    directory: &Path,
    assets: &Path,
    environment: &std::collections::BTreeMap<OsString, OsString>,
) -> Result<(), String> {
    let mut watcher = Command::new("node");
    watcher
        .args([
            root.join("apps/web/renderers.mjs").into_os_string(),
            assets.as_os_str().to_owned(),
            "--watch".into(),
            "--supervised".into(),
        ])
        .current_dir(root)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(log(directory, "renderers.log")?)
        .stderr(log(directory, "renderers-error.log")?);
    let mut watcher = Child::spawn_development(watcher).await?;
    session.watcher_inputs.push(
        watcher
            .process
            .stdin
            .take()
            .ok_or("renderer watcher stdin missing")?,
    );
    session.own(watcher, "renderer watcher");
    Ok(())
}
async fn prepare_frontend(
    root: &Path,
    directory: &Path,
    environment: &std::collections::BTreeMap<OsString, OsString>,
    signals: &mut Signals,
) -> Result<(), String> {
    let pnpm = repository_pnpm(root)?;
    let output = log(directory, "frontend-install.log")?;
    let mut command = Command::new("node");
    command
        .arg(pnpm)
        .args([
            "install",
            "--frozen-lockfile",
            "--ignore-scripts",
            "--store-dir",
        ])
        .arg(root.join("target/rsi-app/pnpm-store"))
        .current_dir(root.join("apps/web"))
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(output.try_clone().map_err(problem)?)
        .stderr(output);
    let mut child = Child::spawn_development(command).await?;
    let result = tokio::select! { biased;
        () = signals.interrupted() => Err(INTERRUPTED.into()),
        status = child.wait() => status.map_err(problem).and_then(|status| if status.success() { Ok(()) } else { Err("frontend dependency preparation failed; inspect frontend-install.log".into()) }),
    };
    child.stop().await?;
    result
}
fn repository_pnpm(root: &Path) -> Result<PathBuf, String> {
    let pnpm = root.join("target/rsi-tools/pnpm/node_modules/pnpm/bin/pnpm.mjs");
    if !pnpm.is_file() {
        return Err("repository pnpm is missing; run the source entrypoint's setup web".into());
    }
    Ok(pnpm)
}
async fn open_browser_link(
    artifact: &crate::dist::BuiltApplication,
    caller: &Path,
    run_id: &str,
    url: &str,
) -> Result<(), String> {
    // This bounded OS handoff deliberately stays outside application Jobs/groups.
    // It invokes the same frozen CLI opener and never starts a product Host.
    let mut command = product(artifact, caller);
    command
        .args(["__dev", "open-browser"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(problem)?;
    let mut input = child.stdin.take().ok_or("opener input missing")?;
    rsi_dev_control::write(
        &mut input,
        &Frame::new(run_id, Message::OpenBrowser { url: url.into() }).map_err(problem)?,
    )
    .await
    .map_err(problem)?;
    drop(input);
    match tokio::time::timeout(std::time::Duration::from_secs(6), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("browser opener unavailable or timed out".into())
        }
    }
}
fn log(directory: &Path, name: &str) -> Result<std::fs::File, String> {
    std::fs::File::create(directory.join(name)).map_err(problem)
}
fn copy_assets(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir(destination).map_err(problem)?;
    for entry in std::fs::read_dir(source).map_err(problem)? {
        let entry = entry.map_err(problem)?;
        if !entry.file_type().map_err(problem)?.is_file() {
            return Err("development assets require flat ordinary files".into());
        }
        std::fs::copy(entry.path(), destination.join(entry.file_name())).map_err(problem)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(&args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>())
    }
    #[test]
    fn ordinary_development_has_no_fixture_or_reset_authority() {
        for args in [
            vec!["tui", "--smoke"],
            vec!["tui", "--", "--reset-state"],
            vec!["web", "--assets", "x"],
            vec!["web", "--", "--port", "42"],
            vec!["web", "--prepare-only"],
            vec!["web", "--port", "42", "--port", "43"],
            vec!["web", "--port", "-1"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        let options = parse(&["web", "--port", "0", "--no-watch", "--no-open"]).unwrap();
        assert_eq!(options.port, 0);
        assert!(!options.watch && !options.open);
        assert!(parse(&["tui"]).is_ok());
    }
    #[test]
    fn writable_renderer_copy_preserves_frozen_input_and_cleanup_is_owned() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("frozen");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("standard.js"), "initial").unwrap();
        let run = root.path().join("run");
        copy_assets(&source, &run).unwrap();
        std::fs::write(run.join("standard.js"), "updated").unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("standard.js")).unwrap(),
            "initial"
        );
        assert!(remove_owned_run(&run, &source).is_err());
        remove_owned_run(root.path(), &run).unwrap();
        assert!(source.exists());
    }
}
