//! Native paired publication; build inputs never inherit provider secrets.
mod addon;
mod publication;
mod source;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn run(arguments: &[String]) -> std::result::Result<(), String> {
    execute(arguments).map_err(|error| format!("paired distribution: {error}"))
}
fn execute(arguments: &[String]) -> Result<()> {
    let root = command_path(std::env::current_dir()?.canonicalize()?);
    crate::require_repository_root(&root)?;
    if arguments == ["--help"] {
        println!(
            "usage: rsi-app-tools dist web|desktop [ABSOLUTE_OUTPUT] [--debug] | gc | addon-workbench ABSOLUTE_NEW_REPORT"
        );
        return Ok(());
    }
    let options = Options::parse(arguments)?;
    if !cfg!(any(target_os = "linux", windows)) {
        return Err("paired publication requires Linux or Windows".into());
    }
    if cfg!(windows) && options.kind == "desktop" {
        return Err("Windows Desktop publication is not supported".into());
    }
    let cache = root.join("target/rsi-app");
    fs::create_dir_all(&cache)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("build.lock"))?;
    eprintln!("Waiting for the paired build owner...");
    lock.lock()?;
    if options.kind == "gc" {
        let removed = publication::collect(&cache)?;
        println!("{}", json!({"event":"application-gc", "removed":removed}));
        return Ok(());
    }
    if let Some(output) = &options.output {
        validate_external_output(&cache, output)?;
    }
    if options.kind == "addon-workbench" {
        return addon::run(&root, &cache, &options, lock);
    }
    let managed = options.output.is_none();
    let id = format!(
        "{:020}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    if !publication::valid_id(&id) {
        return Err("publication timestamp exceeds the generation identifier bound".into());
    }
    let output = options
        .output
        .clone()
        .unwrap_or_else(|| cache.join("generations").join(&id));
    if output.exists() {
        return Err("output must be a new absolute directory".into());
    }
    let output = if managed {
        fs::create_dir_all(&output)?;
        output
    } else {
        claim_external_output(&cache, &output, |path| {
            fs::create_dir_all(path.parent().ok_or("output has no parent")?)?;
            fs::create_dir(path)?;
            Ok(())
        })?
    };
    if managed {
        fs::create_dir_all(cache.join("leases"))?;
        fs::File::create_new(cache.join("leases").join(&id))?;
    }
    finish_build(
        || build(&root, &cache, &output, &options, managed),
        &cache,
        &output,
        managed,
        &id,
    )
}
fn finish_build(
    build: impl FnOnce() -> Result<String>,
    cache: &Path,
    output: &Path,
    managed: bool,
    id: &str,
) -> Result<()> {
    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(build)) {
        Ok(result) => result.map_err(|error| ("build", error)),
        Err(_) => Err(("panic", "paired build panicked".into())),
    }
    .and_then(|family| {
        if managed {
            publication::publish(cache, id).map_err(|error| ("publication", error))?;
        }
        Ok(family)
    });
    match result {
        Ok(family) => {
            if managed && let Err(cleanup) = publication::collect(cache) {
                eprintln!("publication succeeded; collection: {cleanup}");
            }
            println!(
                "{}",
                json!({"event":"application-distribution-built", "bundle":output.join("bundle"), "family_sha256":family})
            );
            eprintln!(
                "Run: \"{}\" web",
                output.join("bundle").join(executable("rsi")).display()
            );
            Ok(())
        }
        Err((category, error)) => {
            if let Err(evidence) = fs::write(
                output.join("failure.json"),
                json!({"stage":"paired-build", "error_type":category}).to_string(),
            ) {
                eprintln!("failure evidence: {evidence}");
            }
            if managed
                && category != "panic"
                && let Err(cleanup) = publication::collect(cache)
            {
                eprintln!("collection: {cleanup}");
            }
            Err(format!("{error}; evidence: {}", output.display()).into())
        }
    }
}
fn validate_external_output(cache: &Path, output: &Path) -> Result<()> {
    if output
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("output cannot contain parent traversal".into());
    }
    let mut ancestor = output;
    let mut suffix = Vec::new();
    let mut resolved = loop {
        match ancestor.canonicalize() {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or("output has no existing ancestor")?,
                );
                ancestor = ancestor.parent().ok_or("output has no parent")?;
            }
            Err(error) => return Err(error.into()),
        }
    };
    for name in suffix.into_iter().rev() {
        resolved.push(name);
    }
    if resolved.starts_with(cache.canonicalize()?) {
        return Err("external output must be outside the managed publication cache".into());
    }
    Ok(())
}
fn claim_external_output(
    cache: &Path,
    output: &Path,
    claim: impl FnOnce(&Path) -> Result<()>,
) -> Result<PathBuf> {
    validate_external_output(cache, output)?;
    claim(output)?;
    let output = command_path(output.canonicalize()?);
    validate_external_output(cache, &output)?;
    Ok(output)
}
struct Options {
    kind: String,
    output: Option<PathBuf>,
    debug: bool,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let Some(kind) = args
            .first()
            .filter(|s| matches!(s.as_str(), "web" | "desktop" | "gc" | "addon-workbench"))
        else {
            return Err(
                "usage: rsi-app-tools dist web|desktop [ABSOLUTE_OUTPUT] [--debug] | gc | addon-workbench ABSOLUTE_NEW_REPORT".into(),
            );
        };
        let mut value = Self {
            kind: kind.clone(),
            output: None,
            debug: false,
        };
        for arg in &args[1..] {
            if arg == "--debug" && !value.debug {
                value.debug = true;
            } else if value.output.is_none() && Path::new(arg).is_absolute() {
                value.output = Some(arg.into());
            } else if value.output.is_none() && !arg.starts_with('-') {
                return Err(format!("output must be an absolute directory: {arg}").into());
            } else {
                return Err(format!("unknown or duplicate distribution argument: {arg}").into());
            }
        }
        if kind == "gc" && args.len() != 1 {
            return Err("gc takes no output or profile".into());
        }
        if kind == "addon-workbench" && (value.output.is_none() || value.debug) {
            return Err("addon-workbench requires one new absolute report directory".into());
        }
        Ok(value)
    }
}
fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}
fn command_path(path: PathBuf) -> PathBuf {
    // Node's JS module resolver does not consistently accept verbatim Win32 paths.
    // Keep canonical identity, but use the ordinary absolute spelling at process boundaries.
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}").into();
        }
        if let Some(drive) = text.strip_prefix(r"\\?\") {
            return drive.into();
        }
    }
    path
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    use std::io::{ErrorKind, Read};
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(format!("{:x}", digest.finalize())),
            Ok(count) => digest.update(&buffer[..count]),
            Err(error) if error.kind() == ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}
