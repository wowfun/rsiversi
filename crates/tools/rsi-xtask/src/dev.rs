//! Explicit isolated development, with the product owning runtime and addon staging.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Debug)]
struct Options {
    surface: String,
    directory: Option<PathBuf>,
    prepare: bool,
    smoke: bool,
    watch: bool,
    port: u16,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let Some(surface) = args
            .first()
            .filter(|value| matches!(value.as_str(), "tui" | "web"))
        else {
            return Err("dev requires tui or web".into());
        };
        let mut value = Self {
            surface: surface.clone(),
            directory: None,
            prepare: false,
            smoke: false,
            watch: true,
            port: 8787,
        };
        let mut args = args[1..].iter();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(option) = args.next() {
            if !seen.insert(option) {
                return Err(format!("duplicate dev option {option}"));
            }
            match option.as_str() {
                "--directory" => {
                    let path = PathBuf::from(
                        args.next()
                            .ok_or("--directory needs an absolute new path")?,
                    );
                    if !path.is_absolute() {
                        return Err("development directory must be absolute".into());
                    }
                    value.directory = Some(path);
                }
                "--prepare-only" => value.prepare = true,
                "--smoke" => value.smoke = true,
                "--no-watch" => value.watch = false,
                "--port" => {
                    if value.surface != "web" {
                        return Err("--port requires dev web".into());
                    }
                    value.port = args
                        .next()
                        .ok_or("--port requires a port")?
                        .parse::<u16>()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or("invalid development port")?;
                }
                _ => return Err(format!("unknown dev option {option}")),
            }
        }
        if value.prepare && value.smoke {
            return Err("select either --prepare-only or --smoke".into());
        }
        Ok(value)
    }
}
pub fn run(args: &[String]) -> Result<(), String> {
    let options = Options::parse(args)?;
    let root = std::env::current_dir().map_err(problem)?;
    super::require_repository_root(&root)?;
    if !cfg!(target_os = "linux") {
        return Err(
            "the isolated product development launcher currently requires Linux or WSL".into(),
        );
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(problem)?
        .block_on(start(root, options))
}
fn problem(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn temporary_environment(preferred: &Path, fallback: &Path) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("rsi-dev-")
        .tempdir_in(preferred)
        .or_else(|_| {
            tempfile::Builder::new()
                .prefix("rsi-dev-")
                .tempdir_in(fallback)
        })
        .map_err(problem)
}
#[derive(Debug)]
struct Development {
    root: PathBuf,
    directory: PathBuf,
    binary: PathBuf,
    log: PathBuf,
    runtime_directory: PathBuf,
}
impl Development {
    fn create(root: PathBuf, options: &Options) -> Result<Self, String> {
        let directory = if let Some(directory) = &options.directory {
            std::fs::create_dir(directory).map_err(problem)?;
            directory.clone()
        } else {
            temporary_environment(Path::new("/var/tmp"), &std::env::temp_dir())?.keep()
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(problem)?;
        }
        for path in ["home", "config/rsi", "state", "cache", "workspace", "logs"] {
            std::fs::create_dir_all(directory.join(path)).map_err(problem)?;
        }
        let runtime = tempfile::Builder::new()
            .prefix("ri-")
            .tempdir_in("/tmp")
            .map_err(problem)?;
        std::fs::write(
            directory.join("runtime-path"),
            runtime.path().as_os_str().as_encoded_bytes(),
        )
        .map_err(problem)?;
        let runtime_directory = runtime.keep();
        Ok(Self {
            root,
            binary: directory.join("rsi"),
            log: directory.join("logs/build.log"),
            directory,
            runtime_directory,
        })
    }
    fn environment(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        let mut values = ["PATH", "LANG", "LC_ALL", "TERM", "COLORTERM", "SHELL"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
            .collect::<Vec<_>>();
        values.push((
            "DBUS_SESSION_BUS_ADDRESS",
            format!(
                "unix:path={}/absent-session-bus",
                self.runtime_directory.display()
            )
            .into(),
        ));
        for (name, path) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            values.push((name, self.directory.join(path).into_os_string()));
        }
        values.push((
            "XDG_RUNTIME_DIR",
            self.runtime_directory.clone().into_os_string(),
        ));
        values
    }
    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .env_clear()
            .envs(self.environment())
            .args(args)
            .current_dir(self.directory.join("workspace"));
        command
    }
    fn write(&self, path: &str, value: impl AsRef<[u8]>) -> Result<(), String> {
        let path = self.directory.join(path);
        std::fs::create_dir_all(path.parent().ok_or("missing parent")?).map_err(problem)?;
        std::fs::write(path, value).map_err(problem)
    }
    fn toml(&self, path: &str, value: &Value) -> Result<(), String> {
        self.write(path, toml::to_string(&toml_value(value)?).map_err(problem)?)
    }
    fn profile(&self, name: &str, application: bool, steps: Vec<Value>) -> Result<(), String> {
        let role = if application { "application" } else { "host" };
        let mut profile = json!({"format":1});
        profile["steps"] = Value::Array(steps);
        self.toml(
            &format!("config/rsi/{role}-profiles/{name}/{role}.profile.toml"),
            &profile,
        )
    }
    fn native_target(&self) -> String {
        self.root
            .join("target/dev-native/cache")
            .to_string_lossy()
            .into_owned()
    }
    fn native_output(&self) -> String {
        use sha2::{Digest as _, Sha256};
        format!(
            "target/dev-native/artifacts/{:x}",
            Sha256::digest(self.directory.as_os_str().as_encoded_bytes())
        )
    }
    fn build_environment(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        let mut environment = self.environment();
        for (name, directory) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
            if let Some(value) = std::env::var_os(name).or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(directory).into_os_string())
            }) {
                environment.push((name, value));
            }
        }
        for name in ["RSI_WASM_BINDGEN", "CARGO_BUILD_JOBS"] {
            if let Some(value) = std::env::var_os(name) {
                environment.push((name, value));
            }
        }
        environment
    }
    fn native_builder(
        &self,
        manifest: &str,
        library: &str,
        environment: &[(&str, std::ffi::OsString)],
    ) -> Result<PathBuf, String> {
        use std::os::unix::fs::PermissionsExt as _;
        let outputs = self.root.join(self.native_output());
        std::fs::create_dir_all(&outputs).map_err(problem)?;
        self.write("native-output-path", outputs.as_os_str().as_encoded_bytes())?;
        let artifact = format!(
            "{}{library}{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        );
        let destination = outputs.join(&artifact);
        let temporary = outputs.join(format!("{artifact}.tmp"));
        let quote = |path: &Path| -> Result<String, String> {
            Ok(shell_argument(
                path.to_str().ok_or("non-UTF-8 development path")?,
            ))
        };
        let body = format!(
            "set -eu\ncd {}\ncargo build --locked --manifest-path {} --target-dir {} --target {}\ncp {} {}\nmv {} {}\n",
            quote(&self.root)?,
            shell_argument(manifest),
            shell_argument(&self.native_target()),
            shell_argument(env!("RSI_XTASK_TARGET")),
            quote(
                &PathBuf::from(self.native_target())
                    .join(env!("RSI_XTASK_TARGET"))
                    .join("debug")
                    .join(artifact)
            )?,
            quote(&temporary)?,
            quote(&temporary)?,
            quote(&destination)?
        );
        let script = format!(
            "#!/bin/sh\nexec env -i {} flock --exclusive {} /bin/sh -c {}\n",
            shell_environment(environment)?,
            quote(&self.root.join("target/dev-native/build.lock"))?,
            shell_argument(&body)
        );
        let name = format!("build-{library}");
        self.write(&name, script)?;
        let path = self.directory.join(name);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(problem)?;
        Ok(path)
    }
    fn remove(&self) -> Result<(), String> {
        for path in [
            self.root.join(self.native_output()),
            self.runtime_directory.clone(),
            self.directory.clone(),
        ] {
            if path.exists() {
                std::fs::remove_dir_all(path).map_err(problem)?;
            }
        }
        Ok(())
    }
    async fn addon(&self, manifest: &str, id: &str, signals: &mut Signals) -> Result<(), String> {
        logged(
            self.command(&[
                "addon",
                "install",
                self.directory
                    .join(manifest)
                    .to_str()
                    .ok_or("non-UTF-8 development path")?,
            ]),
            &self.log,
            signals,
        )
        .await?;
        logged(self.command(&["addon", "enable", id]), &self.log, signals).await
    }
}
fn toml_value(value: &Value) -> Result<toml::Value, String> {
    Ok(match value {
        Value::Null => return Err("null has no TOML representation".into()),
        Value::Bool(value) => toml::Value::Boolean(*value),
        Value::String(value) => toml::Value::String(value.clone()),
        Value::Number(value) => toml::Value::Integer(
            value
                .as_i64()
                .ok_or("generated Profile integer exceeds TOML bounds")?,
        ),
        Value::Array(values) => {
            toml::Value::Array(values.iter().map(toml_value).collect::<Result<_, _>>()?)
        }
        Value::Object(values) => toml::Value::Table(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), toml_value(value)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}
fn step(id: &str, plugin: &str, config: Option<Value>) -> Value {
    let mut value = json!({"kind":"plugin","id":id,"plugin":plugin});
    if let Some(config) = config {
        value["config"] = config;
    }
    value
}
async fn start(root: PathBuf, options: Options) -> Result<(), String> {
    let mut signals = Signals::new().map_err(problem)?;
    let dev = Development::create(root, &options)?;
    let result = start_development(&dev, &options, &mut signals).await;
    if result.is_ok() && options.directory.is_none() && !options.prepare {
        dev.remove()?;
    }
    result
}
async fn start_development(
    dev: &Development,
    options: &Options,
    signals: &mut Signals,
) -> Result<(), String> {
    eprintln!(
        "Preparing {} development in {}. Build log: {}",
        options.surface,
        dev.directory.display(),
        dev.log.display()
    );
    let mut build = tokio::process::Command::new("cargo");
    build
        .env_clear()
        .envs(dev.build_environment())
        .current_dir(&dev.root)
        .args(["build", "--locked", "-p", "rsi", "--target-dir"])
        .arg(dev.root.join("target"))
        .args(["--target", env!("RSI_XTASK_TARGET")]);
    logged(build, &dev.log, signals).await?;
    std::fs::copy(
        dev.root
            .join("target")
            .join(env!("RSI_XTASK_TARGET"))
            .join("debug/rsi"),
        &dev.binary,
    )
    .map_err(problem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dev.binary, std::fs::Permissions::from_mode(0o700))
            .map_err(problem)?;
    }
    write_launcher(dev)?;
    prepare(dev, options, signals).await?;
    println!(
        "{}",
        json!({"event":"dev-prepared","directory":dev.directory,"binary":dev.binary,"profile":format!("dev-{}",options.surface),"build_log":dev.log})
    );
    if options.prepare {
        return Ok(());
    }
    if options.smoke {
        logged(
            dev.command(&[
                "--profile",
                "dev-headless",
                "local development smoke",
                "--output",
                "jsonl",
            ]),
            &dev.directory.join("logs/smoke.jsonl"),
            signals,
        )
        .await?;
        return validate_smoke(&dev.directory.join("logs/smoke.jsonl"));
    }
    launch(dev, options, signals).await
}
fn validate_smoke(path: &Path) -> Result<(), String> {
    if std::fs::metadata(path).map_err(problem)?.len() > 1024 * 1024 {
        return Err("development smoke output exceeds 1 MiB".into());
    }
    let source = std::fs::read_to_string(path).map_err(problem)?;
    let records = source
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).map_err(problem))
        .collect::<Result<Vec<_>, _>>()?;
    let outcome = records
        .last()
        .ok_or("development smoke returned no records")?;
    if outcome["type"] != "outcome" || outcome["outcome"]["status"] != "completed" {
        return Err("development smoke has no completed outcome".into());
    }
    let matching = |record: &&Value| {
        record["session_id"] == outcome["session_id"]
            && record["fact"]["turn_id"] == outcome["turn_id"]
    };
    let selected = records.iter().filter(matching).collect::<Vec<_>>();
    if !selected
        .iter()
        .any(|record| record["fact"]["event"]["delta"]["value"] == "dev: native Language")
        || !selected.iter().any(|record| {
            record["fact"]["type"] == "turn_terminal"
                && record["fact"]["outcome"]["status"] == "completed"
        })
    {
        return Err(
            "development smoke lacks its native-provider or durable-terminal evidence".into(),
        );
    }
    Ok(())
}
fn shell_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn shell_environment(environment: &[(&str, std::ffi::OsString)]) -> Result<String, String> {
    environment
        .iter()
        .map(|(name, value)| {
            Ok(shell_argument(&format!(
                "{name}={}",
                value.to_str().ok_or("non-UTF-8 development environment")?
            )))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|values| values.join(" "))
}
fn write_launcher(dev: &Development) -> Result<(), String> {
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut binary = std::fs::File::open(&dev.binary).map_err(problem)?;
    let mut digest = Sha256::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let length = binary.read(&mut chunk).map_err(problem)?;
        if length == 0 {
            break;
        }
        digest.update(&chunk[..length]);
    }
    dev.write("binary.sha256", format!("{:x}  rsi\n", digest.finalize()))?;
    let mut script = format!(
        "#!/bin/sh\ncd {} || exit 1\nexec env -i",
        shell_argument(
            dev.directory
                .join("workspace")
                .to_str()
                .ok_or("non-UTF-8 development path")?
        )
    );
    for (name, value) in dev.environment() {
        script.push(' ');
        script.push_str(&shell_argument(&format!(
            "{name}={}",
            value.to_str().ok_or("non-UTF-8 development environment")?
        )));
    }
    script.push(' ');
    script.push_str(&shell_argument(
        dev.binary.to_str().ok_or("non-UTF-8 development path")?,
    ));
    script.push_str(" \"$@\"\n");
    dev.write("run", script)?;
    std::fs::set_permissions(
        dev.directory.join("run"),
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(problem)
}
async fn prepare(
    dev: &Development,
    options: &Options,
    signals: &mut Signals,
) -> Result<(), String> {
    let builder = dev.native_builder(
        "fixtures/rsi/native-addon/Cargo.toml",
        "rsi_fixture_native_addon",
        &dev.build_environment(),
    )?;
    let mut command = tokio::process::Command::new(builder);
    command.env_clear().envs(dev.environment());
    logged(command, &dev.log, signals).await?;
    let artifact = format!(
        "{}/{}rsi_fixture_native_addon{}",
        dev.native_output(),
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    dev.toml("provider.toml",&json!({"format":2,"scope":"service","id":"dev.provider","plugin":"fixture.native-addon","target":target,"artifact":artifact,"source_root":dev.root}))?;
    dev.addon("provider.toml", "dev.provider", signals).await?;
    dev.write(
        "config/rsi/settings.json",
        serde_json::to_vec(
            &json!({"rsi.agent":{"default_model":{"deployment":"native","model":"native-text"}}}),
        )
        .map_err(problem)?,
    )?;
    dev.profile("dev",false,vec![step("provider","fixture.native-addon",Some(json!({"label":"dev","tools":false,"ai":true}))),step("adapter","rsi.ai.portable",Some(json!({"service":"fixture.native.ai","deployment":"native","provider_family":"fixture","protocol":"fixture-v1","endpoint_fingerprint":"local-fixture","language":true,"image":false})))])?;
    dev.profile(
        "dev-headless",
        true,
        vec![
            step(
                "connection",
                "rsi.application.connection",
                Some(json!({"host_profile":"dev"})),
            ),
            step("application", "rsi.application.headless", None),
        ],
    )?;
    if options.surface == "tui" {
        prepare_tui(dev, &target, signals).await
    } else {
        prepare_web(dev, options.watch, signals).await
    }
}
async fn prepare_tui(dev: &Development, target: &str, signals: &mut Signals) -> Result<(), String> {
    let builder = dev.native_builder(
        "crates/rsi/terminal-native/Cargo.toml",
        "rsi_terminal_native",
        &dev.build_environment(),
    )?;
    let mut command = tokio::process::Command::new(&builder);
    command.env_clear().envs(dev.environment());
    logged(command, &dev.log, signals).await?;
    dev.toml("terminal.toml",&json!({"format":2,"scope":"application","id":"dev.terminal","plugin":"rsi.terminal.native","target":target,"artifact":format!("{}/{}rsi_terminal_native{}",dev.native_output(),std::env::consts::DLL_PREFIX,std::env::consts::DLL_SUFFIX),"source_root":dev.root,"build":{"command":[builder],"watch":["Cargo.toml","Cargo.lock","crates/rsi/terminal-native/src","crates/rsi/terminal-native/Cargo.toml","crates/rsi/terminal-native/Cargo.lock","crates/rsi/terminal-ui","crates/rsi/ui-protocol","crates/rsi-meta/native"],"timeout_seconds":600}}))?;
    dev.addon("terminal.toml", "dev.terminal", signals).await?;
    dev.profile("dev-tui",true,vec![step("connection","rsi.application.connection",Some(json!({"host_profile":"dev"}))),step("ui","rsi.ui",None),step("target","rsi.ui.target",Some(json!("application"))),step("session-ui","rsi.session.ui",None),step("tree-ui","rsi.session.tree.ui",None),step("files-ui","rsi.session.files.ui",None),step("application","rsi.application.tui",Some(json!({"presentation":[{"id":"native","plugin":"rsi.terminal.native"},{"id":"adapter","plugin":"rsi.terminal.portable"}]})))])
}
async fn prepare_web(dev: &Development, watch: bool, signals: &mut Signals) -> Result<(), String> {
    let assets = dev.directory.join("web");
    let mut install = tokio::process::Command::new("npm");
    install
        .env_clear()
        .envs(dev.build_environment())
        .current_dir(dev.root.join("plugins/rsi/web"))
        .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"]);
    logged(install, &dev.log, signals).await?;
    let mut command = tokio::process::Command::new("node");
    command
        .env_clear()
        .envs(dev.build_environment())
        .current_dir(&dev.root)
        .arg("plugins/rsi/web/build.mjs")
        .arg(&assets)
        .arg("--dev");
    logged(command, &dev.log, signals).await?;
    dev.profile(
        "dev-web",
        true,
        vec![
            step(
                "service",
                "rsi.application.service",
                Some(json!({"host_profile":"dev"})),
            ),
            step(
                "assets",
                "rsi.web.assets",
                Some(json!({"directory":assets,"watch":watch})),
            ),
            step("http", "rsi.application.serve-web", None),
        ],
    )
}
async fn launch(dev: &Development, options: &Options, signals: &mut Signals) -> Result<(), String> {
    let mut watcher = if options.watch {
        let mut command = if options.surface == "tui" {
            dev.command(&[
                "addon",
                "watch",
                dev.directory
                    .join("terminal.toml")
                    .to_str()
                    .ok_or("non-UTF-8 path")?,
                "--enable",
            ])
        } else {
            let mut command = tokio::process::Command::new("node");
            command
                .env_clear()
                .envs(dev.build_environment())
                .current_dir(&dev.root)
                .arg("plugins/rsi/web/renderers.mjs")
                .arg(dev.directory.join("web"))
                .arg("--watch");
            command
        };
        redirect(&mut command, &dev.directory.join("logs/watch.log"))?;
        Some(Child::spawn(command, true)?)
    } else {
        None
    };
    let mut frontend = None;
    let mut command = dev.command(&["--profile", &format!("dev-{}", options.surface)]);
    if options.surface == "web" {
        let origin = format!("http://127.0.0.1:{}", options.port);
        let bind = if options.watch {
            let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(problem)?;
            let address = reserved.local_addr().map_err(problem)?;
            let upstream = format!("http://{address}");
            let mut vite = tokio::process::Command::new("node");
            vite.env_clear()
                .envs(dev.build_environment())
                .env("RSI_DEV_UPSTREAM", upstream)
                .current_dir(dev.root.join("plugins/rsi/web"))
                .arg("node_modules/vite/bin/vite.js")
                .args(["--host", "127.0.0.1", "--port", &options.port.to_string()]);
            redirect(&mut vite, &dev.directory.join("logs/vite.log"))?;
            frontend = Some(Child::spawn(vite, true)?);
            address.to_string()
        } else {
            format!("127.0.0.1:{}", options.port)
        };
        command.args(["--bind", &bind, "--origin", &origin, "--dev-http"]);
        eprintln!(
            "Open {origin}. In another terminal, run {} --profile devices register dev-web and paste the receipt into the sign-in form. Select 'Allow local HTTP for development' before connecting.",
            shell_argument(
                dev.directory
                    .join("run")
                    .to_str()
                    .ok_or("non-UTF-8 development path")?
            )
        );
    }
    let mut application = Child::spawn(command, true)?;
    let _foreground = if options.surface == "tui" {
        Foreground::acquire(&application)?
    } else {
        None
    };
    let result = tokio::select! {biased;
        ()=signals.interrupted()=>{application.stop().await?;true},
        result=application.wait()=>application_succeeded(result.map_err(problem)?),
        stopped=async{match &mut watcher{Some(watcher)=>watcher.wait().await,None=>std::future::pending().await}}=>{stopped.map_err(problem)?;application.stop().await?;return Err("frontend source watcher stopped; inspect logs/watch.log".into());},
        stopped=async{match &mut frontend{Some(frontend)=>frontend.wait().await,None=>std::future::pending().await}}=>{stopped.map_err(problem)?;application.stop().await?;return Err("Vite stopped; inspect logs/vite.log".into());},
    };
    if let Some(frontend) = &mut frontend {
        frontend.stop().await?;
    }
    if let Some(watcher) = &mut watcher {
        watcher.stop().await?;
    }
    if result {
        Ok(())
    } else {
        Err(format!(
            "development application failed; inspect {}",
            dev.directory.display()
        ))
    }
}
fn redirect(command: &mut tokio::process::Command, path: &Path) -> Result<(), String> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(problem)?;
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(problem)?)
        .stderr(log);
    Ok(())
}
async fn logged(
    mut command: tokio::process::Command,
    path: &Path,
    signals: &mut Signals,
) -> Result<(), String> {
    redirect(&mut command, path)?;
    let mut child = Child::spawn(command, true)?;
    let status = tokio::select! {biased; ()=signals.interrupted()=>{child.stop().await?;return Err(format!("development preparation interrupted; log: {}",path.display()));}, status=child.wait()=>status.map_err(problem)?};
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "development command failed ({status}); log: {}",
            path.display()
        ))
    }
}
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
    stopped: bool,
}
impl Signals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
            stopped: false,
        })
    }
    async fn interrupted(&mut self) {
        if !self.stopped {
            tokio::select! {
                _ = self.interrupt.recv() => {},
                _ = self.terminate.recv() => {},
                _ = self.hangup.recv() => {},
                _ = self.quit.recv() => {},
            }
            self.stopped = true;
        }
    }
}
fn application_succeeded(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.success()
        || status.code() == Some(130)
        || status.signal() == Some(rustix::process::Signal::INT.as_raw())
}
struct Child {
    name: String,
    process: tokio::process::Child,
    group: Option<rustix::process::Pid>,
}
struct Foreground(nix::unistd::Pid);
impl Foreground {
    fn acquire(child: &Child) -> Result<Option<Self>, String> {
        use std::io::IsTerminal as _;
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let previous = nix::unistd::tcgetpgrp(std::io::stdin()).map_err(problem)?;
        let group = child.group.ok_or("TUI child has no process group")?;
        Self::set(nix::unistd::Pid::from_raw(group.as_raw_nonzero().get())).map_err(problem)?;
        // A child that read stdin before the handoff may have received SIGTTIN.
        child.signal(rustix::process::Signal::CONT);
        Ok(Some(Self(previous)))
    }
    fn set(group: nix::unistd::Pid) -> nix::Result<()> {
        use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
        let mut blocked = SigSet::empty();
        blocked.add(Signal::SIGTTOU);
        let mut previous = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))?;
        // Synchronous: the mask is restored on this same OS thread before returning.
        let result = nix::unistd::tcsetpgrp(std::io::stdin(), group);
        let restored = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
        result.and(restored)
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = Self::set(self.0);
    }
}
impl Child {
    fn spawn(mut command: tokio::process::Command, group: bool) -> Result<Self, String> {
        #[cfg(unix)]
        if group {
            command.process_group(0);
        }
        command.kill_on_drop(true);
        let name = command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();
        let child = command.spawn().map_err(problem)?;
        let group = if group {
            child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .and_then(rustix::process::Pid::from_raw)
        } else {
            None
        };
        Ok(Self {
            name,
            process: child,
            group,
        })
    }
    fn signal(&self, signal: rustix::process::Signal) {
        if let Some(group) = self.group {
            let _ = rustix::process::kill_process_group(group, signal);
        } else if let Some(pid) = self
            .process
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, signal);
        }
    }
    async fn stop(&mut self) -> Result<(), String> {
        self.stop_with_timeout(Duration::from_secs(15)).await
    }
    async fn exited(&self) -> std::io::Result<()> {
        use rustix::process::{WaitIdOptions, waitid};
        let Some(group) = self.group else {
            return Ok(());
        };
        loop {
            match waitid(
                rustix::process::WaitId::Pid(group),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
            ) {
                Ok(Some(_)) => return Ok(()),
                Ok(None) | Err(rustix::io::Errno::INTR) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.exited().await?;
        // WNOWAIT pins the group ID until every signal has been sent. Never retain
        // a numeric group ID after reaping: it could then belong to another owner.
        self.signal(rustix::process::Signal::KILL);
        self.group = None;
        self.process.wait().await
    }
    async fn stop_with_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        self.signal(rustix::process::Signal::TERM);
        if let Ok(result) = tokio::time::timeout(timeout, self.wait()).await {
            result.map_err(problem)?;
            Ok(())
        } else {
            self.signal(rustix::process::Signal::KILL);
            let name = format!("{} (PID {:?})", self.name, self.process.id());
            kill_completion(self.wait(), &name).await
        }
    }
}
async fn kill_completion(
    wait: impl std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
    name: &str,
) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(2), wait).await {
        Ok(result) => {
            result.map_err(problem)?;
            Err(format!(
                "development child {name} exceeded cleanup deadline; killed"
            ))
        }
        Err(_) => Err(format!(
            "development child {name} did not exit within 2 seconds after KILL; cleanup incomplete"
        )),
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.signal(rustix::process::Signal::KILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn unresponsive_killed_child_reports_a_bounded_incomplete_cleanup() {
        let start = tokio::time::Instant::now();
        let error = kill_completion(std::future::pending(), "fixture PID 123")
            .await
            .unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        assert!(error.contains("fixture PID 123"));
        assert!(error.contains("cleanup incomplete"));
    }
    #[test]
    fn temporary_environment_falls_back_when_preferred_location_is_unusable() {
        let parent = tempfile::tempdir().unwrap();
        let blocked = parent.path().join("file");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let environment = temporary_environment(&blocked, parent.path()).unwrap();
        assert_eq!(environment.path().parent(), Some(parent.path()));
        assert!(Options::parse(&["tui".into(), "--port".into(), "9000".into()]).is_err());
        assert_eq!(
            Options::parse(&["web".into(), "--port".into(), "9000".into()])
                .unwrap()
                .port,
            9000
        );
    }
    #[test]
    fn generated_profile_numbers_remain_toml_integers_and_credentials_are_absent() {
        let value = json!({"format":2,"build":{"timeout_seconds":600},"language":true});
        let encoded = toml::to_string(&toml_value(&value).unwrap()).unwrap();
        let parsed: toml::Value = toml::from_str(&encoded).unwrap();
        assert_eq!(parsed["format"].as_integer(), Some(2));
        assert_eq!(parsed["build"]["timeout_seconds"].as_integer(), Some(600));
        assert!(!encoded.contains("private::Number"));
        assert!(toml_value(&json!(u64::MAX)).is_err());
        assert!(toml_value(&Value::Null).is_err());
    }
    #[test]
    fn launcher_preserves_literal_arguments_and_excludes_ambient_secrets() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("dev 'quote' $(not-a-command)");
        let options = Options::parse(&[
            "tui".into(),
            "--directory".into(),
            directory.to_str().unwrap().into(),
            "--prepare-only".into(),
        ])
        .unwrap();
        let dev = Development::create(parent.path().to_owned(), &options).unwrap();
        assert!(dev.runtime_directory.as_os_str().len() < 24);
        assert_eq!(
            std::fs::read_to_string(directory.join("runtime-path")).unwrap(),
            dev.runtime_directory.to_str().unwrap()
        );
        std::fs::write(&dev.binary, "#!/usr/bin/python3\nimport os,json,sys\nprint(json.dumps({'args':sys.argv[1:], 'home':os.getenv('HOME'), 'private':os.getenv('RSI_TEST_SECRET'), 'cwd':os.getcwd(), 'cargo':os.getenv('CARGO_HOME')}))\n").unwrap();
        std::fs::set_permissions(&dev.binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_launcher(&dev).unwrap();
        let argument = "literal `not-a-command` $(not-a-command) 'quote'\nsecond line";
        let output = std::process::Command::new(directory.join("run"))
            .arg(argument)
            .env("RSI_TEST_SECRET", "fixture-only")
            .output()
            .unwrap();
        assert!(output.status.success());
        let output: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output["args"], json!([argument]));
        assert_eq!(output["home"], json!(directory.join("home")));
        assert_eq!(output["cwd"], json!(directory.join("workspace")));
        assert!(output["private"].is_null());
        assert!(
            output["cargo"].is_null(),
            "product must not inherit build credentials"
        );
        assert_eq!(
            std::fs::metadata(directory.join("run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::remove_dir_all(&dev.runtime_directory).unwrap();
    }
    #[test]
    fn smoke_requires_matching_provider_text_and_durable_completion() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("smoke.jsonl");
        let mut records = vec![
            json!({"session_id":"s","fact":{"turn_id":"t","event":{"delta":{"value":"dev: native Language"}}}}),
            json!({"session_id":"s","fact":{"turn_id":"t","type":"turn_terminal","outcome":{"status":"completed"}}}),
            json!({"type":"outcome","session_id":"s","turn_id":"t","outcome":{"status":"completed"}}),
        ];
        let write = |records: &[Value]| {
            std::fs::write(
                &path,
                records
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
        };
        write(&records);
        assert!(validate_smoke(&path).is_ok());
        records[0]["session_id"] = json!("foreign");
        write(&records);
        assert!(validate_smoke(&path).is_err());
        records[0]["session_id"] = json!("s");
        records[1]["fact"]["outcome"]["status"] = json!("failed");
        write(&records);
        assert!(validate_smoke(&path).is_err());
    }

    #[test]
    fn native_compilation_cache_is_reused_across_development_directories() {
        let root = tempfile::tempdir().unwrap();
        let first = Development::create(
            root.path().into(),
            &Options::parse(&["tui".into()]).unwrap(),
        )
        .unwrap();
        let second = Development::create(
            root.path().into(),
            &Options::parse(&["tui".into()]).unwrap(),
        )
        .unwrap();
        let targets = (first.native_target(), second.native_target());
        for dev in [first, second] {
            std::fs::remove_dir_all(dev.directory).unwrap();
            std::fs::remove_dir_all(dev.runtime_directory).unwrap();
        }
        assert_eq!(
            targets.0, targets.1,
            "random state directories must not duplicate the compilation cache"
        );
    }

    #[tokio::test]
    async fn shared_build_cache_copies_each_concurrent_build_before_unlocking() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let cargo = bin.join("cargo");
        // Competing builds write the same Cargo output. A lock over compilation
        // alone would allow the second build to replace the first one's copy.
        std::fs::write(&cargo, "#!/bin/sh\nset -eu\nwhile [ $# -gt 0 ]; do case $1 in --manifest-path) shift; label=$1;; --target-dir) shift; target=$1;; --target) shift; triple=$1;; esac; shift; done\nmkdir -p \"$target/$triple/debug\"\nprintf '%s' \"$label\" > \"$target/$triple/debug/libfixture.so\"\nsleep 0.1\n").unwrap();
        std::fs::set_permissions(cargo, std::fs::Permissions::from_mode(0o700)).unwrap();
        let options = Options::parse(&["tui".into()]).unwrap();
        let first = Development::create(root.path().into(), &options).unwrap();
        let second = Development::create(root.path().into(), &options).unwrap();
        let environment = [("PATH", format!("{}:/usr/bin:/bin", bin.display()).into())];
        let a = first
            .native_builder("first ' literal", "fixture", &environment)
            .unwrap();
        let b = second
            .native_builder("second $(literal)", "fixture", &environment)
            .unwrap();
        let (a, b) = tokio::join!(
            tokio::process::Command::new(a).status(),
            tokio::process::Command::new(b).status()
        );
        assert!(a.unwrap().success() && b.unwrap().success());
        for (dev, expected) in [(&first, "first ' literal"), (&second, "second $(literal)")] {
            assert_eq!(
                std::fs::read_to_string(
                    root.path().join(dev.native_output()).join("libfixture.so")
                )
                .unwrap(),
                expected
            );
            assert_eq!(
                std::fs::read_to_string(dev.directory.join("native-output-path")).unwrap(),
                root.path().join(dev.native_output()).to_str().unwrap()
            );
        }
        first.remove().unwrap();
        assert!(second.directory.exists());
        assert!(root.path().join(second.native_output()).exists());
        assert!(Path::new(&first.native_target()).exists());
        second.remove().unwrap();
        assert!(!first.directory.exists() && !first.runtime_directory.exists());
        assert!(!second.directory.exists() && !second.runtime_directory.exists());
    }
    #[test]
    fn invalid_dev_arguments_do_not_create_the_requested_directory() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("must-not-exist");
        let args = [
            "tui".to_owned(),
            "--directory".into(),
            directory.to_str().unwrap().into(),
            "--unknown".into(),
        ];
        assert!(super::run(&args).is_err());
        assert!(!directory.exists());
        for args in [
            vec!["tui", "--prepare-only", "--smoke"],
            vec!["web", "--port", "0"],
            vec!["tui", "--directory", "relative"],
            vec!["tui", "--no-watch", "--no-watch"],
        ] {
            assert!(
                Options::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err()
            );
        }
    }
    #[cfg(target_os = "linux")]
    fn running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.split_once(") ").unwrap().1.starts_with('Z'))
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn process_group_cleanup_reaches_descendants_after_the_leader_exits() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 60 & printf '%s' $! > \"$1\"", "fixture"])
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = Child::spawn(command, true).unwrap();
        child.exited().await.unwrap();
        let pid = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert!(running(pid));
        drop(child);
        tokio::time::timeout(Duration::from_secs(2), async {
            while running(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn explicit_stop_kills_term_ignoring_descendants_before_reaping_leader() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "(trap '' TERM; printf '%s' ready > \"$1.ready\"; exec sleep 60) & printf '%s' $! > \"$1\"; while [ ! -f \"$1.ready\" ]; do sleep 0.01; done", "fixture"]).arg(&path);
        let mut child = Child::spawn(command, true).unwrap();
        child.exited().await.unwrap();
        let pid = std::fs::read_to_string(path).unwrap().parse().unwrap();
        assert!(running(pid));
        child
            .stop_with_timeout(Duration::from_millis(100))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while running(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("explicit stop must finish cleanup while Child remains alive");
        drop(child);
    }

    // Process-global signal handlers are exercised only in this isolated child.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn signal_fixture() {
        use std::os::unix::fs::PermissionsExt as _;
        let Some(directory) = std::env::var_os("RSI_DEV_SIGNAL_FIXTURE") else {
            return;
        };
        let parent = PathBuf::from(directory);
        let options = Options::parse(&[
            std::env::var("RSI_DEV_SIGNAL_SURFACE").unwrap_or("web".into()),
            "--directory".into(),
            parent.join("dev").to_str().unwrap().into(),
            "--no-watch".into(),
        ])
        .unwrap();
        let dev = Development::create(parent.clone(), &options).unwrap();
        let source = format!(
            "#!/bin/sh\ntrap 'exit 0' TERM\nsleep 60 &\nprintf '%s %s' $$ $! > {}\nwait\n",
            shell_argument(parent.join("ready").to_str().unwrap())
        );
        std::fs::write(&dev.binary, source).unwrap();
        std::fs::set_permissions(&dev.binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut signals = Signals::new().unwrap();
        if std::env::var_os("RSI_DEV_SIGNAL_PREPARATION").is_some() {
            let error = logged(dev.command(&[]), &dev.log, &mut signals)
                .await
                .unwrap_err();
            assert!(error.contains("preparation interrupted"));
        } else {
            launch(&dev, &options, &mut signals).await.unwrap();
        }
        dev.remove().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn signals_stop_application_and_preparation_groups() {
        use rustix::process::Signal;
        for surface in ["tui", "web"] {
            for preparation in [false, true] {
                for signal in [Signal::INT, Signal::TERM, Signal::HUP, Signal::QUIT] {
                    let temp = tempfile::tempdir().unwrap();
                    let mut command =
                        tokio::process::Command::new(std::env::current_exe().unwrap());
                    command
                        .args(["--exact", "dev::tests::signal_fixture", "--nocapture"])
                        .env("RSI_DEV_SIGNAL_FIXTURE", temp.path())
                        .env("RSI_DEV_SIGNAL_SURFACE", surface)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());
                    if preparation {
                        command.env("RSI_DEV_SIGNAL_PREPARATION", "1");
                    }
                    let mut fixture = Child::spawn(command, true).unwrap();
                    let pids = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let Ok(source) = std::fs::read_to_string(temp.path().join("ready")) {
                                let pids = source
                                    .split_whitespace()
                                    .map(str::parse::<u32>)
                                    .collect::<Result<Vec<_>, _>>()
                                    .unwrap();
                                if pids.len() == 2 {
                                    break pids;
                                }
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    rustix::process::kill_process(fixture.group.unwrap(), signal).unwrap();
                    assert!(
                        tokio::time::timeout(Duration::from_secs(5), fixture.wait())
                            .await
                            .unwrap()
                            .unwrap()
                            .success(),
                        "{signal:?}, preparation={preparation}"
                    );
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while pids.iter().any(|pid| running(*pid)) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("supervised group survived launcher shutdown");
                }
            }
        }
    }

    #[test]
    fn interrupt_exit_is_success_even_if_child_wait_wins_the_signal_race() {
        use std::os::unix::process::ExitStatusExt as _;
        assert!(application_succeeded(std::process::ExitStatus::from_raw(2)));
        assert!(application_succeeded(std::process::ExitStatus::from_raw(
            130 << 8
        )));
        assert!(!application_succeeded(std::process::ExitStatus::from_raw(
            1 << 8
        )));
    }
}