#[allow(clippy::too_many_lines)] // One ordered producer binds captured source, toolchains, compilation and receipt before publication.
fn build(
    root: &Path,
    cache: &Path,
    output: &Path,
    options: &Options,
    managed: bool,
) -> Result<String> {
    let work = cache.join("build");
    let captured = work.join("source");
    fs::create_dir_all(&work)?;
    let manifest_path = work.join("build-family.json");
    let previous = match read_manifest(&manifest_path) {
        Ok(value) => value,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Value::Null
        }
        Err(error) => return Err(error),
    };
    eprintln!("Capturing source bytes...");
    let files = source::capture(root, &captured, previous.get("files"))?;
    let profile = if options.debug { "debug" } else { "release" };
    let mut runner = Runner {
        cwd: captured.clone(),
        environment: environment(cache)?,
        log: fs::File::create(output.join("build.log"))?,
        log_path: output.join("build.log"),
    };
    let rustc = runner.version("rustc", &["-vV"])?;
    let cargo = runner.version("cargo", &["-V"])?;
    let target = rustc
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or("rustc did not report a host")?
        .to_owned();
    let helper_target = helper_target(&target)?;
    let helper = if let Some(target) = &helper_target {
        let cc = std::env::var("RSI_MUSL_CC").unwrap_or_else(|_| "musl-gcc".into());
        if std::env::var_os("RSI_MUSL_CC").is_some() && !Path::new(&cc).is_absolute() {
            return Err("RSI_MUSL_CC must be absolute".into());
        }
        let version = runner.version(&cc, &["--version"])?;
        let compiler = resolve_program(&cc, runner.environment.get(std::ffi::OsStr::new("PATH")))?
            .canonicalize()?;
        let compiler_sha256 = hash_file(&compiler)?;
        runner.environment.insert(
            format!(
                "CARGO_TARGET_{}_LINKER",
                target.to_uppercase().replace('-', "_")
            )
            .into(),
            cc.clone().into(),
        );
        runner
            .environment
            .insert(format!("CC_{}", target.replace('-', "_")).into(), cc.into());
        json!({"target":target,"profile":"release","compiler":version,"compiler_sha256":compiler_sha256})
    } else {
        Value::Null
    };
    let pnpm_script = root.join("target/rsi-tools/pnpm/node_modules/pnpm/bin/pnpm.mjs");
    let pnpm = if pnpm_script.is_file() {
        vec![
            "node".to_owned(),
            pnpm_script.to_string_lossy().into_owned(),
        ]
    } else if cfg!(windows) {
        return Err("missing repository pnpm; run rsi.ps1 setup".into());
    } else {
        vec!["pnpm".into()]
    };
    let bindgen = std::env::var("RSI_WASM_BINDGEN").unwrap_or_else(|_| {
        let path = root
            .join("target/rsi-tools/wasm-bindgen/bin")
            .join(executable("wasm-bindgen"));
        if path.is_file() {
            path.to_string_lossy().into_owned()
        } else {
            "wasm-bindgen".into()
        }
    });
    runner
        .environment
        .insert("RSI_WASM_BINDGEN".into(), bindgen.clone().into());
    let node = runner.version("node", &["--version"])?;
    let pnpm_version = runner.version(
        &pnpm[0],
        &pnpm[1..]
            .iter()
            .map(String::as_str)
            .chain(["--version"])
            .collect::<Vec<_>>(),
    )?;
    let bindgen_version = runner.version(&bindgen, &["--version"])?;
    let package: Value =
        serde_json::from_slice(&fs::read(captured.join("apps/web/package.json"))?)?;
    let expected = package["packageManager"]
        .as_str()
        .ok_or("missing packageManager")?
        .strip_prefix("pnpm@")
        .ok_or("expected pinned pnpm")?;
    if pnpm_version != expected {
        return Err(format!("pnpm {expected} required; run setup (found {pnpm_version})").into());
    }
    let lock: toml::Value = toml::from_str(&fs::read_to_string(captured.join("Cargo.lock"))?)?;
    let expected_bindgen = lock["package"]
        .as_array()
        .and_then(|p| {
            p.iter()
                .find(|p| p["name"].as_str() == Some("wasm-bindgen"))
        })
        .and_then(|p| p["version"].as_str())
        .ok_or("missing wasm-bindgen version")?;
    if bindgen_version != format!("wasm-bindgen {expected_bindgen}") {
        return Err("wasm-bindgen version differs from Cargo.lock; run setup".into());
    }
    let mut tools = json!({"node":node,"pnpm":pnpm_version,"wasm_bindgen":bindgen_version});
    if options.kind == "desktop" {
        tools["gtk3"] = runner
            .version("pkg-config", &["--modversion", "gtk+-3.0"])?
            .into();
        tools["webkitgtk41"] = runner
            .version("pkg-config", &["--modversion", "webkit2gtk-4.1"])?
            .into();
    }
    let mut packages = vec!["rsi-cli", "rsi-web"];
    if options.kind == "desktop" {
        packages.push("rsi-desktop");
    }
    if helper_target.is_some() {
        packages.push("rsi-ssh-helper-app");
    }
    let manifest = json!({"format":1,"files":files,"rustc":rustc,"cargo":cargo,"target":target,"worker_target":"wasm32-unknown-unknown","profile":profile,"helper":helper,"document_toolchain":tools,"packages":packages,"default_features":true,"features":[],"managed":managed,"flags":{"RUSTFLAGS":std::env::var("RUSTFLAGS").unwrap_or_default(),"CARGO_ENCODED_RUSTFLAGS":std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default()}});
    let bytes = serde_json::to_vec(&manifest)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("source manifest exceeds 16 MiB".into());
    }
    source::put(&manifest_path, &bytes)?;
    let family = hash(&bytes);
    runner.environment.insert(
        "RSI_BUILD_FAMILY_MANIFEST".into(),
        manifest_path.into_os_string(),
    );
    runner
        .environment
        .insert("RSI_BUILD_FAMILY_SHA256".into(), family.clone().into());
    runner.environment.insert(
        "CARGO_TARGET_DIR".into(),
        work.join("target").into_os_string(),
    );
    let bundle = output.join("bundle");
    fs::create_dir(&bundle)?;
    if managed {
        fs::write(bundle.join(".rsi-generation.lock"), b"")?;
    }
    source::verify(&captured, &files)?;
    let mut native = vec![
        "build", "--locked", "--target", &target, "-p", "rsi-cli", "--bins",
    ];
    if options.kind == "desktop" {
        native.extend(["-p", "rsi-desktop"]);
    }
    if !options.debug {
        native.push("--release");
    }
    runner.run("cargo", &native)?;
    if let Some(target) = &helper_target {
        runner.run(
            "cargo",
            &[
                "build",
                "--locked",
                "--target",
                target,
                "-p",
                "rsi-ssh-helper-app",
                "--release",
            ],
        )?;
    }
    runner.cwd = captured.join("apps/web");
    let store = cache.join("pnpm-store");
    let mut install = pnpm[1..].iter().map(String::as_str).collect::<Vec<_>>();
    install.extend([
        "install",
        "--frozen-lockfile",
        "--ignore-scripts",
        "--store-dir",
        store.to_str().ok_or("non-Unicode store path")?,
    ]);
    runner.run(&pnpm[0], &install)?;
    runner.cwd.clone_from(&captured);
    let assets = bundle.join("assets");
    let mut args = vec![
        "apps/web/build.mjs",
        assets.to_str().ok_or("non-Unicode output path")?,
    ];
    if options.debug {
        args.push("--dev");
    }
    runner.run("node", &args)?;
    source::verify(&captured, &files)?;
    let mut artifacts = BTreeMap::new();
    let mut binaries = vec!["rsi"];
    if options.kind == "desktop" {
        binaries.push("rsi-desktop");
    }
    for binary in binaries {
        let name = executable(binary);
        let destination = bundle.join(&name);
        fs::copy(
            work.join("target").join(&target).join(profile).join(&name),
            &destination,
        )?;
        source::executable(&destination)?;
        artifacts.insert(name, hash_file(&destination)?);
    }
    if let Some(target) = &helper_target {
        let destination = bundle.join("rsi-ssh-helper");
        fs::copy(
            work.join("target")
                .join(target)
                .join("release/rsi-ssh-helper"),
            &destination,
        )?;
        source::executable(&destination)?;
        artifacts.insert("rsi-ssh-helper".into(), hash_file(&destination)?);
    }
    for entry in fs::read_dir(&assets)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            artifacts.insert(
                format!("assets/{}", entry.file_name().to_string_lossy()),
                hash_file(&entry.path())?,
            );
        }
    }
    fs::write(bundle.join("build-family.json"), &bytes)?;
    fs::write(
        bundle.join("receipt.json"),
        serde_json::to_vec_pretty(
            &json!({"format":2,"family_sha256":family,"target":target,"profile":profile,"kind":options.kind,"mode":"published","helper_target":helper_target,"artifacts":artifacts}),
        )?,
    )?;
    Ok(family)
}
fn helper_target(native: &str) -> Result<Option<String>> {
    if native == "x86_64-pc-windows-msvc" {
        return Ok(None);
    }
    let cpu = native.split('-').next().ok_or("missing target CPU")?;
    if matches!(cpu, "x86_64" | "aarch64") && native.contains("-linux-") {
        Ok(Some(format!("{cpu}-unknown-linux-musl")))
    } else {
        Err(format!("unsupported publication target: {native}").into())
    }
}
fn environment(cache: &Path) -> Result<BTreeMap<OsString, OsString>> {
    let mut values = BTreeMap::new();
    for key in [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "SystemDrive",
        "WINDIR",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
        "CommonProgramFiles",
        "CommonProgramFiles(x86)",
        "COMSPEC",
        "LIB",
        "LIBPATH",
        "INCLUDE",
        "VCINSTALLDIR",
        "VCToolsInstallDir",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "CARGO_BUILD_JOBS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
    ] {
        if let Some(value) = std::env::var_os(key) {
            values.insert(key.into(), value);
        }
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or("missing user home")?;
    for (key, suffix) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
        values
            .entry(key.into())
            .or_insert_with(|| Path::new(&home).join(suffix).into_os_string());
    }
    for (key, suffix) in [
        ("HOME", "build-home"),
        ("USERPROFILE", "build-home"),
        ("APPDATA", "build-home/appdata"),
        ("LOCALAPPDATA", "build-home/local"),
        ("TEMP", "tmp"),
        ("TMP", "tmp"),
        ("XDG_CACHE_HOME", "tool-cache"),
        ("COREPACK_HOME", "corepack"),
    ] {
        let path = cache.join(suffix);
        fs::create_dir_all(&path)?;
        values.insert(key.into(), path.into_os_string());
    }
    values.insert("CI".into(), "true".into());
    Ok(values)
}
struct Runner {
    cwd: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    log: fs::File,
    log_path: PathBuf,
}
fn read_manifest(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&manifest_bytes(path)?)?)
}
fn manifest_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("source manifest exceeds 16 MiB".into());
    }
    Ok(bytes)
}
fn resolve_program(name: &str, path: Option<&OsString>) -> Result<PathBuf> {
    if Path::new(name).is_absolute() {
        return Ok(name.into());
    }
    std::env::split_paths(path.ok_or("missing tool PATH")?)
        .map(|part| part.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("tool not found: {name}").into())
}
impl Runner {
    fn command(&self, program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.environment);
        command
    }
    fn version(&self, program: &str, args: &[&str]) -> Result<String> {
        let result = self
            .command(program, args)
            .output()
            .map_err(|e| format!("{program}: {e}; run the source entrypoint's doctor"))?;
        if !result.status.success() {
            return Err(format!(
                "{program} version check failed: {}",
                String::from_utf8_lossy(&result.stderr)
            )
            .into());
        }
        Ok(String::from_utf8(result.stdout)?.trim().into())
    }
    fn run(&mut self, program: &str, args: &[&str]) -> Result<()> {
        eprintln!(
            "{} {} (log: {})",
            program,
            args.join(" "),
            self.log_path.display()
        );
        let status = self
            .command(program, args)
            .stdin(Stdio::null())
            .stdout(self.log.try_clone()?)
            .stderr(self.log.try_clone()?)
            .status()?;
        if !status.success() {
            return Err(format!("{program} failed: {status}").into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panics_retain_evidence_without_attempting_collection() {
        let root = tempfile::tempdir().unwrap();
        for id in 1..=3 {
            let bundle = root
                .path()
                .join("generations")
                .join(format!("{id:020}"))
                .join("bundle");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(bundle.join("receipt.json"), b"{}").unwrap();
        }
        let id = "00000000000000000004";
        let output = root.path().join("generations").join(id);
        fs::create_dir(&output).unwrap();
        assert!(finish_build(|| panic!("fixture panic"), root.path(), &output, true, id).is_err());
        assert_eq!(
            read_manifest(&output.join("failure.json")).unwrap()["error_type"],
            "panic"
        );
        assert!(
            root.path()
                .join("generations/00000000000000000001")
                .exists()
        );
        assert!(!root.path().join("trash").exists());
    }
    #[test]
    fn successful_publication_is_not_reclassified_by_collection_failure() {
        let root = tempfile::tempdir().unwrap();
        let id = "00000000000000000001";
        let output = root.path().join("generations").join(id);
        fs::create_dir_all(output.join("bundle")).unwrap();
        fs::write(output.join("bundle/receipt.json"), b"{}").unwrap();
        fs::create_dir_all(root.path().join("trash/foreign")).unwrap();
        finish_build(|| Ok("family".into()), root.path(), &output, true, id).unwrap();
        assert_eq!(
            publication::current(root.path()).unwrap().as_deref(),
            Some(id)
        );
        assert!(!output.join("failure.json").exists());
    }
    #[test]
    fn publication_errors_and_panics_preserve_failure_categories() {
        let root = tempfile::tempdir().unwrap();
        let id = "00000000000000000001";
        publication::publish(root.path(), id).unwrap();
        for category in ["publication", "panic"] {
            let output = root.path().join(category);
            fs::create_dir(&output).unwrap();
            let result = finish_build(
                || {
                    assert!(category != "panic", "fixture build panic");
                    Ok("family".into())
                },
                root.path(),
                &output,
                true,
                "invalid",
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(&output.display().to_string())
            );
            let evidence: Value =
                serde_json::from_slice(&fs::read(output.join("failure.json")).unwrap()).unwrap();
            assert_eq!(evidence["error_type"], category);
            assert_eq!(
                publication::current(root.path()).unwrap().as_deref(),
                Some(id)
            );
        }
    }
    #[test]
    fn external_outputs_cannot_enter_managed_cache_even_through_aliases() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        for output in [
            cache.clone(),
            cache.join("generations/notes"),
            cache.join("generations/00000000000000000001"),
            cache.join("missing/../escape"),
        ] {
            assert!(
                validate_external_output(&cache, &output).is_err(),
                "{}",
                output.display()
            );
        }
        validate_external_output(&cache, &root.path().join("external/nested/new")).unwrap();
        let alias = root.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cache, &alias).unwrap();
        #[cfg(windows)]
        assert!(
            Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(&alias)
                .arg(&cache)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(any(unix, windows))]
        assert!(validate_external_output(&cache, &alias.join("generations/notes")).is_err());
        assert!(!cache.join("generations").exists());
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn external_output_rechecks_an_alias_replaced_during_creation() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let outside = root.path().join("outside");
        let alias = root.path().join("alias");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&outside).unwrap();
        let link = |target: &Path| {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &alias).unwrap();
            #[cfg(windows)]
            assert!(
                Command::new("cmd.exe")
                    .args(["/C", "mklink", "/J"])
                    .arg(&alias)
                    .arg(target)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        link(&outside);
        let output = alias.join("new-report");
        let error = claim_external_output(&cache, &output, |path| {
            #[cfg(unix)]
            fs::remove_file(&alias)?;
            #[cfg(windows)]
            fs::remove_dir(&alias)?;
            link(&cache);
            fs::create_dir(path)?;
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("outside the managed"));
        assert_eq!(fs::read_dir(cache.join("new-report")).unwrap().count(), 0);
        assert!(!outside.join("new-report").exists());
        let existing = root.path().join("existing");
        fs::create_dir(&existing).unwrap();
        assert!(
            claim_external_output(&cache, &existing, |path| Ok(fs::create_dir(path)?)).is_err()
        );
    }
    #[test]
    fn streamed_file_hash_matches_sha256_vectors_across_multiple_buffers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("artifact");
        assert!(hash_file(&path).is_err());
        fs::write(&path, b"").unwrap();
        assert_eq!(
            hash_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        fs::write(&path, vec![b'a'; 1_000_000]).unwrap();
        assert_eq!(
            hash_file(&path).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
    #[test]
    fn failed_evidence_write_preserves_build_error_and_still_collects() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path();
        for id in [
            "00000000000000000001",
            "00000000000000000002",
            "00000000000000000003",
        ] {
            let bundle = cache.join("generations").join(id).join("bundle");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(bundle.join("receipt.json"), b"{}").unwrap();
        }
        publication::publish(cache, "00000000000000000003").unwrap();
        let output = cache.join("generations/00000000000000000004");
        fs::create_dir_all(output.join("failure.json")).unwrap();
        let error = finish_build(
            || Err("original compilation failure".into()),
            cache,
            &output,
            true,
            "00000000000000000004",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("original compilation failure"), "{error}");
        assert!(error.contains(&output.display().to_string()), "{error}");
        assert!(!cache.join("generations/00000000000000000001").exists());
        assert_eq!(
            publication::current(cache).unwrap().as_deref(),
            Some("00000000000000000003")
        );
    }
    #[test]
    fn cached_manifests_are_bounded_before_json_admission() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("manifest.json");
        fs::File::create(&path)
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            read_manifest(&path)
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
        fs::write(&path, b"not JSON").unwrap();
        assert!(read_manifest(&path).is_err());
    }
    #[test]
    fn target_capabilities_and_arguments_are_explicit() {
        assert!(
            Options::parse(&["addon-workbench".into(), "relative/report".into()])
                .err()
                .unwrap()
                .to_string()
                .contains("absolute directory")
        );
        assert_eq!(helper_target("x86_64-pc-windows-msvc").unwrap(), None);
        assert_eq!(
            helper_target("aarch64-unknown-linux-gnu").unwrap(),
            Some("aarch64-unknown-linux-musl".into())
        );
        assert!(helper_target("aarch64-pc-windows-msvc").is_err());
        assert!(Options::parse(&["web".into(), "--debug".into(), "--debug".into()]).is_err());
        assert!(Options::parse(&["gc".into(), "--debug".into()]).is_err());
    }
}
